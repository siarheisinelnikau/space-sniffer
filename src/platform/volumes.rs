//! A mount snapshot shared by the desktop UI. Lookup never stats individual files.
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug)]
pub struct Volume {
    pub mount: PathBuf,
    pub source: String,
    pub filesystem: String,
    pub identity: Vec<u8>,
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub shortcut: bool,
    pub color: usize,
}

impl Volume {
    pub fn description(&self) -> String {
        if self.filesystem.is_empty() {
            self.mount.display().to_string()
        } else {
            format!(
                "{} · {} · {}",
                self.source,
                self.mount.display(),
                self.filesystem
            )
        }
    }
}

#[derive(Default)]
pub struct Volumes {
    pub entries: Vec<Volume>,
    by_mount: HashMap<PathBuf, usize>,
    alias: Option<(PathBuf, PathBuf)>,
}

impl Volumes {
    pub fn new(mut entries: Vec<Volume>) -> Self {
        let mut identities: Vec<_> = entries.iter().map(|v| v.identity.clone()).collect();
        identities.sort();
        identities.dedup();
        for volume in &mut entries {
            volume.color = identities.binary_search(&volume.identity).unwrap();
        }
        // The most specific component prefix wins, including file bind mounts.
        entries.sort_by_key(|v| std::cmp::Reverse(v.mount.components().count()));
        let by_mount = entries
            .iter()
            .enumerate()
            .map(|(index, volume)| (comparison_path(&volume.mount).into_owned(), index))
            .collect();
        Self {
            entries,
            by_mount,
            alias: None,
        }
    }

    /// Resolve root ancestor aliases once, preserving the scanner's original paths.
    pub fn set_root(&mut self, root: &Path) {
        self.alias = std::path::absolute(root).ok().and_then(|absolute| {
            std::fs::canonicalize(&absolute)
                .ok()
                .map(|canonical| (absolute, comparison_path(&canonical).into_owned()))
        });
    }

    fn resolved<'a>(&self, path: &'a Path) -> std::borrow::Cow<'a, Path> {
        if let Some((alias, canonical)) = &self.alias
            && alias != canonical
            && let Ok(relative) = path.strip_prefix(alias)
        {
            return std::borrow::Cow::Owned(canonical.join(relative));
        }
        std::borrow::Cow::Borrowed(path)
    }

    pub fn find(&self, path: &Path) -> Option<usize> {
        let path = self.resolved(path);
        let path = comparison_path(&path);
        path.ancestors()
            .find_map(|ancestor| self.by_mount.get(ancestor).copied())
    }

    /// A directory spanning mounts and an aggregate tile must not claim that
    /// all of their bytes belong to the directory's own filesystem.
    pub fn tile_volume(&self, path: &Path, directory: bool, collapsed: usize) -> Option<usize> {
        if collapsed != 0 {
            return None;
        }
        let index = self.find(path)?;
        if directory {
            let path = self.resolved(path);
            if self.entries.iter().any(|v| {
                v.mount.starts_with(path.as_ref()) && v.identity != self.entries[index].identity
            }) {
                return None;
            }
        }
        Some(index)
    }
}

#[cfg(not(target_os = "windows"))]
fn comparison_path(path: &Path) -> std::borrow::Cow<'_, Path> {
    std::borrow::Cow::Borrowed(path)
}

#[cfg(target_os = "windows")]
fn comparison_path(path: &Path) -> std::borrow::Cow<'_, Path> {
    use std::path::{Component, Prefix};
    let mut components = path.components();
    let Some(Component::Prefix(prefix)) = components.next() else {
        return std::borrow::Cow::Borrowed(path);
    };
    let mut normalized = match prefix.kind() {
        Prefix::Disk(letter) | Prefix::VerbatimDisk(letter) => {
            PathBuf::from(format!("{}:", char::from(letter.to_ascii_uppercase())))
        }
        _ => return std::borrow::Cow::Borrowed(path),
    };
    for component in components {
        normalized.push(component.as_os_str());
    }
    std::borrow::Cow::Owned(normalized)
}

pub fn discover() -> Volumes {
    #[cfg(target_os = "linux")]
    {
        Volumes::new(super::linux::mounts())
    }
    #[cfg(not(target_os = "linux"))]
    {
        let entries = super::roots()
            .into_iter()
            .filter(|path| {
                // Home is a shortcut, not a separate volume on Unix desktops.
                cfg!(target_os = "windows")
                    || path == Path::new("/")
                    || path.starts_with("/Volumes")
            })
            .map(|mount| Volume {
                identity: mount.as_os_str().as_encoded_bytes().to_vec(),
                source: mount.display().to_string(),
                mount,
                filesystem: String::new(),
                shortcut: true,
                color: 0,
            })
            .collect();
        Volumes::new(entries)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn volume(mount: &str, identity: &str) -> Volume {
        Volume {
            mount: mount.into(),
            source: identity.into(),
            filesystem: "ext4".into(),
            identity: identity.as_bytes().into(),
            shortcut: true,
            color: 0,
        }
    }

    #[test]
    fn nested_mounts_bind_aliases_and_component_boundaries() {
        let volumes = Volumes::new(vec![
            volume("/", "a"),
            volume("/media/data", "b"),
            volume("/media/data/nested", "c"),
            volume("/backup", "b"),
        ]);
        let lookup = |path| &volumes.entries[volumes.find(Path::new(path)).unwrap()];
        assert_eq!(lookup("/media/data/file").source, "b");
        assert_eq!(lookup("/media/database/file").source, "a");
        assert_eq!(lookup("/media/data/nested/file").source, "c");
        assert_eq!(
            lookup("/backup/file").color,
            lookup("/media/data/file").color
        );
        assert_ne!(
            lookup("/backup/file").color,
            lookup("/media/data/nested/file").color
        );
        assert!(
            volumes
                .tile_volume(Path::new("/media/data"), true, 0)
                .is_none()
        );
        assert!(volumes.tile_volume(Path::new("/backup"), true, 0).is_some());
        assert!(volumes.tile_volume(Path::new("/backup"), true, 2).is_none());
        assert!(Volumes::default().find(Path::new("/missing")).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn root_alias_resolves_once_and_keeps_nested_mounts() {
        let temp = tempfile::tempdir().unwrap();
        let real = temp.path().join("real");
        std::fs::create_dir_all(real.join("data")).unwrap();
        let alias = temp.path().join("alias");
        std::os::unix::fs::symlink(&real, &alias).unwrap();
        let mut volumes = Volumes::new(vec![
            volume("/", "a"),
            volume(
                real.join("data").canonicalize().unwrap().to_str().unwrap(),
                "b",
            ),
        ]);
        volumes.set_root(&alias);
        let index = volumes.find(&alias.join("data/file")).unwrap();
        assert_eq!(volumes.entries[index].source, "b");
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_canonical_prefix_matches_drive_shortcuts() {
        let volumes = Volumes::new(vec![volume(r"C:\", "c"), volume(r"D:\", "d")]);
        for path in [r"C:\data\file", r"c:\data\file", r"\\?\C:\data\file"] {
            assert_eq!(
                volumes.entries[volumes.find(Path::new(path)).unwrap()].source,
                "c"
            );
        }
    }
}
