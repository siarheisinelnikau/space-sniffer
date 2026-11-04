//! Desktop adaptation of the mount table shared with filesystem traversal.
use super::volumes::Volume;
use space_sniffer::linux_mounts::{self, Mount};

pub(super) fn mounts() -> Vec<Volume> {
    linux_mounts::mounts()
        .unwrap_or_default()
        .into_iter()
        .map(volume)
        .collect()
}

fn volume(mount: Mount) -> Volume {
    let shortcut = mount.mount == std::path::Path::new("/") || !mount.auxiliary();
    Volume {
        mount: mount.mount,
        source: mount.source,
        filesystem: mount.filesystem,
        identity: mount.identity,
        shortcut,
        color: 0,
    }
}

#[cfg(test)]
fn parse_mountinfo(data: &[u8]) -> Vec<Volume> {
    linux_mounts::parse_mountinfo(data)
        .into_iter()
        .map(volume)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::super::volumes::Volumes;
    use super::*;
    use std::path::Path;
    use std::{ffi::OsString, os::unix::ffi::OsStringExt, path::PathBuf};

    #[test]
    fn parses_optional_fields_escapes_network_and_virtual_mounts() {
        let data = b"20 1 8:1 / / rw shared:1 - ext4 /dev/sda1 rw\n\
21 20 8:2 / /media/My\\040Disk rw master:2 shared:3 - ext4 /dev/sdb1 rw\n\
22 20 8:2 /subdir /mnt/bind rw - ext4 /dev/sdb1 rw\n\
23 20 0:4 / /proc rw - proc proc rw\n\
24 20 0:5 / /mnt/network rw - nfs server:/share rw\n\
25 20 7:1 / /snap/app/1 ro - squashfs /dev/loop1 ro\n\
26 20 8:3 / /mnt/invalid\xff rw - ext4 /dev/sdc1 rw\n\
broken line\n\
27 20 8:4 / /bad\\999 rw - ext4 /dev/sdd1 rw\n";
        let volumes = Volumes::new(parse_mountinfo(data));
        assert_eq!(volumes.entries.len(), 7);
        let at = |path| &volumes.entries[volumes.find(Path::new(path)).unwrap()];
        assert_eq!(at("/media/My Disk/file").source, "/dev/sdb1");
        assert_eq!(at("/mnt/bind/file").color, at("/media/My Disk/file").color);
        assert_eq!(at("/mnt/network/file").filesystem, "nfs");
        assert!(!at("/proc/self/status").shortcut);
        assert!(!at("/snap/app/1/file").shortcut);
        let raw = PathBuf::from(OsString::from_vec(b"/mnt/invalid\xff/file".to_vec()));
        assert_eq!(
            volumes.entries[volumes.find(&raw).unwrap()].source,
            "/dev/sdc1"
        );
    }

    #[test]
    fn file_mounts_override_parent_and_malformed_records_are_ignored() {
        let volumes = Volumes::new(parse_mountinfo(
            b"1 0 8:1 / / rw - ext4 /dev/a rw\n\
2 1 0:3 / /etc/hosts rw - tmpfs tmpfs rw\n\
3 1 8:2 / /data rw - ext4 /dev/b rw\n\
4 1 8:3 / /data rw - ext4 /dev/c rw\n\
5 1 bad / /bad rw - ext4 /dev/d rw\n\
6 1 8:4 / relative rw - ext4 /dev/e rw\n",
        ));
        assert_eq!(volumes.entries.len(), 3);
        assert_eq!(
            volumes.entries[volumes.find(Path::new("/etc/hosts")).unwrap()].filesystem,
            "tmpfs"
        );
        assert_eq!(
            volumes.entries[volumes.find(Path::new("/data/file")).unwrap()].source,
            "/dev/c"
        );
    }
}
