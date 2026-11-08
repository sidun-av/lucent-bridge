//! Runner selection: host architecture × program architecture × installed
//! tools → how to run it (or a human-readable reason why we can't).
//!
//! This module is pure: no file system, no environment. `tools::detect`
//! produces the [`Toolset`]; everything here is decided from values, so the
//! whole decision table is unit-tested exhaustively.
//!
//! The four ways a program can run — none of them a virtual machine:
//!
//! * [`RunnerKind::Wine`] — x86-64 host. Wine translates Windows API calls;
//!   the CPU executes the program's x86 instructions directly.
//! * [`RunnerKind::WineFex`] — AArch64 host, **primary** x86 path. An ARM64
//!   Wine built with ARM64EC + i386 PE support runs natively; only the
//!   program's own x86 code is translated, by FEX-Emu loaded into the process
//!   as Wine's emulator DLL (`libarm64ecfex.dll` for x86-64 code via
//!   ARM64EC, `libwow64fex.dll` for 32-bit x86 code via WoW64).
//! * [`RunnerKind::FexInterpreter`] — AArch64 host, **fallback** x86 path. A
//!   complete x86-64 Linux Wine from the FEX RootFS runs under
//!   `FEXInterpreter`, so Wine itself is translated too.
//! * [`RunnerKind::WineArm64`] — AArch64 host, ARM64/ARM64EC/ARM64X Windows
//!   programs. Native, nothing translated.

use crate::arch::{ExeArch, HostArch};
use crate::t;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const FEX_ARM64EC_DLL: &str = "libarm64ecfex.dll";
pub const FEX_WOW64_DLL: &str = "libwow64fex.dll";
/// Wine environment variables selecting the x86-64 (ARM64EC) and i386
/// (WoW64) emulator DLLs (equivalent to HKLM\Software\Microsoft\Wow64\amd64
/// and \x86).
pub const ENV_HODLL64: &str = "HODLL64";
pub const ENV_HODLL: &str = "HODLL";

pub const REQUIRED_PAGE_SIZE: u64 = 4096;

/// Which PE architectures the installed Wine has DLLs for
/// (`lib/wine/<arch>-windows`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct WinePeArches {
    pub i386: bool,
    pub x86_64: bool,
    pub aarch64: bool,
    /// The aarch64 DLLs are ARM64X hybrids, i.e. Wine was built with
    /// `--enable-archs=arm64ec,aarch64,...` and can host x86-64 code.
    pub arm64ec: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct WineInstall {
    pub wine: PathBuf,
    pub wineserver: Option<PathBuf>,
    pub pe_arches: WinePeArches,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct FexTools {
    /// `libarm64ecfex.dll` (x86-64 emulation inside ARM64EC Wine).
    pub arm64ec_dll: Option<PathBuf>,
    /// `libwow64fex.dll` (i386 emulation inside WoW64 Wine).
    pub wow64_dll: Option<PathBuf>,
    /// `FEXInterpreter` binary.
    pub interpreter: Option<PathBuf>,
    /// x86-64 Linux Wine inside the configured FEX RootFS (guest path).
    pub guest_wine: Option<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Toolset {
    /// Host-native Wine.
    pub wine: Option<WineInstall>,
    pub fex: FexTools,
    /// Kernel page size in bytes.
    pub page_size: u64,
}

impl Default for Toolset {
    fn default() -> Self {
        Toolset {
            wine: None,
            fex: FexTools::default(),
            page_size: REQUIRED_PAGE_SIZE,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RunnerKind {
    Wine,
    WineFex,
    FexInterpreter,
    WineArm64,
}

impl RunnerKind {
    pub const ALL: [RunnerKind; 4] = [
        RunnerKind::Wine,
        RunnerKind::WineFex,
        RunnerKind::FexInterpreter,
        RunnerKind::WineArm64,
    ];

    pub fn describe(self) -> &'static str {
        match self {
            RunnerKind::Wine => crate::t!("Wine (direct execution on x86-64)"),
            RunnerKind::WineFex => crate::t!("Wine ARM64EC/WoW64 + FEX (x86 → ARM translation)"),
            RunnerKind::FexInterpreter => {
                crate::t!("x86-64 Wine under FEXInterpreter (x86 → ARM translation)")
            }
            RunnerKind::WineArm64 => crate::t!("Wine ARM64 (native ARM execution)"),
        }
    }

    /// Whether x86 instructions are translated to ARM on this path.
    pub fn translates(self) -> bool {
        matches!(self, RunnerKind::WineFex | RunnerKind::FexInterpreter)
    }
}

impl std::fmt::Display for RunnerKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            RunnerKind::Wine => "wine",
            RunnerKind::WineFex => "wine-fex",
            RunnerKind::FexInterpreter => "fex-interpreter",
            RunnerKind::WineArm64 => "wine-arm64",
        })
    }
}

