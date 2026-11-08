//! Minimal Windows shell-link (.lnk, MS-SHLLINK) reader: just enough to
//! learn what a Start-menu shortcut created by an installer points at.

use crate::pe::bytes::{slice_at, u16_at, u32_at, utf16_n_at};

const HAS_ID_LIST: u32 = 0x01;
const HAS_LINK_INFO: u32 = 0x02;
const HAS_NAME: u32 = 0x04;
const HAS_RELATIVE_PATH: u32 = 0x08;
const HAS_WORKING_DIR: u32 = 0x10;
const HAS_ARGUMENTS: u32 = 0x20;
const HAS_ICON_LOCATION: u32 = 0x40;
const IS_UNICODE: u32 = 0x80;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ShellLink {
    /// Windows path of the target, e.g. `C:\Program Files\Foo\foo.exe`.
    pub target: Option<String>,
    pub relative_path: Option<String>,
    pub description: Option<String>,
    pub working_dir: Option<String>,
    pub arguments: Option<String>,
    pub icon_location: Option<String>,
}

fn cstr(d: &[u8], off: usize) -> Option<String> {
    let rest = d.get(off..)?;
    let end = rest.iter().position(|&b| b == 0)?;
    // ANSI code page is unknown; Latin-1 keeps ASCII paths intact.
    Some(rest[..end].iter().map(|&b| b as char).collect())
}

fn utf16z(d: &[u8], off: usize) -> Option<String> {
    let mut units = Vec::new();
    let mut pos = off;
    loop {
        let u = u16_at(d, pos)?;
        if u == 0 {
            break;
        }
        units.push(u);
        pos += 2;
    }
    Some(String::from_utf16_lossy(&units))
}

pub fn parse(d: &[u8]) -> Option<ShellLink> {
    if u32_at(d, 0)? != 0x4c || slice_at(d, 4, 4)? != [0x01, 0x14, 0x02, 0x00] {
        return None;
    }
    let flags = u32_at(d, 0x14)?;
    let mut pos = 0x4c;
    if flags & HAS_ID_LIST != 0 {
        pos += 2 + u16_at(d, pos)? as usize;
    }
    let mut link = ShellLink::default();
    if flags & HAS_LINK_INFO != 0 {
        let li = pos;
        let size = u32_at(d, li)? as usize;
        let header = u32_at(d, li + 4)? as usize;
        let li_flags = u32_at(d, li + 8)?;
        let info = slice_at(d, li, size)?;
        if li_flags & 1 != 0 {
            let base = if header >= 0x24 {
                u32_at(info, 0x1c)
                    .filter(|&o| o != 0)
                    .and_then(|o| utf16z(info, o as usize))
            } else {
                None
            }
            .or_else(|| cstr(info, u32_at(info, 16)? as usize));
            let suffix = if header >= 0x24 {
                u32_at(info, 0x20)
                    .filter(|&o| o != 0)
                    .and_then(|o| utf16z(info, o as usize))
            } else {
                None
            }
            .or_else(|| cstr(info, u32_at(info, 24)? as usize))
            .unwrap_or_default();
            link.target = base
                .map(|b| format!("{b}{suffix}"))
                .filter(|t| !t.is_empty());
        }
        pos += size;
    }
    let unicode = flags & IS_UNICODE != 0;
    let read_string = |pos: &mut usize| -> Option<String> {
        let count = u16_at(d, *pos)? as usize;
        *pos += 2;
        let s = if unicode {
            let s = utf16_n_at(d, *pos, count)?;
            *pos += count * 2;
            s
        } else {
            let b = slice_at(d, *pos, count)?;
            *pos += count;
            b.iter().map(|&c| c as char).collect()
        };
        Some(s)
    };
    // StringData fields appear in this fixed order when their flag is set.
    for (flag, slot) in [
        (HAS_NAME, &mut link.description),
        (HAS_RELATIVE_PATH, &mut link.relative_path),
        (HAS_WORKING_DIR, &mut link.working_dir),
        (HAS_ARGUMENTS, &mut link.arguments),
        (HAS_ICON_LOCATION, &mut link.icon_location),
    ] {
        if flags & flag != 0 {
            match read_string(&mut pos) {
                // Some writers count a terminating NUL in the length.
                Some(s) => {
                    let s = s.split('\0').next().unwrap_or_default();
                    *slot = Some(s.to_string()).filter(|s| !s.trim().is_empty());
                }
                None => break,
            }
        }
    }
    Some(link)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testpe::lnk_file;

    #[test]
    fn parses_wine_style_link() {
        let d = lnk_file(
            r"C:\Program Files\Foo\foo.exe",
            Some("Foo editor"),
            Some(r"C:\Program Files\Foo"),
            Some("--fast"),
        );
        let l = parse(&d).unwrap();
        assert_eq!(l.target.as_deref(), Some(r"C:\Program Files\Foo\foo.exe"));
        assert_eq!(l.description.as_deref(), Some("Foo editor"));
        assert_eq!(l.working_dir.as_deref(), Some(r"C:\Program Files\Foo"));
        assert_eq!(l.arguments.as_deref(), Some("--fast"));
        assert_eq!(l.icon_location, None);
    }

    #[test]
    fn nul_inside_a_string_ends_it() {
        // Seen for real: KeePass 2.62's Inno Setup shortcut, written by Wine,
        // has a 1-character Arguments field that is a NUL, which became a
        // literal NUL argument in the launcher entry.
        let l = parse(&lnk_file(
            r"C:\Program Files\KeePass Password Safe 2\KeePass.exe",
            Some("KeePass\0junk"),
            None,
            Some("\0"),
        ))
        .unwrap();
        assert_eq!(l.arguments, None);
        assert_eq!(l.description.as_deref(), Some("KeePass"));
    }

    #[test]
    fn minimal_link() {
        let l = parse(&lnk_file(r"C:\x.exe", None, None, None)).unwrap();
        assert_eq!(l.target.as_deref(), Some(r"C:\x.exe"));
        assert_eq!(l.description, None);
    }

    #[test]
    fn rejects_garbage_and_survives_truncation() {
        assert_eq!(parse(b"not a link"), None);
        assert_eq!(parse(&[]), None);
        let d = lnk_file(
            r"C:\Program Files\Foo\foo.exe",
            Some("n"),
            Some("w"),
            Some("a"),
        );
        for cut in 0..d.len() {
            let _ = parse(&d[..cut]);
        }
    }
}
