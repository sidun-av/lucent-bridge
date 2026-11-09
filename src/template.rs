//! Prebuilt prefix template: a fully initialised Wine prefix (Wine Mono
//! and Gecko already installed) that the OS image ships, so a program's
//! first launch copies it (reflinked where the file system can) instead of
//! running `wineboot --init`.
//!
//! The template is built once, at image build time, by
//! `lucent-bridge build-template` for a placeholder Windows user
//! ([`TEMPLATE_USER`]). Adopting it for a real user means doing what
//! `wineboot` would have done differently for them:
//!
//! - the profile directory `drive_c/users/<name>` and every registry
//!   reference to it carry the user's name (Wine takes it from `$USER`);
//! - the "My Documents", Desktop, ... shell folders are symlinks into the
//!   user's XDG directories when those exist (Wine's `create_link` rules).

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Windows user name the template is built for. Distinctive, so a plain
/// token replacement can't touch anything else.
pub const TEMPLATE_USER: &str = "lucent-template";
/// Marker file inside a template directory.
pub const STAMP_FILE: &str = "lucent-bridge-template.toml";
/// Where the OS image puts the template.
pub const SYSTEM_DIR: &str = "/usr/share/lucent-bridge/prefix-template";
/// Override the template location; empty or `none` disables templates.
pub const ENV_TEMPLATE: &str = "LUCENT_BRIDGE_PREFIX_TEMPLATE";

/// Where the kernel exposes SELinux (its `enforce` file exists when SELinux
/// is enabled).
pub const SELINUX_FS: &str = "/sys/fs/selinux";

