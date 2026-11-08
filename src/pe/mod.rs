//! Portable Executable inspection.
//!
//! A deliberately small, fully bounds-checked reader for exactly the parts of
//! the PE/COFF format Lucent Bridge needs: machine type (including ARM64EC /
//! ARM64X detection through the load-config CHPE pointer), subsystem,
//! resources (version info and icons) and the overlay offset used by
//! installer detection. Generic object-file crates don't expose CHPE
//! metadata or overlay boundaries, and the format subset is small enough to
//! audit in one sitting.

pub mod bytes;
pub mod icon;
pub mod version;

use crate::arch::ExeArch;
use bytes::{u16_at, u32_at, u64_at, utf16_n_at};
use serde::Serialize;

pub const MACHINE_I386: u16 = 0x014c;
pub const MACHINE_AMD64: u16 = 0x8664;
pub const MACHINE_ARM64: u16 = 0xaa64;
pub const MACHINE_ARM64EC: u16 = 0xa641;
pub const MACHINE_ARM64X: u16 = 0xa64e;

const DIR_RESOURCE: usize = 2;
const DIR_LOAD_CONFIG: usize = 10;
const DIR_CLR: usize = 14;

pub const RT_ICON: u16 = 3;
pub const RT_GROUP_ICON: u16 = 14;
pub const RT_VERSION: u16 = 16;
pub const RT_MANIFEST: u16 = 24;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeError {
    NotMz,
    NotPe,
    Truncated(&'static str),
    BadOptionalHeader(u16),
}

impl std::fmt::Display for PeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PeError::NotMz => f.write_str("not a DOS/Windows executable (no MZ header)"),
            PeError::NotPe => f.write_str("MZ file without a PE header (16-bit DOS program?)"),
            PeError::Truncated(what) => write!(f, "file is truncated ({what})"),
            PeError::BadOptionalHeader(m) => write!(f, "unknown optional header magic 0x{m:x}"),
        }
    }
}

impl std::error::Error for PeError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Subsystem {
    Gui,
    Console,
    Other(u16),
}

#[derive(Debug, Clone)]
pub struct Section {
    pub name: String,
    pub virtual_address: u32,
    pub virtual_size: u32,
    pub raw_offset: u32,
    pub raw_size: u32,
}

/// Resource name or numeric ID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResName {
    Id(u16),
    Name(String),
}

#[derive(Debug, Clone)]
pub struct Resource<'a> {
    pub name: ResName,
    pub lang: u16,
    pub data: &'a [u8],
}

/// A parsed PE image borrowing the file bytes.
pub struct Pe<'a> {
    pub data: &'a [u8],
    pub raw_machine: u16,
    pub pe32_plus: bool,
    pub subsystem: Subsystem,
    pub is_dll: bool,
    pub sections: Vec<Section>,
    dirs: Vec<(u32, u32)>,
}

