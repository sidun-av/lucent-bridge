use anyhow::{Context as _, Result, bail};
use clap::{Parser, Subcommand};
use lucent_bridge::app::{self, Context};
use lucent_bridge::arch::HostArch;
use lucent_bridge::inspect::Inspection;
use lucent_bridge::notify::Notifier;
use lucent_bridge::prefix;
use lucent_bridge::runner::RunnerKind;
use lucent_bridge::t;
use lucent_bridge::{integrate, tools};
use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::process::ExitCode;

/// Run Windows programs on Lucent OS — directly on x86-64, with x86 → ARM
/// translation (FEX-Emu) on AArch64. No virtual machines.
#[derive(Parser)]
#[command(name = "lucent-bridge", version, about)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Show what a .exe/.msi is: architecture, subsystem, version info,
    /// installer type, icon.
    Inspect {
        exe: PathBuf,
        #[arg(long)]
        json: bool,
        /// Write the program's icon as PNG to this path.
        #[arg(long, value_name = "PNG")]
        extract_icon: Option<PathBuf>,
    },
    /// Dry run: print exactly what `run` would execute, without executing
    /// anything.
    Plan {
        exe: PathBuf,
        /// Pretend to be this host architecture (x86_64, aarch64).
        #[arg(long)]
        host: Option<String>,
        /// Assume a stock Lucent OS toolset instead of probing this machine.
        #[arg(long)]
        assume_tools: bool,
        /// Prefer a runner (wine-fex, fex-interpreter, ...).
        #[arg(long)]
        runner: Option<RunnerKind>,
        #[arg(long)]
        json: bool,
        /// Plan an unattended install (see `run --silent`).
        #[arg(long)]
        silent: bool,
        /// Arguments for the program.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Run a Windows program (installers get launcher entries afterwards).
    Run {
        exe: PathBuf,
        /// Run even if the compatibility database says it will not work.
        #[arg(long)]
        force: bool,
        /// Prefer a runner (wine-fex, fex-interpreter, ...).
        #[arg(long)]
        runner: Option<RunnerKind>,
        /// Don't create launcher entries after an installer finishes.
        #[arg(long)]
        no_integrate: bool,
        /// Install without dialogs, accepting the installer's defaults
        /// (switches from the compat DB or the installer type).
        #[arg(long)]
        silent: bool,
        /// Arguments for the program.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// List installed apps (one Wine prefix each).
    List {
        #[arg(long)]
        json: bool,
    },
    /// Delete an app: its prefix, launcher entries and icons.
    Remove {
        app_id: String,
        /// Don't ask for confirmation.
        #[arg(short, long)]
        yes: bool,
    },
    /// Re-scan an app's prefix for shortcuts and (re)create launcher entries.
    Integrate { app_id: String },
    /// Build the prefix template the OS image ships (run as root at image
    /// build time; default output /usr/share/lucent-bridge/prefix-template).
    BuildTemplate {
        #[arg(default_value = lucent_bridge::template::SYSTEM_DIR)]
        out: PathBuf,
    },
}

/// `binfmt_misc` invokes us as `lucent-bridge-binfmt <exe> [args...]`.
fn argv() -> Vec<String> {
    let mut args: Vec<String> = std::env::args().collect();
    let invoked_as = args
        .first()
        .and_then(|a| std::path::Path::new(a).file_name())
        .map(|n| n.to_string_lossy().into_owned());
    if invoked_as.as_deref() == Some("lucent-bridge-binfmt") && args.len() >= 2 {
        let rest = args.split_off(1);
        args = vec!["lucent-bridge".into(), "run".into(), "--".into()];
        args.extend(rest);
    }
    args
}

fn main() -> ExitCode {
    lucent_bridge::i18n::init_from_env();
    let cli = Cli::parse_from(argv());
    match real_main(cli) {
        Ok(code) => ExitCode::from(code.clamp(0, 255) as u8),
        Err(e) => {
            eprintln!("lucent-bridge: {e:#}");
            ExitCode::from(2)
        }
    }
}

fn notifier() -> Notifier {
    let env = |k: &str| std::env::var(k).ok();
    let probe = tools::Probe {
        env: &env,
        root: PathBuf::from("/"),
        page_size: 4096,
    };
    Notifier::detect(&env, &|n| probe.which(n))
}

fn real_main(cli: Cli) -> Result<i32> {
    match cli.cmd {
        Cmd::Inspect {
            exe,
            json,
            extract_icon,
        } => {
            let i = Inspection::from_path(&exe)?;
            if let Some(out) = extract_icon {
                let icon = i.icon.as_ref().context(t!("this file has no icon"))?;
                std::fs::write(&out, &icon.png)
                    .with_context(|| format!("writing {}", out.display()))?;
            }
            if json {
                println!("{}", serde_json::to_string_pretty(&i)?);
            } else {
                print!("{}", i.summary());
            }
            Ok(0)
        }
        Cmd::Plan {
            exe,
            host,
            assume_tools,
            runner,
            json,
            silent,
            args,
        } => {
            let mut ctx = Context::system()?;
            if let Some(h) = host {
                ctx.host = HostArch::from_name(&h);
                if ctx.host == HostArch::Other {
                    bail!(
                        "{}",
                        t!("unknown host architecture '{}' (use x86_64 or aarch64)", h)
                    );
                }
            }
            if assume_tools {
                ctx.tools = tools::assumed(ctx.host);
                ctx.winetricks = Some("/usr/bin/winetricks".into());
            }
            for e in &ctx.compat_errors {
                eprintln!("lucent-bridge: {}", t!("warning: {}", e));
            }
            let (_, plan) = ctx.plan_for(&exe, &args, runner, true, silent)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&plan)?);
            } else {
                print!("{}", plan.render());
                if !cfg!(target_os = "linux") && !assume_tools {
                    println!(
                        "\n{}",
                        t!(
                            "(This is not a Linux system; add --assume-tools [--host aarch64] to \
                         see the plan for a stock Lucent OS install.)"
                        )
                    );
                }
            }
            Ok(if plan.can_run() { 0 } else { 1 })
        }
        Cmd::Run {
            exe,
            force,
            runner,
            no_integrate,
            silent,
            args,
        } => {
            let notifier = notifier();
            if !cfg!(target_os = "linux") {
                bail!(
                    "{}",
                    t!("`run` only works on Linux; use `plan` for a dry run")
                );
            }
            let ctx = Context::system()?;
            for e in &ctx.compat_errors {
                eprintln!("lucent-bridge: {}", t!("warning: {}", e));
            }
            let (insp, plan) = match ctx.plan_for(&exe, &args, runner, !no_integrate, silent) {
                Ok(v) => v,
                Err(e) => {
                    notifier.error(t!("Can't open this file"), &format!("{e:#}"));
                    return Err(e);
                }
            };
            for n in &plan.notes {
                eprintln!("lucent-bridge: {}", t!("note: {}", n));
            }
            app::execute(&ctx, &plan, &insp, &notifier, force)
        }
        Cmd::List { json } => {
            let dirs = prefix::Dirs::from_env(|k| std::env::var(k).ok())?;
            let apps = prefix::list(&dirs);
            if json {
                let v: Vec<_> = apps.iter().map(|(_, m)| m).collect();
                println!("{}", serde_json::to_string_pretty(&v)?);
            } else if apps.is_empty() {
                println!(
                    "{}",
                    t!("No apps yet. Double-click a .exe or run `lucent-bridge run <file.exe>`.")
                );
            } else {
                println!(
                    "{:<32} {:<28} {:<16} {}",
                    t!("APP ID"),
                    t!("NAME"),
                    t!("RUNNER"),
                    t!("LAUNCHERS")
                );
                for (_, m) in &apps {
                    println!(
                        "{:<32} {:<28} {:<16} {}",
                        m.app_id,
                        m.name,
                        m.runner,
                        m.desktop_entries.len()
                    );
                }
            }
            Ok(0)
        }
        Cmd::Remove { app_id, yes } => {
            let dirs = prefix::Dirs::from_env(|k| std::env::var(k).ok())?;
            let r = prefix::plan_removal(&dirs, &app_id)?;
            println!("{}", t!("This will permanently delete:"));
            println!(
                "  {}",
                t!(
                    "{} (the app and all its files and settings)",
                    r.prefix.display()
                )
            );
            for f in &r.files {
                println!("  {}", f.display());
            }
            if r.icon_dir.exists() {
                println!("  {}", r.icon_dir.display());
            }
            if !yes {
                if !std::io::stdin().is_terminal() {
                    bail!(
                        "{}",
                        t!("refusing to delete without confirmation; pass --yes")
                    );
                }
                print!("{}", t!("Continue? [y/N] "));
                std::io::stdout().flush()?;
                let mut answer = String::new();
                std::io::stdin().read_line(&mut answer)?;
                if !matches!(
                    answer.trim().to_lowercase().as_str(),
                    "y" | "yes" | "д" | "да"
                ) {
                    println!("{}", t!("Nothing removed."));
                    return Ok(1);
                }
            }
            // Stop anything still running in the prefix first (best effort).
            let tools = tools::detect_system();
            if let Some(w) = &tools.wine {
                let ws = w
                    .wineserver
                    .clone()
                    .unwrap_or_else(|| w.wine.with_file_name("wineserver"));
                let _ = std::process::Command::new(ws)
                    .arg("-k")
                    .env("WINEPREFIX", &r.prefix)
                    .status();
            }
            prefix::execute_removal(&r)?;
            println!("{}", t!("Removed {}.", app_id));
            Ok(0)
        }
        Cmd::BuildTemplate { out } => {
            if !cfg!(target_os = "linux") {
                bail!("{}", t!("`build-template` only works on Linux"));
            }
            let ctx = Context::system_wide();
            let t0 = std::time::Instant::now();
            let stamp = app::build_template(&ctx, &out)?;
            println!(
                "{}",
                t!(
                    "Built prefix template for {} in {} ({} s)",
                    stamp.wine_version,
                    out.display(),
                    format!("{:.1}", t0.elapsed().as_secs_f64())
                )
            );
            Ok(0)
        }
        Cmd::Integrate { app_id } => {
            let dirs = prefix::Dirs::from_env(|k| std::env::var(k).ok())?;
            if !lucent_bridge::appid::is_valid_app_id(&app_id) || !dirs.prefix(&app_id).is_dir() {
                bail!(
                    "{}",
                    t!("no app with ID '{}' (see `lucent-bridge list`)", app_id)
                );
            }
            let out = integrate::integrate(&dirs, &app_id, &app::launcher_name())?;
            let p = dirs.prefix(&app_id);
            if let Some(mut m) = prefix::Metadata::load(&p)? {
                for e in &out.entries {
                    if !m.desktop_entries.contains(e) {
                        m.desktop_entries.push(e.clone());
                    }
                }
                for e in &out.icons {
                    if !m.icons.contains(e) {
                        m.icons.push(e.clone());
                    }
                }
                m.save(&p)?;
            }
            for e in &out.entries {
                println!("{}", e.display());
            }
            if out.entries.is_empty() {
                println!("{}", t!("No shortcuts found in this app's prefix."));
            }
            Ok(0)
        }
    }
}
