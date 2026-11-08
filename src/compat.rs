//! Compatibility database: per-program tweaks (env, DLL overrides,
//! winetricks verbs, runner preference) and notes, in TOML.

use crate::inspect::Inspection;
use crate::runner::RunnerKind;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Built-in entries shipped inside the binary.
pub const BUILTIN: &[(&str, &str)] = &[
    ("compat/apps.toml", include_str!("../compat/apps.toml")),
    ("compat/games.toml", include_str!("../compat/games.toml")),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Works,
    Partial,
    Broken,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Match {
    #[serde(default)]
    pub sha256: Vec<String>,
    #[serde(default)]
    pub product: Vec<String>,
    #[serde(default)]
    pub exe: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub name: String,
    #[serde(default, rename = "match")]
    pub matcher: Match,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub dll_overrides: BTreeMap<String, String>,
    #[serde(default)]
    pub verbs: Vec<String>,
    #[serde(default)]
    pub runner: Option<RunnerKind>,
    #[serde(default)]
    pub status: Status,
    #[serde(default)]
    pub native_alternative: Option<String>,
    #[serde(default)]
    pub notes: String,
    /// Where this entry was loaded from (not part of the file format).
    #[serde(skip_deserializing, default)]
    pub source: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    #[serde(default)]
    app: Vec<Entry>,
}

/// How an entry matched; earlier variants are stronger.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchKind {
    Sha256,
    Product,
    ExeName,
}

#[derive(Debug, Default, Clone)]
pub struct CompatDb {
    pub entries: Vec<Entry>,
}

fn valid_env_key(k: &str) -> bool {
    let mut chars = k.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_uppercase() || c == '_')
        && chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

/// Normalise a DLL override mode to Wine's short syntax.
pub fn normalize_override(mode: &str) -> Option<String> {
    let mode = mode.trim().to_ascii_lowercase();
    if mode == "disabled" || mode.is_empty() {
        return Some(String::new());
    }
    let parts: Option<Vec<&str>> = mode
        .split(',')
        .map(|p| match p.trim() {
            "n" | "native" => Some("n"),
            "b" | "builtin" => Some("b"),
            _ => None,
        })
        .collect();
    let parts = parts?;
    if parts.len() > 2 || (parts.len() == 2 && parts[0] == parts[1]) {
        return None;
    }
    Some(parts.join(","))
}

fn validate(e: &Entry) -> Result<()> {
    if e.name.trim().is_empty() {
        bail!("entry without a name");
    }
    let m = &e.matcher;
    if m.sha256.is_empty() && m.product.is_empty() && m.exe.is_empty() {
        bail!(
            "'{}': needs at least one of match.sha256 / product / exe",
            e.name
        );
    }
    for h in &m.sha256 {
        if h.len() != 64 || !h.bytes().all(|b| b.is_ascii_hexdigit()) {
            bail!("'{}': bad sha256 '{h}'", e.name);
        }
    }
    for k in e.env.keys() {
        if !valid_env_key(k) {
            bail!("'{}': bad environment variable name '{k}'", e.name);
        }
    }
    for (dll, mode) in &e.dll_overrides {
        if dll.is_empty() || dll.contains([';', '=', ',', ' ']) {
            bail!("'{}': bad DLL name '{dll}'", e.name);
        }
        if normalize_override(mode).is_none() {
            bail!("'{}': bad override mode '{mode}' for {dll}", e.name);
        }
    }
    for v in &e.verbs {
        if v.is_empty() || !v.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
            bail!("'{}': bad winetricks verb '{v}'", e.name);
        }
    }
    Ok(())
}

impl CompatDb {
    pub fn parse(&mut self, source: &str, text: &str) -> Result<()> {
        let file: File = toml::from_str(text).with_context(|| format!("parsing {source}"))?;
        for mut e in file.app {
            validate(&e).with_context(|| format!("in {source}"))?;
            e.source = source.to_string();
            self.entries.push(e);
        }
        Ok(())
    }

    pub fn builtin() -> CompatDb {
        let mut db = CompatDb::default();
        for (name, text) in BUILTIN {
            db.parse(name, text).expect("built-in compat DB is valid");
        }
        db
    }

