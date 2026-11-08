//! Synthetic PE / LNK fixture builders.
//!
//! Tests (unit and integration) generate tiny but structurally valid Windows
//! binaries with exactly the features under test, so no third-party `.exe`
//! files are ever committed to the repository.

use std::collections::BTreeMap;

pub const MACHINE_I386: u16 = 0x014c;
pub const MACHINE_AMD64: u16 = 0x8664;
pub const MACHINE_ARM64: u16 = 0xaa64;
pub const MACHINE_ARM64EC: u16 = 0xa641;

pub const SUBSYSTEM_GUI: u16 = 2;
pub const SUBSYSTEM_CONSOLE: u16 = 3;

pub const RT_ICON: u16 = 3;
pub const RT_GROUP_ICON: u16 = 14;
pub const RT_VERSION: u16 = 16;
pub const RT_MANIFEST: u16 = 24;

const FILE_ALIGN: usize = 0x200;
const SECT_ALIGN: usize = 0x1000;
const HEADERS_SIZE: usize = 0x400;

/// Resource name: integer ID or string. Named entries sort before IDs.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ResKey {
    Name(String),
    Id(u16),
}

#[derive(Clone, Debug)]
pub struct PeBuilder {
    pub machine: u16,
    pub pe32_plus: bool,
    pub subsystem: u16,
    pub dll: bool,
    /// Emit a load-config directory with a non-zero CHPEMetadataPointer
    /// (marks ARM64EC / ARM64X images). Only meaningful for PE32+.
    pub chpe: bool,
    /// Emit a CLR (COM descriptor) data directory, i.e. a .NET assembly.
    pub dotnet: bool,
    pub resources: BTreeMap<ResKey, BTreeMap<ResKey, BTreeMap<u16, Vec<u8>>>>,
    pub extra_sections: Vec<(String, Vec<u8>)>,
    pub overlay: Vec<u8>,
}

impl PeBuilder {
    pub fn new(machine: u16) -> Self {
        PeBuilder {
            machine,
            pe32_plus: machine != MACHINE_I386,
            subsystem: SUBSYSTEM_GUI,
            dll: false,
            chpe: false,
            dotnet: false,
            resources: BTreeMap::new(),
            extra_sections: Vec::new(),
            overlay: Vec::new(),
        }
    }

    pub fn subsystem(mut self, s: u16) -> Self {
        self.subsystem = s;
        self
    }

    pub fn chpe(mut self) -> Self {
        self.chpe = true;
        self
    }

    pub fn dotnet(mut self) -> Self {
        self.dotnet = true;
        self
    }

    pub fn resource(mut self, ty: ResKey, name: ResKey, lang: u16, data: Vec<u8>) -> Self {
        self.resources
            .entry(ty)
            .or_default()
            .entry(name)
            .or_default()
            .insert(lang, data);
        self
    }

    pub fn version(self, fixed: [u16; 4], strings: &[(&str, &str)]) -> Self {
        self.resource(
            ResKey::Id(RT_VERSION),
            ResKey::Id(1),
            0x0409,
            version_resource(fixed, strings),
        )
    }

    /// Add one icon group (named or numbered) built from the given icon
    /// images. Icon IDs start at `first_icon_id`.
    pub fn icon_group(mut self, group: ResKey, first_icon_id: u16, images: &[Vec<u8>]) -> Self {
        let mut grp = Vec::new();
        grp.extend_from_slice(&0u16.to_le_bytes());
        grp.extend_from_slice(&1u16.to_le_bytes());
        grp.extend_from_slice(&(images.len() as u16).to_le_bytes());
        for (i, img) in images.iter().enumerate() {
            let (w, h, bpp) = icon_image_dims(img);
            grp.push(if w >= 256 { 0 } else { w as u8 });
            grp.push(if h >= 256 { 0 } else { h as u8 });
            grp.push(0); // colour count
            grp.push(0); // reserved
            grp.extend_from_slice(&1u16.to_le_bytes()); // planes
            grp.extend_from_slice(&bpp.to_le_bytes());
            grp.extend_from_slice(&(img.len() as u32).to_le_bytes());
            grp.extend_from_slice(&(first_icon_id + i as u16).to_le_bytes());
            self = self.resource(
                ResKey::Id(RT_ICON),
                ResKey::Id(first_icon_id + i as u16),
                0x0409,
                img.clone(),
            );
        }
        self.resource(ResKey::Id(RT_GROUP_ICON), group, 0x0409, grp)
    }

