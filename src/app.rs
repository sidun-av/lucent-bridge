//! Glue between the pure pieces and the real system: build a plan for a
//! file, and execute it.

use crate::arch::HostArch;
use crate::compat::{self, CompatDb};
use crate::inspect::Inspection;
use crate::integrate;
use crate::notify::Notifier;
use crate::plan::{self, Plan, StepKind};
use crate::prefix::{self, Dirs, Metadata};
use crate::progress::{self, Decision, ErrorKind, Event, Phase, Reporter};
use crate::runner::{self, RunnerKind, Toolset};
use crate::{t, template, tools};
use anyhow::{Context as _, Result, bail};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

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
        Ok(Self::with_dirs(Dirs::from_env(|k| std::env::var(k).ok())?))
    }

    /// For `build-template`, which a system service runs with no HOME:
    /// nothing of a user's is needed (the template gets a temporary home of
    /// its own), so a missing HOME only means no user compat.d is read.
    pub fn system_wide() -> Context {
        let dirs = Dirs::from_env(|k| std::env::var(k).ok()).unwrap_or_else(|_| Dirs {
            data_home: PathBuf::from("/nonexistent"),
            config_home: PathBuf::from("/nonexistent"),
        });
        Self::with_dirs(dirs)
    }

    fn with_dirs(dirs: Dirs) -> Context {
        let (compat, compat_errors) = CompatDb::load(&compat::default_dirs(&dirs.config_home));
        let env = |k: &str| std::env::var(k).ok();
        let probe = tools::Probe {
            env: &env,
            root: PathBuf::from("/"),
            page_size: tools::page_size(),
        };
        Context {
            winetricks: probe.which("winetricks"),
            tools: probe.detect(),
            host: HostArch::current(),
            dirs,
            compat,
            compat_errors,
        }
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
            std::fs::canonicalize(exe).with_context(|| t!("cannot open {}", exe.display()))?;
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
        let native_wine = selection
            .runner()
            .is_some_and(|r| r.kind != RunnerKind::FexInterpreter);
        let dxvk = native_wine && crate::dxvk::bundle(&env, self.host).is_some();
        let z_is_root = crate::elevation::z_drive_is_root(&prefix);
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
            winedebug: env("WINEDEBUG"),
            start_helper: crate::elevation::helper(&env),
            z_is_root,
            dxvk,
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

enum Polled {
    Done(std::process::ExitStatus),
    Cancelled,
}

/// Run a preparation step, reporting eased progress within `span` and
/// stopping it if the user cancels.
fn run_polled(
    step: &plan::Step,
    quiet: bool,
    reporter: &mut Reporter,
    span: (f64, f64),
    expected: Duration,
) -> Result<Polled> {
    let (prog, args) = step.argv.split_first().context("empty command")?;
    let mut cmd = Command::new(prog);
    cmd.args(args).envs(&step.env);
    if quiet {
        cmd.stdout(std::process::Stdio::null());
    }
    let mut child = cmd
        .spawn()
        .with_context(|| format!("could not start {prog}"))?;
    let t0 = Instant::now();
    loop {
        if let Some(st) = child.try_wait()? {
            reporter.send(&Event::Progress { fraction: span.1 });
            return Ok(Polled::Done(st));
        }
        if reporter.cancel_requested() {
            terminate(&mut child);
            return Ok(Polled::Cancelled);
        }
        let f = span.0 + (span.1 - span.0) * progress::eased(t0.elapsed(), expected);
        reporter.send(&Event::Progress { fraction: f });
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// SIGTERM, then SIGKILL if it doesn't go within 3 s.
fn terminate(child: &mut std::process::Child) {
    #[cfg(unix)]
    // SAFETY: signalling our own child.
    unsafe {
        libc::kill(child.id() as libc::pid_t, libc::SIGTERM);
    }
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(3) {
        if let Ok(Some(_)) = child.try_wait() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// Make a freshly copied template this user's.
fn adopt_template(prefix: &Path) -> Result<()> {
    let dirs = Dirs::from_env(|k| std::env::var(k).ok())?;
    let env = |k: &str| std::env::var(k).ok();
    let home = env("HOME").map(PathBuf::from).context("HOME is not set")?;
    template::adopt(
        prefix,
        &template::wine_user_name(&env),
        &home,
        &dirs.config_home,
    )?;
    // Best effort: a wrong label doesn't stop the program from running.
    let probe = tools::Probe {
        env: &env,
        root: PathBuf::from("/"),
        page_size: tools::page_size(),
    };
    let restorecon = probe.which("restorecon");
    if let Some(argv) = template::relabel_argv(
        Path::new(template::SELINUX_FS),
        restorecon.as_deref(),
        prefix,
    ) {
        let _ = Command::new(&argv[0])
            .args(&argv[1..])
            .stdout(std::process::Stdio::null())
            .status();
    }
    Ok(())
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
    env.insert("WINEDLLOVERRIDES".into(), plan::dll_overrides(None, false));
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
    crate::sysdlls::provide(&tmp, ctx.host);
    // The bundled DXVK, so every prefix copied from the template has it
    // (as reflinks on btrfs) without copying it again.
    let env_fn = |k: &str| std::env::var(k).ok();
    if let Some(b) = crate::dxvk::bundle(&env_fn, ctx.host) {
        crate::dxvk::provide(&tmp, ctx.host, &b).context("copying DXVK into the template")?;
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

/// Explain an error: in the launch sheet when there is one (and wait for
/// the user's answer there), else in a dialog box or notification.
pub fn report_error(
    reporter: &mut Reporter,
    notifier: &Notifier,
    kind: ErrorKind,
    title: &str,
    message: &str,
    hints: &[String],
    can_run_anyway: bool,
) -> Decision {
    if reporter.has_sheet() {
        reporter.show();
    }
    let event = Event::Error {
        kind,
        title: title.to_string(),
        message: message.to_string(),
        hints: hints.to_vec(),
        can_run_anyway,
    };
    if reporter.has_sheet() {
        reporter.send(&event);
        // The sheet stays until the user closes it; so do we, so nothing
        // tears it down (and to hear "Run anyway").
        return reporter.decision(Duration::from_secs(900));
    }
    reporter.send(&event);
    let mut msg = message.to_string();
    for h in hints {
        msg.push_str("\n• ");
        msg.push_str(h);
    }
    notifier.error(title, &msg);
    Decision::Close
}

/// Stop everything in a prefix, and remove it if this run created it.
fn abandon(plan: &Plan, ctx: &Context, created: bool) {
    if let Some((ws, rest)) = plan.wineserver.split_first() {
        let _ = Command::new(ws)
            .args(rest)
            .arg("-k")
            .env("WINEPREFIX", &plan.prefix)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
    if created {
        let _ = std::fs::remove_dir_all(&plan.prefix);
        let _ = std::fs::remove_dir_all(ctx.dirs.icons(&plan.app_id));
    }
}

/// Wait (bounded) for the prefix's wineserver to exit by itself: the one a
/// preparation step started lingers a few seconds after it.
fn wait_for_server(plan: &Plan) {
    let Some((ws, rest)) = plan.wineserver.split_first() else {
        return;
    };
    let child = Command::new(ws)
        .args(rest)
        .arg("-w")
        .env("WINEPREFIX", &plan.prefix)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    if let Ok(mut c) = child {
        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_secs(10) {
            if let Ok(Some(_)) = c.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = c.kill();
        let _ = c.wait();
    }
}

/// Last touches before the program starts: system DLLs Wine can't find,
/// the bundled DXVK, and the desktop's look (all best effort).
fn prepare_launch(plan: &Plan, server_started: bool) {
    let env = |k: &str| std::env::var(k).ok();
    if plan.runner != Some(RunnerKind::FexInterpreter) {
        crate::sysdlls::provide(&plan.prefix, plan.host);
        if let Some(b) = crate::dxvk::bundle(&env, plan.host)
            && let Err(e) = crate::dxvk::provide(&plan.prefix, plan.host, &b)
        {
            eprintln!(
                "lucent-bridge: {}",
                t!("couldn't copy DXVK into the Windows environment: {}", e)
            );
        }
    }
    let Some(look) = crate::appearance::detect(&env, &crate::appearance::read_desktop(&env)) else {
        return;
    };
    let look = look.for_program(plan.compat.as_ref().is_some_and(|c| c.light_colors));
    if server_started && crate::appearance::server_running(&plan.prefix) {
        wait_for_server(plan);
    }
    let mode = match (look.mode, look.colors) {
        (crate::appearance::Mode::Light, _) => t!("light"),
        (crate::appearance::Mode::Dark, crate::appearance::Mode::Dark) => t!("dark"),
        (crate::appearance::Mode::Dark, crate::appearance::Mode::Light) => {
            t!("dark (this program keeps light colours)")
        }
    };
    let font = look.font.clone().unwrap_or_else(|| "Tahoma".into());
    match crate::appearance::apply(&plan.prefix, &look, prefix::now_secs()) {
        Ok(crate::appearance::Outcome::Busy) => eprintln!(
            "lucent-bridge: {}",
            t!("appearance unchanged: another program of this app is running")
        ),
        Ok(crate::appearance::Outcome::NoPrefix) => {}
        Ok(_) => eprintln!(
            "lucent-bridge: {}",
            t!(
                "appearance: {}, {} dpi ({}), {}",
                mode,
                look.dpi,
                look.dpi_from,
                font
            )
        ),
        Err(e) => eprintln!(
            "lucent-bridge: {}",
            t!("couldn't apply the appearance: {}", e)
        ),
    }
}

/// Execute a plan. Returns the program's exit code.
pub fn execute(
    ctx: &Context,
    plan: &Plan,
    insp: &Inspection,
    notifier: &Notifier,
    reporter: &mut Reporter,
    force: bool,
) -> Result<i32> {
    let icon = insp.icon.as_ref().and_then(|i| {
        let dir = ctx.dirs.icons(&plan.app_id);
        std::fs::create_dir_all(&dir).ok()?;
        let p = dir.join("_source.png");
        std::fs::write(&p, &i.png).ok()?;
        Some(p)
    });
    reporter.send(&Event::Start {
        name: plan.name.clone(),
        file: insp.file_name.clone(),
        size: insp.size,
        icon: icon.as_ref().map(|p| p.to_string_lossy().into_owned()),
        installer: plan.installer,
        program_arch: progress::arch_label(plan.exe_arch).into(),
        host_arch: progress::host_label(plan.host).into(),
        translation: plan.runner.is_some_and(|r| r.translates()),
        first_launch: !plan.prefix_exists,
    });
    if let Some(u) = &plan.unsupported {
        let mut msg = u.reason.clone();
        for h in &u.hints {
            msg.push_str("\n• ");
            msg.push_str(h);
        }
        report_error(
            reporter,
            notifier,
            ErrorKind::Unsupported,
            &t!("Can't run {}", plan.name),
            &u.reason,
            &u.hints,
            false,
        );
        bail!("{msg}");
    }
    if let Some(b) = &plan.blocked
        && !force
    {
        report_error(
            reporter,
            notifier,
            ErrorKind::Blocked,
            &t!("{} won't work", plan.name),
            b,
            &[],
            false,
        );
        bail!("{b}\n{}", t!("(use --force to try anyway)"));
    }
    if !force
        && !plan.installer
        && let Some(disk) = crate::windisk::hibernated_disk(&plan.exe)
    {
        let message = t!(
            "{} is on the disk of a Windows that didn't shut down completely (Fast Startup or hibernation). Lucent keeps that disk read-only so nothing on it is damaged, so programs started from there can't save anything next to themselves.",
            plan.name
        );
        let hints = vec![
            t!("To use the disk fully, start Windows and shut it down while holding Shift.").into(),
            t!("Or install the program in Lucent: open its installer from your Downloads folder.")
                .into(),
        ];
        let d = report_error(
            reporter,
            notifier,
            ErrorKind::Hibernated,
            t!("Windows didn't shut down completely"),
            &message,
            &hints,
            true,
        );
        if d != Decision::RunAnyway {
            bail!("{message} ({})", disk.display());
        }
    }
    let created = !plan.prefix.exists();
    std::fs::create_dir_all(&plan.prefix)
        .with_context(|| format!("creating {}", plan.prefix.display()))?;
    let mut meta = ensure_metadata(plan, insp)?;

    // Spans of the progress bar: preparing the environment, then
    // components, then the launch fills it.
    let has = |k: StepKind| plan.steps.iter().any(|s| s.kind == k);
    let preparing = has(StepKind::CopyTemplate) || has(StepKind::CreatePrefix);
    let components = has(StepKind::Winetricks);
    let prep_end = if components { 0.5 } else { 0.95 };
    let comp_start = if preparing { 0.5 } else { 0.0 };
    if preparing || components {
        reporter.show();
    }
    let mut signals = reporter.active().then(progress::CancelOnSignal::install);

    let mut code = 0;
    let mut prefix_ready = false;
    let mut server_started = false;
    let mut added = 0;
    let mut fraction = 0.0;
    let last_integrate = plan
        .steps
        .iter()
        .rposition(|s| s.kind == StepKind::Integrate);
    macro_rules! cancelled {
        () => {{
            eprintln!("lucent-bridge: {}", t!("cancelled"));
            abandon(plan, ctx, created);
            reporter.send(&Event::Cancelled);
            reporter.finish();
            return Ok(130);
        }};
    }
    for (index, step) in plan.steps.iter().enumerate() {
        match step.kind {
            StepKind::CopyTemplate => {
                eprintln!("lucent-bridge: {}", step.description);
                reporter.send(&Event::Step {
                    step: Phase::Prepare,
                    detail: None,
                });
                let t0 = Instant::now();
                let span = (0.0, prep_end * 0.9);
                let copied =
                    match run_polled(step, true, reporter, span, Duration::from_millis(1500))? {
                        Polled::Cancelled => cancelled!(),
                        Polled::Done(st) if st.success() => adopt_template(&plan.prefix),
                        Polled::Done(st) => Err(anyhow::anyhow!("cp exited with {st}")),
                    };
                match copied {
                    Ok(()) => {
                        prefix_ready = true;
                        fraction = prep_end;
                        let secs = format!("{:.2}", t0.elapsed().as_secs_f64());
                        eprintln!(
                            "lucent-bridge: {}",
                            t!("Windows environment ready in {} s (from template)", secs)
                        );
                    }
                    Err(e) => {
                        let e = format!("{e:#}");
                        eprintln!(
                            "lucent-bridge: {}",
                            t!(
                                "the prefix template didn't work ({}); creating a new environment",
                                e
                            )
                        );
                        clear_prefix(&plan.prefix)?;
                    }
                }
            }
            StepKind::CreatePrefix if prefix_ready => {}
            StepKind::CreatePrefix => {
                if !reporter.has_sheet() {
                    notifier.info(
                        &t!("Setting up {}", plan.name),
                        t!(
                            "Preparing a Windows environment. This happens once and takes a minute."
                        ),
                        icon.as_deref(),
                    );
                }
                reporter.send(&Event::Step {
                    step: Phase::Prepare,
                    detail: None,
                });
                eprintln!("lucent-bridge: {}", step.description);
                let t0 = Instant::now();
                let expected = if plan.runner.is_some_and(|r| r.translates()) {
                    Duration::from_secs(9)
                } else {
                    Duration::from_secs(10)
                };
                let st = match run_polled(step, true, reporter, (fraction, prep_end), expected)? {
                    Polled::Cancelled => cancelled!(),
                    Polled::Done(st) => st,
                };
                server_started = true;
                fraction = prep_end;
                if !st.success() {
                    eprintln!(
                        "lucent-bridge: {}",
                        t!("wineboot exited with {}; continuing", st)
                    );
                }
                let secs = format!("{:.2}", t0.elapsed().as_secs_f64());
                eprintln!(
                    "lucent-bridge: {}",
                    t!("Windows environment ready in {} s (wineboot)", secs)
                );
            }
            StepKind::Winetricks => {
                eprintln!("lucent-bridge: {}", step.description);
                if !reporter.has_sheet() {
                    notifier.info(&plan.name, &step.description, icon.as_deref());
                }
                reporter.send(&Event::Step {
                    step: Phase::Components,
                    detail: Some(
                        step.argv
                            .iter()
                            .skip(2)
                            .cloned()
                            .collect::<Vec<_>>()
                            .join(", "),
                    ),
                });
                let span = (fraction.max(comp_start), 0.95);
                let st = match run_polled(step, false, reporter, span, Duration::from_secs(30))? {
                    Polled::Cancelled => cancelled!(),
                    Polled::Done(st) => st,
                };
                server_started = true;
                fraction = 0.95;
                if st.success() {
                    let verbs = step.argv.iter().skip(2).cloned();
                    meta.verbs_applied.extend(verbs);
                    meta.verbs_applied.sort();
                    meta.verbs_applied.dedup();
                    meta.save(&plan.prefix)?;
                } else {
                    eprintln!(
                        "lucent-bridge: {}",
                        t!("winetricks failed ({}); launching anyway", st)
                    );
                }
            }
            StepKind::Launch => {
                if reporter.cancel_requested() {
                    cancelled!();
                }
                // From here on, signals are the program's business again.
                signals.take();
                reporter.send(&Event::Step {
                    step: Phase::Launch,
                    detail: None,
                });
                prepare_launch(plan, server_started);
                let (prog, args) = step.argv.split_first().context("empty command")?;
                let mut child = Command::new(prog)
                    .args(args)
                    .envs(&step.env)
                    .spawn()
                    .with_context(|| format!("could not start {prog}"))?;
                reporter.send(&Event::Progress { fraction: 1.0 });
                reporter.send(&Event::Launched);
                let st = child.wait()?;
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
                // Integration runs when the installer exits and again when
                // the prefix is idle: tell the user once, as soon as the
                // program is in the launcher.
                let n = out.entries.len();
                if n > 0 && added == 0 {
                    eprintln!("lucent-bridge: {}", t!("launcher entries added: {}", n));
                    notifier.info(
                        &t!("{} is installed", plan.name),
                        t!("You can find it in the application launcher."),
                        icon.as_deref(),
                    );
                    reporter.send(&Event::Installed {
                        name: plan.name.clone(),
                        entries: n,
                    });
                }
                added = added.max(n);
                if added == 0 && Some(index) == last_integrate {
                    eprintln!(
                        "lucent-bridge: {}",
                        t!("the installer created no shortcuts")
                    );
                }
            }
        }
    }
    reporter.send(&Event::Exit { code });
    reporter.finish();
    Ok(code)
}
