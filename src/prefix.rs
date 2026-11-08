//! Per-application Wine prefixes and their metadata.
//!
//! Layout (XDG):
//! ```text
//! $XDG_DATA_HOME/lucent-bridge/prefixes/<app-id>/          Wine prefix
//! $XDG_DATA_HOME/lucent-bridge/prefixes/<app-id>/lucent-bridge.toml
//! $XDG_DATA_HOME/lucent-bridge/icons/<app-id>/*.png
//! $XDG_DATA_HOME/applications/lucent-bridge-<app-id>-*.desktop
//! ```

use crate::appid;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::{Component, Path, PathBuf};

pub const METADATA_FILE: &str = "lucent-bridge.toml";
pub const DESKTOP_PREFIX: &str = "lucent-bridge-";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dirs {
    pub data_home: PathBuf,
    pub config_home: PathBuf,
}

impl Dirs {
    /// From `XDG_DATA_HOME` / `XDG_CONFIG_HOME` / `HOME`, per the XDG base
    /// directory spec (relative values are ignored).
    pub fn from_env(get: impl Fn(&str) -> Option<String>) -> Result<Dirs> {
        let abs = |k: &str| get(k).map(PathBuf::from).filter(|p| p.is_absolute());
        let home = abs("HOME");
        let data_home = abs("XDG_DATA_HOME")
            .or_else(|| home.as_ref().map(|h| h.join(".local/share")))
            .context("neither XDG_DATA_HOME nor HOME is set")?;
        let config_home = abs("XDG_CONFIG_HOME")
            .or_else(|| home.as_ref().map(|h| h.join(".config")))
            .context("neither XDG_CONFIG_HOME nor HOME is set")?;
        Ok(Dirs {
            data_home,
            config_home,
        })
    }

    pub fn root(&self) -> PathBuf {
        self.data_home.join("lucent-bridge")
    }

    pub fn prefixes(&self) -> PathBuf {
        self.root().join("prefixes")
    }

    pub fn prefix(&self, app_id: &str) -> PathBuf {
        self.prefixes().join(app_id)
    }

    pub fn icons(&self, app_id: &str) -> PathBuf {
        self.root().join("icons").join(app_id)
    }

    pub fn applications(&self) -> PathBuf {
        self.data_home.join("applications")
    }

    /// If `exe` lives inside one of our prefixes, the app-id of that prefix.
    /// This is how a launcher entry for an installed program finds the
    /// prefix its installer created.
    pub fn owning_app_id(&self, exe: &Path) -> Option<String> {
        // Fedora Atomic links /home to /var/home, so compare both spellings.
        let roots = [
            Some(self.prefixes()),
            std::fs::canonicalize(self.prefixes()).ok(),
        ];
        let rel = roots
            .iter()
            .flatten()
            .find_map(|r| exe.strip_prefix(r).ok())?;
        match rel.components().next()? {
            Component::Normal(id) => {
                let id = id.to_str()?;
                appid::is_valid_app_id(id).then(|| id.to_string())
            }
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Metadata {
    pub app_id: String,
    pub name: String,
    /// The program that created the prefix (often an installer).
    pub source: String,
    pub runner: String,
    /// Seconds since the Unix epoch.
    pub created: u64,
    #[serde(default)]
    pub verbs_applied: Vec<String>,
    #[serde(default)]
    pub desktop_entries: Vec<PathBuf>,
    #[serde(default)]
    pub icons: Vec<PathBuf>,
}

impl Metadata {
    pub fn path(prefix: &Path) -> PathBuf {
        prefix.join(METADATA_FILE)
    }

    pub fn load(prefix: &Path) -> Result<Option<Metadata>> {
        let p = Self::path(prefix);
        match std::fs::read_to_string(&p) {
            Ok(t) => Ok(Some(
                toml::from_str(&t).with_context(|| format!("parsing {}", p.display()))?,
            )),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).with_context(|| format!("reading {}", p.display())),
        }
    }

    pub fn save(&self, prefix: &Path) -> Result<()> {
        let p = Self::path(prefix);
        let tmp = p.with_extension("toml.tmp");
        std::fs::write(&tmp, toml::to_string_pretty(self)?)
            .with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, &p).with_context(|| format!("writing {}", p.display()))?;
        Ok(())
    }
}

pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// All prefixes with metadata, sorted by app-id.
pub fn list(dirs: &Dirs) -> Vec<(PathBuf, Metadata)> {
    let Ok(rd) = std::fs::read_dir(dirs.prefixes()) else {
        return Vec::new();
    };
    let mut out: Vec<(PathBuf, Metadata)> = rd
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .filter_map(|p| {
            let m = Metadata::load(&p).ok()??;
            Some((p, m))
        })
        .collect();
    out.sort_by(|a, b| a.1.app_id.cmp(&b.1.app_id));
    out
}

/// What `remove` would delete, so it can be shown before confirming.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Removal {
    pub prefix: PathBuf,
    pub files: Vec<PathBuf>,
    pub icon_dir: PathBuf,
}