impl std::str::FromStr for RunnerKind {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        RunnerKind::ALL
            .into_iter()
            .find(|k| k.to_string() == s)
            .ok_or_else(|| {
                format!("unknown runner '{s}' (wine, wine-fex, fex-interpreter, wine-arm64)")
            })
    }
}

/// A chosen way to run the program.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Runner {
    pub kind: RunnerKind,
    /// argv prefix that runs a Wine program: e.g. `["/usr/bin/wine"]` or
    /// `["/usr/bin/FEXInterpreter", "/usr/bin/wine"]`.
    pub wine_argv: Vec<PathBuf>,
    /// argv prefix for the matching `wineserver` (used for `-w` / `-k`).
    pub wineserver_argv: Vec<PathBuf>,
    /// Extra environment the runner itself needs.
    pub env: BTreeMap<String, String>,
    /// Caveats worth showing to the user.
    pub notes: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Unsupported {
    pub reason: String,
    pub hints: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum Selection {
    Run(Runner),
    Unsupported(Unsupported),
}

impl Selection {
    pub fn runner(&self) -> Option<&Runner> {
        match self {
            Selection::Run(r) => Some(r),
            Selection::Unsupported(_) => None,
        }
    }
}

fn unsupported(reason: impl Into<String>, hints: &[&str]) -> Selection {
    Selection::Unsupported(Unsupported {
        reason: reason.into(),
        hints: hints.iter().map(|h| h.to_string()).collect(),
    })
}

fn wineserver_for(wine: &WineInstall) -> Vec<PathBuf> {
    vec![
        wine.wineserver
            .clone()
            .unwrap_or_else(|| sibling(&wine.wine, "wineserver")),
    ]
}

fn sibling(path: &Path, name: &str) -> PathBuf {
    path.parent()
        .map(|p| p.join(name))
        .unwrap_or_else(|| PathBuf::from(name))
}

fn native_runner(kind: RunnerKind, wine: &WineInstall) -> Runner {
    Runner {
        kind,
        wine_argv: vec![wine.wine.clone()],
        wineserver_argv: wineserver_for(wine),
        env: BTreeMap::new(),
        notes: Vec::new(),
    }
}

/// Decide how to run a program of architecture `exe` on `host`.
///
/// `prefer` comes from the compatibility database (or the command line) and
/// only re-orders the AArch64 x86 paths; it can never pick a runner that is
/// not available or not applicable.
pub fn select(
    host: HostArch,
    exe: ExeArch,
    tools: &Toolset,
    prefer: Option<RunnerKind>,
) -> Selection {
    let mut sel = match host {
        HostArch::Other => unsupported(
            t!("Lucent Bridge supports x86-64 and AArch64 computers only."),
            &[],
        ),
        HostArch::X86_64 => select_x86_64(exe, tools),
        HostArch::Aarch64 => select_aarch64(exe, tools, prefer),
    };
    if let (Selection::Run(r), Some(p)) = (&mut sel, prefer)
        && r.kind != p
    {
        let kind = r.kind;
        r.notes.push(t!(
            "Preferred runner '{}' is not available or not applicable here; using '{}'.",
            p,
            kind
        ));
    }
    sel
}

fn arm_on_x86(exe: ExeArch) -> Selection {
    unsupported(
        t!(
            "This is a Windows-on-ARM program ({}). x86-64 computers cannot execute ARM \
             code, and Lucent Bridge does not translate ARM to x86.",
            exe
        ),
        &[t!(
            "Download the x64 (or x86) version of the program instead."
        )],
    )
}

fn unknown_machine(m: u16) -> Selection {
    unsupported(
        t!(
            "This program was built for a CPU Lucent Bridge does not support (machine type {}, \
             e.g. Itanium, MIPS or 32-bit ARM).",
            format!("0x{m:04x}")
        ),
        &[],
    )
}

fn no_wine() -> Selection {
    unsupported(
        t!("Wine is not installed."),
        &[t!(
            "Install it with: rpm-ostree install wine-core wine-mono (then reboot)."
        )],
    )
}

fn select_x86_64(exe: ExeArch, tools: &Toolset) -> Selection {
    match exe {
        ExeArch::Arm64 | ExeArch::Arm64ec | ExeArch::Arm64x => arm_on_x86(exe),
        ExeArch::Unknown(m) => unknown_machine(m),
        ExeArch::I386 | ExeArch::X86_64 | ExeArch::MsiPackage => {
            let Some(wine) = &tools.wine else {
                return no_wine();
            };
            let a = &wine.pe_arches;
            if exe == ExeArch::I386 && !a.i386 {
                return unsupported(
                    t!(
                        "This is a 32-bit Windows program, but the installed Wine has no 32-bit \
                     (i386) support."
                    ),
                    &[t!(
                        "Install a Wine build with WoW64 support (Fedora's wine-core package has it)."
                    )],
                );
            }
            if exe == ExeArch::X86_64 && !a.x86_64 {
                return unsupported(
                    t!(
                        "This is a 64-bit Windows program, but the installed Wine has no x86-64 \
                     Windows DLLs."
                    ),
                    &[t!("Reinstall Wine: rpm-ostree install wine-core")],
                );
            }
            if exe == ExeArch::MsiPackage && !a.x86_64 && !a.i386 {
                return unsupported(
                    t!("The installed Wine has no x86 Windows DLLs to run msiexec."),
                    &[t!("Reinstall Wine: rpm-ostree install wine-core")],
                );
            }
            Selection::Run(native_runner(RunnerKind::Wine, wine))
        }
    }
}

/// What an x86 path on AArch64 lacks, for the "unsupported" hints.
fn missing_for_x86(exe: ExeArch, tools: &Toolset) -> Vec<String> {
    let mut hints = Vec::new();
    let arches = tools.wine.as_ref().map(|w| &w.pe_arches);
    let need_ec = exe != ExeArch::I386;
    let need_wow = exe != ExeArch::X86_64;
    let mut primary = Vec::new();
    match arches {
        None => primary.push(t!("an ARM64 Wine").to_string()),
        Some(a) => {
            if !a.aarch64 {
                primary.push(t!("an ARM64 Wine (the installed one is not)").into());
            }
            if need_ec && !a.arm64ec {
                primary.push(t!("a Wine built with ARM64EC support").into());
            }
            if need_wow && !a.i386 {
                primary.push(t!("Wine's 32-bit (i386) PE DLLs").into());
            }
        }
    }
    if need_ec && tools.fex.arm64ec_dll.is_none() {
        primary.push(t!("FEX's {}", FEX_ARM64EC_DLL));
    }
    if need_wow && tools.fex.wow64_dll.is_none() {
        primary.push(t!("FEX's {}", FEX_WOW64_DLL));
    }
    if !primary.is_empty() {
        hints.push(t!(
            "Recommended: install {} (packages wine-core and fex-emu-wine).",
            primary.join(", ")
        ));
    }
    let mut fallback = Vec::new();
    if tools.fex.interpreter.is_none() {
        fallback.push(t!("FEX-Emu (FEXInterpreter)"));
    }
    if tools.fex.guest_wine.is_none() {
        fallback.push(t!("an x86-64 RootFS that contains Wine"));
    }
    if !fallback.is_empty() {
        hints.push(t!("Alternative: install {}.", fallback.join(t!(" and "))));
    }
    hints
}

fn try_wine_fex(exe: ExeArch, tools: &Toolset) -> Option<Runner> {
    let wine = tools.wine.as_ref()?;
    let a = &wine.pe_arches;
    if !a.aarch64 {
        return None;
    }
    let ec = a.arm64ec && tools.fex.arm64ec_dll.is_some();
    let wow = a.i386 && tools.fex.wow64_dll.is_some();
    let usable = match exe {
        ExeArch::X86_64 => ec,
        ExeArch::I386 => wow,
        // msiexec itself is native ARM64; any emulator helps custom actions.
        ExeArch::MsiPackage => ec || wow,
        _ => false,
    };
    if !usable {
        return None;
    }
    let mut r = native_runner(RunnerKind::WineFex, wine);
    // Always configure every emulator we have: a 32-bit installer commonly
    // launches the 64-bit program it just installed, and vice versa.
    if ec {
        r.env.insert(ENV_HODLL64.into(), FEX_ARM64EC_DLL.into());
    }
    if wow {
        r.env.insert(ENV_HODLL.into(), FEX_WOW64_DLL.into());
    }
    if !ec {
        r.notes.push(
            t!(
                "No x86-64 emulator (libarm64ecfex.dll): 64-bit x86 programs started by this one \
             will fail."
            )
            .into(),
        );
    }
    if !wow {
        r.notes.push(
            t!(
                "No 32-bit x86 emulator (libwow64fex.dll): 32-bit programs started by this one \
             will fail."
            )
            .into(),
        );
    }
    Some(r)
}

fn try_fex_interpreter(tools: &Toolset) -> Option<Runner> {
    let interp = tools.fex.interpreter.clone()?;
    let guest_wine = tools.fex.guest_wine.clone()?;
    Some(Runner {
        kind: RunnerKind::FexInterpreter,
        wineserver_argv: vec![interp.clone(), sibling(&guest_wine, "wineserver")],
        wine_argv: vec![interp, guest_wine],
        env: BTreeMap::new(),
        notes: vec![
            t!(
                "Fallback path: all of Wine runs translated, so expect lower performance than the \
             ARM64EC path."
            )
            .into(),
        ],
    })
}

fn select_aarch64(exe: ExeArch, tools: &Toolset, prefer: Option<RunnerKind>) -> Selection {
    if let ExeArch::Unknown(m) = exe {
        return unknown_machine(m);
    }
    if tools.page_size != REQUIRED_PAGE_SIZE {
        return unsupported(
            t!(
                "This kernel uses {} KiB memory pages, but Wine and FEX need 4 KiB pages to run \
                 Windows programs. The usual workaround (on Apple-silicon Macs, for example) is \
                 a micro-VM, and Lucent Bridge does not use virtual machines.",
                tools.page_size / 1024
            ),
            &[t!(
                "Boot a kernel built with 4 KiB pages, if your hardware supports one."
            )],
        );
    }
    match exe {
        ExeArch::Unknown(_) => unreachable!("handled above"),
        ExeArch::Arm64 | ExeArch::Arm64x => match &tools.wine {
            Some(w) if w.pe_arches.aarch64 => {
                Selection::Run(native_runner(RunnerKind::WineArm64, w))
            }
            Some(_) => unsupported(
                t!(
                    "This ARM64 Windows program needs an ARM64 Wine, but the installed Wine is not \
                 an ARM64 build."
                ),
                &[t!("Install Wine for AArch64: rpm-ostree install wine-core")],
            ),
            None => no_wine(),
        },
        ExeArch::Arm64ec => {
            match &tools.wine {
                Some(w) if w.pe_arches.aarch64 && w.pe_arches.arm64ec => {
                    let mut r = native_runner(RunnerKind::WineArm64, w);
                    if tools.fex.arm64ec_dll.is_some() {
                        r.env.insert(ENV_HODLL64.into(), FEX_ARM64EC_DLL.into());
                    } else {
                        r.notes.push(
                        t!("ARM64EC programs may contain x86-64 code; without libarm64ecfex.dll \
                         those parts will fail.")
                        .into(),
                    );
                    }
                    Selection::Run(r)
                }
                Some(_) => unsupported(
                    t!("This ARM64EC program needs a Wine built with ARM64EC support."),
                    &[t!(
                        "Install an ARM64EC-enabled Wine build (Wine 10 or newer, built with \
                   --enable-archs=arm64ec,aarch64,i386)."
                    )],
                ),
                None => no_wine(),
            }
        }
        ExeArch::I386 | ExeArch::X86_64 | ExeArch::MsiPackage => {
            let order = if prefer == Some(RunnerKind::FexInterpreter) {
                [RunnerKind::FexInterpreter, RunnerKind::WineFex]
            } else {
                [RunnerKind::WineFex, RunnerKind::FexInterpreter]
            };
            for kind in order {
                let r = match kind {
                    RunnerKind::WineFex => try_wine_fex(exe, tools),
                    _ => try_fex_interpreter(tools),
                };
                if let Some(r) = r {
                    return Selection::Run(r);
                }
            }
            if exe == ExeArch::MsiPackage
                && let Some(w) = tools.wine.as_ref().filter(|w| w.pe_arches.aarch64)
            {
                let mut r = native_runner(RunnerKind::WineArm64, w);
                r.notes.push(
                    t!("No x86 emulator is installed: msiexec runs natively, but x86 custom actions \
                     in this package (and the installed program, if it is x86) will fail.")
                    .into(),
                );
                return Selection::Run(r);
            }
            let hints = missing_for_x86(exe, tools);
            Selection::Unsupported(Unsupported {
                reason: t!(
                    "{} Running it on this ARM computer needs x86 → ARM translation, and no \
                     complete translation setup is installed.",
                    match exe {
                        ExeArch::MsiPackage =>
                            t!("This is a Windows Installer package.").to_string(),
                        _ => t!("This is an {} Windows program.", exe),
                    }
                ),
                hints,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    fn wine(arches: WinePeArches) -> Option<WineInstall> {
        Some(WineInstall {
            wine: p("/usr/bin/wine"),
            wineserver: Some(p("/usr/bin/wineserver")),
            pe_arches: arches,
        })
    }

    fn x86_wine() -> Option<WineInstall> {
        wine(WinePeArches {
            i386: true,
            x86_64: true,
            ..Default::default()
        })
    }

    fn arm_wine(arm64ec: bool) -> Option<WineInstall> {
        wine(WinePeArches {
            i386: true,
            aarch64: true,
            arm64ec,
            ..Default::default()
        })
    }

    fn fex_dlls() -> FexTools {
        FexTools {
            arm64ec_dll: Some(p("/usr/lib64/wine/aarch64-windows/libarm64ecfex.dll")),
            wow64_dll: Some(p("/usr/lib64/wine/aarch64-windows/libwow64fex.dll")),
            ..Default::default()
        }
    }

    fn fex_interp() -> FexTools {
        FexTools {
            interpreter: Some(p("/usr/bin/FEXInterpreter")),
            guest_wine: Some(p("/usr/bin/wine")),
            ..Default::default()
        }
    }

    fn kind(s: &Selection) -> Option<RunnerKind> {
        s.runner().map(|r| r.kind)
    }

    #[test]
    fn x86_host_runs_x86_directly() {
        let t = Toolset {
            wine: x86_wine(),
            ..Default::default()
        };
        for exe in [ExeArch::I386, ExeArch::X86_64, ExeArch::MsiPackage] {
            let s = select(HostArch::X86_64, exe, &t, None);
            let r = s.runner().expect("runs");
            assert_eq!(r.kind, RunnerKind::Wine);
            assert_eq!(r.wine_argv, vec![p("/usr/bin/wine")]);
            assert_eq!(r.wineserver_argv, vec![p("/usr/bin/wineserver")]);
            assert!(r.env.is_empty());
        }
    }

    #[test]
    fn x86_host_without_32bit_wine() {
        let t = Toolset {
            wine: wine(WinePeArches {
                x86_64: true,
                ..Default::default()
            }),
            ..Default::default()
        };
        let s = select(HostArch::X86_64, ExeArch::I386, &t, None);
        let Selection::Unsupported(u) = s else {
            panic!()
        };
        assert!(u.reason.contains("32-bit"));
        assert_eq!(
            kind(&select(HostArch::X86_64, ExeArch::X86_64, &t, None)),
            Some(RunnerKind::Wine)
        );
    }

    #[test]
    fn x86_host_rejects_arm_programs() {
        let t = Toolset {
            wine: x86_wine(),
            ..Default::default()
        };
        for exe in [ExeArch::Arm64, ExeArch::Arm64ec, ExeArch::Arm64x] {
            let Selection::Unsupported(u) = select(HostArch::X86_64, exe, &t, None) else {
                panic!("{exe:?}")
            };
            assert!(u.reason.contains("Windows-on-ARM"));
            assert!(!u.hints.is_empty());
        }
    }

    #[test]
    fn no_wine_anywhere() {
        let t = Toolset::default();
        let Selection::Unsupported(u) = select(HostArch::X86_64, ExeArch::X86_64, &t, None) else {
            panic!()
        };
        assert_eq!(u.reason, "Wine is not installed.");
    }

    #[test]
    fn arm_host_primary_is_arm64ec_fex() {
        let t = Toolset {
            wine: arm_wine(true),
            fex: FexTools { ..fex_dlls() },
            ..Default::default()
        };
        let s = select(HostArch::Aarch64, ExeArch::X86_64, &t, None);
        let r = s.runner().unwrap();
        assert_eq!(r.kind, RunnerKind::WineFex);
        assert_eq!(
            r.env.get(ENV_HODLL64).map(String::as_str),
            Some(FEX_ARM64EC_DLL)
        );
        assert_eq!(
            r.env.get(ENV_HODLL).map(String::as_str),
            Some(FEX_WOW64_DLL)
        );
        assert!(r.notes.is_empty());
        let r = select(HostArch::Aarch64, ExeArch::I386, &t, None);
        assert_eq!(kind(&r), Some(RunnerKind::WineFex));
    }

    #[test]
    fn arm_host_primary_beats_fallback_unless_preferred() {
        let mut fex = fex_dlls();
        fex.interpreter = fex_interp().interpreter;
        fex.guest_wine = fex_interp().guest_wine;
        let t = Toolset {
            wine: arm_wine(true),
            fex,
            ..Default::default()
        };
        assert_eq!(
            kind(&select(HostArch::Aarch64, ExeArch::X86_64, &t, None)),
            Some(RunnerKind::WineFex)
        );
        let s = select(
            HostArch::Aarch64,
            ExeArch::X86_64,
            &t,
            Some(RunnerKind::FexInterpreter),
        );
        let r = s.runner().unwrap();
        assert_eq!(r.kind, RunnerKind::FexInterpreter);
        assert_eq!(
            r.wine_argv,
            vec![p("/usr/bin/FEXInterpreter"), p("/usr/bin/wine")]
        );
        assert_eq!(
            r.wineserver_argv,
            vec![p("/usr/bin/FEXInterpreter"), p("/usr/bin/wineserver")]
        );
    }

    #[test]
    fn arm_host_falls_back_to_fex_interpreter() {
        // Wine without ARM64EC: x86-64 programs can't use the primary path.
        let t = Toolset {
            wine: arm_wine(false),
            fex: FexTools {
                wow64_dll: fex_dlls().wow64_dll,
                ..fex_interp()
            },
            ..Default::default()
        };
        assert_eq!(
            kind(&select(HostArch::Aarch64, ExeArch::X86_64, &t, None)),
            Some(RunnerKind::FexInterpreter)
        );
        // 32-bit still has the WoW64 primary path.
        assert_eq!(
            kind(&select(HostArch::Aarch64, ExeArch::I386, &t, None)),
            Some(RunnerKind::WineFex)
        );
    }

    #[test]
    fn arm_host_without_translation_explains_what_is_missing() {
        let t = Toolset {
            wine: arm_wine(false),
            ..Default::default()
        };
        let Selection::Unsupported(u) = select(HostArch::Aarch64, ExeArch::X86_64, &t, None) else {
            panic!()
        };
        assert!(u.reason.contains("x86 → ARM translation"));
        let hints = u.hints.join("\n");
        assert!(hints.contains("ARM64EC"), "{hints}");
        assert!(hints.contains(FEX_ARM64EC_DLL), "{hints}");
        assert!(hints.contains("FEXInterpreter"), "{hints}");
        assert!(
            !hints.contains(FEX_WOW64_DLL),
            "i386 DLL not needed: {hints}"
        );
    }

    #[test]
    fn arm_host_runs_arm_programs_natively() {
        let t = Toolset {
            wine: arm_wine(true),
            fex: fex_dlls(),
            ..Default::default()
        };
        let r = select(HostArch::Aarch64, ExeArch::Arm64, &t, None);
        let r = r.runner().unwrap();
        assert_eq!(r.kind, RunnerKind::WineArm64);
        assert!(r.env.is_empty(), "nothing to translate");
        let r = select(HostArch::Aarch64, ExeArch::Arm64ec, &t, None);
        let r = r.runner().unwrap();
        assert_eq!(r.kind, RunnerKind::WineArm64);
        assert!(r.env.contains_key(ENV_HODLL64), "x64 code inside ARM64EC");
        // ARM64EC needs an ARM64EC Wine; ARM64X only needs ARM64.
        let t = Toolset {
            wine: arm_wine(false),
            ..Default::default()
        };
        assert!(kind(&select(HostArch::Aarch64, ExeArch::Arm64ec, &t, None)).is_none());
        assert_eq!(
            kind(&select(HostArch::Aarch64, ExeArch::Arm64x, &t, None)),
            Some(RunnerKind::WineArm64)
        );
    }

    #[test]
    fn arm_host_msi_without_emulator_runs_native_with_warning() {
        let t = Toolset {
            wine: arm_wine(true),
            ..Default::default()
        };
        let s = select(HostArch::Aarch64, ExeArch::MsiPackage, &t, None);
        let r = s.runner().unwrap();
        assert_eq!(r.kind, RunnerKind::WineArm64);
        assert!(r.notes[0].contains("custom actions"));
    }

    #[test]
    fn sixteen_k_pages_refuse_instead_of_vm() {
        let mut fex = fex_dlls();
        fex.interpreter = fex_interp().interpreter;
        fex.guest_wine = fex_interp().guest_wine;
        let t = Toolset {
            wine: arm_wine(true),
            fex,
            page_size: 16384,
        };
        for exe in ExeArch::ALL {
            let Selection::Unsupported(u) = select(HostArch::Aarch64, exe, &t, None) else {
                panic!("{exe:?}")
            };
            if !matches!(exe, ExeArch::Unknown(_)) {
                assert!(u.reason.contains("16 KiB"), "{}", u.reason);
                assert!(u.reason.contains("virtual machines"));
            }
        }
    }

    #[test]
    fn preference_note_when_not_applicable() {
        let t = Toolset {
            wine: x86_wine(),
            ..Default::default()
        };
        let s = select(
            HostArch::X86_64,
            ExeArch::X86_64,
            &t,
            Some(RunnerKind::FexInterpreter),
        );
        let r = s.runner().unwrap();
        assert_eq!(r.kind, RunnerKind::Wine);
        assert!(r.notes.iter().any(|n| n.contains("fex-interpreter")));
    }

    #[test]
    fn runner_kind_roundtrip() {
        for k in RunnerKind::ALL {
            assert_eq!(k.to_string().parse::<RunnerKind>().unwrap(), k);
        }
        assert!("vm".parse::<RunnerKind>().is_err());
    }

    // ---- Exhaustive sweep over every combination of inputs ----

    fn all_wines() -> Vec<Option<WineInstall>> {
        let mut v = vec![None];
        for bits in 0..16u8 {
            v.push(wine(WinePeArches {
                i386: bits & 1 != 0,
                x86_64: bits & 2 != 0,
                aarch64: bits & 4 != 0,
                arm64ec: bits & 8 != 0,
            }));
        }
        v
    }

    fn all_fex() -> Vec<FexTools> {
        (0..16u8)
            .map(|bits| FexTools {
                arm64ec_dll: (bits & 1 != 0).then(|| p("/fex/libarm64ecfex.dll")),
                wow64_dll: (bits & 2 != 0).then(|| p("/fex/libwow64fex.dll")),
                interpreter: (bits & 4 != 0).then(|| p("/usr/bin/FEXInterpreter")),
                guest_wine: (bits & 8 != 0).then(|| p("/usr/bin/wine")),
            })
            .collect()
    }

    fn for_all(mut f: impl FnMut(HostArch, ExeArch, &Toolset, Option<RunnerKind>, &Selection)) {
        let prefs = [
            None,
            Some(RunnerKind::Wine),
            Some(RunnerKind::WineFex),
            Some(RunnerKind::FexInterpreter),
            Some(RunnerKind::WineArm64),
        ];
        for host in HostArch::ALL {
            for exe in ExeArch::ALL {
                for w in all_wines() {
                    for fex in all_fex() {
                        for page_size in [4096, 16384, 65536] {
                            let t = Toolset {
                                wine: w.clone(),
                                fex: fex.clone(),
                                page_size,
                            };
                            for pref in prefs {
                                let s = select(host, exe, &t, pref);
                                f(host, exe, &t, pref, &s);
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn exhaustive_invariants() {
        let mut runs = 0usize;
        let mut total = 0usize;
        for_all(|host, exe, t, pref, s| {
            total += 1;
            let ctx = format!("{host:?} {exe:?} {t:?} {pref:?} → {s:?}");
            // Determinism.
            assert_eq!(&select(host, exe, t, pref), s, "{ctx}");
            match s {
                Selection::Unsupported(u) => {
                    assert!(!u.reason.trim().is_empty(), "{ctx}");
                    assert!(u.reason.ends_with('.'), "reason is a sentence: {ctx}");
                }
                Selection::Run(r) => {
                    runs += 1;
                    assert_ne!(host, HostArch::Other, "{ctx}");
                    assert!(!matches!(exe, ExeArch::Unknown(_)), "{ctx}");
                    // Every binary we launch came from the toolset.
                    let known: Vec<&PathBuf> = t
                        .wine
                        .iter()
                        .flat_map(|w| [Some(&w.wine), w.wineserver.as_ref()])
                        .flatten()
                        .chain(t.fex.interpreter.iter())
                        .chain(t.fex.guest_wine.iter())
                        .collect();
                    assert!(known.contains(&&r.wine_argv[0]), "{ctx}");
                    assert!(!r.wineserver_argv.is_empty(), "{ctx}");
                    match host {
                        HostArch::X86_64 => {
                            // Direct execution only; never translate on x86.
                            assert_eq!(r.kind, RunnerKind::Wine, "{ctx}");
                            assert!(exe.is_x86_family() || exe == ExeArch::MsiPackage, "{ctx}");
                            assert!(r.env.is_empty(), "{ctx}");
                        }
                        HostArch::Aarch64 => {
                            assert_ne!(r.kind, RunnerKind::Wine, "{ctx}");
                            assert_eq!(t.page_size, 4096, "{ctx}");
                            if exe.is_x86_family() {
                                assert!(r.kind.translates(), "x86 on ARM must translate: {ctx}");
                            }
                            if matches!(exe, ExeArch::Arm64 | ExeArch::Arm64x) {
                                assert_eq!(r.kind, RunnerKind::WineArm64, "{ctx}");
                            }
                        }
                        HostArch::Other => unreachable!(),
                    }
                    if r.kind == RunnerKind::WineFex {
                        let w = t.wine.as_ref().unwrap();
                        assert!(w.pe_arches.aarch64, "{ctx}");
                        if exe == ExeArch::X86_64 {
                            assert!(w.pe_arches.arm64ec && t.fex.arm64ec_dll.is_some(), "{ctx}");
                        }
                        if exe == ExeArch::I386 {
                            assert!(w.pe_arches.i386 && t.fex.wow64_dll.is_some(), "{ctx}");
                        }
                        // Env vars only name DLLs that exist.
                        assert_eq!(
                            r.env.contains_key(ENV_HODLL64),
                            w.pe_arches.arm64ec && t.fex.arm64ec_dll.is_some(),
                            "{ctx}"
                        );
                        assert_eq!(
                            r.env.contains_key(ENV_HODLL),
                            w.pe_arches.i386 && t.fex.wow64_dll.is_some(),
                            "{ctx}"
                        );
                    }
                    if r.kind == RunnerKind::FexInterpreter {
                        assert!(t.fex.interpreter.is_some() && t.fex.guest_wine.is_some());
                        assert_eq!(r.wine_argv.len(), 2, "{ctx}");
                    }
                    if pref.is_some_and(|p| p != r.kind) {
                        assert!(r.notes.iter().any(|n| n.contains("Preferred")), "{ctx}");
                    }
                }
            }
        });
        assert_eq!(total, 3 * 7 * 17 * 16 * 3 * 5);
        assert!(runs > 1000, "sweep exercised the success paths ({runs})");
    }

    #[test]
    fn exhaustive_availability_is_exact_for_x86_on_arm() {
        // On a 4K AArch64 host an x86-64 program runs iff the primary path
        // (ARM64 + ARM64EC Wine + libarm64ecfex.dll) or the fallback path
        // (FEXInterpreter + guest Wine) is complete.
        for_all(|host, exe, t, pref, s| {
            if host != HostArch::Aarch64 || t.page_size != 4096 {
                return;
            }
            let a = t
                .wine
                .as_ref()
                .map(|w| w.pe_arches.clone())
                .unwrap_or_default();
            let fallback = t.fex.interpreter.is_some() && t.fex.guest_wine.is_some();
            let expect = match exe {
                ExeArch::X86_64 => {
                    let primary = a.aarch64 && a.arm64ec && t.fex.arm64ec_dll.is_some();
                    Some((primary, fallback))
                }
                ExeArch::I386 => {
                    let primary = a.aarch64 && a.i386 && t.fex.wow64_dll.is_some();
                    Some((primary, fallback))
                }
                _ => None,
            };
            if let Some((primary, fallback)) = expect {
                let ctx = format!("{exe:?} {t:?} {pref:?} → {s:?}");
                assert_eq!(s.runner().is_some(), primary || fallback, "{ctx}");
                let k = kind(s);
                if primary && fallback {
                    let want = if pref == Some(RunnerKind::FexInterpreter) {
                        RunnerKind::FexInterpreter
                    } else {
                        RunnerKind::WineFex
                    };
                    assert_eq!(k, Some(want), "{ctx}");
                } else if primary {
                    assert_eq!(k, Some(RunnerKind::WineFex), "{ctx}");
                } else if fallback {
                    assert_eq!(k, Some(RunnerKind::FexInterpreter), "{ctx}");
                }
            }
        });
    }

    #[test]
    fn exhaustive_x86_host_availability() {
        for_all(|host, exe, t, _pref, s| {
            if host != HostArch::X86_64 {
                return;
            }
            let a = t.wine.as_ref().map(|w| &w.pe_arches);
            let expect = match exe {
                ExeArch::I386 => a.is_some_and(|a| a.i386),
                ExeArch::X86_64 => a.is_some_and(|a| a.x86_64),
                ExeArch::MsiPackage => a.is_some_and(|a| a.i386 || a.x86_64),
                _ => false,
            };
            assert_eq!(s.runner().is_some(), expect, "{exe:?} {t:?}");
        });
    }
}
