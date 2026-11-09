//! Machine-readable progress of `run`: one JSON object per line, for a
//! launch sheet (a calm "Preparing Windows program" window) or any other
//! UI.
//!
//! Where the lines go:
//! - `run --progress-fd N`: to file descriptor N, which the caller opened
//!   (the caller is the UI; no sheet is started);
//! - otherwise, in a graphical session, to the stdin of the sheet program
//!   (`lucent-bridge-sheet` on `PATH`, or `$LUCENT_BRIDGE_SHEET`; `none`
//!   turns it off), started only when there is something to show: a first
//!   launch that prepares the Windows environment, components being
//!   installed, or an error to explain. A program that is already set up
//!   starts without any sheet.
//!
//! The sheet answers through its exit status: [`SHEET_CANCEL`] when the
//! user pressed Cancel (Lucent Bridge stops, and removes a Windows
//! environment it had only begun to create), [`SHEET_RUN_ANYWAY`] when the
//! user chose to run a program despite a warning. With `--progress-fd`,
//! SIGTERM / SIGINT during the preparation cancel in the same way.
//!
//! Protocol version 1 (every line has `"v":1` and `"event"`):
//!
//! ```json
//! {"v":1,"event":"start","name":"Notepad++ Installer","file":"npp.8.9.9.Installer.x64.exe","size":7028968,"icon":"/home/anna/.local/share/lucent-bridge/icons/npp-1/_source.png","installer":true,"program_arch":"x86-64","host_arch":"x86-64","translation":false,"first_launch":true}
//! {"v":1,"event":"step","step":"prepare"}
//! {"v":1,"event":"progress","fraction":0.42}
//! {"v":1,"event":"step","step":"components","detail":"vcrun2019"}
//! {"v":1,"event":"step","step":"launch"}
//! {"v":1,"event":"launched"}
//! {"v":1,"event":"error","kind":"blocked","title":"VALORANT won't work","message":"…","hints":[],"can_run_anyway":false}
//! {"v":1,"event":"installed","name":"Notepad++","entries":1}
//! {"v":1,"event":"cancelled"}
//! {"v":1,"event":"exit","code":0}
//! ```
//!
//! `title`, `message` and `hints` are already translated; everything else
//! is for the UI to word (and translate) itself.

use serde::Serialize;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

pub const VERSION: u32 = 1;
pub const ENV_SHEET: &str = "LUCENT_BRIDGE_SHEET";
pub const SHEET_NAME: &str = "lucent-bridge-sheet";
/// Sheet exit status: the user cancelled.
pub const SHEET_CANCEL: i32 = 2;
/// Sheet exit status: the user chose "Run anyway" on a warning.
pub const SHEET_RUN_ANYWAY: i32 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// Creating the Windows environment (first launch only).
    Prepare,
    /// Installing components (winetricks verbs from the compat DB).
    Components,
    /// Starting the program.
    Launch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ErrorKind {
    /// Can't run on this computer at all (ARM program on x86, a DLL, ...).
    Unsupported,
    /// Known not to work (compat DB), e.g. kernel-mode anti-cheat.
    Blocked,
    /// On the disk of a hibernated Windows (Fast Startup).
    Hibernated,
    /// Not a Windows program, or unreadable.
    OpenFailed,
    /// Preparing or starting failed.
    Failed,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    Start {
        name: String,
        file: String,
        size: u64,
        icon: Option<String>,
        installer: bool,
        /// "x86-64", "x86-32", "ARM64", "ARM64EC", "MSI"
        program_arch: String,
        /// "x86-64" or "ARM64"
        host_arch: String,
        /// x86 instructions are translated to ARM (FEX)
        translation: bool,
        first_launch: bool,
    },
    Step {
        step: Phase,
        #[serde(skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    Progress {
        fraction: f64,
    },
    Launched,
    Installed {
        name: String,
        entries: usize,
    },
    Error {
        kind: ErrorKind,
        title: String,
        message: String,
        hints: Vec<String>,
        can_run_anyway: bool,
    },
    Cancelled,
    Exit {
        code: i32,
    },
}

#[derive(Serialize)]
struct Line<'a> {
    v: u32,
    #[serde(flatten)]
    event: &'a Event,
}

