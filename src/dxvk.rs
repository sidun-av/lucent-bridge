//! DXVK (Direct3D 8/9/10/11 on Vulkan) bundled with Lucent Bridge on
//! x86-64.
//!
//! Fedora 45 ships DXVK 2.7.1, whose swap chain divides by the display's
//! refresh rate without checking it (`computeRefreshPeriod`); a display
//! that reports 0 Hz (Xvfb, some virtual and remote displays) kills every
//! Direct3D 10/11 program with an integer division by zero. Upstream
//! guards it since 3.0, and Fedora has no newer build, so the RPM carries
//! the upstream release (zlib licence) in `/usr/share/lucent-bridge/dxvk/`
//! and every x86-64 prefix gets its DLLs as native DLLs, the way DXVK's own
//! `setup_dxvk.sh` installs them (64-bit into `system32`, 32-bit into
//! `syswow64`), with `WINEDLLOVERRIDES` preferring them. Copied, not
//! linked: a prefix made from the template gets them as btrfs reflinks of
//! the template's copies, and nothing in a prefix depends on `/usr` paths.
//!
//! AArch64 keeps the ARM64EC DXVK that comes with its Wine.

use crate::arch::HostArch;
use std::path::{Path, PathBuf};

pub const DIR: &str = "/usr/share/lucent-bridge/dxvk";
/// Override the bundle location; `none` (or empty) disables bundled DXVK.
pub const ENV_DXVK: &str = "LUCENT_BRIDGE_DXVK";
pub const DLLS: [&str; 5] = ["d3d8", "d3d9", "d3d10core", "d3d11", "dxgi"];

/// (directory inside the prefix, bundle subdirectory) per host.
fn mappings(host: HostArch) -> &'static [(&'static str, &'static str)] {
    match host {
        HostArch::X86_64 => &[
            ("drive_c/windows/system32", "x64"),
            ("drive_c/windows/syswow64", "x32"),
        ],
        _ => &[],
    }
}

/// The bundle to use on this host, if there is one.
pub fn bundle(env: &dyn Fn(&str) -> Option<String>, host: HostArch) -> Option<PathBuf> {
    let dir = match env(ENV_DXVK) {
        Some(v) if v.is_empty() || v == "none" => return None,
        Some(v) => PathBuf::from(v),
        None => PathBuf::from(DIR),
    };
    let first = mappings(host).first()?;
    dir.join(first.1).join("d3d11.dll").is_file().then_some(dir)
}

/// `WINEDLLOVERRIDES` entries: native (the copies in the prefix) first.
pub fn overrides() -> Vec<(String, String)> {
    DLLS.iter()
        .map(|d| (d.to_string(), "n,b".to_string()))
        .collect()
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::metadata(a), std::fs::metadata(b)) {
        (Ok(x), Ok(y)) => {
            x.len() == y.len()
                && x.modified().ok().is_some()
                && x.modified().ok() == y.modified().ok()
        }
        _ => false,
    }
}

/// (source, destination) for every DLL the prefix lacks or has in another
/// version. Directories the prefix doesn't have are skipped.
pub fn pending(prefix: &Path, host: HostArch, bundle: &Path) -> Vec<(PathBuf, PathBuf)> {
    let mut out = Vec::new();
    for (dir, sub) in mappings(host) {
        let dir = prefix.join(dir);
        if !dir.is_dir() {
            continue;
        }
        for dll in DLLS {
            let src = bundle.join(sub).join(format!("{dll}.dll"));
            let dst = dir.join(format!("{dll}.dll"));
            if src.is_file() && !same_file(&src, &dst) {
                out.push((src, dst));
            }
        }
    }
    out
}

/// Copy what is pending (keeping the source's modification time, so the
/// next launch sees it is current). Returns how many DLLs were copied.
pub fn provide(prefix: &Path, host: HostArch, bundle: &Path) -> std::io::Result<usize> {
    let todo = pending(prefix, host, bundle);
    for (src, dst) in &todo {
        let tmp = dst.with_extension("dll.lucent-tmp");
        std::fs::copy(src, &tmp)?;
        let mtime = std::fs::metadata(src)?.modified()?;
        std::fs::File::options()
            .write(true)
            .open(&tmp)?
            .set_modified(mtime)?;
        // Wine's placeholder for a builtin DLL may be a symlink; replace it.
        std::fs::rename(&tmp, dst)?;
    }
    Ok(todo.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bundle_dir(root: &Path) -> PathBuf {
        let b = root.join("dxvk");
        for sub in ["x64", "x32"] {
            std::fs::create_dir_all(b.join(sub)).unwrap();
            for d in DLLS {
                std::fs::write(
                    b.join(sub).join(format!("{d}.dll")),
                    format!("MZ dxvk {sub} {d}"),
                )
                .unwrap();
            }
        }
        b
    }

    #[test]
    fn found_only_on_x86_64_and_overridable() {
        let tmp = tempfile::tempdir().unwrap();
        let b = bundle_dir(tmp.path());
        let v = b.display().to_string();
        let env = move |k: &str| (k == ENV_DXVK).then(|| v.clone());
        assert_eq!(bundle(&env, HostArch::X86_64), Some(b.clone()));
        assert_eq!(bundle(&env, HostArch::Aarch64), None);
        let off = |k: &str| (k == ENV_DXVK).then(|| "none".to_string());
        assert_eq!(bundle(&off, HostArch::X86_64), None);
        let missing = |k: &str| (k == ENV_DXVK).then(|| "/nonexistent".to_string());
        assert_eq!(bundle(&missing, HostArch::X86_64), None);
        assert_eq!(overrides()[3], ("d3d11".to_string(), "n,b".to_string()));
    }

    #[test]
    fn copies_into_both_system_dirs_once() {
        let tmp = tempfile::tempdir().unwrap();
        let b = bundle_dir(tmp.path());
        let prefix = tmp.path().join("prefix");
        let sys32 = prefix.join("drive_c/windows/system32");
        let wow64 = prefix.join("drive_c/windows/syswow64");
        std::fs::create_dir_all(&sys32).unwrap();
        std::fs::create_dir_all(&wow64).unwrap();
        // Wine's builtin placeholder.
        std::fs::write(sys32.join("d3d11.dll"), "MZ Wine builtin DLL").unwrap();

        assert_eq!(pending(&prefix, HostArch::X86_64, &b).len(), 10);
        assert_eq!(provide(&prefix, HostArch::X86_64, &b).unwrap(), 10);
        assert_eq!(
            std::fs::read_to_string(sys32.join("d3d11.dll")).unwrap(),
            "MZ dxvk x64 d3d11"
        );
        assert_eq!(
            std::fs::read_to_string(wow64.join("dxgi.dll")).unwrap(),
            "MZ dxvk x32 dxgi"
        );
        // Current: nothing to do, on any later launch.
        assert!(pending(&prefix, HostArch::X86_64, &b).is_empty());
        // A new DXVK in the bundle: copied again.
        std::fs::write(b.join("x64/d3d9.dll"), "MZ dxvk x64 d3d9, newer").unwrap();
        assert_eq!(provide(&prefix, HostArch::X86_64, &b).unwrap(), 1);
        // Not on AArch64.
        assert!(pending(&prefix, HostArch::Aarch64, &b).is_empty());
    }
}
