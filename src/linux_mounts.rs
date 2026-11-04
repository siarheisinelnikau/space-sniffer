//! Linux mount metadata shared by the scanner, watcher, and volume colors.
use std::{ffi::OsString, os::unix::ffi::OsStringExt, path::PathBuf};

#[derive(Clone, Debug)]
pub struct Mount {
    pub mount: PathBuf,
    pub root: PathBuf,
    pub source: String,
    pub filesystem: String,
    pub identity: Vec<u8>,
}

impl Mount {
    /// Kernel interfaces do not describe stored file sizes, even when selected directly.
    pub fn virtual_filesystem(&self) -> bool {
        matches!(
            self.filesystem.as_str(),
            "proc"
                | "sysfs"
                | "devtmpfs"
                | "devpts"
                | "cgroup"
                | "cgroup2"
                | "securityfs"
                | "debugfs"
                | "tracefs"
                | "pstore"
                | "configfs"
                | "fusectl"
                | "mqueue"
                | "hugetlbfs"
                | "autofs"
                | "binfmt_misc"
                | "rpc_pipefs"
                | "nsfs"
                | "efivarfs"
                | "bpf"
                | "fuse.portal"
                | "fuse.gvfsd-fuse"
        )
    }

    /// RAM storage, mounted images and overlays can be scanned explicitly, but are omitted
    /// when traversing a larger disk to avoid counting temporary or duplicate data.
    pub fn auxiliary(&self) -> bool {
        self.virtual_filesystem()
            || matches!(
                self.filesystem.as_str(),
                "tmpfs" | "ramfs" | "squashfs" | "overlay" | "aufs"
            )
    }
}

pub fn mounts() -> std::io::Result<Vec<Mount>> {
    std::fs::read("/proc/self/mountinfo").map(|data| parse_mountinfo(&data))
}

// mountinfo uses octal escapes for whitespace and backslash. Decode bytes,
// not UTF-8, so a non-Unicode mount path still matches the scanner's PathBuf.
fn unescape(field: &[u8]) -> Option<Vec<u8>> {
    let mut result = Vec::with_capacity(field.len());
    let mut i = 0;
    while i < field.len() {
        if field[i] == b'\\' {
            let digits = field.get(i + 1..i + 4)?;
            if !digits.iter().all(|b| (b'0'..=b'7').contains(b)) {
                return None;
            }
            let value = u16::from(digits[0] - b'0') * 64
                + u16::from(digits[1] - b'0') * 8
                + u16::from(digits[2] - b'0');
            let value = u8::try_from(value).ok()?;
            if value == 0 {
                return None;
            }
            result.push(value);
            i += 4;
        } else {
            result.push(field[i]);
            i += 1;
        }
    }
    Some(result)
}

pub fn parse_mountinfo(data: &[u8]) -> Vec<Mount> {
    let mut mounts = Vec::new();
    for line in data.split(|b| *b == b'\n') {
        let fields: Vec<_> = line.split(|b| *b == b' ').collect();
        let Some(separator) = fields.iter().position(|field| *field == b"-") else {
            continue;
        };
        if separator < 6 || fields.len() < separator + 4 {
            continue;
        }
        let device = fields[2];
        let valid_device = device.split(|b| *b == b':').collect::<Vec<_>>();
        if valid_device.len() != 2
            || valid_device
                .iter()
                .any(|s| s.is_empty() || !s.iter().all(u8::is_ascii_digit))
        {
            continue;
        }
        let Some(root) = unescape(fields[3]) else {
            continue;
        };
        let root = PathBuf::from(OsString::from_vec(root));
        if !root.is_absolute() {
            continue;
        }
        let Some(path) = unescape(fields[4]) else {
            continue;
        };
        let mount = PathBuf::from(OsString::from_vec(path));
        if !mount.is_absolute() {
            continue;
        }
        let Some(source) = unescape(fields[separator + 2]) else {
            continue;
        };
        let filesystem = String::from_utf8_lossy(fields[separator + 1]).into_owned();
        let volume = Mount {
            mount,
            root,
            source: String::from_utf8_lossy(&source).into_owned(),
            filesystem,
            identity: device.to_vec(),
        };
        // A later visible mount at the same location replaces the earlier one.
        if let Some(index) = mounts.iter().position(|v: &Mount| v.mount == volume.mount) {
            mounts[index] = volume;
        } else {
            mounts.push(volume);
        }
    }
    mounts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mount_roots_and_paths_decode_kernel_escapes_without_losing_bytes() {
        assert_eq!(unescape(br"a\011b\012c\134d").unwrap(), b"a\tb\nc\\d");
        assert!(unescape(br"bad\000").is_none());
        let mounts = parse_mountinfo(br"1 0 8:1 /a\040b /bind\134alias rw - ext4 /dev/disk rw");
        assert_eq!(mounts.len(), 1);
        assert_eq!(mounts[0].root, PathBuf::from("/a b"));
        assert_eq!(mounts[0].mount, PathBuf::from("/bind\\alias"));
    }
}
