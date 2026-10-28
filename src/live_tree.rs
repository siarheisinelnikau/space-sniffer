//! Indexed, incremental updates to a scan. Unchanged paths keep their IDs.
//!
//! Removed slots are disconnected, zeroed, and recycled after an update batch.
//! Consequently arena order is no longer a topological order: filtering walks
//! connected children instead. UI navigation should be restored by path after
//! a batch, because a later batch may reuse a removed entry's ID.

use crate::{
    filter::Filter,
    live::Change,
    model::{EntryKind, Node, NodeId, ScanTree},
};
use std::{
    cmp::Ordering,
    collections::HashMap,
    path::{Component, Path, PathBuf},
    time::SystemTime,
};

#[derive(Clone, Copy, Debug, Default)]
pub struct UpdateSummary {
    pub changed: usize,
    /// Model entries inspected, including ancestor and sibling comparisons.
    /// Filesystem inspection happens on the live worker, not in this model.
    pub inspected: usize,
}

#[derive(Debug)]
pub struct LiveTree {
    pub tree: ScanTree,
    pub weights: Vec<u64>,
    index: HashMap<PathBuf, NodeId>,
    alive: Vec<bool>,
    free: Vec<NodeId>,
    retired: Vec<NodeId>,
    filter: Filter,
    root_key: PathBuf,
    // A baseline scan only has a global skipped count. Its unattributed skips
    // remain at the root until root resync; later subtree counts are tracked.
    skipped: HashMap<NodeId, u64>,
}

impl LiveTree {
    pub fn new(tree: ScanTree) -> Self {
        assert!(
            tree.root < tree.nodes.len(),
            "a live tree needs a valid root"
        );
        let root_path = tree.nodes[tree.root].path.clone();
        let root_key = path_key(&root_path);
        let mut index = HashMap::with_capacity(tree.nodes.len());
        let mut alive = vec![false; tree.nodes.len()];
        let mut weights = vec![0; tree.nodes.len()];
        let mut stack = vec![tree.root];
        while let Some(id) = stack.pop() {
            if id >= alive.len() || alive[id] {
                continue;
            }
            alive[id] = true;
            index.insert(path_key(&tree.nodes[id].path), id);
            weights[id] = tree.nodes[id].size;
            stack.extend(tree.nodes[id].children.iter().copied());
        }
        let skipped = HashMap::from([(tree.root, tree.stats.skipped)]);
        Self {
            tree,
            weights,
            index,
            alive,
            free: Vec::new(),
            retired: Vec::new(),
            filter: Filter::default(),
            root_key,
            skipped,
        }
    }

    pub fn find(&self, path: &Path) -> Option<NodeId> {
        self.index.get(&path_key(path)).copied()
    }

    pub fn contains(&self, id: NodeId) -> bool {
        self.alive.get(id).copied().unwrap_or(false)
    }

    pub fn set_filter(&mut self, input: &str) -> Result<(), String> {
        let filter = Filter::parse(input)?;
        let order = self.subtree_ids(self.tree.root);
        for &id in order.iter().rev() {
            self.weights[id] = if self.tree.nodes[id].is_dir() {
                self.tree.nodes[id]
                    .children
                    .iter()
                    .fold(0_u64, |sum, &child| sum.saturating_add(self.weights[child]))
            } else if filter.matches(&self.tree.nodes[id]) {
                self.tree.nodes[id].size
            } else {
                0
            };
        }
        self.filter = filter;
        Ok(())
    }

    pub fn apply(&mut self, changes: &[Change]) -> UpdateSummary {
        let mut summary = UpdateSummary::default();
        for change in changes {
            match change {
                Change::Upsert {
                    path,
                    kind,
                    size,
                    modified,
                } => self.upsert(path, *kind, *size, *modified, &mut summary),
                Change::Remove(path) => {
                    if let Some(id) = self.find(path) {
                        self.remove(id, &mut summary);
                    }
                }
                Change::ReplaceSubtree { path, tree } => {
                    self.replace(path, tree, &mut summary);
                }
            }
        }
        // Do not recycle a removed ID within this batch: callers may retain a
        // selection until apply returns and restore it using its former path.
        self.free.append(&mut self.retired);
        summary
    }

    fn subtree_ids(&self, root: NodeId) -> Vec<NodeId> {
        if !self.contains(root) {
            return Vec::new();
        }
        let mut order = Vec::new();
        let mut stack = vec![root];
        while let Some(id) = stack.pop() {
            order.push(id);
            stack.extend(self.tree.nodes[id].children.iter().rev().copied());
        }
        order
    }

