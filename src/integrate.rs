//! Desktop integration: after an installer finishes, find the shortcuts it
//! created inside the prefix (and any entries Wine's own menu builder wrote
//! for it) and turn them into clean launcher entries with real icons.

use crate::appid;
use crate::desktop::{self, DesktopEntry};
use crate::inspect::Inspection;
use crate::installer::is_uninstaller_name;
use crate::lnk;
use crate::prefix::Dirs;
use anyhow::Result;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shortcut {
    pub name: String,
    pub lnk: PathBuf,
    pub target: PathBuf,
    pub args: Vec<String>,
    pub working_dir: Option<PathBuf>,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Outcome {
    pub entries: Vec<PathBuf>,
    pub icons: Vec<PathBuf>,
    /// Wine-generated entries for this prefix that we replaced.
    pub removed_wine_entries: Vec<PathBuf>,
}

/// Find `name` in `dir` ignoring ASCII case (Wine's file system is
/// case-insensitive from the program's point of view).
/// The on-disk spelling is returned even on case-insensitive file systems.
fn find_ci(dir: &Path, name: &str) -> Option<PathBuf> {
    let names: Vec<String> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names
        .iter()
        .find(|n| *n == name)
        .or_else(|| names.iter().find(|n| n.eq_ignore_ascii_case(name)))
        .map(|n| dir.join(n))
}

/// Map a Windows path inside `prefix` to a Unix path. Drive C is
/// `drive_c`; other drives go through `dosdevices/<x>:` (Z: is `/` by
/// default). Components are matched case-insensitively against what exists;
/// missing tails are appended verbatim.
pub fn windows_to_unix(prefix: &Path, win: &str) -> Option<PathBuf> {
    let win = win.trim().trim_matches('"');
    let mut chars = win.chars();
    let drive = chars.next()?.to_ascii_lowercase();
    if !drive.is_ascii_alphabetic() || chars.next()? != ':' {
        return None;
    }
    let rest: Vec<&str> = win[2..]
        .split(['\\', '/'])
        .filter(|c| !c.is_empty())
        .collect();
    if rest.contains(&"..") {
        return None;
    }
    let mut cur = if drive == 'c' {
        prefix.join("drive_c")
    } else {
        let dev = prefix.join("dosdevices").join(format!("{drive}:"));
        match std::fs::canonicalize(&dev) {
            Ok(p) => p,
            Err(_) if drive == 'z' => PathBuf::from("/"),
            Err(_) => return None,
        }
    };
    for comp in rest {
        cur = find_ci(&cur, comp).unwrap_or_else(|| cur.join(comp));
    }
    Some(cur)
}

/// Directories inside a prefix where installers put shortcuts.
fn shortcut_dirs(prefix: &Path) -> Vec<PathBuf> {
    let c = prefix.join("drive_c");
    let mut dirs = vec![
        c.join("ProgramData/Microsoft/Windows/Start Menu"),
        c.join("users/Public/Desktop"),
    ];
    if let Ok(rd) = std::fs::read_dir(c.join("users")) {
        for u in rd.filter_map(|e| e.ok()) {
            let base = u.path();
            dirs.push(base.join("AppData/Roaming/Microsoft/Windows/Start Menu"));
            dirs.push(base.join("Desktop"));
        }
    }
    dirs
}

fn walk_lnks(dir: &Path, depth: u8, out: &mut Vec<PathBuf>) {
    // Wine links users/<name>/Desktop to the real ~/Desktop; never follow
    // symlinks so shortcuts from other places can't leak in.
    let Ok(meta) = std::fs::symlink_metadata(dir) else {
        return;
    };
    if !meta.is_dir() || depth > 6 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path())).collect();
    entries.sort();
    for p in entries {
        let Ok(m) = std::fs::symlink_metadata(&p) else {
            continue;
        };
        if m.is_dir() {
            walk_lnks(&p, depth + 1, out);
        } else if m.is_file() && p.extension().is_some_and(|e| e.eq_ignore_ascii_case("lnk")) {
            out.push(p);
        }
    }
}