impl<'a> Pe<'a> {
    pub fn parse(data: &'a [u8]) -> Result<Pe<'a>, PeError> {
        if data.len() < 2 || &data[..2] != b"MZ" {
            return Err(PeError::NotMz);
        }
        let e_lfanew = u32_at(data, 0x3c).ok_or(PeError::Truncated("DOS header"))? as usize;
        if data.get(e_lfanew..e_lfanew.saturating_add(4)) != Some(b"PE\0\0") {
            return Err(PeError::NotPe);
        }
        let coff = e_lfanew + 4;
        let t = PeError::Truncated("COFF header");
        let raw_machine = u16_at(data, coff).ok_or(t.clone())?;
        let nsections = u16_at(data, coff + 2).ok_or(t.clone())? as usize;
        let opt_size = u16_at(data, coff + 16).ok_or(t.clone())? as usize;
        let characteristics = u16_at(data, coff + 18).ok_or(t)?;
        let opt = coff + 20;
        let t = PeError::Truncated("optional header");
        let magic = u16_at(data, opt).ok_or(t.clone())?;
        let (pe32_plus, nrva_off, dirs_off) = match magic {
            0x10b => (false, 92, 96),
            0x20b => (true, 108, 112),
            m => return Err(PeError::BadOptionalHeader(m)),
        };
        let subsystem = match u16_at(data, opt + 68).ok_or(t.clone())? {
            2 => Subsystem::Gui,
            3 => Subsystem::Console,
            s => Subsystem::Other(s),
        };
        let nrva = (u32_at(data, opt + nrva_off).ok_or(t)? as usize).min(16);
        let mut dirs = Vec::with_capacity(nrva);
        for i in 0..nrva {
            let at = opt + dirs_off + i * 8;
            if at + 8 > opt + opt_size {
                break;
            }
            match (u32_at(data, at), u32_at(data, at + 4)) {
                (Some(rva), Some(size)) => dirs.push((rva, size)),
                _ => break,
            }
        }
        let mut sections = Vec::with_capacity(nsections.min(96));
        let sh0 = opt + opt_size;
        for i in 0..nsections.min(96) {
            let sh = sh0 + i * 40;
            let Some(name_bytes) = data.get(sh..sh + 8) else {
                return Err(PeError::Truncated("section table"));
            };
            let name = String::from_utf8_lossy(name_bytes)
                .trim_end_matches('\0')
                .to_string();
            let t = PeError::Truncated("section table");
            sections.push(Section {
                name,
                virtual_size: u32_at(data, sh + 8).ok_or(t.clone())?,
                virtual_address: u32_at(data, sh + 12).ok_or(t.clone())?,
                raw_size: u32_at(data, sh + 16).ok_or(t.clone())?,
                raw_offset: u32_at(data, sh + 20).ok_or(t)?,
            });
        }
        Ok(Pe {
            data,
            raw_machine,
            pe32_plus,
            subsystem,
            is_dll: characteristics & 0x2000 != 0,
            sections,
            dirs,
        })
    }

    fn dir(&self, idx: usize) -> Option<(u32, u32)> {
        self.dirs.get(idx).copied().filter(|(rva, _)| *rva != 0)
    }

    /// Translate an RVA into a file offset.
    pub fn rva_to_offset(&self, rva: u32) -> Option<usize> {
        for s in &self.sections {
            let span = s.virtual_size.max(s.raw_size);
            if rva >= s.virtual_address && rva < s.virtual_address.saturating_add(span) {
                let delta = rva - s.virtual_address;
                if delta >= s.raw_size {
                    return None; // in uninitialised (virtual-only) tail
                }
                return Some(s.raw_offset as usize + delta as usize);
            }
        }
        None
    }

    /// Bytes at `rva` (length `len`), if fully backed by the file.
    pub fn data_at_rva(&self, rva: u32, len: usize) -> Option<&'a [u8]> {
        let off = self.rva_to_offset(rva)?;
        bytes::slice_at(self.data, off, len)
    }

    /// File offset where the mapped image ends and appended data begins.
    pub fn overlay_offset(&self) -> usize {
        self.sections
            .iter()
            .map(|s| s.raw_offset as usize + s.raw_size as usize)
            .max()
            .unwrap_or(0)
            .min(self.data.len())
    }

    /// Whether the load-config directory carries a CHPE metadata pointer,
    /// which marks ARM64EC (x64 machine field) and ARM64X (ARM64 machine
    /// field) images.
    pub fn has_chpe_metadata(&self) -> bool {
        if !self.pe32_plus {
            return false;
        }
        let Some((rva, _)) = self.dir(DIR_LOAD_CONFIG) else {
            return false;
        };
        let Some(off) = self.rva_to_offset(rva) else {
            return false;
        };
        let size = u32_at(self.data, off).unwrap_or(0) as usize;
        if size < 208 {
            return false;
        }
        u64_at(self.data, off + 200).unwrap_or(0) != 0
    }

    pub fn is_dotnet(&self) -> bool {
        self.dir(DIR_CLR).is_some()
    }

    pub fn arch(&self) -> ExeArch {
        match self.raw_machine {
            MACHINE_I386 => ExeArch::I386,
            MACHINE_AMD64 if self.has_chpe_metadata() => ExeArch::Arm64ec,
            MACHINE_AMD64 => ExeArch::X86_64,
            MACHINE_ARM64 if self.has_chpe_metadata() => ExeArch::Arm64x,
            MACHINE_ARM64 => ExeArch::Arm64,
            MACHINE_ARM64EC => ExeArch::Arm64ec,
            MACHINE_ARM64X => ExeArch::Arm64x,
            m => ExeArch::Unknown(m),
        }
    }

