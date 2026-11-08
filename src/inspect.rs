//! File-level inspection: everything Lucent Bridge knows about one
//! `.exe`/`.msi` before deciding how to run it.

use crate::appid;
use crate::arch::ExeArch;
use crate::installer::{self, InstallerKind, InstallerMatch};
use crate::pe::icon::{self, ExtractedIcon};
use crate::pe::version::VersionInfo;
use crate::pe::{Pe, Subsystem};
use anyhow::{Context, Result, bail};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::fs::File;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Format {
    Pe32,
    Pe32Plus,
    Msi,
}

#[derive(Debug, Clone, Serialize)]
pub struct Inspection {
    pub path: PathBuf,
    pub file_name: String,
    pub size: u64,
    pub sha256: String,
    pub format: Format,
    pub arch: ExeArch,
    pub subsystem: Option<Subsystem>,
    pub is_dll: bool,
    pub dotnet: bool,
    pub version: Option<VersionInfo>,
    pub installer: Option<InstallerMatch>,
    pub display_name: String,
    pub app_id: String,
    #[serde(skip)]
    pub icon: Option<ExtractedIcon>,
    pub icon_size: Option<u32>,
}

impl Inspection {
    pub fn from_path(path: &Path) -> Result<Inspection> {
        let file = File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
        let len = file.metadata()?.len();
        if len == 0 {
            bail!("{} is empty", path.display());
        }
        // SAFETY: read-only private mapping; a concurrent truncation by
        // another process could SIGBUS, which is the accepted trade-off for
        // not reading multi-gigabyte installers into memory.
        let map = unsafe { memmap2::Mmap::map(&file) }
            .with_context(|| format!("cannot map {}", path.display()))?;
        Self::from_bytes(&map, path)
    }

    pub fn from_bytes(data: &[u8], path: &Path) -> Result<Inspection> {
        let file_name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let stem = path
            .file_stem()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "app".into());
        let sha256 = appid::hex(&Sha256::digest(data));

        if installer::is_msi(data, &file_name) {
            let display_name = stem.clone();
            let id = appid::identity(None, None, &stem);
            return Ok(Inspection {
                path: path.to_path_buf(),
                file_name,
                size: data.len() as u64,
                sha256,
                format: Format::Msi,
                arch: ExeArch::MsiPackage,
                subsystem: None,
                is_dll: false,
                dotnet: false,
                version: None,
                installer: Some(InstallerMatch {
                    kind: InstallerKind::Msi,
                    evidence: "OLE compound file with .msi extension".into(),
                }),
                app_id: appid::app_id(&display_name, &id),
                display_name,
                icon: None,
                icon_size: None,
            });
        }

        let pe = Pe::parse(data).with_context(|| format!("{}", path.display()))?;
        let version = VersionInfo::from_pe(&pe);
        let installer = installer::detect_pe(&pe, version.as_ref(), &file_name);
        let icon = icon::best_icon_png(&pe);
        let display_name = display_name(version.as_ref(), &stem, installer.is_some());
        let product = version.as_ref().and_then(|v| v.product_name());
        let company = version.as_ref().and_then(|v| v.company_name());
        let id = appid::identity(product, company, &stem);
        Ok(Inspection {
            path: path.to_path_buf(),
            file_name,
            size: data.len() as u64,
            sha256,
            format: if pe.pe32_plus {
                Format::Pe32Plus
            } else {
                Format::Pe32
            },
            arch: pe.arch(),
            subsystem: Some(pe.subsystem),
            is_dll: pe.is_dll,
            dotnet: pe.is_dotnet(),
            app_id: appid::app_id(&display_name, &id),
            display_name,
            icon_size: icon.as_ref().map(|i| i.width),
            icon,
            version,
            installer,
        })
    }

    pub fn is_installer(&self) -> bool {
        self.installer.is_some()
    }

    pub fn is_console(&self) -> bool {
        self.subsystem == Some(Subsystem::Console)
    }

    pub fn product_name(&self) -> Option<&str> {
        self.version.as_ref().and_then(|v| v.product_name())
    }

    /// Human-readable multi-line summary for `lucent-bridge inspect`.
    pub fn summary(&self) -> String {
        let mut s = String::new();
        let mut line = |k: &str, v: &str| s.push_str(&format!("{k:<17}{v}\n"));
        line("File:", &self.path.display().to_string());
        line("Name:", &self.display_name);
        line("App ID:", &self.app_id);
        line(
            "Format:",
            match self.format {
                Format::Pe32 => "PE32",
                Format::Pe32Plus => "PE32+",
                Format::Msi => "MSI package",
            },
        );
        line("Architecture:", &self.arch.to_string());
        if let Some(sub) = self.subsystem {
            line(
                "Subsystem:",
                match sub {
                    Subsystem::Gui => "GUI",
                    Subsystem::Console => "console",
                    Subsystem::Other(_) => "other",
                },
            );
        }
        if self.dotnet {
            line(".NET:", "yes (managed assembly; needs Wine Mono or .NET)");
        }
        if self.is_dll {
            line("Kind:", "DLL (not directly runnable)");
        }
        if let Some(v) = &self.version {
            for key in ["ProductName", "FileDescription", "CompanyName"] {
                if let Some(val) = v.get(key) {
                    line(&format!("{key}:"), val);
                }
            }
            if let Some(ver) = v.display_version() {
                line("Version:", &ver);
            }
        }
        match &self.installer {
            Some(m) => line("Installer:", &format!("{} ({})", m.kind, m.evidence)),
            None => line("Installer:", "no"),
        }
        match self.icon_size {
            Some(px) => line("Icon:", &format!("{px}x{px}")),
            None => line("Icon:", "none"),
        }
        line("Size:", &format!("{} bytes", self.size));
        line("SHA-256:", &self.sha256);
        s
    }
}

