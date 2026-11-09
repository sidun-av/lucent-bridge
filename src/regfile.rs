//! Editing Wine's registry files (`user.reg`, `system.reg`) as text, while
//! no `wineserver` has the prefix open (the server loads them when it
//! starts and writes them back when it exits).
//!
//! The format, as Wine writes it:
//!
//! ```text
//! WINE REGISTRY Version 2
//! ;; All keys relative to \\User\\S-1-5-21-0-0-0-1000
//!
//! [Control Panel\\Colors] 1731150000
//! #time=1db32a3c1f4a8e0
//! "ActiveBorder"="212 208 200"
//! "Name"=dword:00000060
//! "Big"=hex:01,02,\
//!   03
//! ```
//!
//! A key's section runs from its `[...]` line to the next blank line. Key
//! and value names compare case-insensitively, as in Windows. Only the
//! values asked for are touched; everything else is kept byte for byte.

/// A registry value to write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    /// REG_SZ
    Str(String),
    /// REG_DWORD
    Dword(u32),
    /// REG_BINARY
    Binary(Vec<u8>),
    /// REG_MULTI_SZ
    MultiStr(Vec<String>),
}

/// Escape a string the way Wine writes it into its `.reg` files.
pub fn escape(s: &str) -> String {
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

impl Value {
    /// The text after `"Name"=`.
    pub fn render(&self) -> String {
        match self {
            Value::Str(s) => format!("\"{}\"", escape(s)),
            Value::Dword(d) => format!("dword:{d:08x}"),
            Value::Binary(b) => {
                let hex: Vec<String> = b.iter().map(|x| format!("{x:02x}")).collect();
                format!("hex:{}", hex.join(","))
            }
            Value::MultiStr(items) => {
                let mut s = String::new();
                for i in items {
                    s.push_str(&escape(i));
                    s.push_str("\\0");
                }
                format!("str(7):\"{s}\"")
            }
        }
    }
}

/// The unescaped name in a `"name"=...` line (or `@` for the default
/// value), if the line starts a value.
fn value_name(line: &str) -> Option<String> {
    if line.starts_with("@=") {
        return Some("@".into());
    }
    let rest = line.strip_prefix('"')?;
    let mut name = String::new();
    let mut chars = rest.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => name.push(chars.next()?),
            '"' => {
                return chars.next().filter(|&c| c == '=').map(|_| name);
            }
            c => name.push(c),
        }
    }
    None
}

/// The key name in a section header line `[Key\\Name] 1234`.
fn section_name(line: &str) -> Option<String> {
    let rest = line.strip_prefix('[')?;
    let end = rest.rfind(']')?;
    Some(rest[..end].replace("\\\\", "\\"))
}

/// Set `values` under `key` (written with single backslashes, e.g.
/// `Control Panel\Colors`) in a registry file's text. A missing key is
/// appended as a new section stamped `now` (seconds since 1970). Returns
/// the new text; it equals the input when nothing changed.
pub fn set_values(text: &str, key: &str, values: &[(&str, Value)], now: u64) -> String {
    let lines: Vec<&str> = text.split('\n').collect();
    let header = lines
        .iter()
        .position(|l| section_name(l).is_some_and(|n| n.eq_ignore_ascii_case(key)));
    let Some(header) = header else {
        let mut out = text.to_string();
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        if !out.is_empty() && !out.ends_with("\n\n") {
            out.push('\n');
        }
        out.push_str(&format!("[{}] {now}\n", key.replace('\\', "\\\\")));
        // FILETIME: 100 ns ticks since 1601.
        let filetime = (now + 11_644_473_600) * 10_000_000;
        out.push_str(&format!("#time={filetime:x}\n"));
        for (name, v) in values {
            out.push_str(&format!("\"{}\"={}\n", escape(name), v.render()));
        }
        return out;
    };
    // The section body: from the line after the header to the first blank
    // line (or the next header, or the end).
    let mut end = header + 1;
    while end < lines.len() && !lines[end].is_empty() && !lines[end].starts_with('[') {
        end += 1;
    }
    let mut body: Vec<String> = Vec::new();
    let mut done = vec![false; values.len()];
    let mut i = header + 1;
    while i < end {
        let line = lines[i];
        // A value's continuation lines (long hex) belong to it.
        let mut last = i;
        while lines[last].ends_with('\\') && last + 1 < end {
            last += 1;
        }
        let name = value_name(line);
        let hit = name.as_ref().and_then(|n| {
            values
                .iter()
                .position(|(want, _)| want.eq_ignore_ascii_case(n))
        });
        match hit {
            Some(k) if !done[k] => {
                done[k] = true;
                body.push(format!(
                    "\"{}\"={}",
                    escape(name.as_deref().unwrap_or(values[k].0)),
                    values[k].1.render()
                ));
            }
            Some(_) => {} // a duplicate of a value just written: drop it
            None => body.extend(lines[i..=last].iter().map(|s| s.to_string())),
        }
        i = last + 1;
    }
    for (k, (name, v)) in values.iter().enumerate() {
        if !done[k] {
            body.push(format!("\"{}\"={}", escape(name), v.render()));
        }
    }
    let mut out: Vec<String> = lines[..=header].iter().map(|s| s.to_string()).collect();
    out.extend(body);
    out.extend(lines[end..].iter().map(|s| s.to_string()));
    out.join("\n")
}

