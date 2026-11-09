//! End-to-end tests of the `lucent-bridge` binary against synthetic PE
//! files. On Linux, `run` is exercised with a fake Wine (a shell script that
//! records what it was asked to do and behaves like an installer).

use lucent_bridge::testpe::{self, PeBuilder, ResKey};
use std::path::PathBuf;
use std::process::{Command, Output};

fn bin() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_lucent-bridge"));
    c.env_remove("DISPLAY").env_remove("WAYLAND_DISPLAY");
    // English unless a test asks for another language.
    for k in ["LANGUAGE", "LC_ALL", "LC_MESSAGES"] {
        c.env_remove(k);
    }
    c.env("LANG", "en_US.UTF-8");
    c
}

struct Env {
    tmp: tempfile::TempDir,
}

impl Env {
    fn new() -> Env {
        Env {
            tmp: tempfile::tempdir().unwrap(),
        }
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.tmp.path().join(rel)
    }

    fn cmd(&self) -> Command {
        let mut c = bin();
        c.env("HOME", self.path("home"))
            .env("XDG_DATA_HOME", self.path("home/.local/share"))
            .env("XDG_CONFIG_HOME", self.path("home/.config"));
        c
    }

    fn write(&self, rel: &str, data: &[u8]) -> PathBuf {
        let p = self.path(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, data).unwrap();
        p
    }
}

fn ok(o: &Output) -> String {
    assert!(
        o.status.success(),
        "status {:?}\nstdout:\n{}\nstderr:\n{}",
        o.status,
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8(o.stdout.clone()).unwrap()
}

fn app_exe(machine: u16) -> Vec<u8> {
    PeBuilder::new(machine)
        .version(
            [8, 6, 4, 0],
            &[
                ("ProductName", "Notepad++"),
                ("CompanyName", "Don Ho"),
                (
                    "FileDescription",
                    "Notepad++ : a free (GNU) source code editor",
                ),
            ],
        )
        .icon_group(ResKey::Id(1), 1, &[testpe::png_icon(48, [0, 128, 0, 255])])
        .build()
}

fn inno_installer() -> Vec<u8> {
    PeBuilder::new(testpe::MACHINE_I386)
        .version(
            [1, 0, 0, 0],
            &[
                ("ProductName", "Foo Editor"),
                ("CompanyName", "Foo Ltd"),
                ("FileDescription", "Foo Editor Setup"),
                ("Comments", "This installation was built with Inno Setup."),
            ],
        )
        .build()
}

#[test]
fn inspect_text_and_json_and_icon() {
    let e = Env::new();
    let exe = e.write("dl/notepad++.exe", &app_exe(testpe::MACHINE_AMD64));
    let out = ok(&e.cmd().arg("inspect").arg(&exe).output().unwrap());
    assert!(out.contains("Notepad++"), "{out}");
    assert!(out.contains("x86-64"), "{out}");
    assert!(out.contains("Icon:            48x48"), "{out}");

    let png = e.path("icon.png");
    let out = ok(&e
        .cmd()
        .args(["inspect", "--json", "--extract-icon"])
        .arg(&png)
        .arg(&exe)
        .output()
        .unwrap());
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["arch"], "x86_64");
    assert_eq!(v["subsystem"], "gui");
    assert_eq!(v["version"]["strings"]["CompanyName"], "Don Ho");
    assert!(
        v["app_id"]
            .as_str()
            .unwrap()
            .starts_with("notepadplusplus-")
    );
    assert!(std::fs::read(&png).unwrap().starts_with(b"\x89PNG"));
}

#[test]
fn inspect_rejects_non_windows_files() {
    let e = Env::new();
    let f = e.write("x.exe", b"#!/bin/sh\necho hi\n");
    let o = e.cmd().arg("inspect").arg(&f).output().unwrap();
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("no MZ header"));
}

#[test]
fn messages_follow_the_locale() {
    let e = Env::new();
    let exe = e.write("dl/npp.exe", &app_exe(testpe::MACHINE_ARM64));
    let plan = |lang: &str| {
        let o = e
            .cmd()
            .env("LANG", lang)
            .args(["plan", "--assume-tools", "--host", "x86_64"])
            .arg(&exe)
            .output()
            .unwrap();
        assert!(!o.status.success(), "ARM program on x86 can't run");
        String::from_utf8(o.stdout).unwrap()
    };
    let en = plan("en_US.UTF-8");
    assert!(
        en.contains("Cannot run: This is a Windows-on-ARM program"),
        "{en}"
    );
    let ru = plan("ru_RU.UTF-8");
    assert!(
        ru.contains("Запуск невозможен: Это программа для Windows на ARM"),
        "{ru}"
    );
    assert!(ru.contains("Скачайте версию программы для x64"), "{ru}");
    let o = e
        .cmd()
        .env("LANG", "ru_RU.UTF-8")
        .arg("list")
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&o.stdout).starts_with("Программ пока нет."));
}

