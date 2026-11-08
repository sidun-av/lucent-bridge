//! Native MinGW DLLs that Wine itself is supposed to find on the system.
//!
//! Fedora's Wine Mono is built against MinGW's `iconv.dll`
//! (`mingw64-win-iconv` / `mingw32-win-iconv`, both required by
//! `wine-core`). Wine finds such DLLs through its compile-time system DLL
//! path, but since Fedora 43 builds Wine in new-WoW64 mode without
//! `--with-system-dllpath`, so on x86-64 Mono fails with "Library iconv.dll
//! ... not found" and every .NET program silently exits (seen with KeePass
//! on Fedora 45, Wine 11.20). Linking the DLLs into the prefix's system
//! directories, where Windows programs' own DLLs are found, restores it.

use std::path::{Path, PathBuf};

/// (directory inside the prefix, MinGW bin directory) per host.
fn mappings(host: crate::arch::HostArch) -> &'static [(&'static str, &'static str)] {
    match host {
        crate::arch::HostArch::X86_64 => &[
            (
                "drive_c/windows/system32",
                "/usr/x86_64-w64-mingw32/sys-root/mingw/bin",
            ),
            (
                "drive_c/windows/syswow64",
                "/usr/i686-w64-mingw32/sys-root/mingw/bin",
            ),
        ],
        // The ARM64EC COPR Wine loads Mono fine (tested), and an x86-64
        // DLL in an ARM64 system32 would be wrong.
        _ => &[],
    }
}

pub const DLLS: &[&str] = &["iconv.dll"];

/// Symlinks to create: (link inside the prefix, target). Only for DLLs the
/// prefix lacks and the system has; `root` is `/` outside tests.
pub fn missing_links(
    prefix: &Path,
    host: crate::arch::HostArch,
    root: &Path,
) -> Vec<(PathBuf, PathBuf)> {
    let mut out = Vec::new();
    for (dir, bin) in mappings(host) {
        let dir = prefix.join(dir);
        if !dir.is_dir() {
            continue;
        }
        for dll in DLLS {
            let target = root.join(bin.trim_start_matches('/')).join(dll);
            let link = dir.join(dll);
            if target.is_file() && std::fs::symlink_metadata(&link).is_err() {
                out.push((link, target));
            }
        }
    }
    out
}

/// Create the missing links (best effort: a failure only means Mono stays
/// broken, as it would have been anyway).
pub fn provide(prefix: &Path, host: crate::arch::HostArch) -> Vec<PathBuf> {
    let mut made = Vec::new();
    for (link, target) in missing_links(prefix, host, Path::new("/")) {
        #[cfg(unix)]
        if std::os::unix::fs::symlink(&target, &link).is_ok() {
            made.push(link);
        }
    }
    made
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arch::HostArch;

    #[test]
    fn links_only_what_is_missing_and_available() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        let bin64 = root.join("usr/x86_64-w64-mingw32/sys-root/mingw/bin");
        std::fs::create_dir_all(&bin64).unwrap();
        std::fs::write(bin64.join("iconv.dll"), b"MZ").unwrap();
        // No 32-bit MinGW iconv installed.
        let prefix = tmp.path().join("prefix");
        let sys32 = prefix.join("drive_c/windows/system32");
        std::fs::create_dir_all(&sys32).unwrap();
        std::fs::create_dir_all(prefix.join("drive_c/windows/syswow64")).unwrap();

        let links = missing_links(&prefix, HostArch::X86_64, &root);
        assert_eq!(
            links,
            vec![(sys32.join("iconv.dll"), bin64.join("iconv.dll"))]
        );
        assert!(missing_links(&prefix, HostArch::Aarch64, &root).is_empty());

        // A DLL the program or the user put there is never replaced.
        std::fs::write(sys32.join("iconv.dll"), b"MZ mine").unwrap();
        assert!(missing_links(&prefix, HostArch::X86_64, &root).is_empty());
    }
}