/// Wine-generated `.desktop` files (from winemenubuilder) whose
/// `WINEPREFIX` is `prefix`, with the `.lnk` each one launches.
pub fn wine_generated_entries(applications: &Path, prefix: &Path) -> Vec<(PathBuf, PathBuf)> {
    let mut files = Vec::new();
    collect_desktop_files(&applications.join("wine"), 0, &mut files);
    let mut out = Vec::new();
    for f in files {
        let Ok(text) = std::fs::read_to_string(&f) else {
            continue;
        };
        let group = desktop::parse_group(&text);
        let Some(exec) = group.get("Exec") else {
            continue;
        };
        let argv = desktop::split_exec(exec);
        let wp = argv
            .iter()
            .find_map(|a| a.strip_prefix("WINEPREFIX="))
            .map(PathBuf::from);
        if wp.as_deref() != Some(prefix) {
            continue;
        }
        // Either `start.exe /Unix <unix path>` or a bare `C:\...\x.lnk`.
        let lnk = if let Some(i) = argv.iter().position(|a| a.eq_ignore_ascii_case("/unix")) {
            argv.get(i + 1).map(PathBuf::from)
        } else {
            argv.iter()
                .find(|a| a.to_ascii_lowercase().ends_with(".lnk"))
                .and_then(|a| windows_to_unix(prefix, a))
        };
        if let Some(lnk) = lnk {
            out.push((f, lnk));
        }
    }
    out
}

fn collect_desktop_files(dir: &Path, depth: u8, out: &mut Vec<PathBuf>) {
    if depth > 8 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path())).collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            collect_desktop_files(&p, depth + 1, out);
        } else if p.extension().is_some_and(|e| e == "desktop") {
            out.push(p);
        }
    }
}

/// Split a Windows command line into arguments (double quotes group).
pub fn split_windows_args(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_q = false;
    let mut has = false;
    for c in s.chars() {
        match c {
            '"' => {
                in_q = !in_q;
                has = true;
            }
            c if c.is_whitespace() && !in_q => {
                if has || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                    has = false;
                }
            }
            c => cur.push(c),
        }
    }
    if has || !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn is_noise(name: &str, target: &Path) -> bool {
    let target_name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let lname = name.to_ascii_lowercase();
    is_uninstaller_name(&target_name)
        || lname.contains("uninstall")
        || lname.starts_with("remove ")
        || !target_name.to_ascii_lowercase().ends_with(".exe")
}

/// Every launchable shortcut belonging to `prefix`, de-duplicated by target.
pub fn find_shortcuts(prefix: &Path, applications: &Path) -> Vec<Shortcut> {
    let mut lnks = Vec::new();
    for d in shortcut_dirs(prefix) {
        walk_lnks(&d, 0, &mut lnks);
    }
    for (_, lnk) in wine_generated_entries(applications, prefix) {
        if !lnks.contains(&lnk) {
            lnks.push(lnk);
        }
    }
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for lnk_path in lnks {
        let Ok(data) = std::fs::read(&lnk_path) else {
            continue;
        };
        let Some(link) = lnk::parse(&data) else {
            continue;
        };
        let Some(target) = link
            .target
            .as_deref()
            .and_then(|t| windows_to_unix(prefix, t))
        else {
            continue;
        };
        let name = lnk_path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        if is_noise(&name, &target) || !target.is_file() || !seen.insert(target.clone()) {
            continue;
        }
        out.push(Shortcut {
            name,
            target,
            args: link
                .arguments
                .as_deref()
                .map(split_windows_args)
                .unwrap_or_default(),
            working_dir: link
                .working_dir
                .as_deref()
                .and_then(|w| windows_to_unix(prefix, w))
                .filter(|p| p.is_dir()),
            description: link.description,
            lnk: lnk_path,
        });
    }
    out
}

/// Build the launcher entry for one shortcut.
pub fn entry_for(
    sc: &Shortcut,
    app_id: &str,
    launcher: &str,
    icon: Option<&Path>,
    target: Option<&Inspection>,
) -> DesktopEntry {
    let mut exec = vec![launcher.to_string(), "run".into()];
    exec.push(sc.target.to_string_lossy().into_owned());
    if !sc.args.is_empty() {
        exec.push("--".into());
        exec.extend(sc.args.iter().cloned());
    }
    let comment = sc.description.clone().or_else(|| {
        target
            .and_then(|t| t.version.as_ref())
            .and_then(|v| v.file_description())
            .map(str::to_string)
    });
    DesktopEntry {
        name: sc.name.clone(),
        comment: comment.filter(|c| c != &sc.name),
        exec,
        working_dir: sc.working_dir.clone(),
        icon: icon.map(|p| p.to_string_lossy().into_owned()),
        terminal: target.is_some_and(|t| t.is_console()),
        startup_wm_class: sc
            .target
            .file_name()
            .map(|n| n.to_string_lossy().to_lowercase()),
        categories: vec![],
        app_id: app_id.to_string(),
    }
}