    fn allocate(&mut self) -> NodeId {
        if let Some(id) = self.free.pop() {
            return id;
        }
        let id = self.tree.nodes.len();
        self.tree.nodes.push(Node {
            name: String::new(),
            path: PathBuf::new(),
            kind: EntryKind::File,
            size: 0,
            children: Vec::new(),
            parent: None,
            modified: None,
        });
        self.weights.push(0);
        self.alive.push(false);
        id
    }

    fn count(&mut self, kind: EntryKind, add: bool) {
        let counter = match kind {
            EntryKind::File => &mut self.tree.stats.files,
            EntryKind::Directory => &mut self.tree.stats.directories,
            EntryKind::Symlink => &mut self.tree.stats.symlinks,
        };
        *counter = if add {
            counter.saturating_add(1)
        } else {
            counter.saturating_sub(1)
        };
    }

    fn detach(&mut self, id: NodeId, summary: &mut UpdateSummary) -> Option<NodeId> {
        let parent = self.tree.nodes[id].parent?;
        let nodes = &self.tree.nodes;
        let position = nodes[parent]
            .children
            .binary_search_by(|&sibling| {
                summary.inspected += 1;
                compare(&nodes[sibling], &nodes[id])
            })
            .ok()
            .or_else(|| {
                nodes[parent].children.iter().position(|&sibling| {
                    summary.inspected += 1;
                    sibling == id
                })
            });
        if let Some(position) = position {
            self.tree.nodes[parent].children.remove(position);
        }
        Some(parent)
    }

    fn attach(&mut self, id: NodeId, parent: Option<NodeId>, summary: &mut UpdateSummary) {
        self.tree.nodes[id].parent = parent;
        if let Some(parent) = parent {
            let nodes = &self.tree.nodes;
            let position = nodes[parent]
                .children
                .binary_search_by(|&sibling| {
                    summary.inspected += 1;
                    compare(&nodes[sibling], &nodes[id])
                })
                .unwrap_or_else(|position| position);
            self.tree.nodes[parent].children.insert(position, id);
        }
    }

    fn adjust(
        &mut self,
        mut parent: Option<NodeId>,
        old: (u64, u64),
        new: (u64, u64),
        summary: &mut UpdateSummary,
    ) {
        if old == new {
            return;
        }
        while let Some(id) = parent {
            summary.inspected += 1;
            let grandparent = self.detach(id, summary);
            self.tree.nodes[id].size = self.tree.nodes[id]
                .size
                .saturating_sub(old.0)
                .saturating_add(new.0);
            self.weights[id] = self.weights[id].saturating_sub(old.1).saturating_add(new.1);
            self.attach(id, grandparent, summary);
            parent = grandparent;
        }
    }

    fn ensure_parent(&mut self, path: &Path, summary: &mut UpdateSummary) -> Option<NodeId> {
        let parent = path.parent()?;
        let key = path_key(parent);
        if !key.starts_with(&self.root_key) {
            return None;
        }
        if let Some(&id) = self.index.get(&key) {
            return self.tree.nodes[id].is_dir().then_some(id);
        }
        // Ancestors absent from a coalesced event batch may be inserted as
        // empty directories. Never turn an existing file/link into a parent.
        self.upsert(parent, EntryKind::Directory, 0, None, summary);
        self.index
            .get(&key)
            .copied()
            .filter(|&id| self.tree.nodes[id].is_dir())
    }

