//! Native watching established before the initial scan, with bounded queues,
//! coalescing, incremental file updates, and explicit resync/polling recovery.

use crate::{
    live_tree::{LiveTree, path_key},
    model::{EntryKind, Node, ScanTree},
    scanner::{self, ScanProgress},
};
use notify::{
    Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher,
    event::{MetadataKind, ModifyKind},
};
use std::{
    collections::{HashMap, HashSet, hash_map::DefaultHasher},
    fs::{self, Metadata},
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError},
    },
    thread,
    time::{Duration, Instant, SystemTime},
};

const DEBOUNCE: Duration = Duration::from_millis(100);
const MAX_DEBOUNCE: Duration = Duration::from_millis(250);
const ROOT_PROBE: Duration = Duration::from_millis(500);
const POLL_INTERVAL: Duration = Duration::from_secs(2);
const WATCH_RETRY: Duration = Duration::from_secs(5);
const RAW_CAPACITY: usize = 1024;
const OUTPUT_CAPACITY: usize = 8;
const MAX_PENDING: usize = 4096;
const PREVIEW_CHUNK: usize = 8192;

#[derive(Clone, Debug)]
pub enum Change {
    Upsert {
        path: PathBuf,
        kind: EntryKind,
        size: u64,
        modified: Option<SystemTime>,
    },
    Remove(PathBuf),
    ReplaceSubtree {
        path: PathBuf,
        tree: ScanTree,
    },
}

#[derive(Debug)]
pub enum LiveEvent {
    Progress(ScanProgress),
    /// A bounded, contiguous prefix of the first scan. The root is sent alone
    /// at start=0. Directory sizes/children are rebuilt by the receiving model.
    Preview {
        start: usize,
        nodes: Vec<Node>,
    },
    Ready(Box<LiveTree>),
    Changes(Vec<Change>),
    Status {
        watching: bool,
        message: String,
    },
    Failed(String),
}

pub struct LiveHandle {
    pub receiver: Receiver<LiveEvent>,
    pub cancel: Arc<AtomicBool>,
}

impl Drop for LiveHandle {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

pub fn start_live(path: PathBuf) -> LiveHandle {
    start_live_with_wake(path, || {})
}

/// The wake callback is called after each delivered event, and while a full
/// output queue waits for the UI. It should be fast, e.g. request_repaint().
pub fn start_live_with_wake(path: PathBuf, wake: impl Fn() + Send + Sync + 'static) -> LiveHandle {
    let (sender, receiver) = mpsc::sync_channel(OUTPUT_CAPACITY);
    let cancel = Arc::new(AtomicBool::new(false));
    let sink = Sink {
        sender,
        cancel: Arc::clone(&cancel),
        wake: Arc::new(wake),
    };
    let failed = sink.clone();
    if let Err(error) = thread::Builder::new()
        .name("filesystem-live".into())
        .spawn(move || Worker::new(path, sink).run())
    {
        failed.try_event(LiveEvent::Failed(format!(
            "Unable to start live scanning: {error}"
        )));
    }
    LiveHandle { receiver, cancel }
}

#[derive(Clone)]
struct Sink {
    sender: SyncSender<LiveEvent>,
    cancel: Arc<AtomicBool>,
    wake: Arc<dyn Fn() + Send + Sync>,
}

impl Sink {
    fn send(&self, mut event: LiveEvent) -> bool {
        loop {
            if self.cancel.load(Ordering::Relaxed) {
                return false;
            }
            match self.sender.try_send(event) {
                Ok(()) => {
                    (self.wake)();
                    return true;
                }
                Err(TrySendError::Full(returned)) => {
                    event = returned;
                    (self.wake)();
                    thread::sleep(Duration::from_millis(10));
                }
                Err(TrySendError::Disconnected(_)) => {
                    self.cancel.store(true, Ordering::Relaxed);
                    return false;
                }
            }
        }
    }