/// Write launcher entries and icons for every shortcut in the prefix, and
/// remove Wine's own entries for it. Idempotent.
pub fn integrate(dirs: &Dirs, app_id: &str, launcher: &str) -> Result<Outcome> {
    let prefix = dirs.prefix(app_id);
    let apps = dirs.applications();
    let mut outcome = Outcome::default();
    let mut used = BTreeSet::new();
    for sc in find_shortcuts(&prefix, &apps) {
        let mut slug = appid::slug(&sc.name);
        let base = slug.clone();
        let mut n = 2;
        while !used.insert(slug.clone()) {
            slug = format!("{base}-{n}");
            n += 1;
        }
        let target = Inspection::from_path(&sc.target).ok();
        let icon = match target.as_ref().and_then(|t| t.icon.as_ref()) {
            Some(icon) => {
                let dir = dirs.icons(app_id);
                std::fs::create_dir_all(&dir)?;
                let p = dir.join(format!("{slug}.png"));
                std::fs::write(&p, &icon.png)?;
                outcome.icons.push(p.clone());
                Some(p)
            }
            None => None,
        };
        let entry = entry_for(&sc, app_id, launcher, icon.as_deref(), target.as_ref());
        let path = desktop::write(&apps, &desktop::file_name(app_id, &slug), &entry)?;
        outcome.entries.push(path);
    }
    for (file, _) in wine_generated_entries(&apps, &prefix) {
        if std::fs::remove_file(&file).is_ok() {
            outcome.removed_wine_entries.push(file);
        }
    }
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testpe::{self, PeBuilder, ResKey, lnk_file};

    struct Fixture {
        _tmp: tempfile::TempDir,
        dirs: Dirs,
        id: String,
        prefix: PathBuf,
    }

    fn fixture() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let dirs = Dirs {
            data_home: tmp.path().join("share"),
            config_home: tmp.path().join("config"),
        };
        let id = "foo-12345678".to_string();
        let prefix = dirs.prefix(&id);
        std::fs::create_dir_all(prefix.join("drive_c/users/steamuser")).unwrap();
        Fixture {
            _tmp: tmp,
            dirs,
            id,
            prefix,
        }
    }

    fn write(path: &Path, data: &[u8]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, data).unwrap();
    }

    fn app_exe() -> Vec<u8> {
        PeBuilder::new(testpe::MACHINE_AMD64)
            .version(
                [2, 0, 0, 0],
                &[("ProductName", "Foo"), ("FileDescription", "Foo Editor")],
            )
            .icon_group(ResKey::Id(1), 1, &[testpe::png_icon(64, [9, 8, 7, 255])])
            .build()
    }

    #[test]
    fn windows_paths_map_into_prefix_case_insensitively() {
        let f = fixture();
        write(&f.prefix.join("drive_c/Program Files/Foo/Foo.exe"), b"x");
        assert_eq!(
            windows_to_unix(&f.prefix, r"c:\PROGRAM FILES\foo\foo.EXE").unwrap(),
            f.prefix.join("drive_c/Program Files/Foo/Foo.exe")
        );
        assert_eq!(
            windows_to_unix(&f.prefix, r"C:\\Program Files\\New\\x.exe").unwrap(),
            f.prefix.join("drive_c/Program Files/New/x.exe")
        );
        assert_eq!(
            windows_to_unix(&f.prefix, r"Z:\usr\bin").unwrap(),
            PathBuf::from("/usr/bin")
        );
        assert_eq!(windows_to_unix(&f.prefix, r"D:\x"), None, "unmapped drive");
        assert_eq!(windows_to_unix(&f.prefix, r"C:\..\..\etc"), None);
        assert_eq!(windows_to_unix(&f.prefix, r"relative\x"), None);
    }

    #[test]
    fn finds_start_menu_shortcuts_and_skips_noise() {
        let f = fixture();
        let c = f.prefix.join("drive_c");
        write(&c.join("Program Files/Foo/foo.exe"), &app_exe());
        write(&c.join("Program Files/Foo/unins000.exe"), &app_exe());
        write(&c.join("Program Files/Foo/readme.txt"), b"hi");
        let menu = c.join("ProgramData/Microsoft/Windows/Start Menu/Programs/Foo");
        write(
            &menu.join("Foo.lnk"),
            &lnk_file(
                r"C:\Program Files\Foo\foo.exe",
                Some("The Foo editor"),
                Some(r"C:\Program Files\Foo"),
                Some(r#"--profile "my profile""#),
            ),
        );
        write(
            &menu.join("Uninstall Foo.lnk"),
            &lnk_file(r"C:\Program Files\Foo\unins000.exe", None, None, None),
        );
        write(
            &menu.join("Read me.lnk"),
            &lnk_file(r"C:\Program Files\Foo\readme.txt", None, None, None),
        );
        write(
            &menu.join("Gone.lnk"),
            &lnk_file(r"C:\Program Files\Foo\missing.exe", None, None, None),
        );
        // Same target from the desktop: de-duplicated.
        write(
            &c.join("users/Public/Desktop/Foo.lnk"),
            &lnk_file(r"C:\Program Files\Foo\foo.exe", None, None, None),
        );
        let found = find_shortcuts(&f.prefix, &f.dirs.applications());
        assert_eq!(found.len(), 1, "{found:#?}");
        let sc = &found[0];
        assert_eq!(sc.name, "Foo");
        assert_eq!(sc.args, ["--profile", "my profile"]);
        assert_eq!(
            sc.working_dir.as_deref(),
            Some(c.join("Program Files/Foo").as_path())
        );
    }

    #[test]
    fn integrate_writes_entries_icons_and_replaces_wine_litter() {
        let f = fixture();
        let c = f.prefix.join("drive_c");
        write(&c.join("Program Files/Foo/foo.exe"), &app_exe());
        let lnk =
            c.join("users/steamuser/AppData/Roaming/Microsoft/Windows/Start Menu/Programs/Foo.lnk");
        write(
            &lnk,
            &lnk_file(r"C:\Program Files\Foo\foo.exe", None, None, None),
        );
        let apps = f.dirs.applications();
        // Wine's own entry for this prefix, plus one for another prefix.
        let ours = apps.join("wine/Programs/Foo.desktop");
        write(
            &ours,
            format!(
                "[Desktop Entry]\nName=Foo\nExec=env WINEPREFIX=\"{}\" wine C:\\\\\\\\windows\\\\\\\\command\\\\\\\\start.exe /Unix {}\nType=Application\n",
                f.prefix.display(),
                lnk.display().to_string().replace(' ', "\\\\ ")
            )
            .as_bytes(),
        );
        let theirs = apps.join("wine/Programs/Other.desktop");
        write(
            &theirs,
            b"[Desktop Entry]\nName=Other\nExec=env WINEPREFIX=\"/home/u/.wine\" wine C:\\\\\\\\x.lnk\n",
        );

        let out = integrate(&f.dirs, &f.id, "lucent-bridge").unwrap();
        assert_eq!(out.entries.len(), 1);
        assert_eq!(out.removed_wine_entries, vec![ours.clone()]);
        assert!(!ours.exists() && theirs.exists());
        let text = std::fs::read_to_string(&out.entries[0]).unwrap();
        let g = desktop::parse_group(&text);
        assert_eq!(g["Name"], "Foo");
        assert_eq!(g["Comment"], "Foo Editor");
        assert_eq!(g["StartupWMClass"], "foo.exe");
        assert_eq!(g["X-LucentBridge-AppId"], f.id);
        let argv = desktop::split_exec(&g["Exec"]);
        assert_eq!(argv[..2], ["lucent-bridge", "run"]);
        assert_eq!(PathBuf::from(&argv[2]), c.join("Program Files/Foo/foo.exe"));
        let icon = PathBuf::from(&g["Icon"]);
        assert!(icon.starts_with(f.dirs.icons(&f.id)));
        assert!(std::fs::read(&icon).unwrap().starts_with(b"\x89PNG"));
        // Running again is idempotent.
        let again = integrate(&f.dirs, &f.id, "lucent-bridge").unwrap();
        assert_eq!(again.entries, out.entries);
        assert!(again.removed_wine_entries.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_desktop_is_not_followed() {
        let f = fixture();
        let elsewhere = f._tmp.path().join("RealDesktop");
        write(&elsewhere.join("x.exe"), &app_exe());
        write(
            &elsewhere.join("Leak.lnk"),
            &lnk_file(r"Z:\nonexistent\x.exe", None, None, None),
        );
        std::os::unix::fs::symlink(&elsewhere, f.prefix.join("drive_c/users/steamuser/Desktop"))
            .unwrap();
        let mut lnks = Vec::new();
        for d in shortcut_dirs(&f.prefix) {
            walk_lnks(&d, 0, &mut lnks);
        }
        assert!(lnks.is_empty(), "{lnks:?}");
    }

    #[test]
    fn windows_arg_splitting() {
        assert_eq!(split_windows_args(r#"-a "b c" d"#), ["-a", "b c", "d"]);
        assert_eq!(split_windows_args(r#""""#), [""]);
        assert!(split_windows_args("   ").is_empty());
    }
}
