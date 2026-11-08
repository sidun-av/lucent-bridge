//! VS_VERSIONINFO parsing (ProductName, FileDescription, versions, ...).

use super::bytes::{align4, slice_at, u16_at, u32_at, utf16z_at};
use super::{Pe, RT_VERSION};
use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct VersionInfo {
    /// Fixed file version (from VS_FIXEDFILEINFO), e.g. "8.6.4.0".
    pub file_version: Option<String>,
    pub product_version: Option<String>,
    /// String table entries (ProductName, FileDescription, CompanyName, ...)
    /// from the preferred language table.
    pub strings: BTreeMap<String, String>,
}

impl VersionInfo {
    pub fn from_pe(pe: &Pe<'_>) -> Option<VersionInfo> {
        let res = pe.resources(RT_VERSION);
        // Prefer US English, then language-neutral, then whatever is first.
        let pick = res
            .iter()
            .find(|r| r.lang == 0x0409)
            .or_else(|| res.iter().find(|r| r.lang == 0))
            .or_else(|| res.first())?;
        Some(parse(pick.data))
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.strings
            .get(key)
            .map(String::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
    }

    pub fn product_name(&self) -> Option<&str> {
        self.get("ProductName")
    }

    pub fn file_description(&self) -> Option<&str> {
        self.get("FileDescription")
    }

    pub fn company_name(&self) -> Option<&str> {
        self.get("CompanyName")
    }

    /// Best human-readable version: string ProductVersion, then
    /// FileVersion, then the fixed binary versions.
    pub fn display_version(&self) -> Option<String> {
        self.get("ProductVersion")
            .or_else(|| self.get("FileVersion"))
            .map(str::to_string)
            .or_else(|| self.product_version.clone())
            .or_else(|| self.file_version.clone())
    }
}

struct Node<'a> {
    key: String,
    value: &'a [u8],
    is_text: bool,
    children: Vec<Node<'a>>,
}

/// Parse one node; returns the node and its total aligned length.
fn parse_node(d: &[u8], depth: u8) -> Option<(Node<'_>, usize)> {
    let len = u16_at(d, 0)? as usize;
    let value_len = u16_at(d, 2)? as usize;
    let ty = u16_at(d, 4)?;
    if len < 6 || len > d.len() {
        return None;
    }
    let (key, after_key) = utf16z_at(d, 6, len);
    let value_start = align4(after_key).min(len);
    let is_text = ty == 1;
    let (value, children_start) = if is_text {
        // wValueLength is in characters for text values, but many tools get
        // it wrong; bound the value by the terminator instead.
        if value_len == 0 {
            (&d[value_start..value_start], value_start)
        } else {
            let (_, end) = utf16z_at(d, value_start, len);
            (&d[value_start..end.max(value_start)], align4(end))
        }
    } else {
        let end = (value_start + value_len).min(len);
        (&d[value_start..end], align4(end))
    };
    let mut children = Vec::new();
    if depth < 4 {
        let mut pos = children_start;
        while pos + 6 <= len {
            let Some((child, clen)) = parse_node(&d[pos..len], depth + 1) else {
                break;
            };
            children.push(child);
            pos += align4(clen.max(1));
        }
    }
    Some((
        Node {
            key,
            value,
            is_text,
            children,
        },
        len,
    ))
}

fn text_value(n: &Node<'_>) -> String {
    let (s, _) = utf16z_at(n.value, 0, n.value.len());
    s
}

fn fixed_version(ms: u32, ls: u32) -> String {
    format!("{}.{}.{}.{}", ms >> 16, ms & 0xffff, ls >> 16, ls & 0xffff)
}

pub fn parse(d: &[u8]) -> VersionInfo {
    let mut info = VersionInfo::default();
    let Some((root, _)) = parse_node(d, 0) else {
        return info;
    };
    if let Some(ffi) = slice_at(root.value, 0, 52)
        && u32_at(ffi, 0) == Some(0xfeef_04bd)
    {
        let v = |o| u32_at(ffi, o).unwrap_or(0);
        info.file_version = Some(fixed_version(v(8), v(12)));
        info.product_version = Some(fixed_version(v(16), v(20)));
    }
    for sfi in root.children.iter().filter(|c| c.key == "StringFileInfo") {
        let tables = &sfi.children;
        let table = tables
            .iter()
            .find(|t| t.key.to_ascii_uppercase().starts_with("0409"))
            .or_else(|| tables.iter().find(|t| t.key.starts_with("0000")))
            .or_else(|| tables.first());
        if let Some(t) = table {
            for s in &t.children {
                let v = if s.is_text {
                    text_value(s)
                } else {
                    String::from_utf8_lossy(s.value)
                        .trim_end_matches('\0')
                        .to_string()
                };
                info.strings.insert(s.key.clone(), v);
            }
        }
    }
    info
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testpe::{self, PeBuilder, ResKey};

    #[test]
    fn parses_strings_and_fixed_versions() {
        let bytes = PeBuilder::new(testpe::MACHINE_AMD64)
            .version(
                [8, 6, 4, 0],
                &[
                    ("CompanyName", "Don Ho"),
                    ("FileDescription", "Notepad++"),
                    ("ProductName", "Notepad++"),
                    ("ProductVersion", "8.6.4"),
                ],
            )
            .build();
        let pe = Pe::parse(&bytes).unwrap();
        let v = VersionInfo::from_pe(&pe).unwrap();
        assert_eq!(v.product_name(), Some("Notepad++"));
        assert_eq!(v.company_name(), Some("Don Ho"));
        assert_eq!(v.file_version.as_deref(), Some("8.6.4.0"));
        assert_eq!(v.display_version().as_deref(), Some("8.6.4"));
    }

    #[test]
    fn non_ascii_and_empty_values() {
        let bytes = PeBuilder::new(testpe::MACHINE_I386)
            .version(
                [1, 0, 0, 0],
                &[("ProductName", "Программа «Ёж»"), ("Comments", "")],
            )
            .build();
        let pe = Pe::parse(&bytes).unwrap();
        let v = VersionInfo::from_pe(&pe).unwrap();
        assert_eq!(v.product_name(), Some("Программа «Ёж»"));
        assert_eq!(v.get("Comments"), None);
        assert_eq!(v.display_version().as_deref(), Some("1.0.0.0"));
    }

    #[test]
    fn prefers_english_resource_language() {
        let ru = testpe::version_resource([1, 0, 0, 0], &[("ProductName", "Блокнот")]);
        let en = testpe::version_resource([1, 0, 0, 0], &[("ProductName", "Notepad")]);
        let bytes = PeBuilder::new(testpe::MACHINE_AMD64)
            .resource(ResKey::Id(testpe::RT_VERSION), ResKey::Id(1), 0x0419, ru)
            .resource(ResKey::Id(testpe::RT_VERSION), ResKey::Id(1), 0x0409, en)
            .build();
        let pe = Pe::parse(&bytes).unwrap();
        assert_eq!(
            VersionInfo::from_pe(&pe).unwrap().product_name(),
            Some("Notepad")
        );
    }

    #[test]
    fn missing_version_resource() {
        let bytes = PeBuilder::new(testpe::MACHINE_AMD64).build();
        let pe = Pe::parse(&bytes).unwrap();
        assert!(VersionInfo::from_pe(&pe).is_none());
    }

    #[test]
    fn garbage_blob_is_empty_info() {
        assert_eq!(parse(&[0xff; 40]), VersionInfo::default());
        assert_eq!(parse(&[]), VersionInfo::default());
    }
}
