//! One traversal policy for initial scans, live rescans, and native watches.
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Default)]
pub(crate) struct ScanScope {
    excluded: HashSet<PathBuf>,
    boundaries: HashSet<PathBuf>,
    error: Option<String>,
}

impl ScanScope {
    pub(crate) fn new(root: &Path) -> Self {
        #[cfg(target_os = "linux")]
        {
            match crate::linux_mounts::mounts() {
                Ok(mounts) if !mounts.is_empty() => Self::from_mounts(root, &mounts),
                result => Self {
                    error: Some(format!(
                        "Unable to read Linux mount information: {}",
                        result
                            .err()
                            .map_or_else(|| "empty mount table".into(), |e| e.to_string())
                    )),
                    ..Self::default()
                },
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = root;
            Self::default()
        }
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn from_mounts(root: &Path, mounts: &[crate::linux_mounts::Mount]) -> Self {
        // Resolve ancestor aliases once, off the UI thread, while keeping scan
        // paths unchanged. The scanner still rejects a directly selected link.
        let absolute = std::path::absolute(root).unwrap_or_else(|_| root.to_owned());
        let canonical = std::fs::canonicalize(&absolute).unwrap_or_else(|_| absolute.clone());
        let containing = mounts
            .iter()
            .filter(|m| canonical.starts_with(&m.mount))
            .max_by_key(|m| m.mount.components().count());
        let Some(containing) = containing else {
            return Self {
                error: Some("The selected path has no Linux mount information".into()),
                ..Self::default()
            };
        };
        if containing.virtual_filesystem() {
            return Self {
                error: Some(format!(
                    "{} is a virtual filesystem ({}), not stored file data. Select a disk or folder instead.",
                    root.display(),
                    containing.filesystem
                )),
                ..Self::default()
            };
        }
        let backing_root = containing
            .root
            .join(canonical.strip_prefix(&containing.mount).unwrap());
        let mut excluded = Vec::new();
        for mount in mounts {
            if mount.mount == canonical || !mount.mount.starts_with(&canonical) {
                continue;
            }
            // A bind mount back into the scanned storage duplicates data and
            // can create a traversal cycle. Keep independent mounted disks.
            let duplicate = (mount.identity == containing.identity
                && (mount.root.starts_with(&backing_root)
                    || backing_root.starts_with(&mount.root)))
                || mounts.iter().any(|parent| {
                    parent.mount != mount.mount
                        && parent.mount.starts_with(&canonical)
                        && mount.mount.starts_with(&parent.mount)
                        && parent.identity == mount.identity
                        && (mount.root.starts_with(&parent.root)
                            || parent.root.starts_with(&mount.root))
                });
            if mount.auxiliary() || duplicate {
                excluded.push(absolute.join(mount.mount.strip_prefix(&canonical).unwrap()));
            }
        }
        Self::excluding(excluded)
    }

    #[cfg(any(target_os = "linux", all(test, unix)))]
    pub(crate) fn excluding(paths: impl IntoIterator<Item = PathBuf>) -> Self {
        let excluded: HashSet<_> = paths.into_iter().collect();
        let boundaries = excluded
            .iter()
            .flat_map(|p| p.ancestors().skip(1).map(Path::to_owned))
            .collect();
        Self {
            excluded,
            boundaries,
            error: None,
        }
    }

    pub(crate) fn allows(&self, path: &Path) -> bool {
        self.error.is_none()
            && (self.excluded.is_empty() || !path.ancestors().any(|p| self.excluded.contains(p)))
    }

    pub(crate) fn validate(&self, path: &Path) -> Result<(), String> {
        if let Some(error) = &self.error {
            return Err(error.clone());
        }
        if !self.allows(path) {
            return Err(format!("{} is excluded from this scan", path.display()));
        }
        Ok(())
    }

    /// Recursive native watches are safe only below the last excluded mount.
    pub(crate) fn partitioned(&self, path: &Path) -> bool {
        self.boundaries.contains(path)
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use crate::linux_mounts::parse_mountinfo;

    #[test]
    fn root_skips_virtual_ram_images_and_recursive_binds_but_keeps_other_disks() {
        let mounts = parse_mountinfo(
            b"1 0 8:1 / / rw - ext4 /dev/a rw\n\
2 1 0:1 / /proc rw - proc proc rw\n\
3 1 0:2 / /tmp rw - tmpfs tmpfs rw\n\
4 1 7:1 / /snap/tool/1 ro - squashfs /dev/loop1 ro\n\
5 1 8:2 / /media/disk rw - ext4 /dev/b rw\n\
6 1 8:1 / /mnt/loop rw - ext4 /dev/a rw\n\
7 5 8:2 / /media/disk/again rw - ext4 /dev/b rw\n\
8 1 8:1 / /home/repeat rw - ext4 /dev/a rw\n\
9 1 0:9 / /var/lib/container/merged rw - overlay overlay rw\n",
        );
        let scope = ScanScope::from_mounts(Path::new("/"), &mounts);
        for path in [
            "/proc/kcore",
            "/tmp/a",
            "/snap/tool/1/file",
            "/mnt/loop/etc",
            "/media/disk/again/file",
        ] {
            assert!(!scope.allows(Path::new(path)), "{path}");
        }
        for path in ["/home/user/proc/file", "/usr/bin", "/media/disk/file"] {
            assert!(scope.allows(Path::new(path)), "{path}");
        }
        assert!(!scope.allows(Path::new("/var/lib/container/merged/file")));
        assert!(
            !ScanScope::from_mounts(Path::new("/home"), &mounts)
                .allows(Path::new("/home/repeat/home/repeat/file"))
        );
        assert!(scope.partitioned(Path::new("/")));
        assert!(scope.partitioned(Path::new("/snap")));
        assert!(scope.partitioned(Path::new("/home")));
        assert!(!scope.partitioned(Path::new("/usr")));
        assert!(ScanScope::from_mounts(Path::new("/tmp"), &mounts).allows(Path::new("/tmp/a")));
        assert!(
            ScanScope::from_mounts(Path::new("/snap/tool/1"), &mounts)
                .allows(Path::new("/snap/tool/1/file"))
        );
        assert!(
            ScanScope::from_mounts(Path::new("/proc/kcore"), &mounts)
                .validate(Path::new("/proc/kcore"))
                .is_err()
        );
    }
}