/// One protocol line, newline included.
pub fn line(event: &Event) -> String {
    let mut s = serde_json::to_string(&Line { v: VERSION, event }).unwrap_or_default();
    s.push('\n');
    s
}

/// Progress through a step of unknown length: rises quickly at first, then
/// slows, and never reaches the end before the step does (calm, never
/// stuck, never "99 %" for a minute).
pub fn eased(elapsed: Duration, expected: Duration) -> f64 {
    let t = elapsed.as_secs_f64() / expected.as_secs_f64().max(0.001);
    (1.0 - (-t * 1.6).exp()).min(0.95)
}

static SIGNALLED: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_: libc::c_int) {
    SIGNALLED.store(true, Ordering::SeqCst);
}

/// While preparing, SIGTERM/SIGINT/SIGHUP mean "cancel" (cleanly), not
/// "die now"; afterwards the defaults return, so the running program is
/// not affected.
pub struct CancelOnSignal;

impl CancelOnSignal {
    pub fn install() -> CancelOnSignal {
        #[cfg(unix)]
        // SAFETY: the handler only stores to an atomic.
        unsafe {
            for s in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
                libc::signal(s, on_signal as *const () as libc::sighandler_t);
            }
        }
        CancelOnSignal
    }
}

impl Drop for CancelOnSignal {
    fn drop(&mut self) {
        #[cfg(unix)]
        // SAFETY: restoring the default disposition.
        unsafe {
            for s in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
                libc::signal(s, libc::SIG_DFL);
            }
        }
    }
}

enum Sheet {
    /// No sheet program, or not wanted.
    None,
    /// Available, not started yet.
    Available(PathBuf),
    Running(Child, Option<ChildStdin>),
    /// It exited (with this status, if known).
    Exited(Option<i32>),
}

/// What the user decided on a warning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Close,
    RunAnyway,
}

pub struct Reporter {
    fd: Option<std::fs::File>,
    sheet: Sheet,
    /// Lines sent before the sheet was shown (it gets them all on start).
    backlog: Vec<String>,
    last_fraction: f64,
}

impl Reporter {
    pub fn none() -> Reporter {
        Reporter {
            fd: None,
            sheet: Sheet::None,
            backlog: Vec::new(),
            last_fraction: -1.0,
        }
    }

    /// Report to an inherited file descriptor (`--progress-fd`).
    #[cfg(unix)]
    pub fn to_fd(fd: i32) -> std::io::Result<Reporter> {
        use std::os::fd::FromRawFd;
        // SAFETY: fcntl only queries the descriptor.
        if unsafe { libc::fcntl(fd, libc::F_GETFD) } == -1 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: the caller handed this descriptor to us to write to.
        let file = unsafe { std::fs::File::from_raw_fd(fd) };
        let mut r = Reporter::none();
        r.fd = Some(file);
        Ok(r)
    }

    /// Report to a sheet program, started on first need.
    pub fn with_sheet(program: PathBuf) -> Reporter {
        let mut r = Reporter::none();
        r.sheet = Sheet::Available(program);
        r
    }

    /// The sheet program to use, if this is a graphical session.
    pub fn find_sheet(
        env: &dyn Fn(&str) -> Option<String>,
        which: &dyn Fn(&str) -> Option<PathBuf>,
    ) -> Option<PathBuf> {
        let graphical = ["DISPLAY", "WAYLAND_DISPLAY"]
            .iter()
            .any(|k| env(k).is_some_and(|v| !v.is_empty()));
        if !graphical {
            return None;
        }
        match env(ENV_SHEET) {
            Some(v) if v.is_empty() || v == "none" => None,
            Some(v) => Some(PathBuf::from(v)).filter(|p| p.is_file()),
            None => which(SHEET_NAME),
        }
    }

    /// Whether anything is listening (a descriptor or a sheet).
    pub fn active(&self) -> bool {
        self.fd.is_some() || !matches!(self.sheet, Sheet::None | Sheet::Exited(_))
    }

    /// Whether a sheet is (or can be) shown: then errors go to it instead
    /// of a dialog box.
    pub fn has_sheet(&self) -> bool {
        matches!(self.sheet, Sheet::Available(_) | Sheet::Running(..))
    }