    fn try_event(&self, event: LiveEvent) {
        match self.sender.try_send(event) {
            Ok(()) => (self.wake)(),
            Err(TrySendError::Disconnected(_)) => self.cancel.store(true, Ordering::Relaxed),
            Err(TrySendError::Full(_)) => (self.wake)(),
        }
    }
}

#[derive(Default)]
struct PreviewPublisher {
    next: usize,
}

impl PreviewPublisher {
    fn publish(&mut self, tree: &ScanTree, sink: &Sink) {
        if self.next >= tree.nodes.len() || sink.cancel.load(Ordering::Relaxed) {
            return;
        }
        let end = if self.next == 0 {
            1
        } else {
            (self.next + PREVIEW_CHUNK).min(tree.nodes.len())
        };
        // Copy each delivered node only once, never its growing child list.
        // Slow/hidden UIs may lag; the authoritative Ready event supersedes
        // any remaining prefix. A full queue never stalls filesystem workers.
        let nodes = tree.nodes[self.next..end]
            .iter()
            .map(|node| Node {
                name: node.name.clone(),
                path: node.path.clone(),
                kind: node.kind,
                size: if node.kind == EntryKind::File {
                    node.size
                } else {
                    0
                },
                children: Vec::new(),
                parent: node.parent,
                modified: node.modified,
            })
            .collect();
        match sink.sender.try_send(LiveEvent::Preview {
            start: self.next,
            nodes,
        }) {
            Ok(()) => {
                self.next = end;
                (sink.wake)();
            }
            Err(TrySendError::Full(_)) => (sink.wake)(),
            Err(TrySendError::Disconnected(_)) => sink.cancel.store(true, Ordering::Relaxed),
        }
    }
}

#[derive(Default)]
struct Pending {
    paths: HashMap<PathBuf, Request>,
    first: Option<Instant>,
    last: Option<Instant>,
    resync: bool,
}

struct Request {
    path: PathBuf,
    recursive: bool,
    structural: bool,
}

impl Pending {
    fn touch(&mut self) {
        let now = Instant::now();
        self.first.get_or_insert(now);
        self.last = Some(now);
    }

    fn force_resync(&mut self) {
        self.resync = true;
        self.paths.clear();
        self.touch();
    }

    fn add(&mut self, path: PathBuf, recursive: bool) {
        self.add_event(path, recursive, false);
    }

    fn add_event(&mut self, path: PathBuf, recursive: bool, structural: bool) {
        self.touch();
        if self.resync {
            return;
        }
        let entry = self
            .paths
            .entry(path_key(&path))
            .or_insert_with(|| Request {
                path: path.clone(),
                recursive: false,
                structural: false,
            });
        entry.path = path;
        entry.recursive |= recursive;
        entry.structural |= structural;
        if self.paths.len() > MAX_PENDING {
            self.force_resync();
        }
    }

    fn ready(&self, now: Instant) -> bool {
        self.first
            .is_some_and(|first| now.duration_since(first) >= MAX_DEBOUNCE)
            || self
                .last
                .is_some_and(|last| now.duration_since(last) >= DEBOUNCE)
    }
}

struct Worker {
    root: PathBuf,
    root_key: PathBuf,
    sink: Sink,
    raw_sender: SyncSender<notify::Result<Event>>,
    raw_receiver: Receiver<notify::Result<Event>>,
    overflow: Arc<AtomicBool>,
    watchers: Vec<RecommendedWatcher>,
    native: bool,
    initialized: bool,
    initial_failure_sent: bool,
    root_present: bool,
    root_kind: Option<EntryKind>,
    pending: Pending,
    last_status: Option<(bool, String)>,
    last_poll: Instant,
    last_probe: Instant,
    last_retry: Instant,
    fingerprint: Option<u64>,
}

impl Worker {
    fn new(path: PathBuf, sink: Sink) -> Self {
        let root = watch_root(path);
        let (raw_sender, raw_receiver) = mpsc::sync_channel(RAW_CAPACITY);
        Self {
            root_key: path_key(&root),
            root,
            sink,
            raw_sender,
            raw_receiver,
            overflow: Arc::new(AtomicBool::new(false)),
            watchers: Vec::new(),
            native: false,
            initialized: false,
            initial_failure_sent: false,
            root_present: false,
            root_kind: None,
            pending: Pending::default(),
            last_status: None,
            last_poll: Instant::now(),
            last_probe: Instant::now(),
            last_retry: Instant::now(),
            fingerprint: None,
        }
    }

    fn cancelled(&self) -> bool {
        self.sink.cancel.load(Ordering::Relaxed)
    }

    fn status(&mut self, watching: bool, message: String) {
        if self.last_status.as_ref() == Some(&(watching, message.clone())) {
            return;
        }
        self.last_status = Some((watching, message.clone()));
        self.sink.send(LiveEvent::Status { watching, message });
    }