    pub fn section(mut self, name: &str, data: Vec<u8>) -> Self {
        self.extra_sections.push((name.to_string(), data));
        self
    }

    pub fn overlay(mut self, data: &[u8]) -> Self {
        self.overlay.extend_from_slice(data);
        self
    }

    pub fn build(&self) -> Vec<u8> {
        // Section contents, in order. RVAs are assigned sequentially.
        struct Sect {
            name: String,
            data: Vec<u8>,
            rva: usize,
            raw: usize,
            chars: u32,
        }
        let mut sects: Vec<Sect> = Vec::new();
        let mut next_rva = SECT_ALIGN;
        let mut next_raw = HEADERS_SIZE;
        let mut push = |name: &str, data: Vec<u8>, chars: u32, sects: &mut Vec<Sect>| {
            let rva = next_rva;
            let raw = next_raw;
            next_rva += align(data.len().max(1), SECT_ALIGN);
            next_raw += align(data.len().max(1), FILE_ALIGN);
            sects.push(Sect {
                name: name.to_string(),
                data,
                rva,
                raw,
                chars,
            });
            (rva, next_rva)
        };

        let (text_rva, mut end_rva) = push(".text", vec![0xc3; 16], 0x6000_0020, &mut sects);
        let mut load_config = None;
        if self.chpe {
            let mut lc = vec![0u8; 256];
            lc[0..4].copy_from_slice(&256u32.to_le_bytes());
            // CHPEMetadataPointer (a VA) at offset 200 of the 64-bit layout.
            lc[200..208].copy_from_slice(&0x1_4000_2100u64.to_le_bytes());
            let (rva, end) = push(".rdata", lc, 0x4000_0040, &mut sects);
            end_rva = end;
            load_config = Some((rva, 256u32));
        }
        let mut rsrc_dir = None;
        if !self.resources.is_empty() {
            let rva = end_rva;
            let data = build_rsrc(rva as u32, &self.resources);
            let len = data.len() as u32;
            let (_, end) = push(".rsrc", data, 0x4000_0040, &mut sects);
            end_rva = end;
            rsrc_dir = Some((rva, len));
        }
        for (name, data) in &self.extra_sections {
            let (_, end) = push(name, data.clone(), 0x4000_0040, &mut sects);
            end_rva = end;
        }
        let size_of_image = end_rva;

        let mut out = vec![0u8; HEADERS_SIZE];
        out[0] = b'M';
        out[1] = b'Z';
        let e_lfanew = 0x40usize;
        out[0x3c..0x40].copy_from_slice(&(e_lfanew as u32).to_le_bytes());
        out[e_lfanew..e_lfanew + 4].copy_from_slice(b"PE\0\0");
        let coff = e_lfanew + 4;
        let opt_size: usize = if self.pe32_plus { 240 } else { 224 };
        put16(&mut out, coff, self.machine);
        put16(&mut out, coff + 2, sects.len() as u16);
        put32(&mut out, coff + 4, 0x6000_0000); // timestamp
        put16(&mut out, coff + 16, opt_size as u16);
        let mut chars: u16 = 0x0002; // executable
        chars |= if self.pe32_plus { 0x0020 } else { 0x0100 };
        if self.dll {
            chars |= 0x2000;
        }
        put16(&mut out, coff + 18, chars);

        let opt = coff + 20;
        put16(&mut out, opt, if self.pe32_plus { 0x20b } else { 0x10b });
        put32(&mut out, opt + 16, text_rva as u32); // entry point
        put32(&mut out, opt + 20, text_rva as u32); // base of code
        if self.pe32_plus {
            put64(&mut out, opt + 24, 0x1_4000_0000);
        } else {
            put32(&mut out, opt + 28, 0x0040_0000);
        }
        put32(&mut out, opt + 32, SECT_ALIGN as u32);
        put32(&mut out, opt + 36, FILE_ALIGN as u32);
        put16(&mut out, opt + 40, 6);
        put16(&mut out, opt + 48, 6);
        put32(&mut out, opt + 56, size_of_image as u32);
        put32(&mut out, opt + 60, HEADERS_SIZE as u32);
        put16(&mut out, opt + 68, self.subsystem);
        put16(&mut out, opt + 70, 0x8160);
        let (nrva_off, dirs) = if self.pe32_plus {
            (opt + 108, opt + 112)
        } else {
            (opt + 92, opt + 96)
        };
        put32(&mut out, nrva_off, 16);
        let set_dir = |out: &mut Vec<u8>, idx: usize, rva: usize, size: u32| {
            put32(out, dirs + idx * 8, rva as u32);
            put32(out, dirs + idx * 8 + 4, size);
        };
        if let Some((rva, len)) = rsrc_dir {
            set_dir(&mut out, 2, rva, len);
        }
        if let Some((rva, len)) = load_config {
            set_dir(&mut out, 10, rva, len);
        }
        if self.dotnet {
            set_dir(&mut out, 14, text_rva, 72);
        }

        let mut sh = opt + opt_size;
        for s in &sects {
            let mut name = [0u8; 8];
            for (i, b) in s.name.bytes().take(8).enumerate() {
                name[i] = b;
            }
            out[sh..sh + 8].copy_from_slice(&name);
            put32(&mut out, sh + 8, s.data.len() as u32);
            put32(&mut out, sh + 12, s.rva as u32);
            put32(
                &mut out,
                sh + 16,
                align(s.data.len().max(1), FILE_ALIGN) as u32,
            );
            put32(&mut out, sh + 20, s.raw as u32);
            put32(&mut out, sh + 36, s.chars);
            sh += 40;
        }
        for s in &sects {
            out.resize(s.raw, 0);
            out.extend_from_slice(&s.data);
            out.resize(s.raw + align(s.data.len().max(1), FILE_ALIGN), 0);
        }
        out.extend_from_slice(&self.overlay);
        out
    }
}

