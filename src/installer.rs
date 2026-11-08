//! Installer detection: Inno Setup, NSIS, WiX Burn, InstallShield, MSI and
//! file-name heuristics.

use crate::pe::Pe;
use crate::pe::version::VersionInfo;
use memchr::memmem;
use serde::Serialize;

pub const OLE_CFB_MAGIC: [u8; 8] = [0xd0, 0xcf, 0x11, 0xe0, 0xa1, 0xb1, 0x1a, 0xe1];

/// How much of the overlay (appended payload) to scan for installer
/// signatures. Installer stubs put their markers at the very start of it.
const OVERLAY_SCAN: usize = 4 << 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallerKind {
    InnoSetup,
    Nsis,
    WixBurn,
    InstallShield,
    Msi,
    /// Looks like an installer by name only.
    Generic,
}

impl std::fmt::Display for InstallerKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            InstallerKind::InnoSetup => "Inno Setup",
            InstallerKind::Nsis => "NSIS (Nullsoft)",
            InstallerKind::WixBurn => "WiX Burn bundle",
            InstallerKind::InstallShield => "InstallShield",
            InstallerKind::Msi => "Windows Installer (MSI)",
            InstallerKind::Generic => "installer (by name)",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InstallerMatch {
    pub kind: InstallerKind,
    /// Why we think so, for `inspect` output and debugging.
    pub evidence: String,
}

fn found(kind: InstallerKind, evidence: impl Into<String>) -> Option<InstallerMatch> {
    Some(InstallerMatch {
        kind,
        evidence: evidence.into(),
    })
}

pub fn is_msi(data: &[u8], file_name: &str) -> bool {
    data.starts_with(&OLE_CFB_MAGIC) && file_name.to_ascii_lowercase().ends_with(".msi")
}

/// Uninstallers must never be treated as installers (and never get a
/// launcher entry).
pub fn is_uninstaller_name(file_name: &str) -> bool {
    let n = file_name.to_ascii_lowercase();
    n.starts_with("unins") || n.contains("uninstall") || n.starts_with("uninst")
}

fn name_heuristic(file_name: &str) -> bool {
    if is_uninstaller_name(file_name) {
        return false;
    }
    let n = file_name.to_ascii_lowercase();
    let stem = n.strip_suffix(".exe").unwrap_or(&n);
    stem.contains("setup") || stem.contains("install")
}