    fn watching_status(&mut self) {
        if self.native {
            self.status(
                true,
                "Live updates active · native filesystem watcher".into(),
            );
        } else {
            self.status(
                false,
                "Native watching unavailable · polling every 2s; retrying native watcher".into(),
            );
        }
    }

    fn install_watch(&mut self) {
        // Rebuild after a root replacement/error so stale native handles do
        // not remain attached to a deleted inode. Setup always precedes scan.
        self.watchers.clear();
        self.native = false;
        self.last_retry = Instant::now();
        let root_directory = fs::symlink_metadata(&self.root)
            .is_ok_and(|metadata| metadata.is_dir() && !is_link(&metadata));
        let mut installed = false;
        let mut failure = None;
        match self.make_watcher(true, root_directory) {
            Ok(mut parent_watcher) => {
                // A separate callback avoids treating a root LastWriteTime
                // notification from Windows' parent watch as a subtree change.
                for ancestor in self.root.ancestors().skip(1) {
                    #[cfg(target_os = "macos")]
                    if ancestor.parent().is_none() {
                        // Do not add a recursive FSEvents subscription to / as
                        // an incidental parent watch. Explicitly scanning /
                        // still installs the requested root watcher below.
                        break;
                    }
                    if fs::symlink_metadata(ancestor)
                        .is_ok_and(|metadata| metadata.is_dir() && !is_link(&metadata))
                    {
                        match parent_watcher.watch(ancestor, RecursiveMode::NonRecursive) {
                            Ok(()) => installed = true,
                            Err(error) => failure = Some(error.to_string()),
                        }
                        // FSEvents filters nonrecursive notifications in its
                        // callback. Stop at the nearest existing parent; the
                        // root probe detects missing/recreated roots.
                        #[cfg(target_os = "macos")]
                        break;
                    }
                }
                self.watchers.push(parent_watcher);
            }
            Err(error) => failure = Some(error.to_string()),
        }
        if root_directory {
            match self.make_watcher(false, true) {
                Ok(mut root_watcher) => {
                    match root_watcher.watch(&self.root, RecursiveMode::Recursive) {
                        Ok(()) => installed = true,
                        Err(error) => failure = Some(error.to_string()),
                    }
                    self.watchers.push(root_watcher);
                }
                Err(error) => failure = Some(error.to_string()),
            }
        }
        self.native = installed && failure.is_none();
        if let Some(error) = failure {
            self.status(false, format!("Watcher error ({error}) · polling every 2s"));
        } else if !installed {
            self.status(false, "No watchable ancestor · polling every 2s".into());
        }
    }

    fn make_watcher(
        &self,
        parent_only: bool,
        root_directory: bool,
    ) -> notify::Result<RecommendedWatcher> {
        let sender = self.raw_sender.clone();
        let overflow = Arc::clone(&self.overflow);
        let root_key = self.root_key.clone();
        let cancel = Arc::clone(&self.sink.cancel);
        RecommendedWatcher::new(
            move |result: notify::Result<Event>| {
                if cancel.load(Ordering::Relaxed) {
                    return;
                }
                if let Ok(event) = &result {
                    if ignored(event) {
                        return;
                    }
                    if parent_only
                        && !event.need_rescan()
                        && matches!(
                            event.kind,
                            EventKind::Any
                                | EventKind::Modify(
                                    ModifyKind::Any
                                        | ModifyKind::Data(_)
                                        | ModifyKind::Metadata(_)
                                        | ModifyKind::Other
                                )
                        )
                        && (root_directory
                            || !event.paths.iter().any(|path| path_key(path) == root_key))
                    {
                        return;
                    }
                    if !event.need_rescan()
                        && !event.paths.is_empty()
                        && !event.paths.iter().any(|path| {
                            let key = path_key(path);
                            key.starts_with(&root_key) || root_key.starts_with(&key)
                        })
                    {
                        return;
                    }
                }
                match sender.try_send(result) {
                    Ok(()) => {}
                    Err(TrySendError::Full(_)) => overflow.store(true, Ordering::Relaxed),
                    Err(TrySendError::Disconnected(_)) => {}
                }
            },
            Config::default().with_follow_symlinks(false),
        )
    }

