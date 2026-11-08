//! Bounds-checked little-endian readers. Every accessor returns `None`
//! instead of panicking, so malformed or truncated files can never crash
//! the inspector.

pub fn u8_at(d: &[u8], off: usize) -> Option<u8> {
    d.get(off).copied()
}

pub fn u16_at(d: &[u8], off: usize) -> Option<u16> {
    let b = d.get(off..off.checked_add(2)?)?;
    Some(u16::from_le_bytes([b[0], b[1]]))
}

pub fn u32_at(d: &[u8], off: usize) -> Option<u32> {
    let b = d.get(off..off.checked_add(4)?)?;
    Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

pub fn u64_at(d: &[u8], off: usize) -> Option<u64> {
    let b = d.get(off..off.checked_add(8)?)?;
    let mut a = [0u8; 8];
    a.copy_from_slice(b);
    Some(u64::from_le_bytes(a))
}

pub fn slice_at(d: &[u8], off: usize, len: usize) -> Option<&[u8]> {
    d.get(off..off.checked_add(len)?)
}

/// Round `v` up to a multiple of 4 (VS_VERSIONINFO alignment).
pub fn align4(v: usize) -> usize {
    (v + 3) & !3
}

/// Read a NUL-terminated UTF-16LE string starting at `off`, never reading
/// past `end`. Returns the string and the offset just past the terminator
/// (or `end` if no terminator was found).
pub fn utf16z_at(d: &[u8], off: usize, end: usize) -> (String, usize) {
    let end = end.min(d.len());
    let mut units = Vec::new();
    let mut pos = off;
    while pos + 1 < end {
        let u = u16::from_le_bytes([d[pos], d[pos + 1]]);
        pos += 2;
        if u == 0 {
            return (String::from_utf16_lossy(&units), pos);
        }
        units.push(u);
    }
    (String::from_utf16_lossy(&units), end.max(off))
}

/// Decode `count` UTF-16LE code units at `off`.
pub fn utf16_n_at(d: &[u8], off: usize, count: usize) -> Option<String> {
    let b = slice_at(d, off, count.checked_mul(2)?)?;
    let units: Vec<u16> = b
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_le_bytes(*c))
        .collect();
    Some(String::from_utf16_lossy(&units))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readers_are_bounds_checked() {
        let d = [1u8, 2, 3];
        assert_eq!(u16_at(&d, 0), Some(0x0201));
        assert_eq!(u16_at(&d, 2), None);
        assert_eq!(u32_at(&d, 0), None);
        assert_eq!(u64_at(&d, usize::MAX - 1), None);
        assert_eq!(slice_at(&d, 1, 2), Some(&d[1..3]));
        assert_eq!(slice_at(&d, usize::MAX, 2), None);
    }

    #[test]
    fn utf16z_stops_at_nul_and_bound() {
        let d = [b'H', 0, b'i', 0, 0, 0, b'X', 0];
        assert_eq!(utf16z_at(&d, 0, d.len()), ("Hi".to_string(), 6));
        // No terminator before the bound.
        assert_eq!(utf16z_at(&d, 0, 4), ("Hi".to_string(), 4));
    }

    #[test]
    fn align4_rounds_up() {
        assert_eq!(align4(0), 0);
        assert_eq!(align4(1), 4);
        assert_eq!(align4(4), 4);
        assert_eq!(align4(6), 8);
    }
}