#[test]
fn plan_dry_run_x86_64() {
    let e = Env::new();
    let exe = e.write("dl/npp.exe", &app_exe(testpe::MACHINE_AMD64));
    let out = ok(&e
        .cmd()
        .args(["plan", "--assume-tools", "--host", "x86_64"])
        .arg(&exe)
        .args(["--", "-multiInst", "file one.txt"])
        .output()
        .unwrap());
    assert!(
        out.contains("Runner:    wine — Wine (direct execution on x86-64)"),
        "{out}"
    );
    assert!(out.contains("Compat:    Notepad++"), "{out}");
    assert!(out.contains("wineboot --init"), "{out}");
    assert!(out.contains("-multiInst 'file one.txt'"), "{out}");
    assert!(!out.contains("HODLL"), "{out}");
    // Dry run: nothing was created.
    assert!(!e.path("home/.local/share/lucent-bridge").exists());
}

#[test]
fn plan_dry_run_aarch64_json() {
    let e = Env::new();
    let exe = e.write("dl/setup.exe", &inno_installer());
    let out = ok(&e
        .cmd()
        .args(["plan", "--assume-tools", "--host", "aarch64", "--json"])
        .arg(&exe)
        .output()
        .unwrap());
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["runner"], "wine-fex");
    assert_eq!(v["installer"], true);
    let steps = v["steps"].as_array().unwrap();
    let kinds: Vec<&str> = steps.iter().map(|s| s["kind"].as_str().unwrap()).collect();
    assert_eq!(
        kinds,
        [
            "create_prefix",
            "launch",
            "integrate",
            "wait_for_prefix",
            "integrate"
        ]
    );
    assert_eq!(steps[1]["env"]["HODLL"], "libwow64fex.dll");
    assert_eq!(steps[1]["env"]["HODLL64"], "libarm64ecfex.dll");
    let prefix = v["prefix"].as_str().unwrap();
    assert!(
        prefix.contains("/lucent-bridge/prefixes/foo-editor-"),
        "{prefix}"
    );

    // Fallback runner on request.
    let out = ok(&e
        .cmd()
        .args([
            "plan",
            "--assume-tools",
            "--host",
            "aarch64",
            "--runner",
            "fex-interpreter",
        ])
        .arg(&exe)
        .output()
        .unwrap());
    assert!(
        out.contains("/usr/bin/FEXInterpreter /usr/bin/wine"),
        "{out}"
    );
}

#[test]
fn file_manager_invocation_shape() {
    // The MIME handler runs `lucent-bridge run -- %f`; the same argument
    // shape must parse for `plan`, even for files that look like flags.
    let e = Env::new();
    let exe = e.write("dl/--weird name.exe", &app_exe(testpe::MACHINE_I386));
    let out = ok(&e
        .cmd()
        .args(["plan", "--assume-tools", "--host", "x86_64", "--"])
        .arg(&exe)
        .output()
        .unwrap());
    assert!(out.contains("--weird name.exe'"), "{out}");
}

#[test]
fn plan_explains_unsupported() {
    let e = Env::new();
    let exe = e.write("dl/arm.exe", &PeBuilder::new(testpe::MACHINE_ARM64).build());
    let o = e
        .cmd()
        .args(["plan", "--assume-tools", "--host", "x86_64"])
        .arg(&exe)
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(1));
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(
        out.contains("Cannot run: This is a Windows-on-ARM program"),
        "{out}"
    );
}

