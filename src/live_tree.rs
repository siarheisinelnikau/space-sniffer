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
    collections::{BinaryHeap, HashMap, HashSet, hash_map::RandomState},
    hash::{BuildHasher, BuildHasherDefault, Hasher},
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering as AtomicOrdering},
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
    index: PathIndex,
    alive: Vec<bool>,
    free: Vec<NodeId>,
    retired: Vec<NodeId>,
    filter: Filter,
    root_key: PathBuf,
    scan_append_ordered: bool,
    preview_unsorted: bool,
    identity: u64,
    revision: u64,
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
        let root_key = path_key(&tree.nodes[tree.root].path);
        let mut index = PathIndex::with_capacity(tree.nodes.len());
        let mut alive = vec![false; tree.nodes.len()];
        let mut weights = vec![0; tree.nodes.len()];
        let ordered = tree.root == 0
            && tree.nodes.iter().enumerate().all(|(id, node)| {
                if id == tree.root {
                    node.parent.is_none()
                } else {
                    node.parent
                        .is_some_and(|parent| parent < id && tree.nodes[parent].is_dir())
                }
            })
            && tree.nodes.iter().enumerate().all(|(id, node)| {
                node.children.iter().all(|&child| {
                    child > id && child < tree.nodes.len() && tree.nodes[child].parent == Some(id)
                })
            });
        let mut directory_keys = HashMap::new();
        if ordered {
            alive[tree.root] = true;
            for id in 0..tree.nodes.len() {
                if !alive[id] {
                    continue;
                }
                let node = &tree.nodes[id];
                let key = construction_key(&tree, id, &root_key, &directory_keys);
                index.insert(key.as_os_str(), id, &tree.nodes);
                weights[id] = node.size;
                if node.is_dir() {
                    directory_keys.insert(id, key);
                }
                for &child in &node.children {
                    alive[child] = true;
                }
            }
        } else {
            let mut stack = vec![tree.root];
            while let Some(id) = stack.pop() {
                if id >= alive.len() || alive[id] {
                    continue;
                }
                alive[id] = true;
                let key = construction_key(&tree, id, &root_key, &directory_keys);
                index.insert(key.as_os_str(), id, &tree.nodes);
                weights[id] = tree.nodes[id].size;
                if tree.nodes[id].is_dir() {
                    directory_keys.insert(id, key);
                }
                stack.extend(tree.nodes[id].children.iter().copied());
            }
        }
        let skipped = HashMap::from([(tree.root, tree.stats.skipped)]);
        let scan_append_ordered = ordered && index.len() == tree.nodes.len();
        Self {
            tree,
            weights,
            index,
            alive,
            free: Vec::new(),
            retired: Vec::new(),
            filter: Filter::default(),
            root_key,
            scan_append_ordered,
            preview_unsorted: false,
            identity: next_identity(),
            revision: 0,
            skipped,
        }
    }

    pub fn find(&self, path: &Path) -> Option<NodeId> {
        self.index.get(path_key(path).as_os_str(), &self.tree.nodes)
    }

    pub fn contains(&self, id: NodeId) -> bool {
        self.alive.get(id).copied().unwrap_or(false)
    }

    pub fn set_filter(&mut self, input: &str) -> Result<(), String> {
        let mut job = self.begin_filter(input)?;
        while !job.step(self, usize::MAX)? {}
        self.commit_filter(job)
    }

    /// Start a filter without cloning the model or walking its entries. Active
    /// weights remain visible until a complete, non-stale job is committed.
    pub fn begin_filter(&self, input: &str) -> Result<FilterJob, String> {
        let filter = Filter::parse(input)?;
        let unchanged = filter == self.filter;
        let (work, total) = if unchanged {
            (FilterWork::Complete, self.index.len())
        } else if filter.is_empty() {
            (FilterWork::Copy { next: 0 }, self.tree.nodes.len())
        } else if self.scan_append_ordered {
            (
                FilterWork::Ordered {
                    next: self.tree.nodes.len(),
                },
                self.tree.nodes.len(),
            )
        } else if self.contains(self.tree.root) {
            (
                FilterWork::Traversal {
                    stack: vec![FilterFrame {
                        id: self.tree.root,
                        next_child: 0,
                        sum: 0,
                    }],
                },
                self.index.len(),
            )
        } else {
            (FilterWork::Complete, 0)
        };
        Ok(FilterJob {
            filter,
            weights: (!unchanged).then(|| vec![0; self.tree.nodes.len()]),
            work,
            identity: self.identity,
            revision: self.revision,
            source_len: self.tree.nodes.len(),
            processed: if unchanged { total } else { 0 },
            total,
        })
    }

    /// Atomically install a finished filter. The job must have read this exact
    /// model revision; stale or incomplete jobs leave active weights untouched.
    pub fn commit_filter(&mut self, mut job: FilterJob) -> Result<(), String> {
        if job.is_stale(self) {
            return Err("Filter source changed; restart the filter job.".into());
        }
        if !matches!(job.work, FilterWork::Complete) {
            return Err("The filter job is not complete.".into());
        }
        if let Some(weights) = job.weights.take() {
            self.weights = weights;
        }
        self.filter = job.filter;
        // An already-active/no-allocation job relies on the active filter as
        // well as source nodes. Committing another job invalidates that basis.
        self.revision = self.revision.wrapping_add(1);
        Ok(())
    }

    /// Append at most 8,192 discovery-order nodes from the initial scanner.
    ///
    /// The seed root must already exist. IDs are dense and parents precede
    /// children. Input children must be empty; directory and link sizes must
    /// be zero. The entire chunk is checked before any model state changes.
    /// File sizes and active filter weights are folded through new nodes and
    /// affected existing ancestors in batches, without sorting siblings.
    ///
    /// Preview children stay in discovery order. Normally final Ready replaces
    /// this model with the sorted scan. Calling apply before that is safe but
    /// sorts the preview once and permanently ends its append-only phase.
    pub fn append_scan(&mut self, start: usize, nodes: Vec<Node>) -> Result<UpdateSummary, String> {
        if start != self.tree.nodes.len() {
            return Err(format!(
                "Preview chunk starts at {start}; expected {}.",
                self.tree.nodes.len()
            ));
        }
        if nodes.len() > 8192 {
            return Err("A preview chunk cannot contain more than 8192 entries.".into());
        }
        if nodes.is_empty() {
            return Ok(UpdateSummary::default());
        }
        if !self.scan_append_ordered {
            return Err("Preview chunks require an append-only initial scan model.".into());
        }
        let end = start
            .checked_add(nodes.len())
            .ok_or_else(|| "Preview entry indices overflowed.".to_string())?;

        let mut keys = Vec::with_capacity(nodes.len());
        let mut existing_parents: HashMap<NodeId, PathBuf> = HashMap::new();
        for (offset, node) in nodes.iter().enumerate() {
            let id = start + offset;
            let fail = |message: &str| format!("Preview entry {id}: {message}");
            if !node.children.is_empty() {
                return Err(fail(
                    "children must be empty until their own chunks arrive.",
                ));
            }
            if node.kind != EntryKind::File && node.size != 0 {
                return Err(fail("directory and link sizes must start at zero."));
            }
            let parent = node
                .parent
                .ok_or_else(|| fail("only the separately initialized root can have no parent."))?;
            if parent >= id {
                return Err(fail("the parent must precede its child."));
            }
            let (parent_path, parent_key) = if parent < start {
                if !self.contains(parent) || !self.tree.nodes[parent].is_dir() {
                    return Err(fail("the parent must be an existing directory."));
                }
                let key = match existing_parents.entry(parent) {
                    std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
                    std::collections::hash_map::Entry::Vacant(entry) => {
                        let key = path_key(&self.tree.nodes[parent].path);
                        if !key.starts_with(&self.root_key) {
                            return Err(fail("the parent is outside the selected root."));
                        }
                        entry.insert(key)
                    }
                };
                (self.tree.nodes[parent].path.as_path(), &*key)
            } else {
                if !nodes[parent - start].is_dir() {
                    return Err(fail("a file or link cannot have children."));
                }
                (nodes[parent - start].path.as_path(), &keys[parent - start])
            };
            // Scanner children normally reuse their parent's exact path
            // spelling. Reuse that validated normalized prefix rather than
            // allocating and normalizing the whole absolute path per file.
            // Alias/case/extended-prefix spellings still use the full check.
            let (key, direct_child) = match (node.path.parent(), node.path.file_name()) {
                (Some(actual_parent), Some(name))
                    if actual_parent.as_os_str() == parent_path.as_os_str() =>
                {
                    (child_key(parent_key, name), true)
                }
                _ => (path_key(&node.path), false),
            };
            if !direct_child
                && (key == self.root_key
                    || !key.starts_with(&self.root_key)
                    || key.parent() != Some(parent_key.as_path()))
            {
                return Err(fail("the path must be directly inside its indexed parent."));
            }
            keys.push(key);
        }
        let mut unique = HashSet::with_capacity(nodes.len());
        for (offset, key) in keys.iter().enumerate() {
            if self.index.get(key.as_os_str(), &self.tree.nodes).is_some()
                || !unique.insert(key.as_os_str())
            {
                return Err(format!(
                    "Preview entry {}: the path is already present in this scan.",
                    start + offset
                ));
            }
        }
        drop(unique);
        drop(existing_parents);

        // Geometric arena/index growth is amortized; no existing entries are
        // revisited or reindexed for each preview chunk.
        self.tree.nodes.reserve(nodes.len());
        self.weights.reserve(nodes.len());
        self.alive.reserve(nodes.len());
        let mut summary = UpdateSummary {
            changed: nodes.len(),
            inspected: nodes.len() * 2,
        };
        for (offset, (node, key)) in nodes.into_iter().zip(keys).enumerate() {
            let id = start + offset;
            let parent = node.parent.expect("the complete chunk was validated");
            let weight = if self.filter.matches(&node) {
                node.size
            } else {
                0
            };
            self.count(node.kind, true);
            self.tree.nodes.push(node);
            self.weights.push(weight);
            self.alive.push(true);
            self.index.insert(key.as_os_str(), id, &self.tree.nodes);
            self.tree.nodes[parent].children.push(id);
        }

        let mut deltas: HashMap<NodeId, (u64, u64)> = HashMap::new();
        let mut ancestors = BinaryHeap::new();
        for id in (start..end).rev() {
            let parent = self.tree.nodes[id].parent.expect("validated parent");
            let delta = (self.tree.nodes[id].size, self.weights[id]);
            if parent >= start {
                self.tree.nodes[parent].size = self.tree.nodes[parent].size.saturating_add(delta.0);
                self.weights[parent] = self.weights[parent].saturating_add(delta.1);
            } else {
                let entry = deltas.entry(parent).or_insert_with(|| {
                    ancestors.push(parent);
                    (0, 0)
                });
                entry.0 = entry.0.saturating_add(delta.0);
                entry.1 = entry.1.saturating_add(delta.1);
            }
        }
        // Existing parents also precede children in this initial-scan phase.
        // Processing highest IDs first merges shared ancestry before visiting
        // it, so a flat chunk touches the old root exactly once.
        while let Some(id) = ancestors.pop() {
            let delta = deltas.remove(&id).expect("one queued entry per ancestor");
            summary.inspected += 1;
            self.tree.nodes[id].size = self.tree.nodes[id].size.saturating_add(delta.0);
            self.weights[id] = self.weights[id].saturating_add(delta.1);
            if let Some(parent) = self.tree.nodes[id].parent {
                let entry = deltas.entry(parent).or_insert_with(|| {
                    ancestors.push(parent);
                    (0, 0)
                });
                entry.0 = entry.0.saturating_add(delta.0);
                entry.1 = entry.1.saturating_add(delta.1);
            }
        }
        self.preview_unsorted = true;
        self.revision = self.revision.wrapping_add(1);
        Ok(summary)
    }

    pub fn apply(&mut self, changes: &[Change]) -> UpdateSummary {
        let mut summary = UpdateSummary::default();
        if !changes.is_empty() {
            self.revision = self.revision.wrapping_add(1);
            self.scan_append_ordered = false;
            if self.preview_unsorted {
                // Native changes normally start after sorted final Ready. A
                // direct caller can nevertheless transition a preview safely.
                for id in 0..self.tree.nodes.len() {
                    if self.contains(id) && self.tree.nodes[id].is_dir() {
                        let mut children = std::mem::take(&mut self.tree.nodes[id].children);
                        children.sort_unstable_by(|&left, &right| {
                            summary.inspected += 1;
                            compare(&self.tree.nodes[left], &self.tree.nodes[right])
                        });
                        self.tree.nodes[id].children = children;
                    }
                }
                self.preview_unsorted = false;
            }
        }
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
        mut old: (u64, u64),
        mut new: (u64, u64),
        summary: &mut UpdateSummary,
    ) {
        if old == new {
            return;
        }
        while let Some(id) = parent {
            summary.inspected += 1;
            let grandparent = self.detach(id, summary);
            let before = (self.tree.nodes[id].size, self.weights[id]);
            self.tree.nodes[id].size = if before.0 == u64::MAX && old.0 > new.0 {
                self.tree.nodes[id]
                    .children
                    .iter()
                    .fold(0_u64, |sum, &child| {
                        summary.inspected += 1;
                        sum.saturating_add(self.tree.nodes[child].size)
                    })
            } else {
                before.0.saturating_sub(old.0).saturating_add(new.0)
            };
            self.weights[id] = if before.1 == u64::MAX && old.1 > new.1 {
                self.tree.nodes[id]
                    .children
                    .iter()
                    .fold(0_u64, |sum, &child| {
                        summary.inspected += 1;
                        sum.saturating_add(self.weights[child])
                    })
            } else {
                before.1.saturating_sub(old.1).saturating_add(new.1)
            };
            let after = (self.tree.nodes[id].size, self.weights[id]);
            self.attach(id, grandparent, summary);
            if before == after {
                break;
            }
            old = before;
            new = after;
            parent = grandparent;
        }
    }

    fn ensure_parent(&mut self, path: &Path, summary: &mut UpdateSummary) -> Option<NodeId> {
        let parent = path.parent()?;
        let key = path_key(parent);
        if !key.starts_with(&self.root_key) {
            return None;
        }
        if let Some(id) = self.index.get(key.as_os_str(), &self.tree.nodes) {
            return self.tree.nodes[id].is_dir().then_some(id);
        }
        // Ancestors absent from a coalesced event batch may be inserted as
        // empty directories. Never turn an existing file/link into a parent.
        self.upsert(parent, EntryKind::Directory, 0, None, summary);
        self.index
            .get(key.as_os_str(), &self.tree.nodes)
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
        let existing = self.index.get(key.as_os_str(), &self.tree.nodes);
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
        self.index.insert(key.as_os_str(), id, &self.tree.nodes);
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
        self.index.remove(
            path_key(&self.tree.nodes[id].path).as_os_str(),
            &self.tree.nodes,
        );
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
        let existing = self.index.get(key.as_os_str(), &self.tree.nodes);
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
            self.index.remove(old_key.as_os_str(), &self.tree.nodes);
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
            self.index
                .insert(path_key(&node.path).as_os_str(), id, &self.tree.nodes);
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

fn construction_key(
    tree: &ScanTree,
    id: NodeId,
    root_key: &Path,
    directory_keys: &HashMap<NodeId, PathBuf>,
) -> PathBuf {
    if id == tree.root {
        return root_key.to_owned();
    }
    let node = &tree.nodes[id];
    if let Some(parent) = node.parent
        && let Some(parent_key) = directory_keys.get(&parent)
        && let (Some(actual_parent), Some(name)) = (node.path.parent(), node.path.file_name())
        && actual_parent.as_os_str() == tree.nodes[parent].path.as_os_str()
    {
        return child_key(parent_key, name);
    }
    path_key(&node.path)
}

const PATH_SHARDS: usize = 1024;
type IndexShard = HashMap<u64, NodeId, BuildHasherDefault<IndexHasher>>;

/// The input u64 is already randomized by a per-index RandomState. Avoid a
/// second cryptographic hash while retaining standard HashMap collision checks.
#[derive(Default, Debug)]
struct IndexHasher(u64);
impl Hasher for IndexHasher {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write_u64(&mut self, value: u64) {
        self.0 = value;
    }
    fn write(&mut self, bytes: &[u8]) {
        self.0 = bytes.iter().fold(0xcbf29ce484222325, |hash, &byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
        });
    }
}

