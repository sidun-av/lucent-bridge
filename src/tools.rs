//! Discover the Wine / FEX installation on this machine and turn it into a
//! [`Toolset`] for the (pure) runner selector.
//!
//! Every probe takes an explicit environment and root-relative paths, so
//! tests can build a fake file system in a temp dir.

use crate::arch::{ExeArch, HostArch};
use crate::inspect::Inspection;
use crate::runner::{FEX_ARM64EC_DLL, FEX_WOW64_DLL, FexTools, Toolset, WineInstall, WinePeArches};
use std::path::{Path, PathBuf};

/// Overrides for unusual installs.
pub const ENV_WINE: &str = "LUCENT_BRIDGE_WINE";
pub const ENV_FEX_DLL_DIR: &str = "LUCENT_BRIDGE_FEX_DLL_DIR";
pub const ENV_FEX_INTERPRETER: &str = "LUCENT_BRIDGE_FEX_INTERPRETER";
pub const ENV_FEX_GUEST_WINE: &str = "LUCENT_BRIDGE_FEX_GUEST_WINE";

pub struct Probe<'a> {
    pub env: &'a dyn Fn(&str) -> Option<String>,
    /// Prepended to the built-in absolute search locations (tests use a
    /// temp dir; production uses `/`).
    pub root: PathBuf,
    pub page_size: u64,
}

impl Probe<'_> {
    fn path_dirs(&self) -> Vec<PathBuf> {
        (self.env)("PATH")
            .map(|p| std::env::split_paths(&p).collect())
            .unwrap_or_default()
    }

    fn rooted(&self, abs: &str) -> PathBuf {
        self.root.join(abs.trim_start_matches('/'))
    }

    pub fn which(&self, name: &str) -> Option<PathBuf> {
        self.path_dirs()
            .into_iter()
            .map(|d| d.join(name))
            .find(|p| is_executable(p))
    }

    fn env_path(&self, key: &str) -> Option<PathBuf> {
        (self.env)(key).filter(|v| !v.is_empty()).map(PathBuf::from)
    }

    pub fn detect(&self) -> Toolset {
        let wine = self.detect_wine();
        let lib_dirs = wine
            .as_ref()
            .map(|w| wine_lib_dirs(&w.wine))
            .unwrap_or_default();
        Toolset {
            fex: self.detect_fex(&lib_dirs),
            wine,
            page_size: self.page_size,
        }
    }

    fn detect_wine(&self) -> Option<WineInstall> {
        let wine = self
            .env_path(ENV_WINE)
            .filter(|p| is_executable(p))
            .or_else(|| self.which("wine"))
            .or_else(|| self.which("wine64"))?;
        let wineserver = Some(wine.with_file_name("wineserver"))
            .filter(|p| is_executable(p))
            .or_else(|| self.which("wineserver"));
        let pe_arches = wine_pe_arches(&wine_lib_dirs(&wine));
        Some(WineInstall {
            wine,
            wineserver,
            pe_arches,
        })
    }

    fn detect_fex(&self, wine_lib_dirs: &[PathBuf]) -> FexTools {
        let mut dll_dirs: Vec<PathBuf> = self.env_path(ENV_FEX_DLL_DIR).into_iter().collect();
        for d in wine_lib_dirs {
            dll_dirs.push(d.join("aarch64-windows"));
            dll_dirs.push(d.join("arm64ec-windows"));
        }
        for d in [
            "/usr/lib64/wine/aarch64-windows",
            "/usr/lib/wine/aarch64-windows",
            "/usr/lib/aarch64-linux-gnu/wine/aarch64-windows",
            "/usr/lib64/fex-emu-wine",
            "/usr/lib/fex-emu-wine",
            "/usr/share/fex-emu/wine",
        ] {
            dll_dirs.push(self.rooted(d));
        }
        let find = |name: &str| dll_dirs.iter().map(|d| d.join(name)).find(|p| p.is_file());
        let interpreter = self
            .env_path(ENV_FEX_INTERPRETER)
            .filter(|p| is_executable(p))
            .or_else(|| self.which("FEXInterpreter"));
        let guest_wine = self
            .env_path(ENV_FEX_GUEST_WINE)
            .or_else(|| interpreter.as_ref().and_then(|_| self.rootfs_wine()));
        FexTools {
            arm64ec_dll: find(FEX_ARM64EC_DLL),
            wow64_dll: find(FEX_WOW64_DLL),
            interpreter,
            guest_wine,
        }
    }

    /// x86-64 Wine inside the configured FEX RootFS, as a guest path.
    /// Only directory RootFS layouts can be checked; image-based RootFS
    /// (SquashFS/EroFS) need `LUCENT_BRIDGE_FEX_GUEST_WINE`.
    fn rootfs_wine(&self) -> Option<PathBuf> {
        let home = self.env_path("HOME");
        let config_home = self
            .env_path("XDG_CONFIG_HOME")
            .or_else(|| home.as_ref().map(|h| h.join(".config")));
        let configs = [
            config_home.map(|c| c.join("fex-emu/Config.json")),
            home.as_ref().map(|h| h.join(".fex-emu/Config.json")),
        ];
        let rootfs = configs.into_iter().flatten().find_map(|c| {
            let text = std::fs::read_to_string(c).ok()?;
            let v: serde_json::Value = serde_json::from_str(&text).ok()?;
            v.get("Config")?.get("RootFS")?.as_str().map(str::to_string)
        })?;
        let candidates: Vec<PathBuf> = if Path::new(&rootfs).is_absolute() {
            vec![PathBuf::from(&rootfs)]
        } else {
            let mut v = Vec::new();
            if let Some(h) = &home {
                v.push(h.join(".fex-emu/RootFS").join(&rootfs));
                v.push(h.join(".local/share/fex-emu/RootFS").join(&rootfs));
            }
            v.push(self.rooted("/usr/share/fex-emu/RootFS").join(&rootfs));
            v
        };
        for dir in candidates.into_iter().filter(|d| d.is_dir()) {
            for w in ["usr/bin/wine", "usr/bin/wine64"] {
                if dir.join(w).exists() {
                    return Some(PathBuf::from("/").join(w));
                }
            }
        }
        None
    }
}