    /// All resources of the given type, in directory order (named entries
    /// first, then ascending IDs — the order Explorer uses).
    pub fn resources(&self, type_id: u16) -> Vec<Resource<'a>> {
        let mut out = Vec::new();
        let Some((rva, _)) = self.dir(DIR_RESOURCE) else {
            return out;
        };
        let Some(base) = self.rva_to_offset(rva) else {
            return out;
        };
        let Some(rsrc) = self.data.get(base..) else {
            return out;
        };
        for (name, off, is_dir) in self.res_entries(rsrc, 0) {
            if name != ResName::Id(type_id) || !is_dir {
                continue;
            }
            for (name, off, is_dir) in self.res_entries(rsrc, off) {
                if !is_dir {
                    continue;
                }
                for (lang, off, is_dir) in self.res_entries(rsrc, off) {
                    if is_dir {
                        continue;
                    }
                    let lang = match lang {
                        ResName::Id(l) => l,
                        ResName::Name(_) => 0,
                    };
                    let (Some(drva), Some(dsize)) = (u32_at(rsrc, off), u32_at(rsrc, off + 4))
                    else {
                        continue;
                    };
                    if let Some(data) = self.data_at_rva(drva, dsize as usize) {
                        out.push(Resource {
                            name: name.clone(),
                            lang,
                            data,
                        });
                    }
                }
            }
        }
        out
    }

    /// Entries of one resource directory: (name, offset, is_subdirectory).
    fn res_entries(&self, rsrc: &[u8], dir: usize) -> Vec<(ResName, usize, bool)> {
        let (Some(named), Some(ids)) = (u16_at(rsrc, dir + 12), u16_at(rsrc, dir + 14)) else {
            return Vec::new();
        };
        let count = (named as usize + ids as usize).min(4096);
        let mut out = Vec::with_capacity(count);
        for i in 0..count {
            let e = dir + 16 + i * 8;
            let (Some(name), Some(target)) = (u32_at(rsrc, e), u32_at(rsrc, e + 4)) else {
                break;
            };
            let name = if name & 0x8000_0000 != 0 {
                let at = (name & 0x7fff_ffff) as usize;
                let len = u16_at(rsrc, at).unwrap_or(0) as usize;
                ResName::Name(utf16_n_at(rsrc, at + 2, len).unwrap_or_default())
            } else {
                ResName::Id(name as u16)
            };
            let is_dir = target & 0x8000_0000 != 0;
            let off = (target & 0x7fff_ffff) as usize;
            // A directory pointing at itself or backwards would loop forever
            // in a recursive walker; our walker is fixed-depth, but reject
            // obviously bogus offsets anyway.
            if off >= rsrc.len() {
                continue;
            }
            out.push((name, off, is_dir));
        }
        out
    }

    /// The application manifest, if any (UTF-8 XML).
    pub fn manifest(&self) -> Option<String> {
        self.resources(RT_MANIFEST)
            .first()
            .map(|r| String::from_utf8_lossy(r.data).into_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testpe::{self, PeBuilder, ResKey};

    #[test]
    fn rejects_non_pe() {
        assert_eq!(Pe::parse(b"").err(), Some(PeError::NotMz));
        assert_eq!(Pe::parse(b"\x7fELF....").err(), Some(PeError::NotMz));
        let mut dos = vec![0u8; 0x80];
        dos[0] = b'M';
        dos[1] = b'Z';
        dos[0x3c] = 0x40;
        assert_eq!(Pe::parse(&dos).err(), Some(PeError::NotPe));
    }

    #[test]
    fn machine_types() {
        let cases = [
            (PeBuilder::new(testpe::MACHINE_I386), ExeArch::I386),
            (PeBuilder::new(testpe::MACHINE_AMD64), ExeArch::X86_64),
            (PeBuilder::new(testpe::MACHINE_ARM64), ExeArch::Arm64),
            (
                PeBuilder::new(testpe::MACHINE_AMD64).chpe(),
                ExeArch::Arm64ec,
            ),
            (
                PeBuilder::new(testpe::MACHINE_ARM64).chpe(),
                ExeArch::Arm64x,
            ),
            (PeBuilder::new(testpe::MACHINE_ARM64EC), ExeArch::Arm64ec),
            (PeBuilder::new(0x0200), ExeArch::Unknown(0x0200)),
        ];
        for (b, want) in cases {
            let bytes = b.build();
            let pe = Pe::parse(&bytes).unwrap();
            assert_eq!(pe.arch(), want, "machine 0x{:x}", b.machine);
        }
    }

    #[test]
    fn pe32_vs_pe32plus_and_subsystem() {
        let b = PeBuilder::new(testpe::MACHINE_I386).subsystem(testpe::SUBSYSTEM_CONSOLE);
        let bytes = b.build();
        let pe = Pe::parse(&bytes).unwrap();
        assert!(!pe.pe32_plus);
        assert_eq!(pe.subsystem, Subsystem::Console);
        let bytes = PeBuilder::new(testpe::MACHINE_AMD64).build();
        let pe = Pe::parse(&bytes).unwrap();
        assert!(pe.pe32_plus);
        assert_eq!(pe.subsystem, Subsystem::Gui);
        assert!(!pe.is_dll);
    }

    #[test]
    fn chpe_on_pe32_is_ignored() {
        // CHPE pointer only exists in the 64-bit load-config layout.
        let mut b = PeBuilder::new(testpe::MACHINE_I386).chpe();
        b.pe32_plus = false;
        let bytes = b.build();
        assert_eq!(Pe::parse(&bytes).unwrap().arch(), ExeArch::I386);
    }

    #[test]
    fn dotnet_detection() {
        let bytes = PeBuilder::new(testpe::MACHINE_I386).dotnet().build();
        assert!(Pe::parse(&bytes).unwrap().is_dotnet());
        let bytes = PeBuilder::new(testpe::MACHINE_I386).build();
        assert!(!Pe::parse(&bytes).unwrap().is_dotnet());
    }

    #[test]
    fn overlay_offset_points_past_last_section() {
        let bytes = PeBuilder::new(testpe::MACHINE_I386)
            .overlay(b"APPENDED")
            .build();
        let pe = Pe::parse(&bytes).unwrap();
        assert_eq!(&bytes[pe.overlay_offset()..], b"APPENDED");
    }

    #[test]
    fn resources_by_id_and_name() {
        let bytes = PeBuilder::new(testpe::MACHINE_AMD64)
            .resource(ResKey::Id(10), ResKey::Id(7), 0x0409, b"seven".to_vec())
            .resource(
                ResKey::Id(10),
                ResKey::Name("NAMED".into()),
                0x0419,
                b"named".to_vec(),
            )
            .resource(
                ResKey::Id(RT_MANIFEST),
                ResKey::Id(1),
                0,
                b"<xml/>".to_vec(),
            )
            .build();
        let pe = Pe::parse(&bytes).unwrap();
        let rs = pe.resources(10);
        assert_eq!(rs.len(), 2);
        assert_eq!(rs[0].name, ResName::Name("NAMED".into()));
        assert_eq!(rs[0].lang, 0x0419);
        assert_eq!(rs[0].data, b"named");
        assert_eq!(rs[1].name, ResName::Id(7));
        assert_eq!(rs[1].data, b"seven");
        assert_eq!(pe.manifest().as_deref(), Some("<xml/>"));
        assert!(pe.resources(99).is_empty());
    }

    #[test]
    fn truncated_files_never_panic() {
        let bytes = PeBuilder::new(testpe::MACHINE_AMD64)
            .chpe()
            .version([1, 2, 3, 4], &[("ProductName", "X")])
            .icon_group(ResKey::Id(1), 1, &[testpe::png_icon(16, [1, 2, 3, 255])])
            .build();
        for cut in (0..bytes.len()).step_by(7) {
            let slice = &bytes[..cut];
            if let Ok(pe) = Pe::parse(slice) {
                let _ = pe.arch();
                let _ = pe.resources(RT_VERSION);
                let _ = version::VersionInfo::from_pe(&pe);
                let _ = icon::best_icon_png(&pe);
                let _ = pe.overlay_offset();
            }
        }
    }

    #[test]
    fn garbage_resource_directory_is_tolerated() {
        // Point the resource directory at a section full of 0xff.
        let mut bytes = PeBuilder::new(testpe::MACHINE_AMD64)
            .resource(ResKey::Id(RT_VERSION), ResKey::Id(1), 0, vec![0; 8])
            .build();
        let pe = Pe::parse(&bytes).unwrap();
        let rsrc = pe.sections.iter().find(|s| s.name == ".rsrc").unwrap();
        let (a, b) = (
            rsrc.raw_offset as usize,
            (rsrc.raw_offset + rsrc.raw_size) as usize,
        );
        bytes[a..b].fill(0xff);
        let pe = Pe::parse(&bytes).unwrap();
        assert!(pe.resources(RT_VERSION).is_empty());
    }
}
