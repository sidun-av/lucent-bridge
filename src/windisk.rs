//! Programs on the disk of a hibernated Windows.
//!
//! Windows with Fast Startup (the default) or hibernation doesn't really
//! shut down: its disk keeps an open session (`hiberfil.sys` starts with
//! "HIBR"/"hibr"). Writing to it from another OS can destroy data, so Lucent
//! mounts Windows disks read-only. A program started from such a disk can't
//! save anything next to itself and usually belongs to that Windows
//! installation; Lucent Bridge explains this instead of letting it fail in
//! strange ways (the user may still run it anyway). Installers are fine:
//! they only read themselves.

use std::path::{Path, PathBuf};

/// Undo the octal escapes of `/proc/self/mountinfo` (`\040` = space).
fn unescape(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\'
            && i + 3 < b.len()
            && b[i + 1..i + 4].iter().all(|c| (b'0'..=b'7').contains(c))
        {
            let v = (b[i + 1] - b'0') * 64 + (b[i + 2] - b'0') * 8 + (b[i + 3] - b'0');
            out.push(v);
            i += 4;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The mount holding `path`: (mount point, file system type).
pub fn mount_of(path: &Path, mountinfo: &str) -> Option<(PathBuf, String)> {
    let mut best: Option<(PathBuf, String)> = None;
    for line in mountinfo.lines() {
        let fields: Vec<&str> = line.split(' ').collect();
        let Some(sep) = fields.iter().position(|f| *f == "-") else {
            continue;
        };
        let (Some(mp), Some(fstype)) = (fields.get(4), fields.get(sep + 1)) else {
            continue;
        };
        let mp = PathBuf::from(unescape(mp));
        if path.starts_with(&mp)
            && best
                .as_ref()
                .is_none_or(|(b, _)| mp.components().count() >= b.components().count())
        {
            best = Some((mp, fstype.to_string()));
        }
    }
    best
}

/// NTFS as Linux mounts it: the ntfs3 kernel driver, or ntfs-3g (FUSE).
pub fn is_ntfs(fstype: &str) -> bool {
    matches!(
        fstype,
        "ntfs" | "ntfs3" | "fuseblk" | "fuse.ntfs-3g" | "ntfs-3g"
    )
}

/// Whether a `hiberfil.sys` header says Windows is hibernated.
pub fn hibernated(header: &[u8]) -> bool {
    header.len() >= 4 && header[..4].eq_ignore_ascii_case(b"hibr")
}

/// The mount point of the hibernated Windows disk `exe` is on, if it is.
pub fn hibernated_disk(exe: &Path) -> Option<PathBuf> {
    let mountinfo = std::fs::read_to_string("/proc/self/mountinfo").ok()?;
    let (mp, fstype) = mount_of(exe, &mountinfo)?;
    if !is_ntfs(&fstype) {
        return None;
    }
    let mut header = [0u8; 4];
    let mut f = std::fs::File::open(mp.join("hiberfil.sys")).ok()?;
    std::io::Read::read_exact(&mut f, &mut header).ok()?;
    hibernated(&header).then_some(mp)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MOUNTINFO: &str = "\
23 1 0:21 / / rw,relatime shared:1 - btrfs /dev/vda3 rw,compress=zstd:1
45 23 0:40 /home /var/home rw,relatime shared:2 - btrfs /dev/vda3 rw
88 23 259:3 / /run/media/anna/Local\\040Disk\\040(C:) ro,nosuid,nodev,relatime shared:50 - ntfs3 /dev/nvme0n1p3 ro,uid=1000
89 23 259:4 / /run/media/anna/Data ro shared:51 - fuseblk /dev/sdb1 ro,user_id=0
";

    #[test]
    fn finds_the_innermost_mount() {
        let (mp, fs) = mount_of(
            Path::new("/run/media/anna/Local Disk (C:)/Program Files/App/app.exe"),
            MOUNTINFO,
        )
        .unwrap();
        assert_eq!(mp, Path::new("/run/media/anna/Local Disk (C:)"));
        assert_eq!(fs, "ntfs3");
        assert!(is_ntfs(&fs));
        let (mp, fs) = mount_of(Path::new("/var/home/anna/Downloads/x.exe"), MOUNTINFO).unwrap();
        assert_eq!(
            (mp.as_path(), fs.as_str()),
            (Path::new("/var/home"), "btrfs")
        );
        assert!(!is_ntfs("btrfs"));
        assert_eq!(
            mount_of(Path::new("/run/media/anna/Data/x.exe"), MOUNTINFO)
                .unwrap()
                .1,
            "fuseblk"
        );
    }

    #[test]
    fn hibernation_signature() {
        assert!(hibernated(b"HIBR\0\0"));
        assert!(hibernated(b"hibr"));
        assert!(!hibernated(b"wake"));
        assert!(!hibernated(b"\0\0\0\0"));
        assert!(!hibernated(b"HI"));
    }
}