fn is_executable(p: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        p.is_file()
            && std::fs::metadata(p)
                .map(|m| m.permissions().mode() & 0o111 != 0)
                .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        p.is_file()
    }
}

/// `lib*/wine` directories belonging to a Wine binary's installation.
pub fn wine_lib_dirs(wine: &Path) -> Vec<PathBuf> {
    // The path as given first (keeps paths stable for display), then the
    // symlink-resolved one (alternatives, /usr/local/bin links, ...).
    let mut bases: Vec<PathBuf> = Vec::new();
    for p in [Some(wine.to_path_buf()), std::fs::canonicalize(wine).ok()]
        .into_iter()
        .flatten()
    {
        if let Some(b) = p.parent().and_then(Path::parent)
            && !bases.iter().any(|x| same_dir(x, b))
        {
            bases.push(b.to_path_buf());
        }
    }
    // `<base>/lib*/wine` and one level deeper: Debian's multiarch
    // `lib/<triplet>/wine`, Fedora's new-WoW64 `lib64/wine-wow64/wine`.
    let mut out: Vec<PathBuf> = Vec::new();
    for base in bases {
        let Ok(rd) = std::fs::read_dir(&base) else {
            continue;
        };
        let mut libs: Vec<PathBuf> = rd
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("lib"))
                    && p.is_dir()
            })
            .collect();
        libs.sort();
        for lib in libs {
            let mut candidates = vec![lib.join("wine")];
            if let Ok(rd) = std::fs::read_dir(&lib) {
                let mut sub: Vec<PathBuf> = rd
                    .filter_map(|e| e.ok().map(|e| e.path().join("wine")))
                    .collect();
                sub.sort();
                candidates.extend(sub);
            }
            for d in candidates {
                // A real Wine lib dir has at least one <arch>-windows dir.
                let is_wine = std::fs::read_dir(&d).is_ok_and(|rd| {
                    rd.filter_map(|e| e.ok())
                        .any(|e| e.file_name().to_string_lossy().ends_with("-windows"))
                });
                if is_wine && !out.iter().any(|x| same_dir(x, &d)) {
                    out.push(d);
                }
            }
        }
    }
    out
}

