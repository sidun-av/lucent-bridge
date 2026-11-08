//! Desktop notifications through `notify-send` or `kdialog`, when present
//! and a graphical session exists. Everything is best-effort: a missing
//! tool or a failing call never affects the program being run.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tool {
    NotifySend(PathBuf),
    Kdialog(PathBuf),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Error,
}

#[derive(Debug, Clone, Default)]
pub struct Notifier {
    /// Passive notifications.
    pub passive: Option<Tool>,
    /// Errors: a modal kdialog box if available (the user double-clicked an
    /// .exe and nothing visible happened otherwise), else a critical
    /// notification.
    pub error: Option<Tool>,
}

impl Notifier {
    pub fn none() -> Notifier {
        Notifier::default()
    }

    pub fn detect(
        env: &dyn Fn(&str) -> Option<String>,
        which: &dyn Fn(&str) -> Option<PathBuf>,
    ) -> Notifier {
        let graphical = ["DISPLAY", "WAYLAND_DISPLAY"]
            .iter()
            .any(|k| env(k).is_some_and(|v| !v.is_empty()));
        if !graphical {
            return Notifier::none();
        }
        let ns = which("notify-send").map(Tool::NotifySend);
        let kd = which("kdialog").map(Tool::Kdialog);
        Notifier {
            passive: ns.clone().or_else(|| kd.clone()),
            error: kd.or(ns),
        }
    }

    pub fn info(&self, title: &str, body: &str, icon: Option<&Path>) {
        if let Some(t) = &self.passive {
            spawn(command(t, Level::Info, title, body, icon));
        }
    }

    pub fn error(&self, title: &str, body: &str) {
        if let Some(t) = &self.error {
            spawn(command(t, Level::Error, title, body, None));
        }
    }
}

/// The exact argv for one notification (pure; tested).
pub fn command(
    tool: &Tool,
    level: Level,
    title: &str,
    body: &str,
    icon: Option<&Path>,
) -> Vec<String> {
    match tool {
        Tool::NotifySend(p) => {
            let mut v = vec![
                p.to_string_lossy().into_owned(),
                "--app-name=Lucent Bridge".into(),
            ];
            if level == Level::Error {
                v.push("--urgency=critical".into());
            }
            let icon = icon
                .map(|i| i.to_string_lossy().into_owned())
                .unwrap_or_else(|| {
                    if level == Level::Error {
                        "dialog-error".into()
                    } else {
                        "application-x-ms-dos-executable".into()
                    }
                });
            v.push(format!("--icon={icon}"));
            v.push("--".into());
            v.push(title.into());
            v.push(body.into());
            v
        }
        Tool::Kdialog(p) => {
            let mut v = vec![
                p.to_string_lossy().into_owned(),
                "--title".into(),
                title.into(),
            ];
            match level {
                Level::Info => {
                    v.push("--passivepopup".into());
                    v.push(body.into());
                    v.push("8".into());
                }
                Level::Error => {
                    v.push("--error".into());
                    v.push(body.into());
                }
            }
            v
        }
    }
}

/// Fire and forget; a helper thread reaps the child so long `run`
/// sessions don't accumulate zombies. Modal dialogs don't block us.
fn spawn(argv: Vec<String>) {
    let Some((prog, args)) = argv.split_first() else {
        return;
    };
    let child = Command::new(prog)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    if let Ok(mut c) = child {
        std::thread::spawn(move || {
            let _ = c.wait();
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn which_both(n: &str) -> Option<PathBuf> {
        Some(PathBuf::from(format!("/usr/bin/{n}")))
    }

    #[test]
    fn silent_without_graphical_session() {
        let n = Notifier::detect(&|_| None, &which_both);
        assert!(n.passive.is_none() && n.error.is_none());
        // And calling it is a no-op.
        n.info("t", "b", None);
        n.error("t", "b");
    }

    #[test]
    fn prefers_notify_send_for_info_and_kdialog_for_errors() {
        let env = |k: &str| (k == "WAYLAND_DISPLAY").then(|| "wayland-0".to_string());
        let n = Notifier::detect(&env, &which_both);
        assert_eq!(
            n.passive,
            Some(Tool::NotifySend("/usr/bin/notify-send".into()))
        );
        assert_eq!(n.error, Some(Tool::Kdialog("/usr/bin/kdialog".into())));
        let only_kd = |name: &str| (name == "kdialog").then(|| PathBuf::from("/usr/bin/kdialog"));
        let n = Notifier::detect(&env, &only_kd);
        assert_eq!(n.passive, Some(Tool::Kdialog("/usr/bin/kdialog".into())));
        let nothing = |_: &str| None;
        let n = Notifier::detect(&env, &nothing);
        assert!(n.passive.is_none() && n.error.is_none());
    }

    #[test]
    fn commands() {
        let ns = Tool::NotifySend("/usr/bin/notify-send".into());
        let c = command(
            &ns,
            Level::Info,
            "Foo",
            "-installed",
            Some(Path::new("/i.png")),
        );
        assert_eq!(
            c,
            [
                "/usr/bin/notify-send",
                "--app-name=Lucent Bridge",
                "--icon=/i.png",
                "--",
                "Foo",
                "-installed"
            ]
        );
        let c = command(&ns, Level::Error, "T", "B", None);
        assert!(c.contains(&"--urgency=critical".to_string()));
        let kd = Tool::Kdialog("/usr/bin/kdialog".into());
        assert_eq!(
            command(&kd, Level::Error, "T", "B", None),
            ["/usr/bin/kdialog", "--title", "T", "--error", "B"]
        );
        assert_eq!(
            command(&kd, Level::Info, "T", "B", None),
            [
                "/usr/bin/kdialog",
                "--title",
                "T",
                "--passivepopup",
                "B",
                "8"
            ]
        );
    }
}