/// ProductName, then FileDescription (with " Setup"/" Installer" removed for
/// installers), then the file name.
fn display_name(v: Option<&VersionInfo>, stem: &str, is_installer: bool) -> String {
    if let Some(p) = v.and_then(|v| v.product_name()) {
        return p.to_string();
    }
    if let Some(d) = v.and_then(|v| v.file_description()) {
        let mut d = d.to_string();
        if is_installer {
            for suffix in [" Setup", " setup", " Installer", " installer"] {
                if let Some(stripped) = d.strip_suffix(suffix) {
                    d = stripped.to_string();
                }
            }
        }
        if !d.trim().is_empty() {
            return d;
        }
    }
    stem.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testpe::{self, PeBuilder, ResKey};

    #[test]
    fn full_inspection() {
        let bytes = PeBuilder::new(testpe::MACHINE_AMD64)
            .version(
                [24, 8, 0, 0],
                &[
                    ("ProductName", "7-Zip"),
                    ("CompanyName", "Igor Pavlov"),
                    ("FileDescription", "7-Zip File Manager"),
                ],
            )
            .icon_group(ResKey::Id(1), 1, &[testpe::png_icon(48, [1, 2, 3, 255])])
            .build();
        let i = Inspection::from_bytes(&bytes, Path::new("/x/7zFM.exe")).unwrap();
        assert_eq!(i.arch, ExeArch::X86_64);
        assert_eq!(i.format, Format::Pe32Plus);
        assert_eq!(i.display_name, "7-Zip");
        assert!(i.app_id.starts_with("7-zip-"));
        assert_eq!(i.icon_size, Some(48));
        assert!(!i.is_installer());
        let s = i.summary();
        assert!(s.contains("x86-64") && s.contains("Igor Pavlov"));
    }

    #[test]
    fn installer_display_name_strips_setup() {
        let bytes = PeBuilder::new(testpe::MACHINE_I386)
            .version([1, 0, 0, 0], &[("FileDescription", "Foo Setup")])
            .build();
        let i = Inspection::from_bytes(&bytes, Path::new("foo-1.0.exe")).unwrap();
        assert!(i.is_installer());
        assert_eq!(i.display_name, "Foo");
    }

    #[test]
    fn no_version_uses_file_stem() {
        let bytes = PeBuilder::new(testpe::MACHINE_I386)
            .subsystem(testpe::SUBSYSTEM_CONSOLE)
            .build();
        let i = Inspection::from_bytes(&bytes, Path::new("tool.exe")).unwrap();
        assert_eq!(i.display_name, "tool");
        assert!(i.is_console());
        assert_eq!(i.icon_size, None);
    }

    #[test]
    fn msi_inspection() {
        let mut d = installer::OLE_CFB_MAGIC.to_vec();
        d.resize(512, 0);
        let i = Inspection::from_bytes(&d, Path::new("Product-1.2.msi")).unwrap();
        assert_eq!(i.arch, ExeArch::MsiPackage);
        assert_eq!(i.installer.unwrap().kind, InstallerKind::Msi);
        assert_eq!(i.display_name, "Product-1.2");
    }

    #[test]
    fn non_windows_file_errors() {
        let e = Inspection::from_bytes(b"#!/bin/sh\n", Path::new("x.exe")).unwrap_err();
        assert!(format!("{e:#}").contains("no MZ header"));
    }
}