/// The command that gives a prefix just copied from the template the SELinux
/// labels files in the user's home get. `cp -a` carries the template's own
/// labels (`/var/lib/lucent-bridge`: `var_lib_t`) along as an extended
/// attribute, even with `--no-preserve=context`, and copying without xattrs
/// would lose Wine's `user.DOSATTRIB`. `None` without SELinux or
/// `restorecon`: then there is nothing to fix, or nothing to fix it with.
pub fn relabel_argv(
    selinux_fs: &Path,
    restorecon: Option<&Path>,
    prefix: &Path,
) -> Option<Vec<String>> {
    if !selinux_fs.join("enforce").exists() {
        return None;
    }
    Some(vec![
        restorecon?.to_string_lossy().into_owned(),
        "-R".into(),
        prefix.to_string_lossy().into_owned(),
    ])
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stamp {
    /// Windows user name inside the template.
    pub user: String,
    /// The Wine binary that built it; only that Wine may use it.
    pub wine: PathBuf,
    pub wine_version: String,
    pub created: u64,
    pub built_by: String,
}

impl Stamp {
    pub fn load(dir: &Path) -> Option<Stamp> {
        let text = std::fs::read_to_string(dir.join(STAMP_FILE)).ok()?;
        toml::from_str(&text).ok()
    }
}

/// The template to use for prefixes run by `wine`, if there is a usable
/// one. `env` is the process environment.
pub fn find(env: &dyn Fn(&str) -> Option<String>, wine: &Path) -> Option<PathBuf> {
    let dir = match env(ENV_TEMPLATE) {
        Some(v) if v.is_empty() || v == "none" => return None,
        Some(v) => PathBuf::from(v),
        None => PathBuf::from(SYSTEM_DIR),
    };
    let stamp = Stamp::load(&dir)?;
    (dir.join("system.reg").is_file() && same_file(&stamp.wine, wine)).then_some(dir)
}

fn same_file(a: &Path, b: &Path) -> bool {
    a == b
        || matches!(
            (std::fs::canonicalize(a), std::fs::canonicalize(b)),
            (Ok(x), Ok(y)) if x == y
        )
}

/// The Windows user name Wine will use: `$USER`, else the passwd name,
/// else "wine"; anything up to a `/` or `\` is dropped (as Wine does).
pub fn wine_user_name(env: &dyn Fn(&str) -> Option<String>) -> String {
    let name = env("USER")
        .filter(|u| !u.is_empty())
        .or_else(passwd_name)
        .unwrap_or_else(|| "wine".into());
    name.rsplit(['/', '\\'])
        .next()
        .unwrap_or("wine")
        .to_string()
}

fn passwd_name() -> Option<String> {
    #[cfg(unix)]
    {
        // SAFETY: getpwuid returns a pointer to static storage or NULL; the
        // name is copied out before any other passwd call.
        unsafe {
            let pw = libc::getpwuid(libc::getuid());
            if pw.is_null() || (*pw).pw_name.is_null() {
                return None;
            }
            Some(
                std::ffi::CStr::from_ptr((*pw).pw_name)
                    .to_string_lossy()
                    .into_owned(),
            )
        }
    }
    #[cfg(not(unix))]
    None
}

/// Escape a string the way Wine writes it into its `.reg` files.
fn reg_escape(s: &str) -> String {
    let mut out = String::new();
    for u in s.encode_utf16() {
        match u {
            0x5c => out.push_str("\\\\"),
            0x22 => out.push_str("\\\""),
            0x20..=0x7e => out.push(u as u8 as char),
            _ => out.push_str(&format!("\\x{u:04x}")),
        }
    }
    out
}

/// Replace every (ASCII case-insensitive) occurrence of `from` in a Wine
/// registry file with `to`, escaped for the file.
pub fn rewrite_registry(text: &str, from: &str, to: &str) -> String {
    let to = reg_escape(to);
    let lower = text.to_ascii_lowercase();
    let needle = from.to_ascii_lowercase();
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for (i, _) in lower.match_indices(&needle) {
        out.push_str(&text[last..i]);
        out.push_str(&to);
        last = i + needle.len();
    }
    out.push_str(&text[last..]);
    out
}

/// The user's XDG directory for `var` (from `user-dirs.dirs`), parsed the
/// way Wine's shell32 parses it: only absolute or `$HOME/` values count.
fn xdg_user_dir(config: &str, var: &str, home: &Path) -> Option<PathBuf> {
    for line in config.lines() {
        let Some(rest) = line.trim_start().strip_prefix(var) else {
            continue;
        };
        let Some(v) = rest.trim_start().strip_prefix('=') else {
            continue;
        };
        let v = v.trim_start().strip_prefix('"')?;
        let v = &v[..v.find('"')?];
        let v = v
            .replace("\\\\", "\u{0}")
            .replace('\\', "")
            .replace('\u{0}', "\\");
        return if let Some(r) = v.strip_prefix("$HOME/") {
            Some(home.join(r))
        } else if v.starts_with('/') {
            Some(PathBuf::from(v))
        } else {
            None
        };
    }
    None
}

/// Shell folders Wine links into the user's home: (path inside the
/// profile, XDG variable, default under $HOME).
pub const SHELL_LINKS: &[(&str, &str, &str)] = &[
    ("Documents", "XDG_DOCUMENTS_DIR", "Documents"),
    ("Desktop", "XDG_DESKTOP_DIR", "Desktop"),
    ("Pictures", "XDG_PICTURES_DIR", "Pictures"),
    ("Videos", "XDG_VIDEOS_DIR", "Movies"),
    ("Music", "XDG_MUSIC_DIR", "Music"),
    ("Downloads", "XDG_DOWNLOAD_DIR", "Downloads"),
    (
        "AppData/Roaming/Microsoft/Windows/Templates",
        "XDG_TEMPLATES_DIR",
        "Templates",
    ),
];

/// Where Wine would point each shell folder for this user: the XDG
/// directory if it exists, else `$HOME/<default>` if it exists, else
/// nothing (a plain directory).
pub fn link_targets(home: &Path, xdg_config_home: &Path) -> Vec<(&'static str, Option<PathBuf>)> {
    let config =
        std::fs::read_to_string(xdg_config_home.join("user-dirs.dirs")).unwrap_or_default();
    SHELL_LINKS
        .iter()
        .map(|(folder, var, default)| {
            let target = xdg_user_dir(&config, var, home)
                .filter(|p| p.is_dir())
                .or_else(|| Some(home.join(default)).filter(|p| p.is_dir()));
            (*folder, target)
        })
        .collect()
}

/// Turn a freshly copied template into this user's prefix.
pub fn adopt(prefix: &Path, user: &str, home: &Path, xdg_config_home: &Path) -> Result<()> {
    let stamp = Stamp::load(prefix).context("the copied template has no stamp file")?;
    let users = prefix.join("drive_c/users");
    let from = users.join(&stamp.user);
    let to = users.join(user);
    if user.is_empty() || user.contains('/') {
        bail!("unusable user name '{user}'");
    }
    if stamp.user != user {
        if to.exists() {
            bail!("{} already exists", to.display());
        }
        std::fs::rename(&from, &to)
            .with_context(|| format!("renaming {} to {}", from.display(), to.display()))?;
        for reg in ["system.reg", "user.reg", "userdef.reg"] {
            let p = prefix.join(reg);
            let Ok(text) = std::fs::read_to_string(&p) else {
                continue;
            };
            let new = rewrite_registry(&text, &stamp.user, user);
            if new != text {
                let tmp = p.with_extension("reg.tmp");
                std::fs::write(&tmp, new).with_context(|| format!("writing {}", tmp.display()))?;
                std::fs::rename(&tmp, &p).with_context(|| format!("writing {}", p.display()))?;
            }
        }
    }
    for (folder, target) in link_targets(home, xdg_config_home) {
        let p = to.join(folder);
        let Ok(meta) = std::fs::symlink_metadata(&p) else {
            continue;
        };
        match target {
            Some(t) => {
                // Only an empty placeholder directory (or a stale link) is
                // replaced; Wine does the same.
                if meta.file_type().is_symlink() {
                    std::fs::remove_file(&p)?;
                } else if meta.is_dir() && std::fs::remove_dir(&p).is_err() {
                    continue;
                }
                #[cfg(unix)]
                std::os::unix::fs::symlink(&t, &p)
                    .with_context(|| format!("linking {} to {}", p.display(), t.display()))?;
            }
            None if meta.file_type().is_symlink() => {
                std::fs::remove_file(&p)?;
                std::fs::create_dir(&p)?;
            }
            None => {}
        }
    }
    std::fs::remove_file(prefix.join(STAMP_FILE))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_copied_prefix_is_relabelled_only_with_selinux_and_restorecon() {
        let tmp = std::env::temp_dir().join(format!("lb-selinuxfs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let rc = Path::new("/usr/sbin/restorecon");
        let prefix = Path::new("/home/anna/.local/share/lucent-bridge/prefixes/foo-1");
        // No SELinux (no enforce file under the selinuxfs mount): nothing to do.
        assert_eq!(relabel_argv(&tmp, Some(rc), prefix), None);
        std::fs::write(tmp.join("enforce"), "1").unwrap();
        assert_eq!(
            relabel_argv(&tmp, Some(rc), prefix),
            Some(vec![
                "/usr/sbin/restorecon".to_string(),
                "-R".to_string(),
                prefix.to_string_lossy().into_owned()
            ])
        );
        // SELinux but no restorecon: skip rather than fail the launch.
        assert_eq!(relabel_argv(&tmp, None, prefix), None);
        std::fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn registry_rewrite_is_case_insensitive_and_escapes() {
        let text = "[Software\\\\Microsoft\\\\Windows NT\\\\CurrentVersion\\\\ProfileList\\\\S-1-5-21-0-0-0-1000] 1\n\
                    \"ProfileImagePath\"=str(2):\"C:\\\\users\\\\lucent-template\"\n\
                    \"Desktop\"=\"C:\\\\users\\\\Lucent-Template\\\\Desktop\"\n";
        let out = rewrite_registry(text, TEMPLATE_USER, "anna");
        assert!(out.contains("\"C:\\\\users\\\\anna\"\n"), "{out}");
        assert!(out.contains("\"C:\\\\users\\\\anna\\\\Desktop\""), "{out}");
        assert!(!out.to_ascii_lowercase().contains(TEMPLATE_USER));
        let out = rewrite_registry(text, TEMPLATE_USER, "Jürgen");
        assert!(out.contains("users\\\\J\\x00fcrgen\\\\Desktop"), "{out}");
    }

    #[test]
    fn user_name_like_wine() {
        let env = |k: &str| (k == "USER").then(|| "dom\\anna".to_string());
        assert_eq!(wine_user_name(&env), "anna");
        let env = |k: &str| (k == "USER").then(|| "bob".to_string());
        assert_eq!(wine_user_name(&env), "bob");
        assert!(!wine_user_name(&|_| None).is_empty());
    }

    #[test]
    fn xdg_parsing_matches_wine() {
        let home = Path::new("/home/a");
        let cfg = "# comment\nXDG_DESKTOP_DIR=\"$HOME/Рабочий стол\"\n  XDG_MUSIC_DIR = \"/srv/music\"\nXDG_VIDEOS_DIR=\"relative\"\n";
        assert_eq!(
            xdg_user_dir(cfg, "XDG_DESKTOP_DIR", home),
            Some(PathBuf::from("/home/a/Рабочий стол"))
        );
        assert_eq!(
            xdg_user_dir(cfg, "XDG_MUSIC_DIR", home),
            Some(PathBuf::from("/srv/music"))
        );
        assert_eq!(xdg_user_dir(cfg, "XDG_VIDEOS_DIR", home), None);
        assert_eq!(xdg_user_dir(cfg, "XDG_DOWNLOAD_DIR", home), None);
    }

    fn template(root: &Path) -> PathBuf {
        let p = root.join("prefix");
        let prof = p.join("drive_c/users").join(TEMPLATE_USER);
        for f in SHELL_LINKS {
            std::fs::create_dir_all(prof.join(f.0)).unwrap();
        }
        std::fs::create_dir_all(prof.join("AppData/Local/Temp")).unwrap();
        std::fs::write(
            p.join("user.reg"),
            "\"Personal\"=\"C:\\\\users\\\\lucent-template\\\\Documents\"\n",
        )
        .unwrap();
        std::fs::write(p.join("system.reg"), "nothing to see\n").unwrap();
        let stamp = Stamp {
            user: TEMPLATE_USER.into(),
            wine: "/usr/bin/wine".into(),
            wine_version: "wine-11.21".into(),
            created: 0,
            built_by: "test".into(),
        };
        std::fs::write(p.join(STAMP_FILE), toml::to_string(&stamp).unwrap()).unwrap();
        p
    }

    #[cfg(unix)]
    #[test]
    fn adopt_renames_rewrites_and_links() {
        let tmp = tempfile::tempdir().unwrap();
        let p = template(tmp.path());
        let home = tmp.path().join("home");
        let docs = home.join("Dokumente");
        std::fs::create_dir_all(&docs).unwrap();
        std::fs::create_dir_all(home.join("Downloads")).unwrap();
        let cfg = home.join(".config");
        std::fs::create_dir_all(&cfg).unwrap();
        std::fs::write(
            cfg.join("user-dirs.dirs"),
            "XDG_DOCUMENTS_DIR=\"$HOME/Dokumente\"\n",
        )
        .unwrap();
        // A non-empty placeholder is left alone.
        std::fs::write(
            p.join("drive_c/users")
                .join(TEMPLATE_USER)
                .join("Music/keep.txt"),
            "x",
        )
        .unwrap();
        std::fs::create_dir_all(home.join("Music")).unwrap();

        adopt(&p, "anna", &home, &cfg).unwrap();
        let prof = p.join("drive_c/users/anna");
        assert!(prof.join("AppData/Local/Temp").is_dir());
        assert!(!p.join("drive_c/users").join(TEMPLATE_USER).exists());
        assert_eq!(std::fs::read_link(prof.join("Documents")).unwrap(), docs);
        assert_eq!(
            std::fs::read_link(prof.join("Downloads")).unwrap(),
            home.join("Downloads")
        );
        assert!(
            std::fs::symlink_metadata(prof.join("Desktop"))
                .unwrap()
                .is_dir()
        );
        assert!(prof.join("Music/keep.txt").is_file());
        let reg = std::fs::read_to_string(p.join("user.reg")).unwrap();
        assert!(reg.contains("C:\\\\users\\\\anna\\\\Documents"), "{reg}");
        assert!(!p.join(STAMP_FILE).exists());
    }

    #[test]
    fn adopt_for_the_template_user_only_links() {
        let tmp = tempfile::tempdir().unwrap();
        let p = template(tmp.path());
        let home = tmp.path().join("home");
        adopt(&p, TEMPLATE_USER, &home, &home.join(".config")).unwrap();
        assert!(p.join("drive_c/users").join(TEMPLATE_USER).is_dir());
    }

    #[test]
    fn find_respects_override_and_wine() {
        let tmp = tempfile::tempdir().unwrap();
        let p = template(tmp.path());
        let v = p.display().to_string();
        let env = move |k: &str| (k == ENV_TEMPLATE).then(|| v.clone());
        assert_eq!(find(&env, Path::new("/usr/bin/wine")), Some(p.clone()));
        assert_eq!(find(&env, Path::new("/opt/other/wine")), None);
        let off = |k: &str| (k == ENV_TEMPLATE).then(|| "none".to_string());
        assert_eq!(find(&off, Path::new("/usr/bin/wine")), None);
    }
}