    fn upsert(
        &mut self,
        path: &Path,
        kind: EntryKind,
        size: u64,
        modified: Option<SystemTime>,
        summary: &mut UpdateSummary,
    ) {
        let key = path_key(path);
        if !key.starts_with(&self.root_key) {
            return;
        }
        summary.inspected += 1;
        let existing = self.index.get(&key).copied();
        let parent = if key == self.root_key {
            None
        } else if let Some(id) = self.ensure_parent(path, summary) {
            Some(id)
        } else {
            return;
        };
        let id = existing.unwrap_or_else(|| {
            if key == self.root_key {
                self.tree.root
            } else {
                self.allocate()
            }
        });
        let old = if self.contains(id) {
            let node = &self.tree.nodes[id];
            if node.kind == kind
                && (kind == EntryKind::Directory || node.size == size)
                && node.path == path
            {
                if node.modified != modified {
                    // Modification time is not an ordering or weight key.
                    // Avoid shifting a flat directory's entire child vector
                    // when a content write leaves its logical size unchanged.
                    self.tree.nodes[id].modified = modified;
                    summary.changed += 1;
                }
                return;
            }
            let old = (node.size, self.weights[id]);
            self.detach(id, summary);
            if node_kind_changed(&self.tree.nodes[id], kind) {
                self.clear_descendants(id, summary);
                self.count(self.tree.nodes[id].kind, false);
                self.count(kind, true);
                self.tree.nodes[id].size = 0;
                self.weights[id] = 0;
            }
            old
        } else {
            self.count(kind, true);
            (0, 0)
        };

        let directory_size = self.tree.nodes[id].size;
        self.tree.nodes[id].name = basename(path);
        self.tree.nodes[id].path = path.to_owned();
        self.tree.nodes[id].kind = kind;
        self.tree.nodes[id].modified = modified;
        self.tree.nodes[id].size = if kind == EntryKind::Directory {
            directory_size
        } else if kind == EntryKind::File {
            size
        } else {
            0
        };
        self.alive[id] = true;
        self.index.insert(key, id);
        if kind != EntryKind::Directory {
            self.weights[id] = if self.filter.matches(&self.tree.nodes[id]) {
                self.tree.nodes[id].size
            } else {
                0
            };
        }
        self.attach(id, parent, summary);
        self.adjust(
            parent,
            old,
            (self.tree.nodes[id].size, self.weights[id]),
            summary,
        );
        summary.changed += 1;
    }

    fn retire(&mut self, id: NodeId) {
        self.index.remove(&path_key(&self.tree.nodes[id].path));
        self.count(self.tree.nodes[id].kind, false);
        if let Some(skipped) = self.skipped.remove(&id) {
            self.tree.stats.skipped = self.tree.stats.skipped.saturating_sub(skipped);
        }
        self.alive[id] = false;
        self.weights[id] = 0;
        self.tree.nodes[id].size = 0;
        self.tree.nodes[id].children.clear();
        self.tree.nodes[id].parent = None;
        self.tree.nodes[id].modified = None;
        if id != self.tree.root {
            self.retired.push(id);
        }
    }

    fn clear_descendants(&mut self, id: NodeId, summary: &mut UpdateSummary) {
        let mut pending = std::mem::take(&mut self.tree.nodes[id].children);
        while let Some(child) = pending.pop() {
            pending.extend(self.tree.nodes[child].children.iter().copied());
            self.retire(child);
            summary.inspected += 1;
            summary.changed += 1;
        }
    }

    fn remove(&mut self, id: NodeId, summary: &mut UpdateSummary) {
        let old = (self.tree.nodes[id].size, self.weights[id]);
        let parent = self.detach(id, summary);
        self.clear_descendants(id, summary);
        self.retire(id);
        self.adjust(parent, old, (0, 0), summary);
        summary.inspected += 1;
        summary.changed += 1;
    }

