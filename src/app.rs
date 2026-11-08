//! Glue between the pure pieces and the real system: build a plan for a
//! file, and execute it.

use crate::arch::HostArch;
use crate::compat::{self, CompatDb};
use crate::inspect::Inspection;
use crate::integrate;
use crate::notify::Notifier;
use crate::plan::{self, Plan, StepKind};
use crate::prefix::{self, Dirs, Metadata};
use crate::runner::{self, RunnerKind, Toolset};
use crate::tools;
use anyhow::{Context as _, Result, bail};
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct Context {
    pub dirs: Dirs,
    pub host: HostArch,
    pub tools: Toolset,
    pub compat: CompatDb,
    pub compat_errors: Vec<String>,
    pub winetricks: Option<PathBuf>,
}

impl Context {
    pub fn system() -> Result<Context> {
        let dirs = Dirs::from_env(|k| std::env::var(k).ok())?;
        let (compat, compat_errors) = CompatDb::load(&compat::default_dirs(&dirs.config_home));
        let env = |k: &str| std::env::var(k).ok();
        let probe = tools::Probe {
            env: &env,
            root: PathBuf::from("/"),
            page_size: tools::page_size(),
        };
        Ok(Context {
            winetricks: probe.which("winetricks"),
            tools: probe.detect(),
            host: HostArch::current(),
            dirs,
            compat,
            compat_errors,
        })
    }

    pub fn plan_for(
        &self,
        exe: &Path,
        args: &[String],
        prefer: Option<RunnerKind>,
        integrate: bool,
    ) -> Result<(Inspection, Plan)> {
        let exe =
            std::fs::canonicalize(exe).with_context(|| format!("cannot open {}", exe.display()))?;
        let insp = Inspection::from_path(&exe)?;
        let compat = self.compat.lookup(&insp);
        // A program installed into one of our prefixes runs in that prefix.
        let app_id = self
            .dirs
            .owning_app_id(&exe)
            .unwrap_or_else(|| insp.app_id.clone());
        let prefix = self.dirs.prefix(&app_id);
        let meta = Metadata::load(&prefix).ok().flatten();
        let prefix_exists = prefix.join("system.reg").exists();
        let prefer = prefer.or_else(|| compat.and_then(|(e, _)| e.runner));
        let selection = runner::select(self.host, insp.arch, &self.tools, prefer);
        let verbs = meta.map(|m| m.verbs_applied).unwrap_or_default();
        let plan = plan::build(plan::Inputs {
            inspection: &insp,
            host: self.host,
            selection,
            compat,
            app_id,
            prefix,
            prefix_exists,
            verbs_applied: &verbs,
            winetricks: self.winetricks.clone(),
            args,
            integrate,
        });
        Ok((insp, plan))
    }
}

/// How the binary should be referenced from launcher entries: by name when
/// it is installed in a standard location (survives updates), else by
/// absolute path (development builds).
pub fn launcher_name() -> String {
    match std::env::current_exe() {
        Ok(p) if p.starts_with("/usr/") => "lucent-bridge".into(),
        Ok(p) => p.to_string_lossy().into_owned(),
        Err(_) => "lucent-bridge".into(),
    }
}

fn run_step(step: &plan::Step, quiet: bool) -> Result<std::process::ExitStatus> {
    let (prog, args) = step.argv.split_first().context("empty command")?;
    let mut cmd = Command::new(prog);
    cmd.args(args).envs(&step.env);
    if quiet {
        cmd.stdout(std::process::Stdio::null());
    }
    cmd.status()
        .with_context(|| format!("could not start {prog}"))
}

fn ensure_metadata(plan: &Plan, insp: &Inspection) -> Result<Metadata> {
    if let Some(m) = Metadata::load(&plan.prefix)? {
        return Ok(m);
    }
    let m = Metadata {
        app_id: plan.app_id.clone(),
        name: plan.name.clone(),
        source: insp.path.to_string_lossy().into_owned(),
        runner: plan.runner.map(|r| r.to_string()).unwrap_or_default(),
        created: prefix::now_secs(),
        ..Default::default()
    };
    m.save(&plan.prefix)?;
    Ok(m)
}

/// Execute a plan. Returns the program's exit code.
pub fn execute(
    ctx: &Context,
    plan: &Plan,
    insp: &Inspection,
    notifier: &Notifier,
    force: bool,
) -> Result<i32> {
    if let Some(u) = &plan.unsupported {
        let mut msg = u.reason.clone();
        for h in &u.hints {
            msg.push_str("\n• ");
            msg.push_str(h);
        }
        notifier.error(&format!("Can't run {}", plan.name), &msg);
        bail!("{msg}");
    }
    if let Some(b) = &plan.blocked
        && !force
    {
        notifier.error(&format!("{} won't work", plan.name), b);
        bail!("{b}\n(use --force to try anyway)");
    }
    std::fs::create_dir_all(&plan.prefix)
        .with_context(|| format!("creating {}", plan.prefix.display()))?;
    let mut meta = ensure_metadata(plan, insp)?;
    let icon = insp.icon.as_ref().and_then(|i| {
        let dir = ctx.dirs.icons(&plan.app_id);
        std::fs::create_dir_all(&dir).ok()?;
        let p = dir.join("_source.png");
        std::fs::write(&p, &i.png).ok()?;
        Some(p)
    });
    let mut code = 0;
    for step in &plan.steps {
        match step.kind {
            StepKind::CreatePrefix => {
                notifier.info(
                    &format!("Setting up {}", plan.name),
                    "Preparing a Windows environment. This happens once and takes a minute.",
                    icon.as_deref(),
                );
                eprintln!("lucent-bridge: {}", step.description);
                let st = run_step(step, true)?;
                if !st.success() {
                    eprintln!("lucent-bridge: wineboot exited with {st}; continuing");
                }
            }
            StepKind::Winetricks => {
                eprintln!("lucent-bridge: {}", step.description);
                notifier.info(&plan.name, &step.description, icon.as_deref());
                let st = run_step(step, false)?;
                if st.success() {
                    let verbs = step.argv.iter().skip(2).cloned();
                    meta.verbs_applied.extend(verbs);
                    meta.verbs_applied.sort();
                    meta.verbs_applied.dedup();
                    meta.save(&plan.prefix)?;
                } else {
                    eprintln!("lucent-bridge: winetricks failed ({st}); launching anyway");
                }
            }
            StepKind::Launch => {
                let st = run_step(step, false)?;
                code = st.code().unwrap_or(1);
            }
            StepKind::WaitForPrefix => {
                let _ = run_step(step, true);
            }
            StepKind::Integrate => {
                let out = integrate::integrate(&ctx.dirs, &plan.app_id, &launcher_name())?;
                for p in &out.entries {
                    if !meta.desktop_entries.contains(p) {
                        meta.desktop_entries.push(p.clone());
                    }
                }
                for p in &out.icons {
                    if !meta.icons.contains(p) {
                        meta.icons.push(p.clone());
                    }
                }
                meta.save(&plan.prefix)?;
                match out.entries.len() {
                    0 => eprintln!("lucent-bridge: the installer created no shortcuts"),
                    n => {
                        eprintln!(
                            "lucent-bridge: added {n} launcher entr{}",
                            if n == 1 { "y" } else { "ies" }
                        );
                        notifier.info(
                            &format!("{} is installed", plan.name),
                            "You can find it in the application launcher.",
                            icon.as_deref(),
                        );
                    }
                }
            }
        }
    }
    Ok(code)
}