/// Compact, collision-safe identity index. Paths already live in Nodes; the
/// index stores hashes and IDs rather than another allocation per full path.
/// Small independent shards distribute growth instead of rehashing a million
/// keys synchronously when a single global capacity boundary is crossed.
#[derive(Debug)]
struct PathIndex {
    shards: Box<[IndexShard]>,
    collisions: HashMap<u64, Vec<NodeId>>,
    state: RandomState,
    len: usize,
}

impl PathIndex {
    fn with_capacity(capacity: usize) -> Self {
        let per_shard = if capacity >= PATH_SHARDS {
            capacity.div_ceil(PATH_SHARDS) + 8
        } else {
            0
        };
        let shards = (0..PATH_SHARDS)
            .map(|_| IndexShard::with_capacity_and_hasher(per_shard, BuildHasherDefault::default()))
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Self {
            shards,
            collisions: HashMap::new(),
            state: RandomState::new(),
            len: 0,
        }
    }

    fn len(&self) -> usize {
        self.len
    }
    fn shard(hash: u64) -> usize {
        ((hash >> 32) as usize) & (PATH_SHARDS - 1)
    }
    fn matches(nodes: &[Node], id: NodeId, key: &std::ffi::OsStr) -> bool {
        nodes.get(id).is_some_and(|node| {
            node.path.as_os_str() == key || path_key(&node.path).as_os_str() == key
        })
    }