    fn run(mut self) {
        let _priority = scanner::BackgroundPriority::enter();
        self.install_watch();
        self.full_scan();
        while !self.cancelled() {
            match self.raw_receiver.recv_timeout(Duration::from_millis(40)) {
                Ok(event) => self.collect(event),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
            // Keep collection bounded as well: a continuous writer must not
            // postpone delivery indefinitely by keeping this queue nonempty.
            for _ in 0..RAW_CAPACITY {
                match self.raw_receiver.try_recv() {
                    Ok(event) => self.collect(event),
                    Err(_) => break,
                }
            }
            if self.overflow.swap(false, Ordering::Relaxed) {
                self.pending.force_resync();
            }
            let now = Instant::now();
            if self.pending.ready(now) {
                self.process_pending();
            }
            if now.duration_since(self.last_probe) >= ROOT_PROBE {
                self.last_probe = now;
                let kind = fs::symlink_metadata(&self.root)
                    .ok()
                    .map(|meta| entry_kind(&meta));
                if kind != self.root_kind {
                    self.pending.add(self.root.clone(), true);
                }
            }
            if !self.native && now.duration_since(self.last_retry) >= WATCH_RETRY {
                self.install_watch();
                if self.native {
                    self.pending.force_resync();
                }
            }
            if (!self.native || !self.initialized)
                && now.duration_since(self.last_poll) >= POLL_INTERVAL
            {
                self.full_scan();
            }
        }
    }

    fn collect(&mut self, result: notify::Result<Event>) {
        match result {
            Err(error) => {
                self.native = false;
                self.status(
                    false,
                    format!("Watcher error ({error}) · resyncing, then polling"),
                );
                self.pending.force_resync();
            }
            Ok(event) => {
                if ignored(&event) {
                    return;
                }
                if event.need_rescan() || event.paths.is_empty() {
                    self.pending.force_resync();
                    return;
                }
                let metadata_only = matches!(
                    event.kind,
                    EventKind::Modify(ModifyKind::Data(_))
                        | EventKind::Modify(ModifyKind::Metadata(MetadataKind::WriteTime))
                ) || (cfg!(windows)
                    && matches!(event.kind, EventKind::Modify(ModifyKind::Any)));
                // ReadDirectoryChangesW reports FILE_ACTION_MODIFIED as
                // Modify(Any), including delayed directory LastWriteTime.
                // Child membership has separate add/remove/rename actions.
                let recursive = !metadata_only;
                let structural = matches!(
                    event.kind,
                    EventKind::Create(_)
                        | EventKind::Remove(_)
                        | EventKind::Modify(ModifyKind::Name(_))
                ) || matches!(
                    event.kind,
                    EventKind::Modify(ModifyKind::Metadata(kind))
                        if !matches!(kind, MetadataKind::AccessTime | MetadataKind::WriteTime | MetadataKind::Any | MetadataKind::Other)
                );
                for path in event.paths {
                    let key = path_key(&path);
                    if key.starts_with(&self.root_key) {
                        self.pending.add_event(path, recursive, structural);
                    } else if self.root_key.starts_with(&key) {
                        self.pending
                            .add_event(self.root.clone(), structural, structural);
                    }
                }
            }
        }
    }

    fn full_scan(&mut self) {
        if self.cancelled() {
            return;
        }
        self.last_poll = Instant::now();
        let sink = self.sink.clone();
        let report = |progress| {
            sink.try_event(LiveEvent::Progress(progress));
        };
        let result = if self.initialized {
            scanner::scan_with_progress(&self.root, &self.sink.cancel, report)
        } else {
            self.status(false, "Scanning · the map fills as files are found".into());
            let mut previews = PreviewPublisher::default();
            scanner::scan_with_observer(&self.root, &self.sink.cancel, report, |tree| {
                previews.publish(tree, &sink);
            })
        };
        match result {
            Ok(tree) => {
                self.root_present = true;
                self.root_kind = Some(tree.nodes[tree.root].kind);
                let fingerprint = tree_fingerprint(&tree);
                if !self.initialized {
                    self.initialized = self
                        .sink
                        .send(LiveEvent::Ready(Box::new(LiveTree::new(tree))));
                } else if self.fingerprint != Some(fingerprint) {
                    self.sink
                        .send(LiveEvent::Changes(vec![Change::ReplaceSubtree {
                            path: self.root.clone(),
                            tree,
                        }]));
                }
                self.fingerprint = Some(fingerprint);
                self.watching_status();
            }
            Err(error) if !self.cancelled() => {
                let exists = fs::symlink_metadata(&self.root).is_ok();
                if self.initialized && self.root_present && !exists {
                    self.sink
                        .send(LiveEvent::Changes(vec![Change::Remove(self.root.clone())]));
                }
                self.root_present = exists;
                self.root_kind = fs::symlink_metadata(&self.root)
                    .ok()
                    .map(|meta| entry_kind(&meta));
                if !self.initialized && !self.initial_failure_sent {
                    self.initial_failure_sent = true;
                    self.sink.send(LiveEvent::Failed(error.clone()));
                }
                self.status(false, format!("{error} · waiting for recovery"));
            }
            Err(_) => {}
        }
    }

    fn process_pending(&mut self) {
        let pending = std::mem::take(&mut self.pending);
        if self.cancelled() {
            return;
        }
        if pending.resync {
            self.status(
                false,
                "Filesystem events were lost or uncertain · resyncing".into(),
            );
            self.full_scan();
            return;
        }
        let current_kind = fs::symlink_metadata(&self.root)
            .ok()
            .map(|meta| entry_kind(&meta));
        if current_kind != self.root_kind || !self.initialized {
            self.install_watch();
            self.full_scan();
            return;
        }
        let mut paths: Vec<_> = pending.paths.into_iter().collect();
        let ancestors_with_events: HashSet<_> = paths
            .iter()
            .flat_map(|(key, _)| key.ancestors().skip(1).map(Path::to_owned))
            .collect();
        paths.sort_unstable_by(|(left, _), (right, _)| {
            left.components()
                .count()
                .cmp(&right.components().count())
                .then(left.cmp(right))
        });
        let mut covered = HashSet::new();
        let mut changes = Vec::new();
        let mut partial = false;
        for (key, request) in paths {
            let Request {
                path,
                recursive,
                structural,
            } = request;
            // Directory hints accompanying concrete descendant events do not
            // require scanning those descendants a second time.
            let recursive = recursive && (structural || !ancestors_with_events.contains(&key));
            if self.cancelled() {
                return;
            }
            if key.ancestors().any(|ancestor| covered.contains(ancestor)) {
                continue;
            }
            if key == self.root_key && recursive && current_kind == Some(EntryKind::Directory) {
                // Reattach first, because a delete+recreate can have the same
                // final kind while the old native handle watches a dead inode.
                self.install_watch();
            }
            match inspect_change(&self.root, &path, recursive, &self.sink.cancel) {
                Ok(Some(change)) => {
                    let covering = match &change {
                        Change::Remove(path) | Change::ReplaceSubtree { path, .. } => {
                            Some(path_key(path))
                        }
                        Change::Upsert {
                            path,
                            kind: EntryKind::Symlink,
                            ..
                        } => Some(path_key(path)),
                        _ => None,
                    };
                    if let Some(key) = covering {
                        covered.insert(key);
                    }
                    changes.push(change);
                }
                Ok(None) => {}
                Err(error) => {
                    partial = true;
                    self.status(false, format!("Partial live update: {error}"));
                }
            }
        }
        if !changes.is_empty() {
            self.fingerprint = None;
            self.sink.send(LiveEvent::Changes(changes));
        }
        if self.native && self.root_present && !partial {
            self.watching_status();
        }
    }
}

fn ignored(event: &Event) -> bool {
    !event.need_rescan()
        && matches!(
            event.kind,
            EventKind::Access(_)
                | EventKind::Modify(ModifyKind::Metadata(MetadataKind::AccessTime))
        )
}

fn watch_root(path: PathBuf) -> PathBuf {
    let absolute = std::path::absolute(&path).unwrap_or(path);
    #[cfg(target_os = "macos")]
    {
        // FSEvents canonicalizes watch paths (e.g. /var -> /private/var).
        // Reject a directly selected link through the usual scanner path;
        // resolving legitimate ancestor aliases is needed for event identity.
        if fs::symlink_metadata(&absolute).is_ok_and(|metadata| is_link(&metadata)) {
            return absolute;
        }
        let mut candidate = absolute.clone();
        let mut suffix = Vec::new();
        loop {
            if let Ok(mut canonical) = fs::canonicalize(&candidate) {
                for component in suffix.iter().rev() {
                    canonical.push(component);
                }
                return canonical;
            }
            let Some(name) = candidate.file_name().map(std::ffi::OsStr::to_owned) else {
                break;
            };
            suffix.push(name);
            if !candidate.pop() {
                break;
            }
        }
    }
    absolute
}

fn entry_kind(metadata: &Metadata) -> EntryKind {
    if is_link(metadata) {
        EntryKind::Symlink
    } else if metadata.is_dir() {
        EntryKind::Directory
    } else {
        EntryKind::File
    }
}

fn is_link(metadata: &Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x0400 != 0 {
            return true;
        }
    }
    false
}

/// Validate every ancestor inside the selected root before doing I/O below it.
/// Events for a former directory replaced by a link must never follow that link.
fn inspect_change(
    root: &Path,
    path: &Path,
    recursive: bool,
    cancel: &AtomicBool,
) -> Result<Option<Change>, String> {
    let root_key = path_key(root);
    if !path_key(path).starts_with(&root_key) {
        return Ok(None);
    }
    let mut ancestors: Vec<_> = path
        .ancestors()
        .skip(1)
        .take_while(|parent| path_key(parent).starts_with(&root_key))
        .collect();
    ancestors.reverse();
    // Check from the root downwards: even symlink_metadata on a deep path
    // would otherwise resolve an unchecked intermediate directory symlink.
    for parent in ancestors {
        match fs::symlink_metadata(parent) {
            Ok(metadata) if is_link(&metadata) => {
                return Ok(Some(Change::Upsert {
                    path: parent.to_owned(),
                    kind: EntryKind::Symlink,
                    size: 0,
                    modified: metadata.modified().ok(),
                }));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Some(Change::Remove(parent.to_owned())));
            }
            Err(error) => return Err(format!("Unable to inspect {}: {error}", parent.display())),
        }
    }
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Some(Change::Remove(path.to_owned())));
        }
        Err(error) => return Err(format!("Unable to inspect {}: {error}", path.display())),
    };
    let kind = entry_kind(&metadata);
    if kind == EntryKind::Directory && recursive {
        return match scanner::scan_path(path, cancel) {
            Ok(tree) => Ok(Some(Change::ReplaceSubtree {
                path: path.to_owned(),
                tree,
            })),
            Err(_) if cancel.load(Ordering::Relaxed) => Ok(None),
            Err(error) => Err(error),
        };
    }
    if kind == EntryKind::File && !metadata.is_file() {
        return Ok(Some(Change::Remove(path.to_owned())));
    }
    Ok(Some(Change::Upsert {
        path: path.to_owned(),
        kind,
        size: if kind == EntryKind::File {
            metadata.len()
        } else {
            0
        },
        modified: metadata.modified().ok(),
    }))
}

