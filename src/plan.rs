//! A plan is the exact list of commands `run` will execute. `plan` prints
//! it; `run` executes it step by step — the same data, so the dry run can't
//! drift from reality.

use crate::arch::{ExeArch, HostArch};
use crate::compat::{Entry, MatchKind, Status};
use crate::inspect::Inspection;
use crate::runner::{RunnerKind, Selection, Unsupported};
use crate::t;
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StepKind {
    /// Copy the prebuilt prefix template (first launch, when one exists).
    CopyTemplate,
    /// Initialise a fresh Wine prefix (`wineboot --init`); skipped when
    /// the template was copied.
    CreatePrefix,
    /// Apply winetricks verbs from the compat DB.
    Winetricks,
    /// Start the program.
    Launch,
    /// Wait until every process in the prefix has exited (installers often
    /// hand off to child processes and exit early).
    WaitForPrefix,
    /// Turn shortcuts the installer created into launcher entries.
    Integrate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Step {
    pub kind: StepKind,
    pub description: String,
    /// Command line; empty for in-process steps (integration).
    pub argv: Vec<String>,
    /// Environment overlay for this step.
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CompatSummary {
    pub name: String,
    pub matched_by: MatchKind,
    pub status: Status,
    pub source: String,
    pub notes: String,
    pub native_alternative: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Plan {
    pub app_id: String,
    pub name: String,
    pub exe: PathBuf,
    pub exe_arch: ExeArch,
    pub host: HostArch,
    pub installer: bool,
    pub prefix: PathBuf,
    pub prefix_exists: bool,
    pub runner: Option<RunnerKind>,
    pub compat: Option<CompatSummary>,
    pub steps: Vec<Step>,
    pub notes: Vec<String>,
    /// Set when the program cannot run here at all.
    pub unsupported: Option<Unsupported>,
    /// Set when the compat DB says it will not work (overridable with
    /// `run --force`).
    pub blocked: Option<String>,
}

pub struct Inputs<'a> {
    pub inspection: &'a Inspection,
    pub host: HostArch,
    pub selection: Selection,
    pub compat: Option<(&'a Entry, MatchKind)>,
    pub app_id: String,
    pub prefix: PathBuf,
    pub prefix_exists: bool,
    pub verbs_applied: &'a [String],
    pub winetricks: Option<PathBuf>,
    pub args: &'a [String],
    pub integrate: bool,
    /// Unattended install: add the installer's silent switches.
    pub silent: bool,
    /// Prebuilt prefix to copy instead of running `wineboot --init`.
    pub template: Option<PathBuf>,
    /// The user's own `WINEDEBUG`, kept for debugging; otherwise Wine's
    /// diagnostics are silenced.
    pub winedebug: Option<String>,
}

fn s(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// `WINEDLLOVERRIDES` value: ours first (menu builder off so Wine does not
/// write its own launcher entries), then the compat entry's, sorted.
pub fn dll_overrides(compat: Option<&Entry>) -> String {
    let mut map = BTreeMap::new();
    map.insert("winemenubuilder.exe".to_string(), "d".to_string());
    if let Some(e) = compat {
        map.extend(e.overrides());
    }
    map.iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join(";")
}

pub fn build(i: Inputs<'_>) -> Plan {
    let insp = i.inspection;
    let entry = i.compat.map(|(e, _)| e);
    let compat = i.compat.map(|(e, k)| CompatSummary {
        name: e.name.clone(),
        matched_by: k,
        status: e.status,
        source: e.source.clone(),
        notes: e.notes.clone(),
        native_alternative: e.native_alternative.clone(),
    });
    let mut notes = Vec::new();
    if let Some(c) = &compat {
        if !c.notes.is_empty() {
            notes.push(format!("{}: {}", c.name, c.notes));
        }
        if let Some(alt) = &c.native_alternative {
            notes.push(t!("A native Linux version is available ({}).", alt));
        }
    }
    if insp.dotnet {
        notes.push(
            t!(
                ".NET program: Wine runs it with Wine Mono; if it needs Microsoft .NET, add a \
             dotnet verb to its compat entry."
            )
            .into(),
        );
    }
    let blocked = match &compat {
        Some(c) if c.status == Status::Broken => Some(if c.notes.is_empty() {
            t!("{} is known not to work.", c.name)
        } else {
            c.notes.clone()
        }),
        _ => None,
    };
    let mut plan = Plan {
        app_id: i.app_id.clone(),
        name: insp.display_name.clone(),
        exe: insp.path.clone(),
        exe_arch: insp.arch,
        host: i.host,
        installer: insp.is_installer(),
        prefix: i.prefix.clone(),
        prefix_exists: i.prefix_exists,
        runner: None,
        compat,
        steps: Vec::new(),
        notes,
        unsupported: None,
        blocked,
    };
    if insp.is_dll {
        plan.unsupported = Some(Unsupported {
            reason: t!("This is a DLL (a library), not a program that can be started.").into(),
            hints: vec![],
        });
        return plan;
    }
    let runner = match i.selection {
        Selection::Unsupported(u) => {
            plan.unsupported = Some(u);
            return plan;
        }
        Selection::Run(r) => r,
    };
    plan.runner = Some(runner.kind);
    plan.notes.extend(runner.notes.iter().cloned());

    let mut env = BTreeMap::new();
    let winedebug = i.winedebug.clone().filter(|v| !v.is_empty());
    env.insert(
        "WINEDEBUG".to_string(),
        winedebug.unwrap_or_else(|| "-all".to_string()),
    );
    if let Some(e) = entry {
        env.extend(e.env.clone());
    }
    env.extend(runner.env.clone());
    env.insert("WINEDLLOVERRIDES".into(), dll_overrides(entry));
    env.insert("WINEPREFIX".into(), s(&i.prefix));

    let wine: Vec<String> = runner.wine_argv.iter().map(|p| s(p)).collect();
    let with = |extra: &[&str]| -> Vec<String> {
        wine.iter()
            .cloned()
            .chain(extra.iter().map(|x| x.to_string()))
            .collect()
    };

    if !i.prefix_exists {
        let mut description = t!("Create the Windows environment (first launch only)").to_string();
        if let Some(t) = &i.template {
            plan.steps.push(Step {
                kind: StepKind::CopyTemplate,
                description: t!("Copy the prebuilt Windows environment (first launch only)").into(),
                argv: vec![
                    "cp".into(),
                    "-a".into(),
                    "--reflink=auto".into(),
                    format!("{}/.", s(t)),
                    format!("{}/", s(&i.prefix)),
                ],
                env: BTreeMap::new(),
            });
            description = t!("Create the Windows environment (only if copying it failed)").into();
        }
        plan.steps.push(Step {
            kind: StepKind::CreatePrefix,
            description,
            argv: with(&["wineboot", "--init"]),
            env: env.clone(),
        });
    }

    let pending: Vec<String> = entry
        .map(|e| {
            e.verbs
                .iter()
                .filter(|v| !i.verbs_applied.contains(v))
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    if !pending.is_empty() {
        match (&i.winetricks, runner.wine_argv.as_slice()) {
            (Some(wt), [wine_bin]) => {
                let mut wenv = env.clone();
                wenv.insert("WINE".into(), s(wine_bin));
                if let Some(ws) = runner.wineserver_argv.first() {
                    wenv.insert("WINESERVER".into(), s(ws));
                }
                let mut argv = vec![s(wt), "-q".into()];
                argv.extend(pending.iter().cloned());
                plan.steps.push(Step {
                    kind: StepKind::Winetricks,
                    description: t!("Install components: {}", pending.join(", ")),
                    argv,
                    env: wenv,
                });
            }
            (None, _) => plan.notes.push(t!(
                "winetricks is not installed; skipping recommended components: {}.",
                pending.join(", ")
            )),
            (Some(_), _) => plan.notes.push(t!(
                "Recommended components ({}) are not applied automatically on the {} runner.",
                pending.join(", "),
                runner.kind
            )),
        }
    }

    let exe = s(&insp.path);
    let mut argv = if insp.arch == ExeArch::MsiPackage {
        with(&["msiexec", "/i", &exe])
    } else {
        with(&[&exe])
    };
    if i.silent {
        match silent_args(insp, entry) {
            Ok(a) => argv.extend(a),
            Err(SilentError::NotInstaller) => plan
                .notes
                .push(t!("--silent ignored: this is not an installer.").into()),
            Err(SilentError::Unknown(kind)) => {
                plan.steps.clear();
                plan.unsupported = Some(Unsupported {
                    reason: t!(
                        "Lucent Bridge doesn't know how to run this installer ({}) silently.",
                        kind
                    ),
                    hints: vec![
                        t!("Run it without --silent and click through the installer.").into(),
                        t!("Or add silent_args for it to a compatibility entry.").into(),
                    ],
                });
                return plan;
            }
        }
    }
    argv.extend(i.args.iter().cloned());
    plan.steps.push(Step {
        kind: StepKind::Launch,
        description: t!("Start {}", insp.display_name),
        argv,
        env: env.clone(),
    });

    if plan.installer && i.integrate {
        let mut argv: Vec<String> = runner.wineserver_argv.iter().map(|p| s(p)).collect();
        argv.push("-w".into());
        plan.steps.push(Step {
            kind: StepKind::WaitForPrefix,
            description: t!("Wait for the installer to finish completely").into(),
            argv,
            env: env.clone(),
        });
        plan.steps.push(Step {
            kind: StepKind::Integrate,
            description: t!("Add the installed program to the application launcher").into(),
            argv: vec![],
            env: BTreeMap::new(),
        });
    }
    plan
}

enum SilentError {
    NotInstaller,
    Unknown(crate::installer::InstallerKind),
}

/// Switches for an unattended install: the compat entry's, else the
/// installer type's.
fn silent_args(insp: &Inspection, entry: Option<&Entry>) -> Result<Vec<String>, SilentError> {
    let Some(m) = &insp.installer else {
        return Err(SilentError::NotInstaller);
    };
    if let Some(e) = entry.filter(|e| !e.silent_args.is_empty()) {
        return Ok(e.silent_args.clone());
    }
    m.kind
        .silent_args()
        .map(|a| a.iter().map(|x| x.to_string()).collect())
        .ok_or(SilentError::Unknown(m.kind))
}

/// POSIX-shell quoting, for display.
pub fn shell_quote(a: &str) -> String {
    if !a.is_empty()
        && a.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_./=:,+@%".contains(&b))
    {
        a.to_string()
    } else {
        format!("'{}'", a.replace('\'', r"'\''"))
    }
}

pub fn shell_line(step: &Step) -> String {
    let mut parts = Vec::new();
    if !step.env.is_empty() {
        parts.push("env".to_string());
        for (k, v) in &step.env {
            parts.push(format!("{k}={}", shell_quote(v)));
        }
    }
    parts.extend(step.argv.iter().map(|a| shell_quote(a)));
    parts.join(" ")
}

impl Plan {
    pub fn can_run(&self) -> bool {
        self.unsupported.is_none()
    }

    pub fn render(&self) -> String {
        let labels = [
            t!("Program:"),
            t!("Type:"),
            t!("Host:"),
            t!("Runner:"),
            t!("Prefix:"),
            t!("Compat:"),
        ];
        let w = labels
            .iter()
            .map(|l| l.chars().count())
            .max()
            .unwrap_or(0)
            .max(10);
        let mut o = String::new();
        let mut kv = |k: &str, v: &str| o.push_str(&format!("  {k:<w$} {v}\n"));
        kv(
            labels[0],
            &format!("{} ({})", self.name, self.exe.display()),
        );
        kv(
            labels[1],
            &format!(
                "{}{}",
                self.exe_arch,
                if self.installer {
                    t!(", installer")
                } else {
                    ""
                }
            ),
        );
        kv(labels[2], &self.host.to_string());
        match self.runner {
            Some(r) => kv(labels[3], &format!("{r} — {}", r.describe())),
            None => kv(labels[3], t!("none")),
        }
        kv(
            labels[4],
            &format!(
                "{}{}",
                self.prefix.display(),
                if self.prefix_exists {
                    ""
                } else {
                    t!(" (will be created)")
                }
            ),
        );
        if let Some(c) = &self.compat {
            kv(
                labels[5],
                &t!(
                    "{} (matched by {}, status: {}, from {})",
                    c.name,
                    match c.matched_by {
                        MatchKind::Sha256 => t!("SHA-256"),
                        MatchKind::Product => t!("product name"),
                        MatchKind::ExeName => t!("file name"),
                    },
                    match c.status {
                        Status::Works => t!("works"),
                        Status::Partial => t!("partial"),
                        Status::Broken => t!("broken"),
                        Status::Unknown => t!("unknown"),
                    },
                    c.source
                ),
            );
        }
        let mut out = t!("Lucent Bridge plan for {} [{}]", self.name, self.app_id);
        out.push('\n');
        out.push_str(&o);
        if let Some(u) = &self.unsupported {
            out.push_str(&format!("\n{}\n", t!("Cannot run: {}", u.reason)));
            for h in &u.hints {
                out.push_str(&format!("  - {h}\n"));
            }
        }
        if let Some(b) = &self.blocked {
            out.push_str(&format!(
                "\n{}\n  {}\n",
                t!("Blocked by the compatibility database: {}", b),
                t!("(use `run --force` to try anyway)")
            ));
        }
        if !self.steps.is_empty() {
            out.push_str(&format!("\n{}\n", t!("Steps:")));
            for (n, st) in self.steps.iter().enumerate() {
                out.push_str(&format!("  {}. {}\n", n + 1, st.description));
                if !st.argv.is_empty() {
                    out.push_str(&format!("     $ {}\n", shell_line(st)));
                }
            }
        }
        if !self.notes.is_empty() {
            out.push_str(&format!("\n{}\n", t!("Notes:")));
            for n in &self.notes {
                out.push_str(&format!("  - {n}\n"));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compat::CompatDb;
    use crate::runner::select;
    use crate::testpe::{self, PeBuilder};
    use crate::tools::assumed;

    fn inspection(b: PeBuilder, name: &str) -> Inspection {
        Inspection::from_bytes(&b.build(), Path::new(name)).unwrap()
    }

    fn inputs<'a>(
        insp: &'a Inspection,
        host: HostArch,
        compat: Option<(&'a Entry, MatchKind)>,
        args: &'a [String],
    ) -> Inputs<'a> {
        Inputs {
            inspection: insp,
            host,
            selection: select(
                host,
                insp.arch,
                &assumed(host),
                compat.and_then(|c| c.0.runner),
            ),
            compat,
            app_id: insp.app_id.clone(),
            prefix: PathBuf::from("/home/u/.local/share/lucent-bridge/prefixes").join(&insp.app_id),
            prefix_exists: false,
            verbs_applied: &[],
            winetricks: Some("/usr/bin/winetricks".into()),
            args,
            integrate: true,
            silent: false,
            template: None,
            winedebug: None,
        }
    }

    fn nsis(name: &str, product: &str) -> Inspection {
        let mut ov = vec![0u8; 4];
        ov.extend_from_slice(&[0xef, 0xbe, 0xad, 0xde]);
        ov.extend_from_slice(b"NullsoftInst");
        inspection(
            PeBuilder::new(testpe::MACHINE_I386)
                .version([1, 0, 0, 0], &[("ProductName", product)])
                .overlay(&ov),
            name,
        )
    }

    fn launch_argv(plan: &Plan) -> Vec<String> {
        plan.steps
            .iter()
            .find(|s| s.kind == StepKind::Launch)
            .unwrap()
            .argv
            .clone()
    }

    #[test]
    fn users_winedebug_wins_over_the_quiet_default() {
        let insp = inspection(PeBuilder::new(testpe::MACHINE_AMD64), "/x/tool.exe");
        let mut inp = inputs(&insp, HostArch::X86_64, None, &[]);
        inp.winedebug = Some("+seh,+loaddll".into());
        let plan = build(inp);
        assert!(
            plan.steps
                .iter()
                .all(|s| s.env["WINEDEBUG"] == "+seh,+loaddll")
        );
        let mut inp = inputs(&insp, HostArch::X86_64, None, &[]);
        inp.winedebug = Some(String::new());
        assert_eq!(build(inp).steps[0].env["WINEDEBUG"], "-all");
    }

    #[test]
    fn new_prefix_copies_the_template_with_wineboot_as_fallback() {
        let insp = inspection(PeBuilder::new(testpe::MACHINE_AMD64), "/x/tool.exe");
        let mut inp = inputs(&insp, HostArch::Aarch64, None, &[]);
        inp.template = Some("/usr/share/lucent-bridge/prefix-template".into());
        let plan = build(inp);
        let kinds: Vec<StepKind> = plan.steps.iter().map(|s| s.kind).collect();
        assert_eq!(
            kinds,
            [
                StepKind::CopyTemplate,
                StepKind::CreatePrefix,
                StepKind::Launch
            ]
        );
        assert_eq!(
            plan.steps[0].argv,
            [
                "cp",
                "-a",
                "--reflink=auto",
                "/usr/share/lucent-bridge/prefix-template/.",
                &format!("{}/", s(&plan.prefix))
            ]
        );
        assert!(plan.steps[1].description.contains("if"));

        let mut inp = inputs(&insp, HostArch::Aarch64, None, &[]);
        inp.template = Some("/t".into());
        inp.prefix_exists = true;
        let kinds: Vec<StepKind> = build(inp).steps.iter().map(|s| s.kind).collect();
        assert_eq!(kinds, [StepKind::Launch]);
    }

    #[test]
    fn silent_install_uses_installer_type_defaults() {
        let insp = nsis("/dl/foo-setup.exe", "Foo");
        let args = vec!["/D=C:\\Foo".to_string()];
        let mut inp = inputs(&insp, HostArch::X86_64, None, &args);
        inp.silent = true;
        let plan = build(inp);
        assert!(plan.can_run());
        assert_eq!(
            launch_argv(&plan),
            ["/usr/bin/wine", "/dl/foo-setup.exe", "/S", "/D=C:\\Foo"]
        );

        let inno = inspection(
            PeBuilder::new(testpe::MACHINE_I386).version(
                [1, 0, 0, 0],
                &[("Comments", "This installation was built with Inno Setup.")],
            ),
            "/dl/bar-setup.exe",
        );
        let mut inp = inputs(&inno, HostArch::X86_64, None, &[]);
        inp.silent = true;
        assert_eq!(
            launch_argv(&build(inp))[2..],
            ["/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART", "/SP-"]
        );

        let mut d = crate::installer::OLE_CFB_MAGIC.to_vec();
        d.resize(512, 0);
        let msi = Inspection::from_bytes(&d, Path::new("/dl/tool.msi")).unwrap();
        let mut inp = inputs(&msi, HostArch::X86_64, None, &[]);
        inp.silent = true;
        assert_eq!(
            launch_argv(&build(inp)),
            ["/usr/bin/wine", "msiexec", "/i", "/dl/tool.msi", "/qn"]
        );
    }

    #[test]
    fn silent_install_prefers_compat_args() {
        let mut db = CompatDb::default();
        db.parse(
            "t",
            "[[app]]\nname=\"Foo\"\nmatch.product=[\"Foo\"]\nsilent_args=[\"/S\", \"/NOICONS=0\"]\n",
        )
        .unwrap();
        // Detected only by name: no built-in default, the compat entry knows.
        let insp = inspection(
            PeBuilder::new(testpe::MACHINE_AMD64).version(
                [1, 0, 0, 0],
                &[("ProductName", "Foo"), ("FileDescription", "Foo Installer")],
            ),
            "/dl/foo-1.0.exe",
        );
        let mut inp = inputs(&insp, HostArch::X86_64, db.lookup(&insp), &[]);
        inp.silent = true;
        let plan = build(inp);
        assert_eq!(launch_argv(&plan)[2..], ["/S", "/NOICONS=0"]);
    }

    #[test]
    fn silent_install_without_known_flags_is_refused() {
        let insp = inspection(
            PeBuilder::new(testpe::MACHINE_AMD64)
                .version([1, 0, 0, 0], &[("FileDescription", "Foo Installer")]),
            "/dl/foo-1.0.exe",
        );
        let mut inp = inputs(&insp, HostArch::X86_64, None, &[]);
        inp.silent = true;
        let plan = build(inp);
        assert!(!plan.can_run());
        assert!(plan.unsupported.unwrap().reason.contains("silently"));
    }

    #[test]
    fn silent_is_ignored_for_programs() {
        let insp = inspection(PeBuilder::new(testpe::MACHINE_AMD64), "/x/tool.exe");
        let mut inp = inputs(&insp, HostArch::X86_64, None, &[]);
        inp.silent = true;
        let plan = build(inp);
        assert_eq!(launch_argv(&plan), ["/usr/bin/wine", "/x/tool.exe"]);
        assert!(plan.notes.iter().any(|n| n.contains("not an installer")));
    }

    #[test]
    fn x86_64_program_on_x86_host() {
        let insp = inspection(
            PeBuilder::new(testpe::MACHINE_AMD64).version([1, 0, 0, 0], &[("ProductName", "Foo")]),
            "/dl/foo.exe",
        );
        let args = vec!["--x".to_string(), "a b".to_string()];
        let plan = build(inputs(&insp, HostArch::X86_64, None, &args));
        assert!(plan.can_run());
        assert_eq!(plan.runner, Some(RunnerKind::Wine));
        let kinds: Vec<StepKind> = plan.steps.iter().map(|s| s.kind).collect();
        assert_eq!(kinds, [StepKind::CreatePrefix, StepKind::Launch]);
        let launch = &plan.steps[1];
        assert_eq!(launch.argv, ["/usr/bin/wine", "/dl/foo.exe", "--x", "a b"]);
        assert_eq!(launch.env["WINEPREFIX"], s(&plan.prefix));
        assert_eq!(launch.env["WINEDLLOVERRIDES"], "winemenubuilder.exe=d");
        assert_eq!(launch.env["WINEDEBUG"], "-all");
        assert!(!launch.env.contains_key("HODLL64"));
        let line = shell_line(launch);
        assert!(
            line.ends_with("/usr/bin/wine /dl/foo.exe --x 'a b'"),
            "{line}"
        );
    }

    #[test]
    fn installer_on_arm_host_with_compat() {
        let db = CompatDb::builtin();
        let insp = inspection(
            PeBuilder::new(testpe::MACHINE_I386).version(
                [6, 0, 0, 0],
                &[
                    ("ProductName", "IrfanView"),
                    ("Comments", "This installation was built with Inno Setup."),
                ],
            ),
            "/dl/iview_setup.exe",
        );
        let compat = db.lookup(&insp);
        assert!(compat.is_some());
        let plan = build(inputs(&insp, HostArch::Aarch64, compat, &[]));
        assert_eq!(plan.runner, Some(RunnerKind::WineFex));
        let kinds: Vec<StepKind> = plan.steps.iter().map(|s| s.kind).collect();
        assert_eq!(
            kinds,
            [
                StepKind::CreatePrefix,
                StepKind::Winetricks,
                StepKind::Launch,
                StepKind::WaitForPrefix,
                StepKind::Integrate
            ]
        );
        let launch = &plan.steps[2];
        assert_eq!(launch.env["HODLL"], "libwow64fex.dll");
        assert_eq!(launch.env["HODLL64"], "libarm64ecfex.dll");
        let wt = &plan.steps[1];
        assert_eq!(wt.argv, ["/usr/bin/winetricks", "-q", "corefonts"]);
        assert_eq!(wt.env["WINE"], "/usr/bin/wine");
        assert_eq!(plan.steps[3].argv, ["/usr/bin/wineserver", "-w"]);
        let text = plan.render();
        assert!(text.contains("wine-fex"), "{text}");
        assert!(text.contains("HODLL=libwow64fex.dll"), "{text}");
    }

    #[test]
    fn applied_verbs_and_existing_prefix_are_skipped() {
        let db = CompatDb::builtin();
        let insp = inspection(
            PeBuilder::new(testpe::MACHINE_AMD64)
                .version([1, 0, 0, 0], &[("ProductName", "IrfanView")]),
            "/p/i_view64.exe",
        );
        let applied = vec!["corefonts".to_string()];
        let mut inp = inputs(&insp, HostArch::X86_64, db.lookup(&insp), &[]);
        inp.prefix_exists = true;
        inp.verbs_applied = &applied;
        let plan = build(inp);
        let kinds: Vec<StepKind> = plan.steps.iter().map(|s| s.kind).collect();
        assert_eq!(kinds, [StepKind::Launch]);
    }

    #[test]
    fn compat_env_and_overrides_merge_but_cannot_move_the_prefix() {
        let mut db = CompatDb::default();
        db.parse(
            "t",
            r#"[[app]]
name = "T"
match.exe = ["t.exe"]
env = { WINEPREFIX = "/tmp/evil", DXVK_HUD = "fps" }
dll_overrides = { d3d11 = "native,builtin", winemenubuilder = "disabled" }
"#,
        )
        .unwrap();
        let insp = inspection(PeBuilder::new(testpe::MACHINE_AMD64), "/x/t.exe");
        let plan = build(inputs(&insp, HostArch::X86_64, db.lookup(&insp), &[]));
        let env = &plan.steps.last().unwrap().env;
        assert_eq!(env["DXVK_HUD"], "fps");
        assert_eq!(env["WINEPREFIX"], s(&plan.prefix));
        assert_eq!(
            env["WINEDLLOVERRIDES"],
            "d3d11=n,b;winemenubuilder=;winemenubuilder.exe=d"
        );
    }

    #[test]
    fn msi_goes_through_msiexec() {
        let mut d = crate::installer::OLE_CFB_MAGIC.to_vec();
        d.resize(512, 0);
        let insp = Inspection::from_bytes(&d, Path::new("/dl/tool.msi")).unwrap();
        let plan = build(inputs(&insp, HostArch::X86_64, None, &[]));
        let launch = plan
            .steps
            .iter()
            .find(|s| s.kind == StepKind::Launch)
            .unwrap();
        assert_eq!(
            launch.argv,
            ["/usr/bin/wine", "msiexec", "/i", "/dl/tool.msi"]
        );
        assert!(plan.steps.iter().any(|s| s.kind == StepKind::Integrate));
    }

    #[test]
    fn unsupported_and_blocked() {
        let insp = inspection(PeBuilder::new(testpe::MACHINE_ARM64), "/x/armapp.exe");
        let plan = build(inputs(&insp, HostArch::X86_64, None, &[]));
        assert!(!plan.can_run());
        assert!(plan.steps.is_empty());
        assert!(
            plan.render()
                .contains("Cannot run: This is a Windows-on-ARM program")
        );

        let db = CompatDb::builtin();
        let insp = inspection(PeBuilder::new(testpe::MACHINE_AMD64), "/g/VALORANT.exe");
        let plan = build(inputs(&insp, HostArch::X86_64, db.lookup(&insp), &[]));
        assert!(plan.can_run());
        assert!(plan.blocked.as_deref().unwrap().contains("kernel"));
        assert!(plan.render().contains("--force"));
    }

    #[test]
    fn dlls_are_not_runnable() {
        let mut b = PeBuilder::new(testpe::MACHINE_AMD64);
        b.dll = true;
        let insp = inspection(b, "/x/lib.dll");
        let plan = build(inputs(&insp, HostArch::X86_64, None, &[]));
        assert!(plan.unsupported.unwrap().reason.contains("DLL"));
    }

    #[test]
    fn fex_interpreter_skips_winetricks_with_note() {
        let mut db = CompatDb::default();
        db.parse(
            "t",
            "[[app]]\nname=\"T\"\nmatch.exe=[\"t.exe\"]\nverbs=[\"vcrun2019\"]\nrunner=\"fex-interpreter\"\n",
        )
        .unwrap();
        let insp = inspection(PeBuilder::new(testpe::MACHINE_AMD64), "/x/t.exe");
        let plan = build(inputs(&insp, HostArch::Aarch64, db.lookup(&insp), &[]));
        assert_eq!(plan.runner, Some(RunnerKind::FexInterpreter));
        assert!(plan.steps.iter().all(|s| s.kind != StepKind::Winetricks));
        assert!(plan.notes.iter().any(|n| n.contains("vcrun2019")));
        let launch = plan
            .steps
            .iter()
            .find(|s| s.kind == StepKind::Launch)
            .unwrap();
        assert_eq!(
            launch.argv[..2],
            ["/usr/bin/FEXInterpreter", "/usr/bin/wine"]
        );
    }

    #[test]
    fn quoting() {
        assert_eq!(shell_quote("abc"), "abc");
        assert_eq!(shell_quote(""), "''");
        assert_eq!(shell_quote("a b"), "'a b'");
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
        assert_eq!(shell_quote("n,b;x=d"), "'n,b;x=d'");
    }
}
