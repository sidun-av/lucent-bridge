//! Windows programs start the way Windows starts them for an administrator
//! with UAC on: with the filtered ("limited") token, not as Administrator.
//!
//! Wine's own model (server/token.c, Wine 11.20 and current master): every
//! process token, even the `TokenElevationTypeLimited` one a program
//! started from Linux gets, has BUILTIN\Administrators as an *enabled*
//! group. So `CheckTokenMembership(Administrators)` is TRUE for everything,
//! and programs that test for elevation that way believe they run elevated:
//! Notepad++ puts "[Administrator]" in its title bar. On Windows the
//! limited token has that group as deny-only; only an elevated process
//! (linked full token) has it enabled.
//!
//! The fix: ordinary programs (GUI, not installers) are started through
//! `lucent-start.exe` (`windows/lucent-start.c`), which starts them with a
//! restricted copy of the token where Administrators is deny-only — exactly
//! the Windows limited token for this check. Everything that needs more
//! keeps getting it, as on Windows:
//! - installers (and MSI packages) are started directly, with the token they
//!   always had, so per-machine installs into Program Files are unchanged;
//! - a program whose manifest asks for `requireAdministrator` or
//!   `highestAvailable` is elevated by Wine's loader through the linked full
//!   token, which a restricted token still has.

use crate::arch::ExeArch;
use crate::inspect::Inspection;
use crate::runner::RunnerKind;
use std::path::{Path, PathBuf};

pub const HELPER: &str = "/usr/share/lucent-bridge/windows/lucent-start.exe";
/// Override the helper's location; `none` (or empty) starts programs
/// directly (as Lucent Bridge 0.1 did).
pub const ENV_HELPER: &str = "LUCENT_BRIDGE_START_HELPER";

pub fn helper(env: &dyn Fn(&str) -> Option<String>) -> Option<PathBuf> {
    let p = match env(ENV_HELPER) {
        Some(v) if v.is_empty() || v == "none" => return None,
        Some(v) => PathBuf::from(v),
        None => PathBuf::from(HELPER),
    };
    p.is_file().then_some(p)
}

/// Whether this program should start with the filtered token.
pub fn applies(insp: &Inspection, runner: RunnerKind) -> bool {
    // FEXInterpreter runs a whole x86-64 Wine from a separate root file
    // system, where the helper's path doesn't exist.
    runner != RunnerKind::FexInterpreter
        && insp.arch != ExeArch::MsiPackage
        && !insp.is_dll
        && !insp.is_installer()
        && !insp.is_console()
}

/// The path Windows programs see for `exe` in `prefix`: `C:\...` inside the
/// prefix's drive_c, else `Z:\...` (Wine maps Z: to `/`). `None` when it
/// can't be expressed (a non-UTF-8 path, or Z: mapped elsewhere).
pub fn dos_path(prefix: &Path, exe: &Path, z_is_root: bool) -> Option<String> {
    let (drive, rest) = match exe.strip_prefix(prefix.join("drive_c")) {
        Ok(rest) => ("C:", rest),
        Err(_) if z_is_root && exe.is_absolute() => ("Z:", exe.strip_prefix("/").ok()?),
        Err(_) => return None,
    };
    let mut out = String::from(drive);
    for c in rest.components() {
        out.push('\\');
        out.push_str(c.as_os_str().to_str()?);
    }
    if out.len() == 2 {
        out.push('\\');
    }
    Some(out)
}

/// Whether the prefix's Z: drive is `/` (Wine's default; true for a prefix
/// that doesn't exist yet, since Wine will create it that way).
pub fn z_drive_is_root(prefix: &Path) -> bool {
    let z = prefix.join("dosdevices/z:");
    match std::fs::read_link(&z) {
        Ok(target) => target == Path::new("/"),
        Err(_) => !z.exists() && std::fs::symlink_metadata(&z).is_err(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dos_paths() {
        let p = Path::new("/home/anna/.local/share/lucent-bridge/prefixes/npp-1");
        assert_eq!(
            dos_path(
                p,
                &p.join("drive_c/Program Files/Notepad++/notepad++.exe"),
                true
            )
            .as_deref(),
            Some("C:\\Program Files\\Notepad++\\notepad++.exe")
        );
        assert_eq!(
            dos_path(p, Path::new("/home/anna/Загрузки/tool.exe"), true).as_deref(),
            Some("Z:\\home\\anna\\Загрузки\\tool.exe")
        );
        assert_eq!(dos_path(p, Path::new("/home/anna/tool.exe"), false), None);
        assert_eq!(dos_path(p, Path::new("tool.exe"), true), None);
    }

    #[cfg(unix)]
    #[test]
    fn z_drive() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path();
        assert!(z_drive_is_root(p)); // not created yet: Wine's default
        std::fs::create_dir_all(p.join("dosdevices")).unwrap();
        std::os::unix::fs::symlink("/", p.join("dosdevices/z:")).unwrap();
        assert!(z_drive_is_root(p));
        std::fs::remove_file(p.join("dosdevices/z:")).unwrap();
        std::os::unix::fs::symlink("/media", p.join("dosdevices/z:")).unwrap();
        assert!(!z_drive_is_root(p));
    }

    #[test]
    fn helper_location() {
        let tmp = tempfile::tempdir().unwrap();
        let h = tmp.path().join("lucent-start.exe");
        std::fs::write(&h, b"MZ").unwrap();
        let v = h.display().to_string();
        let env = move |k: &str| (k == ENV_HELPER).then(|| v.clone());
        assert_eq!(helper(&env), Some(h));
        let off = |k: &str| (k == ENV_HELPER).then(|| "none".to_string());
        assert_eq!(helper(&off), None);
    }
}