fn align(v: usize, a: usize) -> usize {
    v.div_ceil(a) * a
}

fn put16(d: &mut [u8], off: usize, v: u16) {
    d[off..off + 2].copy_from_slice(&v.to_le_bytes());
}

fn put32(d: &mut [u8], off: usize, v: u32) {
    d[off..off + 4].copy_from_slice(&v.to_le_bytes());
}

fn put64(d: &mut [u8], off: usize, v: u64) {
    d[off..off + 8].copy_from_slice(&v.to_le_bytes());
}

type ResTree = BTreeMap<ResKey, BTreeMap<ResKey, BTreeMap<u16, Vec<u8>>>>;

/// Lay out a three-level resource tree (type / name / language) the way
/// linkers do: all directories, then data entries, then strings, then data.
fn build_rsrc(base_rva: u32, tree: &ResTree) -> Vec<u8> {
    let dir_len = |n: usize| 16 + 8 * n;
    let mut off = dir_len(tree.len());
    let mut type_dir = Vec::new();
    for names in tree.values() {
        type_dir.push(off);
        off += dir_len(names.len());
    }
    let mut name_dir = Vec::new();
    for names in tree.values() {
        for langs in names.values() {
            name_dir.push(off);
            off += dir_len(langs.len());
        }
    }
    let mut data_entry = Vec::new();
    for names in tree.values() {
        for langs in names.values() {
            for _ in langs {
                data_entry.push(off);
                off += 16;
            }
        }
    }
    let mut strings: BTreeMap<String, usize> = BTreeMap::new();
    let mut add_str = |k: &ResKey, off: &mut usize| {
        if let ResKey::Name(s) = k
            && !strings.contains_key(s)
        {
            strings.insert(s.clone(), *off);
            *off += 2 + 2 * s.encode_utf16().count();
        }
    };
    for (t, names) in tree {
        add_str(t, &mut off);
        for n in names.keys() {
            add_str(n, &mut off);
        }
    }
    off = align(off, 8);
    let mut blob = Vec::new();
    for names in tree.values() {
        for langs in names.values() {
            for data in langs.values() {
                blob.push(off);
                off = align(off + data.len(), 8);
            }
        }
    }
    let mut out = vec![0u8; off];

    let write_dir = |out: &mut Vec<u8>, at: usize, keys: &[(ResKey, u32)]| {
        let named = keys
            .iter()
            .filter(|(k, _)| matches!(k, ResKey::Name(_)))
            .count();
        put16(out, at + 12, named as u16);
        put16(out, at + 14, (keys.len() - named) as u16);
        for (i, (k, target)) in keys.iter().enumerate() {
            let e = at + 16 + i * 8;
            let name_field = match k {
                ResKey::Id(id) => *id as u32,
                ResKey::Name(s) => 0x8000_0000 | strings[s] as u32,
            };
            put32(out, e, name_field);
            put32(out, e + 4, *target);
        }
    };

    let root_keys: Vec<(ResKey, u32)> = tree
        .keys()
        .zip(&type_dir)
        .map(|(k, &o)| (k.clone(), 0x8000_0000 | o as u32))
        .collect();
    write_dir(&mut out, 0, &root_keys);
    let (mut ni, mut li) = (0, 0);
    for (ti, names) in tree.values().enumerate() {
        let keys: Vec<(ResKey, u32)> = names
            .keys()
            .enumerate()
            .map(|(i, k)| (k.clone(), 0x8000_0000 | name_dir[ni + i] as u32))
            .collect();
        write_dir(&mut out, type_dir[ti], &keys);
        for langs in names.values() {
            let keys: Vec<(ResKey, u32)> = langs
                .keys()
                .enumerate()
                .map(|(i, l)| (ResKey::Id(*l), data_entry[li + i] as u32))
                .collect();
            write_dir(&mut out, name_dir[ni], &keys);
            for data in langs.values() {
                let de = data_entry[li];
                put32(&mut out, de, base_rva + blob[li] as u32);
                put32(&mut out, de + 4, data.len() as u32);
                out[blob[li]..blob[li] + data.len()].copy_from_slice(data);
                li += 1;
            }
            ni += 1;
        }
    }
    for (s, &at) in &strings {
        let units: Vec<u16> = s.encode_utf16().collect();
        put16(&mut out, at, units.len() as u16);
        for (i, u) in units.iter().enumerate() {
            put16(&mut out, at + 2 + i * 2, *u);
        }
    }
    out
}