    /// Built-in entries, then `*.toml` from each directory in order. Later
    /// sources win ties, so user files override system ones. Broken files
    /// are reported and skipped rather than blocking every launch.
    pub fn load(dirs: &[PathBuf]) -> (CompatDb, Vec<String>) {
        let mut db = CompatDb::builtin();
        let mut errors = Vec::new();
        for dir in dirs {
            let Ok(rd) = std::fs::read_dir(dir) else {
                continue;
            };
            let mut files: Vec<PathBuf> = rd
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().is_some_and(|x| x == "toml"))
                .collect();
            files.sort();
            for f in files {
                let res = std::fs::read_to_string(&f)
                    .map_err(anyhow::Error::from)
                    .and_then(|t| {
                        let mut scratch = CompatDb::default();
                        scratch.parse(&f.display().to_string(), &t)?;
                        db.entries.extend(scratch.entries);
                        Ok(())
                    });
                if let Err(e) = res {
                    errors.push(format!("{e:#}"));
                }
            }
        }
        (db, errors)
    }

    pub fn lookup(&self, i: &Inspection) -> Option<(&Entry, MatchKind)> {
        self.lookup_by(&i.sha256, i.product_name(), &i.file_name)
    }

    pub fn lookup_by(
        &self,
        sha256: &str,
        product: Option<&str>,
        file_name: &str,
    ) -> Option<(&Entry, MatchKind)> {
        let eq = |a: &str, b: &str| a.trim().eq_ignore_ascii_case(b.trim());
        let mut best: Option<(&Entry, MatchKind)> = None;
        for e in &self.entries {
            let m = &e.matcher;
            let kind = if m.sha256.iter().any(|h| eq(h, sha256)) {
                Some(MatchKind::Sha256)
            } else if product.is_some_and(|p| {
                m.product
                    .iter()
                    .any(|x| x.trim().to_lowercase() == p.trim().to_lowercase())
            }) {
                Some(MatchKind::Product)
            } else if m.exe.iter().any(|x| eq(x, file_name)) {
                Some(MatchKind::ExeName)
            } else {
                None
            };
            if let Some(k) = kind
                && best.is_none_or(|(_, bk)| k <= bk)
            {
                best = Some((e, k));
            }
        }
        best
    }
}

impl Entry {
    /// `WINEDLLOVERRIDES`-style pairs, normalised.
    pub fn overrides(&self) -> BTreeMap<String, String> {
        self.dll_overrides
            .iter()
            .filter_map(|(k, v)| Some((k.to_ascii_lowercase(), normalize_override(v)?)))
            .collect()
    }
}