    fn replace(&mut self, path: &Path, source: &ScanTree, summary: &mut UpdateSummary) {
        let key = path_key(path);
        if !key.starts_with(&self.root_key)
            || source.root >= source.nodes.len()
            || path_key(&source.nodes[source.root].path) != key
        {
            return;
        }
        let parent = if key == self.root_key {
            None
        } else if let Some(id) = self.ensure_parent(path, summary) {
            Some(id)
        } else {
            return;
        };
        let existing = self.index.get(&key).copied();
        let old = existing
            .map(|id| (self.tree.nodes[id].size, self.weights[id]))
            .unwrap_or((0, 0));
        if let Some(id) = existing {
            self.detach(id, summary);
        }
        let old_ids = existing.map(|id| self.subtree_ids(id)).unwrap_or_default();
        let mut old_index = HashMap::with_capacity(old_ids.len());
        for &id in &old_ids {
            let old_key = path_key(&self.tree.nodes[id].path);
            self.index.remove(&old_key);
            old_index.insert(old_key, id);
            self.count(self.tree.nodes[id].kind, false);
            if let Some(skipped) = self.skipped.remove(&id) {
                self.tree.stats.skipped = self.tree.stats.skipped.saturating_sub(skipped);
            }
            summary.inspected += 1;
        }

        let mut order = Vec::new();
        let mut stack = vec![source.root];
        let mut mapping = vec![usize::MAX; source.nodes.len()];
        while let Some(source_id) = stack.pop() {
            if source_id >= source.nodes.len() || mapping[source_id] != usize::MAX {
                continue;
            }
            let node = &source.nodes[source_id];
            let node_key = path_key(&node.path);
            if !node_key.starts_with(&key) {
                continue;
            }
            let id = if node_key == self.root_key {
                old_index.remove(&node_key);
                self.tree.root
            } else {
                old_index
                    .remove(&node_key)
                    .unwrap_or_else(|| self.allocate())
            };
            mapping[source_id] = id;
            order.push(source_id);
            stack.extend(node.children.iter().rev().copied());
        }
        for &source_id in &order {
            let id = mapping[source_id];
            let node = &source.nodes[source_id];
            let new_parent = if source_id == source.root {
                parent
            } else {
                node.parent
                    .and_then(|parent| mapping.get(parent).copied())
                    .filter(|&parent| parent != usize::MAX)
            };
            let children = node
                .children
                .iter()
                .filter_map(|&child| mapping.get(child).copied())
                .filter(|&child| child != usize::MAX)
                .collect();
            let previous = &self.tree.nodes[id];
            if !self.contains(id)
                || previous.path != node.path
                || previous.kind != node.kind
                || previous.size != node.size
                || previous.modified != node.modified
            {
                summary.changed += 1;
            }
            self.tree.nodes[id] = Node {
                name: node.name.clone(),
                path: node.path.clone(),
                kind: node.kind,
                size: node.size,
                modified: node.modified,
                parent: new_parent,
                children,
            };
            self.alive[id] = true;
            self.index.insert(path_key(&node.path), id);
            self.count(node.kind, true);
            summary.inspected += 1;
        }
        for (_, id) in old_index {
            // Counts and index removal were already handled above.
            self.alive[id] = false;
            self.weights[id] = 0;
            self.tree.nodes[id].size = 0;
            self.tree.nodes[id].children.clear();
            self.tree.nodes[id].parent = None;
            self.tree.nodes[id].modified = None;
            if id != self.tree.root {
                self.retired.push(id);
            }
            summary.changed += 1;
        }
        for &source_id in order.iter().rev() {
            let id = mapping[source_id];
            self.weights[id] = if self.tree.nodes[id].is_dir() {
                self.tree.nodes[id]
                    .children
                    .iter()
                    .fold(0_u64, |sum, &child| sum.saturating_add(self.weights[child]))
            } else if self.filter.matches(&self.tree.nodes[id]) {
                self.tree.nodes[id].size
            } else {
                0
            };
        }
        let id = mapping[source.root];
        self.skipped.insert(id, source.stats.skipped);
        self.tree.stats.skipped = self.tree.stats.skipped.saturating_add(source.stats.skipped);
        if key == self.root_key {
            self.tree.stats.elapsed = source.stats.elapsed;
        }
        self.attach(id, parent, summary);
        self.adjust(
            parent,
            old,
            (self.tree.nodes[id].size, self.weights[id]),
            summary,
        );
    }
}

fn node_kind_changed(node: &Node, kind: EntryKind) -> bool {
    node.kind != kind
}

fn compare(left: &Node, right: &Node) -> Ordering {
    right
        .size
        .cmp(&left.size)
        .then_with(|| left.name.cmp(&right.name))
        .then_with(|| left.path.cmp(&right.path))
}

fn basename(path: &Path) -> String {
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}

/// Lexical identity, without canonicalization (which would follow links and
/// would fail for removed paths). Windows extended prefixes and case variants
/// normalize to the same key, while unpaired UTF-16 units are preserved.
pub(crate) fn path_key(path: &Path) -> PathBuf {
    #[cfg(windows)]
    let path = windows_path(path);
    #[cfg(not(windows))]
    let path = path.to_owned();
    let absolute = std::path::absolute(&path).unwrap_or(path);
    let mut result = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if matches!(result.components().next_back(), Some(Component::Normal(_))) {
                    result.pop();
                }
            }
            other => result.push(other.as_os_str()),
        }
    }
    result
}