    fn get(&self, key: &std::ffi::OsStr, nodes: &[Node]) -> Option<NodeId> {
        self.get_hashed(self.state.hash_one(key), key, nodes)
    }

    fn get_hashed(&self, hash: u64, key: &std::ffi::OsStr, nodes: &[Node]) -> Option<NodeId> {
        let &first = self.shards[Self::shard(hash)].get(&hash)?;
        if Self::matches(nodes, first, key) {
            return Some(first);
        }
        self.collisions
            .get(&hash)?
            .iter()
            .copied()
            .find(|&id| Self::matches(nodes, id, key))
    }

    fn insert(&mut self, key: &std::ffi::OsStr, id: NodeId, nodes: &[Node]) {
        self.insert_hashed(self.state.hash_one(key), key, id, nodes);
    }

    fn insert_hashed(&mut self, hash: u64, key: &std::ffi::OsStr, id: NodeId, nodes: &[Node]) {
        let shard = Self::shard(hash);
        let Some(&first) = self.shards[shard].get(&hash) else {
            self.shards[shard].insert(hash, id);
            self.len += 1;
            return;
        };
        if Self::matches(nodes, first, key) {
            self.shards[shard].insert(hash, id);
            return;
        }
        let others = self.collisions.entry(hash).or_default();
        if let Some(existing) = others
            .iter_mut()
            .find(|other| Self::matches(nodes, **other, key))
        {
            *existing = id;
        } else {
            others.push(id);
            self.len += 1;
        }
    }

