//! freedesktop.org Desktop Entry generation (and the bit of parsing needed
//! to read Wine's own generated entries).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopEntry {
    pub name: String,
    pub comment: Option<String>,
    /// argv; quoted per the spec when rendered.
    pub exec: Vec<String>,
    pub working_dir: Option<PathBuf>,
    pub icon: Option<String>,
    pub terminal: bool,
    pub startup_wm_class: Option<String>,
    pub categories: Vec<String>,
    pub app_id: String,
}

/// Escape a value for a desktop-file `string`/`localestring` field.
fn escape_value(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c => out.push(c),
        }
    }
    out
}

/// Quote one Exec argument per the Desktop Entry spec: arguments with
/// reserved characters go in double quotes, inside which `"`, `` ` ``, `$`
/// and `\` are backslash-escaped; `%` is always doubled (it introduces field
/// codes). The result is then subject to normal value escaping.
pub fn quote_exec_arg(arg: &str) -> String {
    const RESERVED: &[char] = &[
        ' ', '\t', '\n', '"', '\'', '\\', '>', '<', '~', '|', '&', ';', '$', '*', '?', '#', '(',
        ')', '`', '=',
    ];
    let arg = arg.replace('%', "%%");
    if !arg.is_empty() && !arg.contains(RESERVED) {
        return arg;
    }
    let mut q = String::from("\"");
    for c in arg.chars() {
        if matches!(c, '"' | '`' | '$' | '\\') {
            q.push('\\');
        }
        q.push(c);
    }
    q.push('"');
    q
}

impl DesktopEntry {
    pub fn render(&self) -> String {
        let mut s = String::from("[Desktop Entry]\nType=Application\nVersion=1.5\n");
        let mut kv = |k: &str, v: &str| {
            s.push_str(k);
            s.push('=');
            s.push_str(v);
            s.push('\n');
        };
        kv("Name", &escape_value(&self.name));
        // English first, every translation as a localised key: the entry
        // must read right whatever language the desktop runs in later.
        use crate::i18n::{Lang, tr_in};
        kv("GenericName", tr_in(Lang::En, "Windows program"));
        kv("GenericName[ru]", tr_in(Lang::Ru, "Windows program"));
        if let Some(c) = &self.comment {
            kv("Comment", &escape_value(c));
        }
        let exec: Vec<String> = self.exec.iter().map(|a| quote_exec_arg(a)).collect();
        kv("Exec", &escape_value(&exec.join(" ")));
        if let Some(p) = &self.working_dir {
            kv("Path", &escape_value(&p.to_string_lossy()));
        }
        if let Some(i) = &self.icon {
            kv("Icon", &escape_value(i));
        }
        kv("Terminal", if self.terminal { "true" } else { "false" });
        if let Some(w) = &self.startup_wm_class {
            kv("StartupWMClass", &escape_value(w));
        }
        let mut cats = self.categories.clone();
        if cats.is_empty() {
            cats.push("Utility".into());
        }
        kv("Categories", &format!("{};", cats.join(";")));
        kv("X-LucentBridge-AppId", &self.app_id);
        s
    }
}

/// Parse the `[Desktop Entry]` group of a desktop file into raw (unescaped)
/// key/value pairs. Localised keys are ignored.
pub fn parse_group(text: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let mut in_group = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_group = line == "[Desktop Entry]";
            continue;
        }
        if !in_group || line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            let k = k.trim();
            if !k.contains('[') {
                out.insert(k.to_string(), unescape_value(v.trim()));
            }
        }
    }
    out
}