/// Detect whether a PE file is an installer. Signature checks beat the name
/// heuristic; `None` means "an ordinary program".
pub fn detect_pe(
    pe: &Pe<'_>,
    version: Option<&VersionInfo>,
    file_name: &str,
) -> Option<InstallerMatch> {
    let data = pe.data;
    let overlay = pe.overlay_offset();
    let scan_end = overlay.saturating_add(OVERLAY_SCAN).min(data.len());
    let region = &data[..scan_end];
    let has = |needle: &[u8]| memmem::find(region, needle).is_some();

    let vstr = |k: &str| version.and_then(|v| v.get(k)).unwrap_or("").to_string();
    let comments = vstr("Comments");
    if comments.contains("Inno Setup") {
        return found(
            InstallerKind::InnoSetup,
            "version Comments mention Inno Setup",
        );
    }
    if has(b"Inno Setup Setup Data") || has(b"rDlPtS") {
        return found(InstallerKind::InnoSetup, "Inno Setup loader signature");
    }
    if has(b"NullsoftInst") {
        return found(InstallerKind::Nsis, "NSIS 'NullsoftInst' header");
    }
    if let Some(m) = pe.manifest()
        && m.contains("Nullsoft.NSIS.exehead")
    {
        return found(InstallerKind::Nsis, "NSIS manifest identity");
    }
    if pe.sections.iter().any(|s| s.name == ".wixburn") {
        return found(InstallerKind::WixBurn, ".wixburn section");
    }
    let all_strings = format!(
        "{} {} {}",
        vstr("CompanyName"),
        vstr("FileDescription"),
        vstr("ProductName")
    );
    if all_strings.contains("InstallShield") || has(b"InstallShield") {
        return found(InstallerKind::InstallShield, "InstallShield marker");
    }
    if name_heuristic(file_name) {
        return found(InstallerKind::Generic, "file name looks like an installer");
    }
    let desc = vstr("FileDescription").to_ascii_lowercase();
    if (desc.contains("setup") || desc.contains("installer")) && !desc.contains("uninstall") {
        return found(
            InstallerKind::Generic,
            "FileDescription says setup/installer",
        );
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testpe::{self, PeBuilder};

    fn detect(b: PeBuilder, name: &str) -> Option<InstallerKind> {
        let bytes = b.build();
        let pe = Pe::parse(&bytes).unwrap();
        let v = VersionInfo::from_pe(&pe);
        detect_pe(&pe, v.as_ref(), name).map(|m| m.kind)
    }

    fn plain() -> PeBuilder {
        PeBuilder::new(testpe::MACHINE_I386)
    }

    #[test]
    fn inno_by_comments_and_signature() {
        let b = plain().version(
            [6, 2, 0, 0],
            &[("Comments", "This installation was built with Inno Setup.")],
        );
        assert_eq!(detect(b, "foo.exe"), Some(InstallerKind::InnoSetup));
        let b = plain().overlay(b"zzzzrDlPtS02\x87eVx");
        assert_eq!(detect(b, "foo.exe"), Some(InstallerKind::InnoSetup));
    }

    #[test]
    fn nsis_by_overlay_and_manifest() {
        let mut ov = vec![0u8; 4];
        ov.extend_from_slice(&[0xef, 0xbe, 0xad, 0xde]);
        ov.extend_from_slice(b"NullsoftInst");
        assert_eq!(
            detect(plain().overlay(&ov), "x.exe"),
            Some(InstallerKind::Nsis)
        );
        let b = plain().resource(
            testpe::ResKey::Id(testpe::RT_MANIFEST),
            testpe::ResKey::Id(1),
            0,
            br#"<assemblyIdentity name="Nullsoft.NSIS.exehead"/>"#.to_vec(),
        );
        assert_eq!(detect(b, "x.exe"), Some(InstallerKind::Nsis));
    }

    #[test]
    fn wix_burn_section() {
        let b = plain().section(".wixburn", vec![0; 52]);
        assert_eq!(detect(b, "bundle.exe"), Some(InstallerKind::WixBurn));
    }

    #[test]
    fn installshield_marker() {
        let b = plain().version([1, 0, 0, 0], &[("CompanyName", "Flexera InstallShield")]);
        assert_eq!(detect(b, "a.exe"), Some(InstallerKind::InstallShield));
    }

    #[test]
    fn name_heuristics() {
        assert_eq!(detect(plain(), "Setup.exe"), Some(InstallerKind::Generic));
        assert_eq!(
            detect(plain(), "npp.8.6.Installer.x64.exe"),
            Some(InstallerKind::Generic)
        );
        assert_eq!(
            detect(plain(), "foo_setup_x64.exe"),
            Some(InstallerKind::Generic)
        );
        assert_eq!(detect(plain(), "notepad++.exe"), None);
        // Uninstallers are never installers.
        assert_eq!(detect(plain(), "unins000.exe"), None);
        assert_eq!(detect(plain(), "Uninstall.exe"), None);
    }

    #[test]
    fn description_heuristic() {
        let b = plain().version([1, 0, 0, 0], &[("FileDescription", "Foo Setup")]);
        assert_eq!(detect(b, "foo-1.2.exe"), Some(InstallerKind::Generic));
        let b = plain().version([1, 0, 0, 0], &[("FileDescription", "Foo Uninstaller")]);
        assert_eq!(detect(b, "foo-1.2.exe"), None);
    }

    #[test]
    fn signature_deep_in_overlay_is_ignored() {
        let mut ov = vec![0u8; OVERLAY_SCAN + 16];
        ov.extend_from_slice(b"NullsoftInst");
        assert_eq!(detect(plain().overlay(&ov), "data.exe"), None);
    }

    #[test]
    fn msi_magic() {
        let mut d = OLE_CFB_MAGIC.to_vec();
        d.extend_from_slice(&[0; 32]);
        assert!(is_msi(&d, "Product.MSI"));
        assert!(!is_msi(&d, "document.doc"));
        assert!(!is_msi(b"MZ", "x.msi"));
    }
}