pub fn plan_removal(dirs: &Dirs, app_id: &str) -> Result<Removal> {
    if !appid::is_valid_app_id(app_id) {
        bail!(
            "{}",
            crate::t!(
                "'{}' is not a valid app ID (see `lucent-bridge list`)",
                app_id
            )
        );
    }
    let prefix = dirs.prefix(app_id);
    if !prefix.is_dir() {
        bail!(
            "{}",
            crate::t!("no app with ID '{}' (see `lucent-bridge list`)", app_id)
        );
    }
    let meta = Metadata::load(&prefix)?.unwrap_or_default();
    let apps = dirs.applications();
    let ours = format!("{DESKTOP_PREFIX}{app_id}");
    // Only ever delete launcher files we could have written ourselves.
    let mut files: Vec<PathBuf> = meta
        .desktop_entries
        .into_iter()
        .filter(|p| {
            p.parent() == Some(apps.as_path())
                && p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with(&ours) && n.ends_with(".desktop"))
        })
        .collect();
    // Also catch entries whose metadata was lost.
    if let Ok(rd) = std::fs::read_dir(&apps) {
        for e in rd.filter_map(|e| e.ok()) {
            let p = e.path();
            if p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(&format!("{ours}-")) && n.ends_with(".desktop"))
                && !files.contains(&p)
            {
                files.push(p);
            }
        }
    }
    files.sort();
    Ok(Removal {
        prefix,
        files,
        icon_dir: dirs.icons(app_id),
    })
}

pub fn execute_removal(r: &Removal) -> Result<()> {
    for f in &r.files {
        match std::fs::remove_file(f) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).with_context(|| format!("removing {}", f.display())),
        }
    }
    if r.icon_dir.exists() {
        std::fs::remove_dir_all(&r.icon_dir)
            .with_context(|| format!("removing {}", r.icon_dir.display()))?;
    }
    std::fs::remove_dir_all(&r.prefix)
        .with_context(|| format!("removing {}", r.prefix.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dirs(root: &Path) -> Dirs {
        Dirs {
            data_home: root.join("share"),
            config_home: root.join("config"),
        }
    }

    #[test]
    fn xdg_resolution() {
        let env = |k: &str| match k {
            "HOME" => Some("/home/u".to_string()),
            "XDG_DATA_HOME" => Some("relative/ignored".to_string()),
            _ => None,
        };
        let d = Dirs::from_env(env).unwrap();
        assert_eq!(d.data_home, PathBuf::from("/home/u/.local/share"));
        assert_eq!(d.config_home, PathBuf::from("/home/u/.config"));
        assert_eq!(
            d.prefix("foo-12345678"),
            PathBuf::from("/home/u/.local/share/lucent-bridge/prefixes/foo-12345678")
        );
        let env = |k: &str| (k == "XDG_DATA_HOME").then(|| "/data".to_string());
        assert!(Dirs::from_env(env).is_err(), "no config home");
        assert!(Dirs::from_env(|_| None).is_err());
    }

    #[test]
    fn owning_app_id() {
        let d = dirs(Path::new("/r"));
        let exe = d
            .prefix("foo-12345678")
            .join("drive_c/Program Files/Foo/foo.exe");
        assert_eq!(d.owning_app_id(&exe).as_deref(), Some("foo-12345678"));
        assert_eq!(
            d.owning_app_id(Path::new("/home/u/Downloads/foo.exe")),
            None
        );
        assert_eq!(d.owning_app_id(&d.prefixes().join("BAD ID/x.exe")), None);
    }

    #[test]
    fn metadata_roundtrip_and_list() {
        let tmp = tempfile::tempdir().unwrap();
        let d = dirs(tmp.path());
        assert!(list(&d).is_empty());
        for id in ["b-00000002", "a-00000001"] {
            let p = d.prefix(id);
            std::fs::create_dir_all(&p).unwrap();
            assert_eq!(Metadata::load(&p).unwrap(), None);
            let m = Metadata {
                app_id: id.into(),
                name: "Thing".into(),
                runner: "wine".into(),
                verbs_applied: vec!["corefonts".into()],
                ..Default::default()
            };
            m.save(&p).unwrap();
            assert_eq!(Metadata::load(&p).unwrap(), Some(m));
        }
        // A directory without metadata is not listed.
        std::fs::create_dir_all(d.prefix("c-00000003")).unwrap();
        let ids: Vec<String> = list(&d).into_iter().map(|(_, m)| m.app_id).collect();
        assert_eq!(ids, ["a-00000001", "b-00000002"]);
    }

    #[test]
    fn removal_only_touches_our_files() {
        let tmp = tempfile::tempdir().unwrap();
        let d = dirs(tmp.path());
        let id = "foo-12345678";
        let p = d.prefix(id);
        std::fs::create_dir_all(p.join("drive_c")).unwrap();
        let apps = d.applications();
        std::fs::create_dir_all(&apps).unwrap();
        let ours = apps.join(format!("lucent-bridge-{id}-foo.desktop"));
        let orphan = apps.join(format!("lucent-bridge-{id}-bar.desktop"));
        let other_app = apps.join("lucent-bridge-foo-99999999-x.desktop");
        let foreign = apps.join("firefox.desktop");
        for f in [&ours, &orphan, &other_app, &foreign] {
            std::fs::write(f, "[Desktop Entry]\n").unwrap();
        }
        std::fs::create_dir_all(d.icons(id)).unwrap();
        Metadata {
            app_id: id.into(),
            // A tampered path outside the applications dir must be ignored.
            desktop_entries: vec![ours.clone(), PathBuf::from("/etc/passwd")],
            ..Default::default()
        }
        .save(&p)
        .unwrap();
        let r = plan_removal(&d, id).unwrap();
        assert_eq!(r.files, vec![orphan.clone(), ours.clone()]);
        execute_removal(&r).unwrap();
        assert!(!p.exists() && !ours.exists() && !orphan.exists());
        assert!(!d.icons(id).exists());
        assert!(other_app.exists() && foreign.exists());
    }

    #[test]
    fn removal_rejects_bad_ids() {
        let tmp = tempfile::tempdir().unwrap();
        let d = dirs(tmp.path());
        assert!(plan_removal(&d, "../../etc").is_err());
        assert!(plan_removal(&d, "missing-12345678").is_err());
    }
}