/// Standard locations for extra compat files.
pub fn default_dirs(config_home: &Path) -> Vec<PathBuf> {
    vec![
        PathBuf::from("/usr/share/lucent-bridge/compat.d"),
        PathBuf::from("/etc/lucent-bridge/compat.d"),
        config_home.join("lucent-bridge/compat.d"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_db_loads_and_is_valid() {
        let db = CompatDb::builtin();
        assert!(db.entries.len() >= 8);
        assert!(db.entries.iter().any(|e| e.status == Status::Broken));
        assert!(db.entries.iter().any(|e| !e.verbs.is_empty()));
        assert!(db.entries.iter().any(|e| !e.dll_overrides.is_empty()));
        assert!(db.entries.iter().any(|e| !e.env.is_empty()));
    }

    #[test]
    fn match_priority_sha_then_product_then_exe() {
        let mut db = CompatDb::default();
        let sha = "a".repeat(64);
        db.parse(
            "t",
            &format!(
                r#"
                [[app]]
                name = "by-exe"
                match.exe = ["Foo.exe"]
                [[app]]
                name = "by-product"
                match.product = ["Foo App"]
                [[app]]
                name = "by-hash"
                match.sha256 = ["{sha}"]
                "#
            ),
        )
        .unwrap();
        let name = |r: Option<(&Entry, MatchKind)>| r.map(|(e, k)| (e.name.clone(), k));
        assert_eq!(
            name(db.lookup_by(&sha.to_uppercase(), Some("foo app"), "foo.exe")),
            Some(("by-hash".into(), MatchKind::Sha256))
        );
        assert_eq!(
            name(db.lookup_by("0", Some(" FOO APP "), "foo.exe")),
            Some(("by-product".into(), MatchKind::Product))
        );
        assert_eq!(
            name(db.lookup_by("0", None, "FOO.EXE")),
            Some(("by-exe".into(), MatchKind::ExeName))
        );
        assert_eq!(name(db.lookup_by("0", Some("Bar"), "bar.exe")), None);
    }

    #[test]
    fn product_match_is_unicode_case_insensitive() {
        let mut db = CompatDb::default();
        db.parse(
            "t",
            "[[app]]\nname = \"x\"\nmatch.product = [\"Блокнот\"]\n",
        )
        .unwrap();
        assert!(db.lookup_by("", Some("БЛОКНОТ"), "x.exe").is_some());
    }

    #[test]
    fn later_sources_win_ties() {
        let mut db = CompatDb::default();
        db.parse(
            "system",
            "[[app]]\nname = \"sys\"\nmatch.exe = [\"a.exe\"]\n",
        )
        .unwrap();
        db.parse("user", "[[app]]\nname = \"usr\"\nmatch.exe = [\"a.exe\"]\n")
            .unwrap();
        assert_eq!(db.lookup_by("", None, "a.exe").unwrap().0.name, "usr");
    }

    #[test]
    fn validation_rejects_bad_entries() {
        let bad = [
            "[[app]]\nname = \"x\"\n",                           // no matcher
            "[[app]]\nname = \"x\"\nmatch.sha256 = [\"abc\"]\n", // short hash
            "[[app]]\nname = \"x\"\nmatch.exe = [\"a\"]\nenv = { \"a b\" = \"1\" }\n",
            "[[app]]\nname = \"x\"\nmatch.exe = [\"a\"]\ndll_overrides = { d3d9 = \"maybe\" }\n",
            "[[app]]\nname = \"x\"\nmatch.exe = [\"a\"]\nverbs = [\"rm -rf\"]\n",
            "[[app]]\nname = \"x\"\nmatch.exe = [\"a\"]\nrunner = \"vm\"\n",
            "[[app]]\nname = \"x\"\nmatch.exe = [\"a\"]\nunknown_field = 1\n",
        ];
        for b in bad {
            assert!(CompatDb::default().parse("t", b).is_err(), "{b}");
        }
    }

    #[test]
    fn override_normalisation() {
        assert_eq!(normalize_override("native,builtin").as_deref(), Some("n,b"));
        assert_eq!(normalize_override("B").as_deref(), Some("b"));
        assert_eq!(normalize_override("disabled").as_deref(), Some(""));
        assert_eq!(normalize_override("n,n"), None);
        assert_eq!(normalize_override("n,b,n"), None);
        assert_eq!(normalize_override("x"), None);
    }

    #[test]
    fn load_reports_broken_files_and_keeps_going() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("a.toml"),
            "[[app]]\nname=\"mine\"\nmatch.exe=[\"m.exe\"]\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("b.toml"), "this is not toml [").unwrap();
        std::fs::write(dir.path().join("c.txt"), "ignored").unwrap();
        let (db, errors) = CompatDb::load(&[dir.path().to_path_buf(), "/nonexistent".into()]);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("b.toml"));
        let (e, _) = db.lookup_by("", None, "m.exe").unwrap();
        assert!(e.source.ends_with("a.toml"));
        assert!(db.entries.len() > CompatDb::builtin().entries.len());
    }

    #[test]
    fn builtin_examples_match_realistic_inputs() {
        let db = CompatDb::builtin();
        let (e, k) = db
            .lookup_by("", Some("Notepad++"), "npp.8.6.4.Installer.x64.exe")
            .unwrap();
        assert_eq!((e.name.as_str(), k), ("Notepad++", MatchKind::Product));
        let (e, _) = db.lookup_by("", None, "VALORANT.exe").unwrap();
        assert_eq!(e.status, Status::Broken);
        assert!(e.notes.contains("kernel"));
        let (e, _) = db
            .lookup_by("", Some("Adobe Photoshop CS6"), "x.exe")
            .unwrap();
        assert_eq!(e.overrides().get("msxml6").map(String::as_str), Some("n,b"));
    }
}
