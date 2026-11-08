//! Icon extraction: RT_GROUP_ICON → best RT_ICON → PNG.

use super::bytes::{u8_at, u16_at, u32_at};
use super::{Pe, RT_GROUP_ICON, RT_ICON, ResName};
use std::io::Cursor;

#[derive(Debug, Clone, PartialEq, Eq)]
struct GroupEntry {
    width: u32,
    height: u32,
    bpp: u16,
    id: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedIcon {
    pub width: u32,
    pub height: u32,
    pub png: Vec<u8>,
}

fn parse_group(d: &[u8]) -> Vec<GroupEntry> {
    let (Some(0), Some(1), Some(count)) = (u16_at(d, 0), u16_at(d, 2), u16_at(d, 4)) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for i in 0..count as usize {
        let e = 6 + i * 14;
        let (Some(w), Some(h), Some(bpp), Some(id)) = (
            u8_at(d, e),
            u8_at(d, e + 1),
            u16_at(d, e + 6),
            u16_at(d, e + 12),
        ) else {
            break;
        };
        let dim = |v: u8| if v == 0 { 256 } else { v as u32 };
        out.push(GroupEntry {
            width: dim(w),
            height: dim(h),
            bpp,
            id,
        });
    }
    out
}

/// Real pixel size of an icon image (PNG header or BITMAPINFOHEADER); the
/// group directory's byte-sized fields can't express >256 and are often
/// wrong in the wild.
fn image_size(img: &[u8]) -> Option<(u32, u32)> {
    if img.starts_with(b"\x89PNG\r\n\x1a\n") {
        let w = u32::from_be_bytes(img.get(16..20)?.try_into().ok()?);
        let h = u32::from_be_bytes(img.get(20..24)?.try_into().ok()?);
        Some((w, h))
    } else {
        let w = u32_at(img, 4)?;
        let h = u32_at(img, 8)? / 2; // DIB height covers XOR + AND masks
        Some((w, h))
    }
}

/// Wrap a single RT_ICON image in an ICO container and decode it to PNG.
fn icon_to_png(img: &[u8], entry: &GroupEntry) -> Option<ExtractedIcon> {
    let mut ico = Vec::with_capacity(img.len() + 22);
    ico.extend_from_slice(&0u16.to_le_bytes());
    ico.extend_from_slice(&1u16.to_le_bytes());
    ico.extend_from_slice(&1u16.to_le_bytes());
    let (w, h) = image_size(img).unwrap_or((entry.width, entry.height));
    ico.push(if w >= 256 { 0 } else { w as u8 });
    ico.push(if h >= 256 { 0 } else { h as u8 });
    ico.push(0);
    ico.push(0);
    ico.extend_from_slice(&1u16.to_le_bytes());
    ico.extend_from_slice(&entry.bpp.to_le_bytes());
    ico.extend_from_slice(&(img.len() as u32).to_le_bytes());
    ico.extend_from_slice(&22u32.to_le_bytes());
    ico.extend_from_slice(img);
    let dir = ico::IconDir::read(Cursor::new(ico)).ok()?;
    let image = dir.entries().first()?.decode().ok()?;
    let mut png = Vec::new();
    image.write_png(&mut png).ok()?;
    Some(ExtractedIcon {
        width: image.width(),
        height: image.height(),
        png,
    })
}

/// The application's main icon (first icon group), largest and deepest
/// image first, as PNG. Falls back to smaller images if the best one fails
/// to decode.
pub fn best_icon_png(pe: &Pe<'_>) -> Option<ExtractedIcon> {
    let groups = pe.resources(RT_GROUP_ICON);
    let group = groups.first()?;
    let icons = pe.resources(RT_ICON);
    let mut entries = parse_group(group.data);
    for e in &mut entries {
        if let Some(r) = icons.iter().find(|r| r.name == ResName::Id(e.id))
            && let Some((w, h)) = image_size(r.data)
            && w > 0
            && w <= 1024
        {
            e.width = w;
            e.height = h;
        }
    }
    entries.sort_by_key(|e| std::cmp::Reverse((e.width, e.bpp)));
    for e in &entries {
        let Some(r) = icons.iter().find(|r| r.name == ResName::Id(e.id)) else {
            continue;
        };
        if let Some(icon) = icon_to_png(r.data, e) {
            return Some(icon);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testpe::{self, PeBuilder, ResKey};

    fn decode_png(png: &[u8]) -> ico::IconImage {
        ico::IconImage::read_png(Cursor::new(png)).unwrap()
    }

    #[test]
    fn picks_largest_png_icon() {
        let bytes = PeBuilder::new(testpe::MACHINE_AMD64)
            .icon_group(
                ResKey::Id(1),
                1,
                &[
                    testpe::bmp_icon(16, [255, 0, 0, 255]),
                    testpe::png_icon(256, [0, 0, 255, 255]),
                    testpe::bmp_icon(48, [0, 255, 0, 255]),
                ],
            )
            .build();
        let pe = Pe::parse(&bytes).unwrap();
        let icon = best_icon_png(&pe).unwrap();
        assert_eq!((icon.width, icon.height), (256, 256));
        let img = decode_png(&icon.png);
        assert_eq!(&img.rgba_data()[..4], &[0, 0, 255, 255]);
    }

    #[test]
    fn decodes_bmp_icons() {
        let bytes = PeBuilder::new(testpe::MACHINE_I386)
            .icon_group(
                ResKey::Name("MAINICON".into()),
                5,
                &[
                    testpe::bmp_icon(32, [10, 20, 30, 255]),
                    testpe::bmp_icon(48, [40, 50, 60, 255]),
                ],
            )
            .build();
        let pe = Pe::parse(&bytes).unwrap();
        let icon = best_icon_png(&pe).unwrap();
        assert_eq!(icon.width, 48);
        assert!(icon.png.starts_with(b"\x89PNG"));
        let img = decode_png(&icon.png);
        assert_eq!(&img.rgba_data()[..4], &[40, 50, 60, 255]);
    }

    #[test]
    fn first_group_wins() {
        // Named groups sort before numbered ones, like Explorer's choice.
        let bytes = PeBuilder::new(testpe::MACHINE_AMD64)
            .icon_group(ResKey::Id(1), 1, &[testpe::bmp_icon(16, [1, 1, 1, 255])])
            .icon_group(
                ResKey::Name("APP".into()),
                10,
                &[testpe::bmp_icon(32, [9, 9, 9, 255])],
            )
            .build();
        let pe = Pe::parse(&bytes).unwrap();
        assert_eq!(best_icon_png(&pe).unwrap().width, 32);
    }

    #[test]
    fn no_icon_and_dangling_ids() {
        let bytes = PeBuilder::new(testpe::MACHINE_AMD64).build();
        assert!(best_icon_png(&Pe::parse(&bytes).unwrap()).is_none());
        // Group references an icon ID that doesn't exist.
        let mut grp = vec![0, 0, 1, 0, 1, 0];
        grp.extend_from_slice(&[32, 32, 0, 0, 1, 0, 32, 0, 0, 1, 0, 0, 99, 0]);
        let bytes = PeBuilder::new(testpe::MACHINE_AMD64)
            .resource(ResKey::Id(testpe::RT_GROUP_ICON), ResKey::Id(1), 0, grp)
            .build();
        assert!(best_icon_png(&Pe::parse(&bytes).unwrap()).is_none());
    }

    #[test]
    fn corrupt_best_image_falls_back() {
        let mut bad = testpe::png_icon(64, [0, 0, 0, 255]);
        let n = bad.len();
        bad[30..n - 12].fill(0); // destroy the IDAT stream
        let bytes = PeBuilder::new(testpe::MACHINE_AMD64)
            .icon_group(
                ResKey::Id(1),
                1,
                &[bad, testpe::bmp_icon(16, [7, 7, 7, 255])],
            )
            .build();
        let icon = best_icon_png(&Pe::parse(&bytes).unwrap()).unwrap();
        assert_eq!(icon.width, 16);
    }
}