    fn remove(&mut self, key: &std::ffi::OsStr, nodes: &[Node]) -> Option<NodeId> {
        self.remove_hashed(self.state.hash_one(key), key, nodes)
    }

    fn remove_hashed(
        &mut self,
        hash: u64,
        key: &std::ffi::OsStr,
        nodes: &[Node],
    ) -> Option<NodeId> {
        let shard = Self::shard(hash);
        let &first = self.shards[shard].get(&hash)?;
        if Self::matches(nodes, first, key) {
            if let Some(replacement) = self.collisions.get_mut(&hash).and_then(Vec::pop) {
                self.shards[shard].insert(hash, replacement);
                if self.collisions.get(&hash).is_some_and(Vec::is_empty) {
                    self.collisions.remove(&hash);
                }
            } else {
                self.shards[shard].remove(&hash);
            }
            self.len -= 1;
            return Some(first);
        }
        let others = self.collisions.get_mut(&hash)?;
        let position = others
            .iter()
            .position(|&id| Self::matches(nodes, id, key))?;
        let removed = others.swap_remove(position);
        if others.is_empty() {
            self.collisions.remove(&hash);
        }
        self.len -= 1;
        Some(removed)
    }
}

/// Bounded, read-only work for an interactive filter. Mutations through apply
/// or append_scan, and replacement with a different LiveTree, invalidate it.
#[derive(Debug)]
pub struct FilterJob {
    filter: Filter,
    weights: Option<Vec<u64>>,
    work: FilterWork,
    identity: u64,
    revision: u64,
    source_len: usize,
    processed: usize,
    total: usize,
}

#[derive(Debug)]
enum FilterWork {
    Copy { next: usize },
    Ordered { next: usize },
    Traversal { stack: Vec<FilterFrame> },
    Complete,
}

#[derive(Debug)]
struct FilterFrame {
    id: NodeId,
    next_child: usize,
    sum: u64,
}

impl FilterJob {
    pub fn processed(&self) -> usize {
        self.processed
    }
    pub fn total(&self) -> usize {
        self.total
    }
    pub fn is_stale(&self, model: &LiveTree) -> bool {
        self.identity != model.identity
            || self.revision != model.revision
            || self.source_len != model.tree.nodes.len()
    }