fn utf16z(s: &str) -> Vec<u8> {
    let mut v: Vec<u8> = s.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
    v.extend_from_slice(&[0, 0]);
    v
}

fn pad4(v: &mut Vec<u8>) {
    while !v.len().is_multiple_of(4) {
        v.push(0);
    }
}

/// One VS_VERSIONINFO-style node: header, key, value, children.
fn vs_node(key: &str, value: &[u8], value_len_field: u16, text: bool, children: &[u8]) -> Vec<u8> {
    let mut v = vec![0u8; 6];
    put16(&mut v, 2, value_len_field);
    put16(&mut v, 4, if text { 1 } else { 0 });
    v.extend_from_slice(&utf16z(key));
    pad4(&mut v);
    v.extend_from_slice(value);
    pad4(&mut v);
    v.extend_from_slice(children);
    let len = v.len() as u16;
    put16(&mut v, 0, len);
    v
}

/// Build an RT_VERSION blob with a fixed file/product version and an
/// English (0409, Unicode) string table.
pub fn version_resource(fixed: [u16; 4], strings: &[(&str, &str)]) -> Vec<u8> {
    let mut ffi = vec![0u8; 52];
    put32(&mut ffi, 0, 0xfeef_04bd);
    put32(&mut ffi, 4, 0x0001_0000);
    let ms = ((fixed[0] as u32) << 16) | fixed[1] as u32;
    let ls = ((fixed[2] as u32) << 16) | fixed[3] as u32;
    put32(&mut ffi, 8, ms);
    put32(&mut ffi, 12, ls);
    put32(&mut ffi, 16, ms);
    put32(&mut ffi, 20, ls);

    let mut table = Vec::new();
    for (k, val) in strings {
        let value = utf16z(val);
        let mut s = vs_node(
            k,
            &value,
            (val.encode_utf16().count() + 1) as u16,
            true,
            &[],
        );
        pad4(&mut s);
        table.extend_from_slice(&s);
    }
    let string_table = vs_node("040904B0", &[], 0, true, &table);
    let sfi = vs_node("StringFileInfo", &[], 0, true, &string_table);
    let mut translation = Vec::new();
    translation.extend_from_slice(&0x0409u16.to_le_bytes());
    translation.extend_from_slice(&1200u16.to_le_bytes());
    let var = vs_node("Translation", &translation, 4, false, &[]);
    let vfi = vs_node("VarFileInfo", &[], 0, true, &var);
    let mut children = sfi;
    pad4(&mut children);
    children.extend_from_slice(&vfi);
    vs_node("VS_VERSION_INFO", &ffi, 52, false, &children)
}