fn same_dir(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => a == b,
    }
}

/// Which PE architectures the Wine install ships DLLs for. ARM64EC support
/// is detected by inspecting the aarch64 `ntdll.dll`: an ARM64EC-enabled
/// build ships it as an ARM64X hybrid.
pub fn wine_pe_arches(lib_dirs: &[PathBuf]) -> WinePeArches {
    let mut a = WinePeArches::default();
    for d in lib_dirs {
        a.i386 |= d.join("i386-windows").is_dir();
        a.x86_64 |= d.join("x86_64-windows").is_dir();
        let arm = d.join("aarch64-windows");
        a.aarch64 |= arm.is_dir();
        a.arm64ec |= d.join("arm64ec-windows").is_dir();
        if !a.arm64ec && arm.is_dir() {
            a.arm64ec |= Inspection::from_path(&arm.join("ntdll.dll"))
                .map(|i| matches!(i.arch, ExeArch::Arm64x | ExeArch::Arm64ec))
                .unwrap_or(false);
        }
    }
    a
}

pub fn page_size() -> u64 {
    #[cfg(unix)]
    {
        // SAFETY: sysconf has no preconditions.
        let v = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        if v > 0 {
            return v as u64;
        }
    }
    4096
}

/// The real system.
pub fn detect_system() -> Toolset {
    let env = |k: &str| std::env::var(k).ok();
    Probe {
        env: &env,
        root: PathBuf::from("/"),
        page_size: page_size(),
    }
    .detect()
}