#[test]
fn list_and_remove_and_integrate() {
    let e = Env::new();
    let out = ok(&e.cmd().arg("list").output().unwrap());
    assert!(out.contains("No apps yet"));

    let id = "foo-editor-0badc0de";
    let prefix = e.path(&format!("home/.local/share/lucent-bridge/prefixes/{id}"));
    std::fs::create_dir_all(&prefix).unwrap();
    std::fs::write(
        prefix.join("lucent-bridge.toml"),
        format!("app_id = \"{id}\"\nname = \"Foo Editor\"\nsource = \"/dl/setup.exe\"\nrunner = \"wine\"\ncreated = 1\n"),
    )
    .unwrap();
    e.write(
        &format!("home/.local/share/lucent-bridge/prefixes/{id}/drive_c/Program Files/Foo/foo.exe"),
        &app_exe(testpe::MACHINE_AMD64),
    );
    e.write(
        &format!("home/.local/share/lucent-bridge/prefixes/{id}/drive_c/ProgramData/Microsoft/Windows/Start Menu/Programs/Foo Editor.lnk"),
        &testpe::lnk_file(r"C:\Program Files\Foo\foo.exe", None, None, None),
    );
    let out = ok(&e.cmd().args(["integrate", id]).output().unwrap());
    let desktop = PathBuf::from(out.trim());
    assert!(
        desktop.ends_with(format!("lucent-bridge-{id}-foo-editor.desktop")),
        "{out}"
    );
    let text = std::fs::read_to_string(&desktop).unwrap();
    assert!(text.contains("Name=Foo Editor"), "{text}");
    assert!(text.contains("Icon=/"), "{text}");
    // Validate against the spec when the reference validator is installed
    // (it is in CI).
    if let Ok(o) = Command::new("desktop-file-validate").arg(&desktop).output() {
        assert!(
            o.status.success(),
            "desktop-file-validate: {}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        );
    }

    let out = ok(&e.cmd().arg("list").output().unwrap());
    assert!(out.contains(id) && out.contains("Foo Editor"), "{out}");

    // Non-interactive remove refuses without --yes.
    let o = e.cmd().args(["remove", id]).output().unwrap();
    assert!(!o.status.success());
    assert!(prefix.exists());
    ok(&e.cmd().args(["remove", "--yes", id]).output().unwrap());
    assert!(!prefix.exists() && !desktop.exists());
    assert!(
        e.cmd()
            .args(["remove", "--yes", "../x"])
            .output()
            .unwrap()
            .status
            .code()
            == Some(2)
    );
}

#[cfg(target_os = "linux")]
mod linux_run {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    fn script(path: &Path, body: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// A fake Wine install: `wineboot --init` creates a prefix, running the
    /// installer drops an app and a Start-menu shortcut into it. Every call
    /// is logged with the relevant environment.
    fn fake_wine(e: &Env) -> PathBuf {
        let root = e.path("fakewine");
        let log = e.path("wine.log");
        e.write("payload/foo.exe", &app_exe(testpe::MACHINE_AMD64));
        e.write(
            "payload/Foo Editor.lnk",
            &testpe::lnk_file(
                r"C:\Program Files\Foo\foo.exe",
                Some("Edit foo"),
                None,
                None,
            ),
        );
        let payload = e.path("payload");
        script(
            &root.join("bin/wine"),
            &format!(
                r#"#!/bin/sh
echo "wine $* | HODLL=${{HODLL:-}} HODLL64=${{HODLL64:-}} OVR=$WINEDLLOVERRIDES" >> "{log}"
case "$1" in
  wineboot) sleep "${{FAKE_WINEBOOT_SECONDS:-0}}"; mkdir -p "$WINEPREFIX/drive_c/users/tester"; touch "$WINEPREFIX/system.reg" ;;
  *setup.exe)
    mkdir -p "$WINEPREFIX/drive_c/Program Files/Foo" \
             "$WINEPREFIX/drive_c/ProgramData/Microsoft/Windows/Start Menu/Programs"
    cp "{payload}/foo.exe" "$WINEPREFIX/drive_c/Program Files/Foo/foo.exe"
    cp "{payload}/Foo Editor.lnk" "$WINEPREFIX/drive_c/ProgramData/Microsoft/Windows/Start Menu/Programs/"
    exit 0 ;;
  *foo.exe) exit 7 ;;
esac
"#,
                log = log.display(),
                payload = payload.display()
            ),
        );
        script(
            &root.join("bin/wineserver"),
            &format!(
                // Records how many launcher entries exist when it is asked
                // to wait: an installer that starts its program at the end
                // ("Run Foo now") keeps the prefix busy until the user
                // closes that program, so entries must exist by then.
                "#!/bin/sh\necho \"wineserver $* entries=$(ls \"$XDG_DATA_HOME/applications\" 2>/dev/null | grep -c 'desktop$')\" >> \"{}\"\n",
                log.display()
            ),
        );
        let lib = root.join("lib64/wine");
        std::fs::create_dir_all(lib.join("i386-windows")).unwrap();
        std::fs::create_dir_all(lib.join("x86_64-windows")).unwrap();
        // On an ARM host, look like an ARM64EC Wine with FEX's DLLs.
        let arm = lib.join("aarch64-windows");
        std::fs::create_dir_all(&arm).unwrap();
        std::fs::write(
            arm.join("ntdll.dll"),
            PeBuilder::new(testpe::MACHINE_ARM64).chpe().build(),
        )
        .unwrap();
        std::fs::write(arm.join("libarm64ecfex.dll"), b"dll").unwrap();
        std::fs::write(arm.join("libwow64fex.dll"), b"dll").unwrap();
        root.join("bin/wine")
    }

    #[test]
    fn run_installer_then_installed_app() {
        let e = Env::new();
        let wine = fake_wine(&e);
        let setup = e.write("dl/foo-setup.exe", &inno_installer());
        let cmd = |e: &Env| {
            let mut c = e.cmd();
            c.env("LUCENT_BRIDGE_WINE", &wine)
                .env("PATH", "/usr/bin:/bin")
                .env_remove("LUCENT_BRIDGE_FEX_DLL_DIR");
            c
        };
        let o = cmd(&e).arg("run").arg(&setup).output().unwrap();
        ok(&o);
        let log = std::fs::read_to_string(e.path("wine.log")).unwrap();
        let lines: Vec<&str> = log.lines().collect();
        assert!(lines[0].starts_with("wine wineboot --init"), "{log}");
        assert!(lines[1].contains("foo-setup.exe"), "{log}");
        assert!(lines[1].contains("OVR=winemenubuilder.exe=d"), "{log}");
        // The entry is added as soon as the installer exits, before
        // waiting for the rest of the prefix (e.g. the app it started).
        assert_eq!(lines[2], "wineserver -w entries=1", "{log}");
        if cfg!(target_arch = "aarch64") {
            assert!(lines[1].contains("HODLL=libwow64fex.dll"), "{log}");
            assert!(lines[1].contains("HODLL64=libarm64ecfex.dll"), "{log}");
        } else {
            assert!(lines[1].contains("HODLL= HODLL64= "), "{log}");
        }

        // A launcher entry pointing at the installed program appeared.
        let apps = e.path("home/.local/share/applications");
        let entries: Vec<PathBuf> = std::fs::read_dir(&apps)
            .unwrap()
            .map(|d| d.unwrap().path())
            .collect();
        assert_eq!(entries.len(), 1, "{entries:?}");
        let text = std::fs::read_to_string(&entries[0]).unwrap();
        assert!(text.contains("Name=Foo Editor\n"), "{text}");
        assert!(text.contains("Comment=Edit foo\n"), "{text}");
        let g = lucent_bridge::desktop::parse_group(&text);
        let argv = lucent_bridge::desktop::split_exec(&g["Exec"]);
        assert_eq!(argv[1], "run");
        let installed = PathBuf::from(&argv[2]);
        assert!(installed.ends_with("drive_c/Program Files/Foo/foo.exe"));
        assert!(Path::new(&g["Icon"]).is_file());

        // Launching the installed program reuses the installer's prefix
        // (no second wineboot) and propagates the exit code.
        let o = cmd(&e).arg("run").arg(&installed).output().unwrap();
        assert_eq!(
            o.status.code(),
            Some(7),
            "{}",
            String::from_utf8_lossy(&o.stderr)
        );
        let log = std::fs::read_to_string(e.path("wine.log")).unwrap();
        assert_eq!(log.matches("wineboot").count(), 1, "{log}");
        assert!(log.lines().last().unwrap().contains("foo.exe"), "{log}");

        let out = ok(&cmd(&e).arg("list").output().unwrap());
        assert!(out.contains("foo-editor-"), "{out}");
    }

    fn events(path: &Path) -> Vec<serde_json::Value> {
        std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    /// `run --progress-fd 3`: a first launch reported as JSON lines.
    #[test]
    fn progress_fd_reports_a_first_launch() {
        use std::os::fd::AsRawFd;
        use std::os::unix::process::CommandExt;
        let e = Env::new();
        let wine = fake_wine(&e);
        let setup = e.write("dl/foo-setup.exe", &inno_installer());
        let out = e.path("progress.jsonl");
        let file = std::fs::File::create(&out).unwrap();
        let fd = file.as_raw_fd();
        let mut c = e.cmd();
        c.env("LUCENT_BRIDGE_WINE", &wine)
            .env("PATH", "/usr/bin:/bin")
            .args(["run", "--progress-fd", "3"])
            .arg(&setup);
        // SAFETY: dup2 and fcntl are async-signal-safe. If the file already
        // is fd 3, dup2 would be a no-op that leaves close-on-exec set.
        unsafe {
            c.pre_exec(move || {
                let r = if fd == 3 {
                    libc::fcntl(3, libc::F_SETFD, 0)
                } else {
                    libc::dup2(fd, 3)
                };
                if r == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        ok(&c.output().unwrap());
        drop(file);
        let ev = events(&out);
        let kinds: Vec<&str> = ev.iter().map(|v| v["event"].as_str().unwrap()).collect();
        assert!(ev.iter().all(|v| v["v"] == 1), "{ev:?}");
        assert_eq!(kinds[0], "start", "{kinds:?}");
        assert_eq!(ev[0]["installer"], true);
        assert_eq!(ev[0]["first_launch"], true);
        assert_eq!(ev[0]["file"], "foo-setup.exe");
        let pos = |k: &str, step: Option<&str>| {
            ev.iter()
                .position(|v| v["event"] == k && step.is_none_or(|s| v["step"] == s))
                .unwrap_or_else(|| panic!("no {k} {step:?} in {kinds:?}"))
        };
        assert!(pos("step", Some("prepare")) < pos("step", Some("launch")));
        assert!(pos("step", Some("launch")) < pos("launched", None));
        assert!(pos("launched", None) < pos("installed", None));
        assert_eq!(kinds.last(), Some(&"exit"));
        assert_eq!(ev.last().unwrap()["code"], 0);
        // A program that is already set up starts without preparation.
        let o = e
            .cmd()
            .env("LUCENT_BRIDGE_WINE", &wine)
            .env("PATH", "/usr/bin:/bin")
            .args(["run", "--progress-fd", "1"])
            .arg(
                e.path("home/.local/share/lucent-bridge/prefixes")
                    .read_dir()
                    .unwrap()
                    .next()
                    .unwrap()
                    .unwrap()
                    .path()
                    .join("drive_c/Program Files/Foo/foo.exe"),
            )
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&o.stdout);
        assert!(!text.contains("\"prepare\""), "{text}");
        assert!(text.contains("\"first_launch\":false"), "{text}");
        assert!(text.contains("\"event\":\"exit\",\"code\":7"), "{text}");
        // An fd that isn't open is an error, not a silent no-op.
        let o = e
            .cmd()
            .args(["run", "--progress-fd", "99"])
            .arg(&setup)
            .output()
            .unwrap();
        assert_eq!(o.status.code(), Some(2));
    }

    /// Cancel in the launch sheet: the half-made Windows environment goes.
    #[test]
    fn cancel_in_the_sheet_removes_the_new_prefix() {
        let e = Env::new();
        let wine = fake_wine(&e);
        let app = e.write("dl/foo.exe", &app_exe(testpe::MACHINE_AMD64));
        let sheet = e.path("bin/sheet");
        let got = e.path("sheet-input.jsonl");
        // Reads what the bridge says until it is preparing, then "Cancel".
        script(
            &sheet,
            &format!(
                "#!/bin/sh\nwhile read -r l; do echo \"$l\" >> '{}'; case \"$l\" in *prepare*) exit 2;; esac; done\n",
                got.display()
            ),
        );
        let o = e
            .cmd()
            .env("LUCENT_BRIDGE_WINE", &wine)
            .env("PATH", "/usr/bin:/bin")
            .env("FAKE_WINEBOOT_SECONDS", "5")
            .env("WAYLAND_DISPLAY", "wayland-test")
            .env("LUCENT_BRIDGE_SHEET", &sheet)
            .arg("run")
            .arg(&app)
            .output()
            .unwrap();
        assert_eq!(
            o.status.code(),
            Some(130),
            "{}",
            String::from_utf8_lossy(&o.stderr)
        );
        let prefixes = e.path("home/.local/share/lucent-bridge/prefixes");
        assert_eq!(
            prefixes.read_dir().map(|d| d.count()).unwrap_or(0),
            0,
            "the new prefix is removed"
        );
        let log = std::fs::read_to_string(e.path("wine.log")).unwrap();
        assert!(log.contains("wineserver -k"), "{log}");
        assert!(!log.contains("foo.exe"), "the program never started: {log}");
        let ev = events(&got);
        assert_eq!(ev[0]["event"], "start");
    }

    /// Installed GUI programs go through lucent-start.exe (filtered token,
    /// no "[Administrator]"); installers don't.
    #[test]
    fn installed_programs_start_with_the_filtered_token() {
        let e = Env::new();
        let wine = fake_wine(&e);
        let helper = e.write("share/lucent-start.exe", b"MZ");
        let setup = e.write("dl/foo-setup.exe", &inno_installer());
        let cmd = |e: &Env| {
            let mut c = e.cmd();
            c.env("LUCENT_BRIDGE_WINE", &wine)
                .env("PATH", "/usr/bin:/bin")
                .env("LUCENT_BRIDGE_START_HELPER", &helper);
            c
        };
        ok(&cmd(&e).arg("run").arg(&setup).output().unwrap());
        let log = std::fs::read_to_string(e.path("wine.log")).unwrap();
        assert!(
            !log.contains("lucent-start.exe"),
            "installers start directly: {log}"
        );
        let prefix = e
            .path("home/.local/share/lucent-bridge/prefixes")
            .read_dir()
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let installed = prefix.join("drive_c/Program Files/Foo/foo.exe");
        ok(&cmd(&e)
            .arg("run")
            .arg(&installed)
            .arg("--")
            .arg("a b")
            .output()
            .unwrap());
        let log = std::fs::read_to_string(e.path("wine.log")).unwrap();
        let last = log.lines().last().unwrap();
        assert!(
            last.starts_with(&format!(
                "wine {} C:\\Program Files\\Foo\\foo.exe a b |",
                helper.display()
            )),
            "{log}"
        );
    }

    /// `lucent-bridge-template.service` (Lucent OS) runs `build-template`
    /// as a system service: no HOME, no XDG variables.
    #[test]
    fn build_template_needs_no_home() {
        let e = Env::new();
        let wine = fake_wine(&e);
        let out = e.path("var/lib/lucent-bridge/prefix-template");
        let o = bin()
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("LUCENT_BRIDGE_WINE", &wine)
            .arg("build-template")
            .arg(&out)
            .output()
            .unwrap();
        ok(&o);
        assert!(out.join("system.reg").is_file());
        assert!(out.join(lucent_bridge::template::STAMP_FILE).is_file());
    }

    #[test]
    fn binfmt_entry_point_runs_the_exe() {
        let e = Env::new();
        let wine = fake_wine(&e);
        let app = e.write("dl/foo.exe", &app_exe(testpe::MACHINE_AMD64));
        let link = e.path("bin/lucent-bridge-binfmt");
        std::fs::create_dir_all(link.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_lucent-bridge"), &link).unwrap();
        let o = Command::new(&link)
            .arg(&app)
            .arg("--flag")
            .env("HOME", e.path("home"))
            .env("XDG_DATA_HOME", e.path("home/.local/share"))
            .env("XDG_CONFIG_HOME", e.path("home/.config"))
            .env("LUCENT_BRIDGE_WINE", &wine)
            .env("PATH", "/usr/bin:/bin")
            .env_remove("DISPLAY")
            .env_remove("WAYLAND_DISPLAY")
            .output()
            .unwrap();
        assert_eq!(
            o.status.code(),
            Some(7),
            "{}",
            String::from_utf8_lossy(&o.stderr)
        );
        let log = std::fs::read_to_string(e.path("wine.log")).unwrap();
        assert!(log.contains("foo.exe --flag"), "{log}");
    }

    #[test]
    fn broken_compat_entry_blocks_unless_forced() {
        let e = Env::new();
        let wine = fake_wine(&e);
        let game = e.write(
            "games/VALORANT.exe",
            &PeBuilder::new(testpe::MACHINE_AMD64).build(),
        );
        let run = |force: bool| {
            let mut c = e.cmd();
            c.env("LUCENT_BRIDGE_WINE", &wine)
                .env("PATH", "/usr/bin:/bin")
                .arg("run");
            if force {
                c.arg("--force");
            }
            c.arg(&game).output().unwrap()
        };
        let o = run(false);
        assert_eq!(o.status.code(), Some(2));
        assert!(String::from_utf8_lossy(&o.stderr).contains("kernel-mode anti-cheat"));
        assert!(!e.path("wine.log").exists(), "nothing was executed");
        let o = run(true);
        assert!(o.status.success() || o.status.code() == Some(0), "{o:?}");
        assert!(e.path("wine.log").exists());
    }
}