/// The raw text after `"name"=` under `key`, if present.
pub fn get_raw(text: &str, key: &str, name: &str) -> Option<String> {
    let mut in_key = false;
    for line in text.split('\n') {
        if let Some(n) = section_name(line) {
            in_key = n.eq_ignore_ascii_case(key);
            continue;
        }
        if in_key {
            if line.is_empty() {
                in_key = false;
                continue;
            }
            if value_name(line).is_some_and(|n| n.eq_ignore_ascii_case(name)) {
                return line.split_once('=').map(|(_, v)| v.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const USER_REG: &str = "WINE REGISTRY Version 2\n\
;; All keys relative to \\\\User\\\\S-1-5-21-0-0-0-1000\n\
\n\
#arch=win64\n\
\n\
[Control Panel\\\\Colors] 1731150000\n\
#time=1db32a3c1f4a8e0\n\
\"ActiveBorder\"=\"212 208 200\"\n\
\"Window\"=\"255 255 255\"\n\
\n\
[Control Panel\\\\Desktop] 1731150000\n\
#time=1db32a3c1f4a8e0\n\
\"UserPreferencesMask\"=hex:9e,1e,07,80,\\\n  12,00,00,00\n\
\"fontsmoothing\"=\"0\"\n\
\n\
[Software\\\\Wine] 1731150000\n\
\"Version\"=\"win10\"\n";

    #[test]
    fn replaces_values_case_insensitively_and_keeps_the_rest() {
        let out = set_values(
            USER_REG,
            "Control Panel\\Desktop",
            &[
                ("FontSmoothing", Value::Str("2".into())),
                ("LogPixels", Value::Dword(144)),
            ],
            1,
        );
        assert!(out.contains("\"fontsmoothing\"=\"2\"\n"), "{out}");
        assert!(out.contains("\"LogPixels\"=dword:00000090\n"), "{out}");
        // The multi-line binary value is untouched.
        assert!(
            out.contains("\"UserPreferencesMask\"=hex:9e,1e,07,80,\\\n  12,00,00,00\n"),
            "{out}"
        );
        // Other keys untouched, sections still separated by blank lines.
        assert!(out.contains("\"Window\"=\"255 255 255\"\n\n[Control Panel"));
        assert!(out.ends_with("\"Version\"=\"win10\"\n"), "{out}");
        assert_eq!(
            get_raw(&out, "control panel\\desktop", "LOGPIXELS").as_deref(),
            Some("dword:00000090")
        );
    }

    #[test]
    fn idempotent() {
        let v = [("Window", Value::Str("253 253 255".into()))];
        let once = set_values(USER_REG, "Control Panel\\Colors", &v, 1);
        let twice = set_values(&once, "Control Panel\\Colors", &v, 2);
        assert_eq!(once, twice);
        assert_ne!(once, USER_REG);
        // Writing what is already there changes nothing at all.
        let same = set_values(
            USER_REG,
            "Control Panel\\Colors",
            &[("Window", Value::Str("255 255 255".into()))],
            3,
        );
        assert_eq!(same, USER_REG);
    }

    #[test]
    fn a_missing_key_becomes_a_new_section() {
        let out = set_values(
            USER_REG,
            "Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize",
            &[("AppsUseLightTheme", Value::Dword(0))],
            1_700_000_000,
        );
        assert!(out.contains(
            "\"Version\"=\"win10\"\n\n[Software\\\\Microsoft\\\\Windows\\\\CurrentVersion\\\\Themes\\\\Personalize] 1700000000\n#time="
        ), "{out}");
        assert!(
            out.ends_with("\"AppsUseLightTheme\"=dword:00000000\n"),
            "{out}"
        );
        assert_eq!(
            get_raw(
                &out,
                "Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize",
                "AppsUseLightTheme"
            )
            .as_deref(),
            Some("dword:00000000")
        );
    }

    #[test]
    fn renders_every_type() {
        assert_eq!(
            Value::Str("C:\\a \"b\" é".into()).render(),
            "\"C:\\\\a \\\"b\\\" \\x00e9\""
        );
        assert_eq!(Value::Dword(0x60).render(), "dword:00000060");
        assert_eq!(
            Value::Binary(vec![0xf4, 0xff, 0, 1]).render(),
            "hex:f4,ff,00,01"
        );
        assert_eq!(
            Value::MultiStr(vec!["tahoma.ttf,Tahoma".into()]).render(),
            "str(7):\"tahoma.ttf,Tahoma\\0\""
        );
    }

    #[test]
    fn names_with_escapes_and_defaults() {
        assert_eq!(value_name("\"a\\\"b\"=\"x\"").as_deref(), Some("a\"b"));
        assert_eq!(value_name("@=\"x\"").as_deref(), Some("@"));
        assert_eq!(value_name("#time=1"), None);
        assert_eq!(value_name("  12,00"), None);
    }
}