/// A solid-colour RGBA icon image encoded as PNG (as modern 256px icons are).
pub fn png_icon(size: u32, rgba: [u8; 4]) -> Vec<u8> {
    let data: Vec<u8> = (0..size * size).flat_map(|_| rgba).collect();
    let img = ico::IconImage::from_rgba_data(size, size, data);
    ico::IconDirEntry::encode_as_png(&img)
        .expect("png encode")
        .data()
        .to_vec()
}

/// A solid-colour icon image encoded as a classic BMP/DIB resource.
pub fn bmp_icon(size: u32, rgba: [u8; 4]) -> Vec<u8> {
    let data: Vec<u8> = (0..size * size).flat_map(|_| rgba).collect();
    let img = ico::IconImage::from_rgba_data(size, size, data);
    ico::IconDirEntry::encode_as_bmp(&img)
        .expect("bmp encode")
        .data()
        .to_vec()
}

fn icon_image_dims(img: &[u8]) -> (u32, u32, u16) {
    if img.starts_with(b"\x89PNG") {
        let w = u32::from_be_bytes([img[16], img[17], img[18], img[19]]);
        let h = u32::from_be_bytes([img[20], img[21], img[22], img[23]]);
        (w, h, 32)
    } else {
        let w = u32::from_le_bytes([img[4], img[5], img[6], img[7]]);
        let h = u32::from_le_bytes([img[8], img[9], img[10], img[11]]) / 2;
        let bpp = u16::from_le_bytes([img[14], img[15]]);
        (w, h, bpp)
    }
}

/// Minimal Windows shell link (.lnk) as written by Wine's IShellLink:
/// header + LinkInfo with a local base path + optional string data.
pub fn lnk_file(
    target: &str,
    name: Option<&str>,
    workdir: Option<&str>,
    args: Option<&str>,
) -> Vec<u8> {
    let mut flags: u32 = 0x02 | 0x80; // HasLinkInfo | IsUnicode
    if name.is_some() {
        flags |= 0x04;
    }
    if workdir.is_some() {
        flags |= 0x10;
    }
    if args.is_some() {
        flags |= 0x20;
    }
    let mut v = vec![0u8; 0x4c];
    put32(&mut v, 0, 0x4c);
    // LinkCLSID 00021401-0000-0000-C000-000000000046
    v[4..20].copy_from_slice(&[
        0x01, 0x14, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0xc0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x46,
    ]);
    put32(&mut v, 0x14, flags);

    // LinkInfo with header size 0x1c, VolumeID + LocalBasePath.
    let mut base = target.as_bytes().to_vec();
    base.push(0);
    let volume = {
        let mut vol = vec![0u8; 0x10];
        put32(&mut vol, 0, 0x11);
        put32(&mut vol, 4, 3); // DRIVE_FIXED
        put32(&mut vol, 12, 0x10); // label offset
        vol.push(0);
        vol
    };
    let header = 0x1cusize;
    let vol_off = header;
    let base_off = vol_off + volume.len();
    let suffix_off = base_off + base.len();
    let total = suffix_off + 1;
    let mut li = vec![0u8; header];
    put32(&mut li, 0, total as u32);
    put32(&mut li, 4, header as u32);
    put32(&mut li, 8, 1); // VolumeIDAndLocalBasePath
    put32(&mut li, 12, vol_off as u32);
    put32(&mut li, 16, base_off as u32);
    put32(&mut li, 20, 0);
    put32(&mut li, 24, suffix_off as u32);
    li.extend_from_slice(&volume);
    li.extend_from_slice(&base);
    li.push(0);
    v.extend_from_slice(&li);

    for s in [name, workdir, args].into_iter().flatten() {
        let units: Vec<u16> = s.encode_utf16().collect();
        v.extend_from_slice(&(units.len() as u16).to_le_bytes());
        for u in units {
            v.extend_from_slice(&u.to_le_bytes());
        }
    }
    v.extend_from_slice(&[0, 0, 0, 0]); // terminal block
    v
}