    /// Inspect at most budget entries/edges. A flat directory never pushes its
    /// complete child list onto a stack. Returns true once commit can succeed.
    pub fn step(&mut self, model: &LiveTree, budget: usize) -> Result<bool, String> {
        if self.is_stale(model) {
            return Err("Filter source changed; restart the filter job.".into());
        }
        if matches!(self.work, FilterWork::Complete) {
            return Ok(true);
        }
        let weights = self.weights.as_mut().expect("unfinished jobs own weights");
        for _ in 0..budget {
            match &mut self.work {
                FilterWork::Copy { next } => {
                    if *next == model.tree.nodes.len() {
                        self.work = FilterWork::Complete;
                        break;
                    }
                    weights[*next] = if model.contains(*next) {
                        model.tree.nodes[*next].size
                    } else {
                        0
                    };
                    *next += 1;
                    self.processed += 1;
                    if *next == model.tree.nodes.len() {
                        self.work = FilterWork::Complete;
                        break;
                    }
                }
                FilterWork::Ordered { next } => {
                    if *next == 0 {
                        self.work = FilterWork::Complete;
                        break;
                    }
                    *next -= 1;
                    let id = *next;
                    let node = &model.tree.nodes[id];
                    if !node.is_dir() {
                        weights[id] = if self.filter.matches(node) {
                            node.size
                        } else {
                            0
                        };
                    }
                    if let Some(parent) = node.parent {
                        weights[parent] = weights[parent].saturating_add(weights[id]);
                    }
                    self.processed += 1;
                    if *next == 0 {
                        self.work = FilterWork::Complete;
                        break;
                    }
                }
                FilterWork::Traversal { stack } => {
                    let Some(frame) = stack.last_mut() else {
                        self.work = FilterWork::Complete;
                        break;
                    };
                    let node = &model.tree.nodes[frame.id];
                    if !node.is_dir() {
                        let value = if self.filter.matches(node) {
                            node.size
                        } else {
                            0
                        };
                        weights[frame.id] = value;
                        stack.pop();
                        if let Some(parent) = stack.last_mut() {
                            parent.sum = parent.sum.saturating_add(value);
                        }
                        self.processed += 1;
                    } else if let Some(&child) = node.children.get(frame.next_child) {
                        frame.next_child += 1;
                        if model.contains(child) {
                            let child_node = &model.tree.nodes[child];
                            if child_node.is_dir() {
                                stack.push(FilterFrame {
                                    id: child,
                                    next_child: 0,
                                    sum: 0,
                                });
                            } else {
                                let value = if self.filter.matches(child_node) {
                                    child_node.size
                                } else {
                                    0
                                };
                                weights[child] = value;
                                frame.sum = frame.sum.saturating_add(value);
                                self.processed += 1;
                            }
                        }
                    } else {
                        weights[frame.id] = frame.sum;
                        let value = frame.sum;
                        stack.pop();
                        if let Some(parent) = stack.last_mut() {
                            parent.sum = parent.sum.saturating_add(value);
                        }
                        self.processed += 1;
                    }
                    if stack.is_empty() {
                        self.work = FilterWork::Complete;
                        break;
                    }
                }
                FilterWork::Complete => break,
            }
        }
        Ok(matches!(self.work, FilterWork::Complete))
    }
}

fn next_identity() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, AtomicOrdering::Relaxed)
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

fn child_key(parent: &Path, name: &std::ffi::OsStr) -> PathBuf {
    #[cfg(windows)]
    {
        if let Some(text) = name.to_str() {
            if text.is_ascii() {
                // The usual lowercase ASCII scanner name needs no temporary
                // UTF-16 buffers or leaf allocation at all.
                if text.bytes().any(|byte| byte.is_ascii_uppercase()) {
                    return join_key(parent, std::ffi::OsStr::new(&text.to_ascii_lowercase()));
                }
                return join_key(parent, name);
            }
            return join_key(parent, std::ffi::OsStr::new(&text.to_lowercase()));
        }
        join_key(parent, windows_path(Path::new(name)).as_os_str())
    }
    #[cfg(not(windows))]
    join_key(parent, name)
}

fn join_key(parent: &Path, name: &std::ffi::OsStr) -> PathBuf {
    let mut joined = std::ffi::OsString::with_capacity(parent.as_os_str().len() + name.len() + 1);
    joined.push(parent.as_os_str());
    if !parent
        .as_os_str()
        .as_encoded_bytes()
        .last()
        .is_some_and(|&last| last == b'/' || (cfg!(windows) && last == b'\\'))
    {
        joined.push(std::path::MAIN_SEPARATOR_STR);
    }
    // A scanner filename is a normal component. Appending its raw spelling
    // also preserves a Unicode leaf whose lowercase begins like a drive name.
    joined.push(name);
    PathBuf::from(joined)
}