/// A stock Lucent OS install for `host`, for `plan --assume-tools` (dry
/// runs on development machines, documentation, tests).
pub fn assumed(host: HostArch) -> Toolset {
    let p = PathBuf::from;
    match host {
        HostArch::X86_64 | HostArch::Other => Toolset {
            wine: Some(WineInstall {
                wine: p("/usr/bin/wine"),
                wineserver: Some(p("/usr/bin/wineserver")),
                pe_arches: WinePeArches {
                    i386: true,
                    x86_64: true,
                    ..Default::default()
                },
            }),
            ..Default::default()
        },
        HostArch::Aarch64 => Toolset {
            wine: Some(WineInstall {
                wine: p("/usr/bin/wine"),
                wineserver: Some(p("/usr/bin/wineserver")),
                pe_arches: WinePeArches {
                    i386: true,
                    aarch64: true,
                    arm64ec: true,
                    ..Default::default()
                },
            }),
            fex: FexTools {
                arm64ec_dll: Some(p("/usr/lib64/wine/aarch64-windows/libarm64ecfex.dll")),
                wow64_dll: Some(p("/usr/lib64/wine/aarch64-windows/libwow64fex.dll")),
                interpreter: Some(p("/usr/bin/FEXInterpreter")),
                guest_wine: Some(p("/usr/bin/wine")),
            },
            page_size: 4096,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testpe::{self, PeBuilder};
    use std::collections::HashMap;

    fn exe(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    fn probe_with(root: &Path, vars: HashMap<String, String>) -> Toolset {
        let env = move |k: &str| vars.get(k).cloned();
        Probe {
            env: &env,
            root: root.to_path_buf(),
            page_size: 4096,
        }
        .detect()
    }

    #[test]
    fn nothing_installed() {
        let tmp = tempfile::tempdir().unwrap();
        let t = probe_with(tmp.path(), HashMap::new());
        assert_eq!(t, Toolset::default());
    }

    #[test]
    fn x86_wine_install() {
        let tmp = tempfile::tempdir().unwrap();
        let usr = tmp.path().join("usr");
        exe(&usr.join("bin/wine"));
        exe(&usr.join("bin/wineserver"));
        std::fs::create_dir_all(usr.join("lib64/wine/x86_64-windows")).unwrap();
        std::fs::create_dir_all(usr.join("lib64/wine/i386-windows")).unwrap();
        let vars = HashMap::from([("PATH".to_string(), usr.join("bin").display().to_string())]);
        let t = probe_with(tmp.path(), vars);
        let w = t.wine.unwrap();
        assert_eq!(w.wine, usr.join("bin/wine"));
        assert_eq!(w.wineserver, Some(usr.join("bin/wineserver")));
        assert_eq!(
            w.pe_arches,
            WinePeArches {
                i386: true,
                x86_64: true,
                ..Default::default()
            }
        );
        assert_eq!(t.fex, FexTools::default());
    }

    #[test]
    fn arm64ec_wine_with_fex_dlls_and_rootfs() {
        let tmp = tempfile::tempdir().unwrap();
        let usr = tmp.path().join("usr");
        exe(&usr.join("bin/wine"));
        exe(&usr.join("bin/FEXInterpreter"));
        let arm = usr.join("lib64/wine/aarch64-windows");
        std::fs::create_dir_all(&arm).unwrap();
        std::fs::create_dir_all(usr.join("lib64/wine/i386-windows")).unwrap();
        // ARM64X ntdll marks an ARM64EC-capable build.
        std::fs::write(
            arm.join("ntdll.dll"),
            PeBuilder::new(testpe::MACHINE_ARM64).chpe().build(),
        )
        .unwrap();
        std::fs::write(arm.join(FEX_ARM64EC_DLL), b"dll").unwrap();
        std::fs::write(arm.join(FEX_WOW64_DLL), b"dll").unwrap();
        let home = tmp.path().join("home");
        std::fs::create_dir_all(home.join(".fex-emu/RootFS/Fedora_42/usr/bin")).unwrap();
        std::fs::write(home.join(".fex-emu/RootFS/Fedora_42/usr/bin/wine"), b"elf").unwrap();
        std::fs::write(
            home.join(".fex-emu/Config.json"),
            r#"{"Config": {"RootFS": "Fedora_42"}}"#,
        )
        .unwrap();
        let vars = HashMap::from([
            ("PATH".to_string(), usr.join("bin").display().to_string()),
            ("HOME".to_string(), home.display().to_string()),
        ]);
        let t = probe_with(tmp.path(), vars);
        let a = &t.wine.as_ref().unwrap().pe_arches;
        assert!(a.aarch64 && a.arm64ec && a.i386 && !a.x86_64);
        assert_eq!(t.fex.arm64ec_dll, Some(arm.join(FEX_ARM64EC_DLL)));
        assert_eq!(t.fex.wow64_dll, Some(arm.join(FEX_WOW64_DLL)));
        assert_eq!(t.fex.interpreter, Some(usr.join("bin/FEXInterpreter")));
        assert_eq!(t.fex.guest_wine, Some(PathBuf::from("/usr/bin/wine")));
    }

    #[test]
    fn fedora_wow64_libdir_layout() {
        // Fedora >= 43 builds Wine in new-WoW64 mode with
        // %_libdir = /usr/lib64/wine-wow64 (seen on Fedora 45, x86_64 and
        // the aarch64 ARM64EC COPR build alike).
        let tmp = tempfile::tempdir().unwrap();
        let usr = tmp.path().join("usr");
        exe(&usr.join("bin/wine"));
        let lib = usr.join("lib64/wine-wow64/wine");
        std::fs::create_dir_all(lib.join("x86_64-windows")).unwrap();
        std::fs::create_dir_all(lib.join("i386-windows")).unwrap();
        std::fs::create_dir_all(lib.join("x86_64-unix")).unwrap();
        assert_eq!(wine_lib_dirs(&usr.join("bin/wine")), vec![lib.clone()]);
        let a = wine_pe_arches(&wine_lib_dirs(&usr.join("bin/wine")));
        assert!(a.i386 && a.x86_64 && !a.aarch64);

        let arm = lib.join("aarch64-windows");
        std::fs::create_dir_all(&arm).unwrap();
        std::fs::write(arm.join(FEX_ARM64EC_DLL), b"dll").unwrap();
        let vars = HashMap::from([("PATH".to_string(), usr.join("bin").display().to_string())]);
        let t = probe_with(tmp.path(), vars);
        assert_eq!(t.fex.arm64ec_dll, Some(arm.join(FEX_ARM64EC_DLL)));
    }

    #[test]
    fn plain_arm64_ntdll_is_not_arm64ec() {
        let tmp = tempfile::tempdir().unwrap();
        let arm = tmp.path().join("lib/wine/aarch64-windows");
        std::fs::create_dir_all(&arm).unwrap();
        std::fs::write(
            arm.join("ntdll.dll"),
            PeBuilder::new(testpe::MACHINE_ARM64).build(),
        )
        .unwrap();
        let a = wine_pe_arches(&[tmp.path().join("lib/wine")]);
        assert!(a.aarch64 && !a.arm64ec);
    }

    #[test]
    fn env_overrides() {
        let tmp = tempfile::tempdir().unwrap();
        let w = tmp.path().join("opt/wine-custom/bin/wine");
        exe(&w);
        let dlls = tmp.path().join("fexdlls");
        std::fs::create_dir_all(&dlls).unwrap();
        std::fs::write(dlls.join(FEX_ARM64EC_DLL), b"x").unwrap();
        let fi = tmp.path().join("opt/fex/FEXInterpreter");
        exe(&fi);
        let vars = HashMap::from([
            (ENV_WINE.to_string(), w.display().to_string()),
            (ENV_FEX_DLL_DIR.to_string(), dlls.display().to_string()),
            (ENV_FEX_INTERPRETER.to_string(), fi.display().to_string()),
            (
                ENV_FEX_GUEST_WINE.to_string(),
                "/opt/wine/bin/wine".to_string(),
            ),
        ]);
        let t = probe_with(tmp.path(), vars);
        assert_eq!(t.wine.unwrap().wine, w);
        assert_eq!(t.fex.arm64ec_dll, Some(dlls.join(FEX_ARM64EC_DLL)));
        assert_eq!(t.fex.wow64_dll, None);
        assert_eq!(t.fex.interpreter, Some(fi));
        assert_eq!(t.fex.guest_wine, Some(PathBuf::from("/opt/wine/bin/wine")));
    }

    #[cfg(unix)]
    #[test]
    fn non_executable_is_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        let bin = tmp.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("wine"), b"not executable").unwrap();
        let vars = HashMap::from([("PATH".to_string(), bin.display().to_string())]);
        assert!(probe_with(tmp.path(), vars).wine.is_none());
    }

    #[test]
    fn assumed_toolsets_select_expected_runners() {
        use crate::runner::{RunnerKind, select};
        let arm = assumed(HostArch::Aarch64);
        let x86 = assumed(HostArch::X86_64);
        let k = |h, e, t: &Toolset| select(h, e, t, None).runner().map(|r| r.kind);
        assert_eq!(
            k(HostArch::X86_64, ExeArch::X86_64, &x86),
            Some(RunnerKind::Wine)
        );
        assert_eq!(
            k(HostArch::Aarch64, ExeArch::X86_64, &arm),
            Some(RunnerKind::WineFex)
        );
        assert_eq!(
            k(HostArch::Aarch64, ExeArch::I386, &arm),
            Some(RunnerKind::WineFex)
        );
        assert_eq!(
            k(HostArch::Aarch64, ExeArch::Arm64, &arm),
            Some(RunnerKind::WineArm64)
        );
    }

    #[test]
    fn page_size_is_sane() {
        let p = page_size();
        assert!(p.is_power_of_two() && p >= 4096);
    }
}