fn tree_fingerprint(tree: &ScanTree) -> u64 {
    // Order-independent: read_dir arena order may differ between two scans.
    let mut fingerprint = tree.nodes.len() as u64;
    for node in &tree.nodes {
        let mut hasher = DefaultHasher::new();
        node.path.hash(&mut hasher);
        node.size.hash(&mut hasher);
        (node.kind as u8).hash(&mut hasher);
        node.modified.hash(&mut hasher);
        fingerprint ^= hasher.finish().rotate_left((node.size % 63) as u32);
    }
    fingerprint ^ tree.stats.skipped
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::event::{AccessKind, DataChange, Flag};
    use std::sync::atomic::{AtomicU64, AtomicUsize};

    struct Directory(PathBuf);

    impl Directory {
        fn new() -> Self {
            static SEQUENCE: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "space-sniffer-live-unit-{}-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                SEQUENCE.fetch_add(1, Ordering::Relaxed),
            ));
            fs::create_dir(&path).unwrap();
            Self(watch_root(path))
        }
    }

    impl Drop for Directory {
        fn drop(&mut self) {
            // Unique test roots only; never remove a computed unrelated path.
            if let (Ok(root), Ok(temp)) =
                (self.0.canonicalize(), std::env::temp_dir().canonicalize())
                && root.parent() == Some(temp.as_path())
                && root
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("space-sniffer-live-unit-")
            {
                let _ = fs::remove_dir_all(root);
            }
        }
    }

    fn worker(path: &Path) -> (Worker, Receiver<LiveEvent>) {
        let (sender, receiver) = mpsc::sync_channel(OUTPUT_CAPACITY);
        let sink = Sink {
            sender,
            cancel: Arc::new(AtomicBool::new(false)),
            wake: Arc::new(|| {}),
        };
        (Worker::new(path.to_owned(), sink), receiver)
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_watch_roots_resolve_ancestor_aliases_but_not_selected_links() {
        use std::os::unix::fs::symlink;
        let directory = Directory::new();
        let alias = std::env::temp_dir().join(directory.0.file_name().unwrap());
        assert_eq!(watch_root(alias.clone()), directory.0);
        assert_eq!(
            watch_root(alias.join("missing/deep")),
            directory.0.join("missing/deep")
        );

        let link = directory.0.join("selected-link");
        symlink(&directory.0, &link).unwrap();
        assert_eq!(watch_root(link.clone()), link);
        assert!(scanner::scan_path(&link, &AtomicBool::new(false)).is_err());
    }

    fn initial(worker: &mut Worker, receiver: &Receiver<LiveEvent>) -> LiveTree {
        worker.full_scan();
        receiver
            .try_iter()
            .find_map(|event| match event {
                LiveEvent::Ready(model) => Some(*model),
                _ => None,
            })
            .expect("initial snapshot")
    }

    #[test]
    fn access_events_are_ignored_but_rescan_flags_always_survive() {
        let access = Event::new(EventKind::Access(AccessKind::Any));
        assert!(ignored(&access));
        assert!(ignored(&Event::new(EventKind::Modify(
            ModifyKind::Metadata(MetadataKind::AccessTime)
        ))));
        assert!(!ignored(&access.set_flag(Flag::Rescan)));
    }

    #[test]
    fn debounce_has_a_quiet_period_and_a_fixed_maximum() {
        let now = Instant::now();
        let mut pending = Pending {
            first: Some(now - Duration::from_millis(80)),
            last: Some(now - Duration::from_millis(80)),
            ..Pending::default()
        };
        assert!(!pending.ready(now));
        pending.last = Some(now - DEBOUNCE);
        assert!(pending.ready(now));
        pending.first = Some(now - MAX_DEBOUNCE);
        pending.last = Some(now);
        assert!(pending.ready(now));
    }

    #[test]
    fn pending_overflow_requests_a_bounded_resync() {
        let mut pending = Pending::default();
        for index in 0..=MAX_PENDING {
            pending.add(PathBuf::from(format!("overflow/{index}")), false);
        }
        assert!(pending.resync);
        assert!(pending.paths.is_empty());
        pending.add(PathBuf::from("overflow/later"), false);
        assert!(pending.paths.is_empty());
    }

    #[test]
    fn lost_event_flag_recovers_the_model_from_disk() {
        let directory = Directory::new();
        let path = directory.0.join("file.txt");
        fs::write(&path, [0; 10]).unwrap();
        let (mut worker, receiver) = worker(&directory.0);
        let mut model = initial(&mut worker, &receiver);
        fs::write(&path, [0; 21]).unwrap();
        worker.collect(Ok(Event::new(EventKind::Any).set_flag(Flag::Rescan)));
        assert!(worker.pending.resync);
        worker.process_pending();
        let mut recovered = false;
        for event in receiver.try_iter() {
            if let LiveEvent::Changes(changes) = event {
                assert!(matches!(
                    changes.first(),
                    Some(Change::ReplaceSubtree { .. })
                ));
                model.apply(&changes);
                recovered = true;
            }
        }
        assert!(recovered);
        assert_eq!(model.tree.total_size(), 21);
    }

    #[test]
    fn generic_directory_hint_does_not_rescan_a_known_changed_leaf() {
        let directory = Directory::new();
        let path = directory.0.join("file.txt");
        fs::write(&path, [0; 10]).unwrap();
        let (mut worker, receiver) = worker(&directory.0);
        let mut model = initial(&mut worker, &receiver);
        fs::write(&path, [0; 21]).unwrap();
        worker.collect(Ok(Event::new(EventKind::Any).add_path(directory.0.clone())));
        worker.collect(Ok(Event::new(EventKind::Modify(ModifyKind::Data(
            DataChange::Content,
        )))
        .add_path(path.clone())));
        worker.process_pending();
        let mut leaf_seen = false;
        for event in receiver.try_iter() {
            if let LiveEvent::Changes(changes) = event {
                assert!(
                    !changes
                        .iter()
                        .any(|change| matches!(change, Change::ReplaceSubtree { .. }))
                );
                leaf_seen |= changes.iter().any(|change| matches!(change, Change::Upsert { path: changed, .. } if changed == &path));
                model.apply(&changes);
            }
        }
        assert!(leaf_seen);
        assert_eq!(model.tree.total_size(), 21);
    }

    #[test]
    fn full_output_queue_can_be_cancelled_and_successful_events_wake() {
        let (sender, receiver) = mpsc::sync_channel(1);
        let cancel = Arc::new(AtomicBool::new(false));
        let wakes = Arc::new(AtomicUsize::new(0));
        let count = Arc::clone(&wakes);
        let sink = Sink {
            sender,
            cancel: Arc::clone(&cancel),
            wake: Arc::new(move || {
                count.fetch_add(1, Ordering::Relaxed);
            }),
        };
        assert!(sink.send(LiveEvent::Progress(ScanProgress::default())));
        assert_eq!(wakes.load(Ordering::Relaxed), 1);
        let waiting = thread::spawn(move || sink.send(LiveEvent::Failed("queued".into())));
        cancel.store(true, Ordering::Relaxed);
        assert!(!waiting.join().unwrap());
        assert!(matches!(receiver.recv().unwrap(), LiveEvent::Progress(_)));
    }

    #[test]
    fn dropping_a_handle_requests_cancellation() {
        let (_, receiver) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let handle = LiveHandle {
            receiver,
            cancel: Arc::clone(&cancel),
        };
        drop(handle);
        assert!(cancel.load(Ordering::Relaxed));
    }

    fn preview_tree(files: usize) -> ScanTree {
        let root = std::path::absolute("publisher-test").unwrap();
        let mut nodes = vec![Node {
            name: "publisher-test".into(),
            path: root.clone(),
            kind: EntryKind::Directory,
            size: files as u64,
            children: (1..=files).collect(),
            parent: None,
            modified: None,
        }];
        for i in 0..files {
            nodes.push(Node {
                name: format!("{i}.bin"),
                path: root.join(format!("{i}.bin")),
                kind: EntryKind::File,
                size: 1,
                children: vec![],
                parent: Some(0),
                modified: None,
            });
        }
        ScanTree {
            nodes,
            root: 0,
            stats: crate::model::ScanStats {
                files: files as u64,
                directories: 1,
                ..Default::default()
            },
        }
    }

    #[test]
    fn previews_are_bounded_dense_and_never_copy_growing_child_lists() {
        let tree = preview_tree(PREVIEW_CHUNK + 3);
        let (sender, receiver) = mpsc::sync_channel(8);
        let sink = Sink {
            sender,
            cancel: Arc::new(AtomicBool::new(false)),
            wake: Arc::new(|| {}),
        };
        let mut publisher = PreviewPublisher::default();
        let mut next = 0;
        for _ in 0..4 {
            publisher.publish(&tree, &sink);
            if let Ok(LiveEvent::Preview { start, nodes }) = receiver.try_recv() {
                assert_eq!(start, next);
                assert!(nodes.len() <= PREVIEW_CHUNK);
                assert!(nodes.iter().all(|node| node.children.is_empty()));
                if start == 0 {
                    assert_eq!(nodes.len(), 1);
                    assert_eq!(nodes[0].size, 0);
                } else {
                    assert!(nodes.iter().all(|node| node.size == 1));
                }
                next += nodes.len();
            }
        }
        assert_eq!(next, tree.nodes.len());
        assert_eq!(publisher.next, next);
    }

    #[test]
    fn full_preview_queue_never_blocks_or_advances_the_prefix() {
        let tree = preview_tree(2);
        let (sender, receiver) = mpsc::sync_channel(1);
        sender.send(LiveEvent::Failed("occupied".into())).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let sink = Sink {
            sender,
            cancel: Arc::clone(&cancel),
            wake: Arc::new(|| {}),
        };
        let mut publisher = PreviewPublisher::default();
        publisher.publish(&tree, &sink);
        assert_eq!(publisher.next, 0);
        let _ = receiver.recv().unwrap();
        publisher.publish(&tree, &sink);
        assert_eq!(publisher.next, 1);
        let _ = receiver.recv().unwrap();
        drop(receiver);
        publisher.publish(&tree, &sink);
        assert!(cancel.load(Ordering::Relaxed));
        assert_eq!(publisher.next, 1);
    }

    #[cfg(unix)]
    #[test]
    fn descendants_of_directory_symlinks_are_never_inspected() {
        let directory = Directory::new();
        let outside = Directory::new();
        fs::create_dir(outside.0.join("deep")).unwrap();
        let link = directory.0.join("link");
        std::os::unix::fs::symlink(&outside.0, &link).unwrap();
        let change = inspect_change(
            &directory.0,
            &link.join("deep/absent.txt"),
            true,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(
            matches!(change, Some(Change::Upsert { path, kind: EntryKind::Symlink, size: 0, .. }) if path == link)
        );
    }
}
