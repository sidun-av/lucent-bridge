//! Stable application IDs: `<slug>-<8 hex>`.
//!
//! The slug keeps IDs readable in `list` output and on disk; the hash of the
//! identity (product + company, or file name when there is no version info)
//! keeps two different programs that slug alike apart.

use sha2::{Digest, Sha256};

const MAX_SLUG: usize = 40;

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn translit(c: char) -> Option<&'static str> {
    // Russian/Ukrainian Cyrillic → Latin (simplified GOST-ish), enough for
    // readable slugs. Anything else non-ASCII is dropped.
    Some(match c {
        'а' => "a",
        'б' => "b",
        'в' => "v",
        'г' | 'ґ' => "g",
        'д' => "d",
        'е' | 'ё' | 'є' => "e",
        'ж' => "zh",
        'з' => "z",
        'и' | 'і' => "i",
        'ї' => "yi",
        'й' => "y",
        'к' => "k",
        'л' => "l",
        'м' => "m",
        'н' => "n",
        'о' => "o",
        'п' => "p",
        'р' => "r",
        'с' => "s",
        'т' => "t",
        'у' => "u",
        'ф' => "f",
        'х' => "kh",
        'ц' => "ts",
        'ч' => "ch",
        'ш' => "sh",
        'щ' => "shch",
        'ы' => "y",
        'э' => "e",
        'ю' => "yu",
        'я' => "ya",
        'ъ' | 'ь' => "",
        _ => return None,
    })
}

/// Lowercase ASCII slug: alphanumerics kept, `+` spelled out (Notepad++),
/// everything else collapsed into single dashes.
pub fn slug(name: &str) -> String {
    fn push(s: &str, out: &mut String, dash: &mut bool) {
        if s.is_empty() {
            return;
        }
        if *dash && !out.is_empty() {
            out.push('-');
        }
        *dash = false;
        out.push_str(s);
    }
    let mut out = String::new();
    let mut dash = false;
    for c in name.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            let mut b = [0u8; 4];
            push(c.encode_utf8(&mut b), &mut out, &mut dash);
        } else if c == '+' {
            push("plus", &mut out, &mut dash);
        } else if let Some(t) = translit(c) {
            push(t, &mut out, &mut dash);
        } else {
            dash = true;
        }
    }
    if out.len() > MAX_SLUG {
        out.truncate(MAX_SLUG);
        while out.ends_with('-') {
            out.pop();
        }
    }
    if out.is_empty() {
        out.push_str("app");
    }
    out
}

/// Normalised identity string the hash is computed over.
pub fn identity(product: Option<&str>, company: Option<&str>, file_stem: &str) -> String {
    match product.map(str::trim).filter(|p| !p.is_empty()) {
        Some(p) => format!(
            "product:{}|{}",
            p.to_lowercase(),
            company.unwrap_or("").trim().to_lowercase()
        ),
        None => format!("file:{}", file_stem.trim().to_lowercase()),
    }
}

pub fn app_id(display_name: &str, identity: &str) -> String {
    let digest = Sha256::digest(identity.as_bytes());
    format!("{}-{}", slug(display_name), hex(&digest[..4]))
}

/// App IDs are used as directory names; accept only what we generate.
pub fn is_valid_app_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_SLUG + 9
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !id.starts_with('-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs() {
        assert_eq!(slug("Notepad++"), "notepadplusplus");
        assert_eq!(slug("7-Zip File Manager"), "7-zip-file-manager");
        assert_eq!(slug("  Foo   Bar!! "), "foo-bar");
        assert_eq!(slug("Программа «Ёж»"), "programma-ezh");
        assert_eq!(slug("日本語"), "app");
        assert_eq!(slug(""), "app");
        let long = slug(&"abc ".repeat(30));
        assert!(long.len() <= MAX_SLUG && !long.ends_with('-'));
    }

    #[test]
    fn ids_are_stable_and_distinct() {
        let a = app_id("Foo", &identity(Some("Foo"), Some("Acme"), "foo"));
        let b = app_id("Foo", &identity(Some("foo "), Some("ACME"), "other"));
        let c = app_id("Foo", &identity(Some("Foo"), Some("Other Corp"), "foo"));
        assert_eq!(a, b, "case/whitespace-insensitive, file name ignored");
        assert_ne!(a, c, "different vendor, different prefix");
        assert!(a.starts_with("foo-") && a.len() == "foo-".len() + 8);
        // Known value: guards against accidental algorithm changes, which
        // would orphan every existing prefix.
        assert_eq!(a, app_id("Foo", "product:foo|acme"));
        assert_eq!(&a[4..], &hex(&Sha256::digest(b"product:foo|acme")[..4]));
    }

    #[test]
    fn identity_falls_back_to_file_name() {
        assert_eq!(identity(None, Some("X"), "Game"), "file:game");
        assert_eq!(identity(Some("  "), None, "Game"), "file:game");
    }

    #[test]
    fn validity() {
        assert!(is_valid_app_id("notepadplusplus-1a2b3c4d"));
        assert!(!is_valid_app_id("../etc"));
        assert!(!is_valid_app_id("Foo-1"));
        assert!(!is_valid_app_id(""));
        assert!(!is_valid_app_id("-x"));
    }
}