    pub fn send(&mut self, event: &Event) {
        if let Event::Progress { fraction } = event {
            if (fraction - self.last_fraction).abs() < 0.01 {
                return;
            }
            self.last_fraction = *fraction;
        }
        let text = line(event);
        if let Some(f) = &mut self.fd {
            let _ = f.write_all(text.as_bytes());
            let _ = f.flush();
        }
        match &mut self.sheet {
            Sheet::Running(_, Some(stdin)) => {
                if stdin
                    .write_all(text.as_bytes())
                    .and_then(|_| stdin.flush())
                    .is_err()
                {
                    self.poll();
                }
            }
            Sheet::Available(_) => self.backlog.push(text),
            _ => {}
        }
    }

    /// Show the sheet now (no-op if there is none or it already runs).
    pub fn show(&mut self) {
        let Sheet::Available(program) = &self.sheet else {
            return;
        };
        let child = Command::new(program)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn();
        self.sheet = match child {
            Ok(mut c) => {
                let mut stdin = c.stdin.take();
                if let Some(s) = &mut stdin {
                    for l in self.backlog.drain(..) {
                        let _ = s.write_all(l.as_bytes());
                    }
                    let _ = s.flush();
                }
                Sheet::Running(c, stdin)
            }
            Err(_) => Sheet::None,
        };
    }

    /// Check on the sheet; returns its exit status once it has exited.
    fn poll(&mut self) -> Option<i32> {
        if let Sheet::Running(c, _) = &mut self.sheet {
            match c.try_wait() {
                Ok(Some(st)) => self.sheet = Sheet::Exited(st.code()),
                Ok(None) => return None,
                Err(_) => self.sheet = Sheet::Exited(None),
            }
        }
        match self.sheet {
            Sheet::Exited(code) => code,
            _ => None,
        }
    }

    /// Whether the user asked to cancel (sheet's Cancel, or a signal).
    pub fn cancel_requested(&mut self) -> bool {
        SIGNALLED.load(Ordering::SeqCst) || self.poll() == Some(SHEET_CANCEL)
    }

    /// After an error event: wait (up to `limit`) for the sheet to close
    /// and tell what the user chose. Without a sheet: [`Decision::Close`].
    pub fn decision(&mut self, limit: Duration) -> Decision {
        let start = Instant::now();
        if matches!(self.sheet, Sheet::Running(..)) {
            // Closing stdin tells the sheet nothing more is coming.
            if let Sheet::Running(_, stdin) = &mut self.sheet {
                stdin.take();
            }
            while start.elapsed() < limit {
                if let Some(code) = self.poll() {
                    return if code == SHEET_RUN_ANYWAY {
                        Decision::RunAnyway
                    } else {
                        Decision::Close
                    };
                }
                if matches!(self.sheet, Sheet::Exited(None)) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        Decision::Close
    }

    /// The run is over: close the pipe (the sheet closes itself when it is
    /// done showing what it shows) and reap it in the background.
    pub fn finish(&mut self) {
        let fd = self.fd.take();
        drop(fd);
        if let Sheet::Running(c, stdin) = std::mem::replace(&mut self.sheet, Sheet::None) {
            drop(stdin);
            let mut c = c;
            std::thread::spawn(move || {
                let _ = c.wait();
            });
        }
    }
}

impl Drop for Reporter {
    fn drop(&mut self) {
        self.finish();
    }
}

/// Program architecture as shown to people.
pub fn arch_label(arch: crate::arch::ExeArch) -> &'static str {
    use crate::arch::ExeArch;
    match arch {
        ExeArch::I386 => "x86-32",
        ExeArch::X86_64 => "x86-64",
        ExeArch::Arm64 => "ARM64",
        ExeArch::Arm64ec => "ARM64EC",
        ExeArch::Arm64x => "ARM64X",
        ExeArch::MsiPackage => "MSI",
        ExeArch::Unknown(_) => "?",
    }
}

pub fn host_label(host: crate::arch::HostArch) -> &'static str {
    match host {
        crate::arch::HostArch::X86_64 => "x86-64",
        crate::arch::HostArch::Aarch64 => "ARM64",
        _ => "?",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_are_versioned_json_objects() {
        assert_eq!(
            line(&Event::Step {
                step: Phase::Prepare,
                detail: None
            }),
            "{\"v\":1,\"event\":\"step\",\"step\":\"prepare\"}\n"
        );
        assert_eq!(line(&Event::Launched), "{\"v\":1,\"event\":\"launched\"}\n");
        let e = line(&Event::Error {
            kind: ErrorKind::OpenFailed,
            title: "T".into(),
            message: "M \"q\"".into(),
            hints: vec!["h".into()],
            can_run_anyway: false,
        });
        let v: serde_json::Value = serde_json::from_str(&e).unwrap();
        assert_eq!(v["kind"], "open-failed");
        assert_eq!(v["message"], "M \"q\"");
        assert_eq!(v["v"], 1);
        let s = line(&Event::Start {
            name: "Notepad++ Installer".into(),
            file: "npp.exe".into(),
            size: 7,
            icon: None,
            installer: true,
            program_arch: "x86-64".into(),
            host_arch: "ARM64".into(),
            translation: true,
            first_launch: true,
        });
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["event"], "start");
        assert_eq!(v["translation"], true);
        assert!(v["icon"].is_null());
    }