#[cfg(windows)]
fn windows_path(path: &Path) -> PathBuf {
    use std::{
        ffi::OsString,
        os::windows::ffi::{OsStrExt, OsStringExt},
    };
    let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    for unit in &mut wide {
        if *unit == b'/' as u16 {
            *unit = b'\\' as u16;
        }
    }
    if wide.starts_with(&[92, 92, 63, 92]) {
        if wide.len() >= 8
            && matches!(wide[4], 85 | 117)
            && matches!(wide[5], 78 | 110)
            && matches!(wide[6], 67 | 99)
            && wide[7] == 92
        {
            wide.splice(0..8, [92, 92]);
        } else {
            wide.drain(0..4);
        }
    }
    let mut folded = Vec::with_capacity(wide.len());
    for character in char::decode_utf16(wide) {
        match character {
            Ok(character) => {
                for lower in character.to_lowercase() {
                    folded.extend_from_slice(lower.encode_utf16(&mut [0; 2]));
                }
            }
            Err(error) => folded.push(error.unpaired_surrogate()),
        }
    }
    PathBuf::from(OsString::from_wide(&folded))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ScanStats;

    fn node(root: &Path, name: &str, kind: EntryKind, size: u64, parent: Option<NodeId>) -> Node {
        let path = root.join(name);
        Node {
            name: basename(&path),
            path,
            kind,
            size,
            parent,
            children: Vec::new(),
            modified: None,
        }
    }

    fn tree() -> ScanTree {
        let root = PathBuf::from("incremental-model-fixture");
        let mut nodes = vec![
            node(&root, "", EntryKind::Directory, 50, None),
            node(&root, "a", EntryKind::Directory, 30, Some(0)),
            node(&root, "b", EntryKind::Directory, 20, Some(0)),
            node(&root, "a/old.txt", EntryKind::File, 30, Some(1)),
            node(&root, "b/keep.bin", EntryKind::File, 20, Some(2)),
        ];
        nodes[0].children = vec![1, 2];
        nodes[1].children = vec![3];
        nodes[2].children = vec![4];
        ScanTree {
            nodes,
            root: 0,
            stats: ScanStats {
                files: 2,
                directories: 3,
                ..ScanStats::default()
            },
        }
    }

    fn upsert(path: PathBuf, size: u64) -> Change {
        Change::Upsert {
            path,
            kind: EntryKind::File,
            size,
            modified: None,
        }
    }

    #[test]
    fn file_updates_touch_only_ancestors_and_reorder_siblings() {
        let mut live = LiveTree::new(tree());
        live.set_filter("*.txt;>25B").unwrap();
        let path = live.tree.nodes[3].path.clone();
        let summary = live.apply(&[upsert(path.clone(), 10)]);
        assert!(summary.inspected < 30, "{summary:?}");
        assert_eq!(live.tree.total_size(), 30);
        assert_eq!(live.weights[0], 0);
        assert_eq!(live.tree.nodes[0].children, [2, 1]);
        assert_eq!(live.find(&path), Some(3));
        live.apply(&[upsert(path, 40)]);
        assert_eq!(live.tree.total_size(), 60);
        assert_eq!(live.weights[0], 40);
        assert_eq!(live.tree.nodes[0].children, [1, 2]);
        assert_eq!(live.tree.stats.files, 2);
    }

    #[test]
    fn recycled_slots_filter_correctly_when_child_precedes_parent() {
        let mut live = LiveTree::new(tree());
        let old_path = live.tree.nodes[1].path.clone();
        live.apply(&[Change::Remove(old_path)]);
        assert!(!live.contains(1));
        assert!(!live.contains(3));
        assert_eq!(live.tree.nodes[3].parent, None);
        let path = live.tree.nodes[2].path.join("new.txt");
        live.apply(&[upsert(path.clone(), 11)]);
        let id = live.find(&path).unwrap();
        assert!(id < 2);
        assert_eq!(live.tree.nodes[id].parent, Some(2));
        live.set_filter("*.txt").unwrap();
        assert_eq!(live.weights[0], 11);
        assert_eq!(live.weights[2], 11);
        assert_eq!(live.tree.total_size(), 31);
        assert_eq!((live.tree.stats.files, live.tree.stats.directories), (2, 2));
        assert!(live.set_filter(">bad-unit").is_err());
        assert_eq!(live.weights[0], 11);
    }

    #[test]
    fn subtree_replacement_preserves_matching_ids_and_counts() {
        let initial = tree();
        let root = initial.nodes[1].path.clone();
        let mut source = ScanTree {
            nodes: vec![
                node(&root, "", EntryKind::Directory, 50, None),
                node(&root, "old.txt", EntryKind::File, 40, Some(0)),
                node(&root, "new.bin", EntryKind::File, 10, Some(0)),
            ],
            root: 0,
            stats: ScanStats {
                files: 2,
                directories: 1,
                ..ScanStats::default()
            },
        };
        source.nodes[0].children = vec![1, 2];
        let mut live = LiveTree::new(initial);
        live.set_filter("*.txt").unwrap();
        live.apply(&[Change::ReplaceSubtree {
            path: root.clone(),
            tree: source,
        }]);
        assert_eq!(live.find(&root), Some(1));
        assert_eq!(live.find(&root.join("old.txt")), Some(3));
        assert_eq!(live.tree.total_size(), 70);
        assert_eq!(live.weights[0], 40);
        assert_eq!((live.tree.stats.files, live.tree.stats.directories), (3, 3));
        assert_eq!(live.tree.nodes[4].size, 20);
    }

    #[test]
    fn root_slot_survives_deletion_and_recovery_in_same_batch() {
        let original = tree();
        let root = original.nodes[0].path.clone();
        let mut live = LiveTree::new(original.clone());
        live.apply(&[Change::Remove(root.clone())]);
        assert!(!live.contains(live.tree.root));
        assert_eq!(live.tree.total_size(), 0);
        assert!(live.find(&root).is_none());
        assert_eq!((live.tree.stats.files, live.tree.stats.directories), (0, 0));
        live.apply(&[Change::ReplaceSubtree {
            path: root.clone(),
            tree: original.clone(),
        }]);
        assert_eq!(live.find(&root), Some(0));
        live.apply(&[
            Change::Remove(root.clone()),
            Change::ReplaceSubtree {
                path: root,
                tree: original,
            },
        ]);
        assert_eq!(live.tree.total_size(), 50);
        assert_eq!((live.tree.stats.files, live.tree.stats.directories), (2, 3));
    }

    #[test]
    fn churn_reuses_slots_and_implicit_parents_do_not_cross_links() {
        let mut live = LiveTree::new(tree());
        let root = live.tree.nodes[0].path.clone();
        for index in 0..100 {
            let path = root.join(format!("temporary-{index}"));
            live.apply(&[upsert(path.clone(), 1)]);
            live.apply(&[Change::Remove(path)]);
        }
        assert_eq!(live.tree.nodes.len(), 6);
        let link = root.join("link");
        live.apply(&[Change::Upsert {
            path: link.clone(),
            kind: EntryKind::Symlink,
            size: 999,
            modified: None,
        }]);
        live.apply(&[upsert(link.join("outside.txt"), 99)]);
        assert!(live.find(&link.join("outside.txt")).is_none());
        assert_eq!(live.tree.total_size(), 50);
        live.apply(&[upsert(root.join("new/deep/file.txt"), 7)]);
        assert_eq!(live.tree.total_size(), 57);
        assert_eq!(live.tree.stats.directories, 5);
    }

    #[test]
    fn path_normalization_is_lexical_for_deleted_paths_and_windows_prefixes() {
        assert_eq!(
            path_key(Path::new("folder/../absent.txt")),
            path_key(Path::new("absent.txt"))
        );
        #[cfg(windows)]
        {
            assert_eq!(
                path_key(Path::new(r"C:\Temp\Dir\..\FILE.txt")),
                path_key(Path::new(r"\\?\c:\temp\file.TXT"))
            );
            assert_eq!(
                path_key(Path::new(r"\\?\unc\SERVER\share\FILE")),
                path_key(Path::new(r"\\server\SHARE\file"))
            );
        }
    }

    #[test]
    fn metadata_only_write_does_not_visit_a_hundred_thousand_siblings() {
        const FILES: usize = 100_000;
        let root = PathBuf::from("metadata-flat-fixture");
        let mut nodes = Vec::with_capacity(FILES + 1);
        nodes.push(node(&root, "", EntryKind::Directory, FILES as u64, None));
        nodes[0].children = (1..=FILES).collect();
        for index in 1..=FILES {
            nodes.push(node(
                &root,
                &format!("f{index:06}.bin"),
                EntryKind::File,
                1,
                Some(0),
            ));
        }
        let changed = nodes[FILES / 2].path.clone();
        let mut live = LiveTree::new(ScanTree {
            nodes,
            root: 0,
            stats: ScanStats {
                files: FILES as u64,
                directories: 1,
                ..ScanStats::default()
            },
        });
        let summary = live.apply(&[Change::Upsert {
            path: changed,
            kind: EntryKind::File,
            size: 1,
            modified: Some(SystemTime::UNIX_EPOCH),
        }]);
        assert_eq!(summary.changed, 1);
        assert!(summary.inspected <= 2, "{summary:?}");
        assert_eq!(live.tree.total_size(), FILES as u64);
        assert_eq!(live.tree.nodes[0].children[FILES / 2 - 1], FILES / 2);
    }
}