/// Lexical identity, without canonicalization (which would follow links and
/// would fail for removed paths). Windows extended prefixes and case variants
/// normalize to the same key, while unpaired UTF-16 units are preserved.
pub(crate) fn path_key(path: &Path) -> PathBuf {
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_owned());
    #[cfg(windows)]
    let absolute = windows_path(&absolute);
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

    fn preview_seed(kind: EntryKind, size: u64) -> LiveTree {
        let root = PathBuf::from("preview-fixture");
        let mut seed = node(&root, "", kind, size, None);
        seed.path = root;
        LiveTree::new(ScanTree {
            nodes: vec![seed],
            root: 0,
            stats: ScanStats {
                files: u64::from(kind == EntryKind::File),
                directories: u64::from(kind == EntryKind::Directory),
                symlinks: u64::from(kind == EntryKind::Symlink),
                ..ScanStats::default()
            },
        })
    }

    #[test]
    fn preview_chunks_fold_new_and_existing_ancestors_with_active_filters() {
        let mut live = preview_seed(EntryKind::Directory, 0);
        let root = live.tree.nodes[0].path.clone();
        live.set_filter("*.txt;>5B").unwrap();
        let first = vec![
            node(&root, "a", EntryKind::Directory, 0, Some(0)),
            node(&root, "keep.txt", EntryKind::File, 4, Some(0)),
            node(&root, "a/large.txt", EntryKind::File, 12, Some(1)),
            node(&root, "a/link", EntryKind::Symlink, 0, Some(1)),
        ];
        let summary = live.append_scan(1, first).unwrap();
        assert_eq!((summary.changed, summary.inspected), (4, 9));
        assert_eq!(live.tree.total_size(), 16);
        assert_eq!(live.weights, [12, 12, 0, 12, 0]);
        assert_eq!(
            (
                live.tree.stats.files,
                live.tree.stats.directories,
                live.tree.stats.symlinks
            ),
            (2, 2, 1)
        );
        live.set_filter("*.txt").unwrap();
        let second = vec![
            node(&root, "a/deep", EntryKind::Directory, 0, Some(1)),
            node(&root, "a/deep/other.bin", EntryKind::File, 7, Some(5)),
            node(&root, "a/deep/new.txt", EntryKind::File, 3, Some(5)),
            node(&root, "another.bin", EntryKind::File, 10, Some(0)),
        ];
        let summary = live.append_scan(5, second).unwrap();
        assert_eq!((summary.changed, summary.inspected), (4, 10));
        assert_eq!(live.tree.total_size(), 36);
        assert_eq!((live.tree.nodes[1].size, live.tree.nodes[5].size), (22, 10));
        assert_eq!(
            (live.weights[0], live.weights[1], live.weights[5]),
            (19, 15, 3)
        );
        live.set_filter(">10B").unwrap();
        let summary = live
            .append_scan(
                9,
                vec![node(
                    &root,
                    "a/deep/extra.txt",
                    EntryKind::File,
                    20,
                    Some(5),
                )],
            )
            .unwrap();
        assert_eq!((summary.changed, summary.inspected), (1, 5));
        assert_eq!(live.tree.total_size(), 56);
        assert_eq!((live.tree.nodes[1].size, live.tree.nodes[5].size), (42, 30));
        assert_eq!(
            (live.weights[0], live.weights[1], live.weights[5]),
            (32, 32, 20)
        );
        assert_eq!(live.find(&root.join("a/deep/extra.txt")), Some(9));
        assert_eq!(
            (
                live.tree.stats.files,
                live.tree.stats.directories,
                live.tree.stats.symlinks
            ),
            (6, 3, 1)
        );
    }

    #[test]
    fn invalid_preview_chunk_is_atomic_including_prior_valid_entries() {
        let mut live = preview_seed(EntryKind::Directory, 0);
        let root = live.tree.nodes[0].path.clone();
        let valid = || node(&root, "file.txt", EntryKind::File, 7, Some(0));
        let mut with_children = valid();
        with_children.children.push(99);
        let cases = vec![
            (2, vec![valid()]),
            (1, vec![valid(), valid()]),
            (
                1,
                vec![node(&root, "future.txt", EntryKind::File, 1, Some(2))],
            ),
            (
                1,
                vec![
                    valid(),
                    node(&root, "file.txt/child", EntryKind::File, 1, Some(1)),
                ],
            ),
            (
                1,
                vec![
                    node(&root, "link", EntryKind::Symlink, 0, Some(0)),
                    node(&root, "link/outside", EntryKind::File, 1, Some(1)),
                ],
            ),
            (
                1,
                vec![node(&root, "nested/child", EntryKind::File, 1, Some(0))],
            ),
            (
                1,
                vec![node(&root, "../outside", EntryKind::File, 1, Some(0))],
            ),
            (1, vec![with_children]),
            (
                1,
                vec![node(
                    &root,
                    "bad-directory",
                    EntryKind::Directory,
                    1,
                    Some(0),
                )],
            ),
            (
                1,
                vec![node(&root, "bad-link", EntryKind::Symlink, 1, Some(0))],
            ),
            (1, vec![node(&root, "no-parent", EntryKind::File, 1, None)]),
            (1, vec![node(&root, "", EntryKind::Directory, 0, Some(0))]),
            (1, vec![valid(); 8193]),
        ];
        let original = format!("{live:?}");
        for (start, nodes) in cases {
            let error = live.append_scan(start, nodes).unwrap_err();
            assert!(!error.is_empty());
            assert_eq!(format!("{live:?}"), original, "{error}");
        }
        live.append_scan(1, vec![valid()]).unwrap();
        let before = format!("{live:?}");
        assert!(live.append_scan(2, vec![valid()]).is_err());
        assert!(
            live.append_scan(
                2,
                vec![node(&root, "file.txt/child", EntryKind::File, 1, Some(1))]
            )
            .is_err()
        );
        assert_eq!(format!("{live:?}"), before);
        assert_eq!(live.find(&root.join("file.txt")), Some(1));
    }

    #[test]
    fn preview_flat_hundred_thousand_files_have_linear_inspection_work() {
        const FILES: usize = 100_000;
        let mut live = preview_seed(EntryKind::Directory, 0);
        live.set_filter("*.bin;>3B").unwrap();
        let root = live.tree.nodes[0].path.clone();
        let (mut total, mut visible, mut inspected, mut chunks) = (0_u64, 0_u64, 0, 0);
        for first in (0..FILES).step_by(8192) {
            let chunk: Vec<_> = (first..(first + 8192).min(FILES))
                .map(|index| {
                    let size = (index % 7 + 1) as u64;
                    total += size;
                    if size > 3 {
                        visible += size;
                    }
                    node(
                        &root,
                        &format!("f{index:06}.bin"),
                        EntryKind::File,
                        size,
                        Some(0),
                    )
                })
                .collect();
            inspected += live.append_scan(first + 1, chunk).unwrap().inspected;
            chunks += 1;
            assert_eq!(live.tree.total_size(), total);
            assert_eq!(live.weights[0], visible);
        }
        assert_eq!(inspected, FILES * 2 + chunks, "root visited once per chunk");
        assert_eq!(live.tree.stats.files, FILES as u64);
        assert_eq!(live.tree.nodes[0].children, (1..=FILES).collect::<Vec<_>>());
        assert_eq!(live.tree.nodes.len(), FILES + 1);
    }

    #[test]
    fn preview_discovery_order_can_transition_to_live_updates_once() {
        let mut live = preview_seed(EntryKind::Directory, 0);
        let root = live.tree.nodes[0].path.clone();
        live.append_scan(
            1,
            vec![
                node(&root, "small", EntryKind::File, 1, Some(0)),
                node(&root, "large", EntryKind::File, 100, Some(0)),
                node(&root, "medium", EntryKind::File, 50, Some(0)),
            ],
        )
        .unwrap();
        assert_eq!(live.tree.nodes[0].children, [1, 2, 3]);
        live.apply(&[upsert(root.join("small"), 200)]);
        assert_eq!(live.tree.nodes[0].children, [1, 2, 3]);
        assert_eq!(live.tree.total_size(), 350);
        assert!(
            live.append_scan(4, vec![node(&root, "late", EntryKind::File, 1, Some(0))])
                .is_err()
        );
        assert_eq!(live.append_scan(4, Vec::new()).unwrap().changed, 0);
    }

    #[test]
    fn file_root_accepts_empty_preview_but_rejects_children() {
        let mut live = preview_seed(EntryKind::File, 12);
        let root = live.tree.nodes[0].path.clone();
        assert_eq!(live.append_scan(1, Vec::new()).unwrap().inspected, 0);
        assert!(
            live.append_scan(1, vec![node(&root, "child", EntryKind::File, 2, Some(0))])
                .is_err()
        );
        assert_eq!(live.tree.total_size(), 12);
        assert_eq!(live.weights, [12]);
        assert_eq!((live.tree.stats.files, live.tree.stats.directories), (1, 0));
    }

    #[test]
    fn preview_fast_keys_agree_with_full_normalization_and_parent_aliases() {
        let mut live = preview_seed(EntryKind::Directory, 0);
        let root = live.tree.nodes[0].path.clone();
        let names = ["UPPER.TXT", "фОТО🦀.txt", "İstanbul.bin"];
        live.append_scan(
            1,
            names
                .iter()
                .map(|name| node(&root, name, EntryKind::File, 1, Some(0)))
                .collect(),
        )
        .unwrap();
        for (offset, name) in names.iter().enumerate() {
            let path = std::path::absolute(root.join(name)).unwrap();
            assert_eq!(live.find(&path), Some(offset + 1));
        }
        let absolute_root = std::path::absolute(&root).unwrap();
        live.append_scan(
            4,
            vec![node(
                &absolute_root,
                "alias.txt",
                EntryKind::File,
                2,
                Some(0),
            )],
        )
        .unwrap();
        assert_eq!(live.find(&root.join("alias.txt")), Some(4));
        assert_eq!(live.tree.total_size(), 5);
    }

    #[test]
    fn filter_jobs_are_bounded_and_atomically_install_complete_weights() {
        let mut live = LiveTree::new(tree());
        let original = live.weights.clone();
        let mut job = live.begin_filter("*.txt").unwrap();
        assert_eq!(job.total(), 5);
        assert!(!job.step(&live, 0).unwrap());
        for step in 1..=5 {
            let before = job.processed();
            let done = job.step(&live, 1).unwrap();
            assert!(job.processed() - before <= 1);
            assert_eq!(live.weights, original);
            assert_eq!(done, step == 5);
        }
        live.commit_filter(job).unwrap();
        assert_eq!(live.weights, [30, 30, 0, 30, 0]);
        let repeat = live.begin_filter("*.TXT").unwrap();
        assert!(matches!(repeat.work, FilterWork::Complete));
        assert!(repeat.weights.is_none());
        let mut clear = live.begin_filter("").unwrap();
        assert!(!clear.step(&live, 1).unwrap());
        assert_eq!(clear.processed(), 1);
        assert!(clear.step(&live, 10).unwrap());
        live.commit_filter(clear).unwrap();
        assert_eq!(live.weights, original);
    }

    #[test]
    fn incomplete_and_stale_filter_jobs_never_change_active_weights() {
        let mut live = LiveTree::new(tree());
        let original = live.weights.clone();
        let incomplete = live.begin_filter("*.txt").unwrap();
        assert!(live.commit_filter(incomplete).is_err());
        assert_eq!(live.weights, original);
        let mut stale = live.begin_filter("*.txt").unwrap();
        stale.step(&live, 1).unwrap();
        let path = live.tree.nodes[3].path.clone();
        live.apply(&[upsert(path, 11)]);
        let current = live.weights.clone();
        assert!(stale.is_stale(&live));
        assert!(stale.step(&live, 100).is_err());
        assert!(live.commit_filter(stale).is_err());
        assert_eq!(live.weights, current);
        let replacement = LiveTree::new(tree());
        assert!(live.begin_filter("*.txt").unwrap().is_stale(&replacement));
    }

    #[test]
    fn filter_job_traverses_recycled_ids_without_a_whole_child_stack() {
        let mut live = LiveTree::new(tree());
        let old = live.tree.nodes[1].path.clone();
        live.apply(&[Change::Remove(old)]);
        let path = live.tree.nodes[2].path.join("new.txt");
        live.apply(&[upsert(path, 11)]);
        let original = live.weights.clone();
        let mut job = live.begin_filter("*.txt").unwrap();
        let mut iterations = 0;
        loop {
            let before = job.processed();
            let done = job.step(&live, 1).unwrap();
            assert!(job.processed() - before <= 1);
            if let FilterWork::Traversal { stack } = &job.work {
                assert!(stack.len() <= 2);
            }
            assert_eq!(live.weights, original);
            iterations += 1;
            if done {
                break;
            }
            assert!(iterations < 20);
        }
        live.commit_filter(job).unwrap();
        assert_eq!(live.weights[0], 11);
        assert_eq!(live.weights[2], 11);
    }

    #[test]
    fn successful_preview_append_invalidates_a_running_filter_job() {
        let mut live = preview_seed(EntryKind::Directory, 0);
        let mut job = live.begin_filter("*.txt").unwrap();
        let root = live.tree.nodes[0].path.clone();
        live.append_scan(
            1,
            vec![node(&root, "file.txt", EntryKind::File, 3, Some(0))],
        )
        .unwrap();
        assert!(job.is_stale(&live));
        assert!(job.step(&live, 1).is_err());
    }

    #[test]
    fn committing_a_filter_invalidates_competing_unchanged_jobs() {
        let mut live = LiveTree::new(tree());
        let unchanged = live.begin_filter("").unwrap();
        assert!(unchanged.weights.is_none());
        let mut different = live.begin_filter("*.txt").unwrap();
        assert!(different.step(&live, 100).unwrap());
        live.commit_filter(different).unwrap();
        let weights = live.weights.clone();
        assert!(unchanged.is_stale(&live));
        assert!(live.commit_filter(unchanged).is_err());
        assert_eq!(live.weights, weights);
        assert_eq!(live.weights[0], 30);
    }

    #[test]
    fn compact_index_resolves_collisions_replaces_and_removes_exact_paths() {
        let mut nodes = tree().nodes;
        let mut index = PathIndex::with_capacity(0);
        let hash = 0x1234_5678_9abc_def0;
        let keys: Vec<_> = nodes.iter().map(|node| path_key(&node.path)).collect();
        for (id, key) in keys.iter().enumerate() {
            index.insert_hashed(hash, key.as_os_str(), id, &nodes);
        }
        assert_eq!(index.len(), keys.len());
        for (id, key) in keys.iter().enumerate() {
            assert_eq!(index.get_hashed(hash, key.as_os_str(), &nodes), Some(id));
        }
        nodes.push(nodes[2].clone());
        index.insert_hashed(hash, keys[2].as_os_str(), 5, &nodes);
        assert_eq!(index.len(), keys.len());
        assert_eq!(index.get_hashed(hash, keys[2].as_os_str(), &nodes), Some(5));
        assert_eq!(
            index.remove_hashed(hash, keys[2].as_os_str(), &nodes),
            Some(5)
        );
        assert_eq!(
            index.remove_hashed(hash, keys[0].as_os_str(), &nodes),
            Some(0)
        );
        for id in [1, 3, 4] {
            assert_eq!(
                index.get_hashed(hash, keys[id].as_os_str(), &nodes),
                Some(id)
            );
            assert_eq!(
                index.remove_hashed(hash, keys[id].as_os_str(), &nodes),
                Some(id)
            );
        }
        assert_eq!(index.len(), 0);
        assert!(index.collisions.is_empty());
    }

    #[cfg(windows)]
    #[test]
    fn compact_index_preserves_distinct_invalid_utf16_and_reused_slots() {
        use std::{ffi::OsString, os::windows::ffi::OsStringExt};
        let root = std::env::temp_dir().join("space-sniffer-invalid-utf16-index");
        let name = |surrogate| OsString::from_wide(&[b'f' as u16, surrogate, b'x' as u16]);
        let first = root.join(name(0xd800));
        let second = root.join(name(0xd801));
        let mut nodes = vec![
            node(&root, "", EntryKind::Directory, 3, None),
            Node {
                name: "lossy first".into(),
                path: first.clone(),
                kind: EntryKind::File,
                size: 1,
                children: Vec::new(),
                parent: Some(0),
                modified: None,
            },
            Node {
                name: "lossy second".into(),
                path: second.clone(),
                kind: EntryKind::File,
                size: 2,
                children: Vec::new(),
                parent: Some(0),
                modified: None,
            },
        ];
        nodes[0].path = root.clone();
        nodes[0].children = vec![2, 1];
        let mut live = LiveTree::new(ScanTree {
            nodes,
            root: 0,
            stats: ScanStats {
                files: 2,
                directories: 1,
                ..ScanStats::default()
            },
        });
        assert_eq!(live.find(&first), Some(1));
        assert_eq!(live.find(&second), Some(2));
        live.apply(&[Change::Remove(first.clone())]);
        let replacement = root.join("replacement.txt");
        live.apply(&[upsert(replacement.clone(), 4)]);
        assert_eq!(live.find(&replacement), Some(1));
        assert_eq!(live.find(&first), None);
        assert_eq!(live.find(&second), Some(2));
        assert_eq!(live.tree.total_size(), 6);
    }

    #[test]
    fn decreases_recompute_saturated_ancestors_and_filter_totals() {
        let root = PathBuf::from("saturated-live-fixture");
        let mut nodes = vec![
            node(&root, "", EntryKind::Directory, u64::MAX, None),
            node(&root, "dir", EntryKind::Directory, u64::MAX, Some(0)),
            node(&root, "dir/a.txt", EntryKind::File, u64::MAX, Some(1)),
            node(&root, "dir/b.txt", EntryKind::File, u64::MAX, Some(1)),
            node(&root, "other.bin", EntryKind::File, 1, Some(0)),
        ];
        nodes[0].children = vec![1, 4];
        nodes[1].children = vec![2, 3];
        let mut live = LiveTree::new(ScanTree {
            nodes,
            root: 0,
            stats: ScanStats {
                files: 3,
                directories: 2,
                ..ScanStats::default()
            },
        });
        live.set_filter("*.txt").unwrap();
        live.apply(&[upsert(root.join("dir/a.txt"), 9)]);
        assert_eq!(live.tree.nodes[1].size, u64::MAX);
        assert_eq!(live.tree.total_size(), u64::MAX);
        assert_eq!(live.weights[0], u64::MAX);
        live.apply(&[Change::Remove(root.join("dir/a.txt"))]);
        assert_eq!(live.tree.nodes[1].size, u64::MAX);
        assert_eq!(live.tree.total_size(), u64::MAX);
        assert_eq!(live.weights[0], u64::MAX);
        live.apply(&[upsert(root.join("dir/b.txt"), 5)]);
        assert_eq!(live.tree.nodes[1].size, 5);
        assert_eq!(live.tree.total_size(), 6);
        assert_eq!(live.weights[1], 5);
        assert_eq!(live.weights[0], 5);
        assert_eq!(live.tree.stats.files, 2);
    }
}
