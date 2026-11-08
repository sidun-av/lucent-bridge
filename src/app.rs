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
use crate::{template, tools};
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
        silent: bool,
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
        let env = |k: &str| std::env::var(k).ok();
        let template = selection
            .runner()
            .and_then(|r| r.wine_argv.first())
            .and_then(|w| template::find(&env, w));
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
            silent,
            template,
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

/// Copy the template into the (new) prefix and make it this user's.
fn copy_template(step: &plan::Step, prefix: &Path) -> Result<()> {
    let st = run_step(step, true)?;
    if !st.success() {
        bail!("cp exited with {st}");
    }
    let dirs = Dirs::from_env(|k| std::env::var(k).ok())?;
    let env = |k: &str| std::env::var(k).ok();
    let home = env("HOME").map(PathBuf::from).context("HOME is not set")?;
    template::adopt(
        prefix,
        &template::wine_user_name(&env),
        &home,
        &dirs.config_home,
    )
}

/// Empty a prefix after a failed template copy, keeping our metadata.
fn clear_prefix(prefix: &Path) -> Result<()> {
    for e in std::fs::read_dir(prefix)?.filter_map(|e| e.ok()) {
        if e.file_name() == prefix::METADATA_FILE {
            continue;
        }
        let p = e.path();
        let r = if p.is_dir() && !p.is_symlink() {
            std::fs::remove_dir_all(&p)
        } else {
            std::fs::remove_file(&p)
        };
        r.with_context(|| format!("removing {}", p.display()))?;
    }
    Ok(())
}

/// Build the prefix template the OS image ships (see [`template`]):
/// `wineboot --init` for the placeholder user, with no display and a
/// throw-away home, then an atomic move into `out`.
pub fn build_template(ctx: &Context, out: &Path) -> Result<template::Stamp> {
    use crate::arch::ExeArch;
    let sel = runner::select(ctx.host, ExeArch::X86_64, &ctx.tools, None);
    let r = match &sel {
        runner::Selection::Run(r) if r.kind != RunnerKind::FexInterpreter => r,
        runner::Selection::Run(_) => {
            bail!(
                "templates need a native Wine (Wine ARM64EC on AArch64); only the FEXInterpreter path is available"
            )
        }
        runner::Selection::Unsupported(u) => bail!("{}", u.reason),
    };
    let parent = out.parent().context("bad output path")?;
    std::fs::create_dir_all(parent)?;
    let name = out
        .file_name()
        .context("bad output path")?
        .to_string_lossy();
    let pid = std::process::id();
    let tmp = parent.join(format!(".{name}.new-{pid}"));
    let home = std::env::temp_dir().join(format!("lucent-bridge-template-home-{pid}"));
    for d in [&tmp, &home] {
        if d.exists() {
            std::fs::remove_dir_all(d)?;
        }
        std::fs::create_dir_all(d)?;
    }
    let mut env = r.env.clone();
    env.insert("WINEDEBUG".into(), "-all".into());
    env.insert("WINEDLLOVERRIDES".into(), plan::dll_overrides(None));
    env.insert("WINEPREFIX".into(), tmp.to_string_lossy().into_owned());
    env.insert("USER".into(), template::TEMPLATE_USER.into());
    env.insert("HOME".into(), home.to_string_lossy().into_owned());
    let wine = &r.wine_argv[0];
    let run = |prog: &Path, args: &[&str]| -> Result<std::process::Output> {
        Command::new(prog)
            .args(args)
            .envs(&env)
            .env_remove("DISPLAY")
            .env_remove("WAYLAND_DISPLAY")
            .env_remove("XDG_CONFIG_HOME")
            .stdin(std::process::Stdio::null())
            .output()
            .with_context(|| format!("could not start {}", prog.display()))
    };
    let init = run(wine, &["wineboot", "--init"])?;
    let _ = run(&r.wineserver_argv[0], &["-w"]);
    let _ = std::fs::remove_dir_all(&home);
    if !tmp.join("system.reg").is_file() {
        let _ = std::fs::remove_dir_all(&tmp);
        bail!(
            "wineboot did not create a prefix ({}):\n{}",
            init.status,
            String::from_utf8_lossy(&init.stderr)
        );
    }
    // Nothing user- or build-specific may stay behind.
    let temp = tmp
        .join("drive_c/users")
        .join(template::TEMPLATE_USER)
        .join("AppData/Local/Temp");
    if let Ok(rd) = std::fs::read_dir(&temp) {
        for e in rd.filter_map(|e| e.ok()) {
            let p = e.path();
            let _ = if p.is_dir() {
                std::fs::remove_dir_all(&p)
            } else {
                std::fs::remove_file(&p)
            };
        }
    }
    let version = run(wine, &["--version"])
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    let stamp = template::Stamp {
        user: template::TEMPLATE_USER.into(),
        wine: wine.clone(),
        wine_version: version,
        created: prefix::now_secs(),
        built_by: format!("lucent-bridge {}", env!("CARGO_PKG_VERSION")),
    };
    std::fs::write(
        tmp.join(template::STAMP_FILE),
        toml::to_string_pretty(&stamp)?,
    )?;
    let old = parent.join(format!(".{name}.old-{pid}"));
    if out.exists() {
        std::fs::rename(out, &old)?;
    }
    std::fs::rename(&tmp, out)?;
    if old.exists() {
        std::fs::remove_dir_all(&old)?;
    }
    Ok(stamp)
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
    let mut prefix_ready = false;
    for step in &plan.steps {
        match step.kind {
            StepKind::CopyTemplate => {
                eprintln!("lucent-bridge: {}", step.description);
                let t0 = std::time::Instant::now();
                match copy_template(step, &plan.prefix) {
                    Ok(()) => {
                        prefix_ready = true;
                        eprintln!(
                            "lucent-bridge: Windows environment ready in {:.2} s (from template)",
                            t0.elapsed().as_secs_f64()
                        );
                    }
                    Err(e) => {
                        eprintln!(
                            "lucent-bridge: the prefix template didn't work ({e:#}); creating a new environment"
                        );
                        clear_prefix(&plan.prefix)?;
                    }
                }
            }
            StepKind::CreatePrefix if prefix_ready => {}
            StepKind::CreatePrefix => {
                notifier.info(
                    &format!("Setting up {}", plan.name),
                    "Preparing a Windows environment. This happens once and takes a minute.",
                    icon.as_deref(),
                );
                eprintln!("lucent-bridge: {}", step.description);
                let t0 = std::time::Instant::now();
                let st = run_step(step, true)?;
                if !st.success() {
                    eprintln!("lucent-bridge: wineboot exited with {st}; continuing");
                }
                eprintln!(
                    "lucent-bridge: Windows environment ready in {:.2} s (wineboot)",
                    t0.elapsed().as_secs_f64()
                );
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