fn unescape_value(v: &str) -> String {
    let mut out = String::new();
    let mut it = v.chars();
    while let Some(c) = it.next() {
        if c == '\\' {
            match it.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some('s') => out.push(' '),
                Some(o) => out.push(o),
                None => {}
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Split an (already value-unescaped) Exec line into argv per the spec's
/// quoting rules. Outside quotes a backslash also escapes the next
/// character, which is how Wine's own entries write `Start\ Menu`.
pub fn split_exec(exec: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut cur = String::new();
    let mut has = false;
    let mut in_q = false;
    let mut it = exec.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '"' => {
                in_q = !in_q;
                has = true;
            }
            '\\' => {
                if let Some(n) = it.next() {
                    cur.push(n);
                }
            }
            ' ' | '\t' if !in_q => {
                if has || !cur.is_empty() {
                    args.push(std::mem::take(&mut cur));
                    has = false;
                }
            }
            c => cur.push(c),
        }
    }
    if has || !cur.is_empty() {
        args.push(cur);
    }
    args
}

/// File name we use for a generated entry.
pub fn file_name(app_id: &str, entry_slug: &str) -> String {
    format!(
        "{}{app_id}-{entry_slug}.desktop",
        crate::prefix::DESKTOP_PREFIX
    )
}

pub fn write(dir: &Path, name: &str, entry: &DesktopEntry) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join(name);
    let tmp = dir.join(format!(".{name}.tmp"));
    std::fs::write(&tmp, entry.render())?;
    std::fs::rename(&tmp, &path)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry() -> DesktopEntry {
        DesktopEntry {
            name: "Foo Editor".into(),
            comment: Some("Edits foo\nfiles".into()),
            exec: vec![
                "lucent-bridge".into(),
                "run".into(),
                "/home/u/.local/share/lucent-bridge/prefixes/foo-1/drive_c/Program Files/Foo/foo.exe"
                    .into(),
                "--".into(),
                "100%".into(),
            ],
            working_dir: Some("/home/u/x y".into()),
            icon: Some("/home/u/.local/share/lucent-bridge/icons/foo-1/foo.png".into()),
            terminal: false,
            startup_wm_class: Some("foo.exe".into()),
            categories: vec![],
            app_id: "foo-1".into(),
        }
    }

    #[test]
    fn render_is_spec_shaped() {
        let r = entry().render();
        assert!(r.starts_with("[Desktop Entry]\nType=Application\n"));
        assert!(r.contains("Name=Foo Editor\n"));
        // English by default plus a Russian translation, whatever LANG is.
        let ru = crate::i18n::with_lang(crate::i18n::Lang::Ru, || entry().render());
        assert_eq!(
            ru, r,
            "entries don't depend on the creating process's locale"
        );
        assert!(r.contains("GenericName=Windows program\nGenericName[ru]=Программа Windows\n"));
        assert!(r.contains("Comment=Edits foo\\nfiles\n"));
        assert!(r.contains(
            "Exec=lucent-bridge run \"/home/u/.local/share/lucent-bridge/prefixes/foo-1/drive_c/Program Files/Foo/foo.exe\" -- 100%%\n"
        ));
        assert!(r.contains("Terminal=false\n"));
        assert!(r.contains("StartupWMClass=foo.exe\n"));
        assert!(r.contains("Categories=Utility;\n"));
        assert!(r.contains("X-LucentBridge-AppId=foo-1\n"));
    }

    #[test]
    fn exec_quoting() {
        assert_eq!(quote_exec_arg("plain"), "plain");
        assert_eq!(quote_exec_arg(""), "\"\"");
        assert_eq!(quote_exec_arg("a b"), "\"a b\"");
        assert_eq!(quote_exec_arg("50%"), "50%%");
        assert_eq!(
            quote_exec_arg(r#"say "hi" $USER"#),
            r#""say \"hi\" \$USER""#
        );
        assert_eq!(quote_exec_arg(r"C:\dir"), r#""C:\\dir""#);
    }

    #[test]
    fn exec_roundtrip_through_file_escaping() {
        let tricky = [
            "/p/with space/x.exe",
            r"C:\Program Files\X",
            "quote\"inside",
            "dollar$and`tick",
            "percent%",
            "",
        ];
        for t in tricky {
            let e = DesktopEntry {
                exec: vec!["lucent-bridge".into(), "run".into(), t.into()],
                ..entry()
            };
            let parsed = parse_group(&e.render());
            let argv = split_exec(&parsed["Exec"]);
            let want = t.replace('%', "%%");
            assert_eq!(argv, vec!["lucent-bridge", "run", want.as_str()], "{t:?}");
        }
    }

    #[test]
    fn parse_wine_generated_entry() {
        let text = r#"[Desktop Entry]
Name=Foo
Name[ru]=Фу
Exec=env WINEPREFIX="/home/u/.local/share/lucent-bridge/prefixes/foo-1" wine C:\\\\users\\\\Public\\\\Desktop\\\\Foo.lnk
Type=Application
StartupNotify=true
Icon=1A2B_foo.0
[Desktop Action Other]
Name=Ignored
"#;
        let g = parse_group(text);
        assert_eq!(g["Name"], "Foo");
        assert_eq!(g["Icon"], "1A2B_foo.0");
        let argv = split_exec(&g["Exec"]);
        assert_eq!(argv[0], "env");
        assert_eq!(
            argv[1],
            "WINEPREFIX=/home/u/.local/share/lucent-bridge/prefixes/foo-1"
        );
        assert_eq!(argv[3], r"C:\users\Public\Desktop\Foo.lnk");
        let g = parse_group(
            "[Desktop Entry]\nExec=env WINEPREFIX=\"/p\" wine C:\\\\\\\\windows\\\\\\\\command\\\\\\\\start.exe /Unix /p/dosdevices/c:/ProgramData/Microsoft/Windows/Start\\\\ Menu/Programs/Foo.lnk\n",
        );
        let argv = split_exec(&g["Exec"]);
        assert_eq!(argv[3], r"C:\windows\command\start.exe");
        assert_eq!(
            argv[5],
            "/p/dosdevices/c:/ProgramData/Microsoft/Windows/Start Menu/Programs/Foo.lnk"
        );
    }

    #[test]
    fn write_atomically() {
        let tmp = tempfile::tempdir().unwrap();
        let p = write(tmp.path(), &file_name("foo-1", "foo"), &entry()).unwrap();
        assert_eq!(p.file_name().unwrap(), "lucent-bridge-foo-1-foo.desktop");
        assert_eq!(std::fs::read_to_string(&p).unwrap(), entry().render());
        assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 1);
    }
}