    #[test]
    fn eased_progress_rises_and_never_finishes_early() {
        let e = Duration::from_secs(2);
        assert_eq!(eased(Duration::ZERO, e), 0.0);
        let a = eased(Duration::from_millis(500), e);
        let b = eased(Duration::from_secs(2), e);
        let c = eased(Duration::from_secs(60), e);
        assert!(0.0 < a && a < b && b < c, "{a} {b} {c}");
        assert!(b > 0.7, "{b}");
        assert_eq!(c, 0.95);
    }

    #[test]
    fn sheet_only_in_a_graphical_session_and_can_be_turned_off() {
        let which =
            |n: &str| (n == SHEET_NAME).then(|| PathBuf::from("/usr/bin/lucent-bridge-sheet"));
        assert_eq!(Reporter::find_sheet(&|_| None, &which), None);
        let wl = |k: &str| (k == "WAYLAND_DISPLAY").then(|| "wayland-0".to_string());
        assert_eq!(
            Reporter::find_sheet(&wl, &which),
            Some(PathBuf::from("/usr/bin/lucent-bridge-sheet"))
        );
        let off = |k: &str| match k {
            "WAYLAND_DISPLAY" => Some("wayland-0".to_string()),
            ENV_SHEET => Some("none".to_string()),
            _ => None,
        };
        assert_eq!(Reporter::find_sheet(&off, &which), None);
    }

    #[test]
    fn progress_lines_are_rate_limited_and_the_sheet_gets_the_backlog() {
        let mut r = Reporter::with_sheet("/nonexistent/sheet".into());
        r.send(&Event::Progress { fraction: 0.1 });
        r.send(&Event::Progress { fraction: 0.105 });
        r.send(&Event::Progress { fraction: 0.2 });
        assert_eq!(r.backlog.len(), 2);
        assert!(r.has_sheet());
        // A sheet that can't start is simply not there.
        r.show();
        assert!(!r.has_sheet() && !r.active());
        assert_eq!(r.decision(Duration::from_millis(10)), Decision::Close);
    }

    #[cfg(unix)]
    #[test]
    fn a_sheet_that_exits_with_cancel_cancels() {
        let tmp = tempfile::tempdir().unwrap();
        let sheet = tmp.path().join("sheet");
        let got = tmp.path().join("got.jsonl");
        std::fs::write(
            &sheet,
            format!(
                "#!/bin/sh\ncat > '{}'\nexit {SHEET_CANCEL}\n",
                got.display()
            ),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&sheet, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut r = Reporter::with_sheet(sheet);
        r.send(&Event::Step {
            step: Phase::Prepare,
            detail: None,
        });
        r.show();
        assert!(!r.cancel_requested());
        r.send(&Event::Progress { fraction: 0.5 });
        // The sheet reads until stdin closes, then "cancels".
        assert_eq!(r.decision(Duration::from_secs(10)), Decision::Close);
        assert!(r.cancel_requested());
        let text = std::fs::read_to_string(&got).unwrap();
        assert_eq!(text.lines().count(), 2, "{text}");
    }
}
