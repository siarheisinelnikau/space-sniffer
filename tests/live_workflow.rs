//! End-to-end filesystem workflows: real files, the native watcher, and the
//! same incremental model used by the UI. No watcher events are fabricated.

use space_sniffer::{
    live::{Change, LiveEvent, LiveHandle, start_live},
    live_tree::LiveTree,
    model::{EntryKind, ScanTree},
    scanner::scan_path,
};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const TIMEOUT: Duration = Duration::from_secs(5);

struct Directory(PathBuf);

impl Directory {
    fn new() -> Self {
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let timestamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
        let path = std::env::temp_dir().join(format!(
            "space-sniffer-live-workflow-{}-{}-{sequence}",
            std::process::id(),
            timestamp.as_nanos()
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn child(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        // Cleanup is restricted to this exact, uniquely named temp directory.
        if let (Ok(target), Ok(temp)) = (self.0.canonicalize(), std::env::temp_dir().canonicalize())
            && target.parent() == Some(temp.as_path())
            && target.file_name().is_some_and(|name| {
                name.to_string_lossy()
                    .starts_with("space-sniffer-live-workflow-")
            })
        {
            let _ = fs::remove_dir_all(target);
        }
    }
}

fn file(path: &Path, size: u64) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    File::create(path).unwrap().set_len(size).unwrap();
}

#[derive(Debug, PartialEq, Eq)]
struct Entry {
    kind: EntryKind,
    size: u64,
}

type Inventory = BTreeMap<PathBuf, Entry>;

fn inventory(tree: &ScanTree) -> Inventory {
    let mut result = BTreeMap::new();
    let mut pending = vec![tree.root];
    while let Some(id) = pending.pop() {
        let node = &tree.nodes[id];
        assert!(
            result
                .insert(
                    node.path.clone(),
                    Entry {
                        kind: node.kind,
                        size: node.size,
                    },
                )
                .is_none(),
            "duplicate live path {}",
            node.path.display()
        );
        if node.kind == EntryKind::Directory {
            assert_eq!(
                node.size,
                node.children
                    .iter()
                    .map(|&child| tree.nodes[child].size)
                    .sum::<u64>(),
                "directory aggregation for {}",
                node.path.display()
            );
        }
        for &child in &node.children {
            assert_eq!(tree.nodes[child].parent, Some(id));
            pending.push(child);
        }
    }
    result
}

struct Session {
    handle: LiveHandle,
    view: Option<LiveTree>,
    statuses: Vec<String>,
    failures: Vec<String>,
    progress_seen: bool,
    batches: usize,
    upserts: Vec<PathBuf>,
    removals: Vec<PathBuf>,
    replacements: Vec<PathBuf>,
}

impl Session {
    fn new(path: &Path) -> Self {
        Self {
            handle: start_live(path.to_owned()),
            view: None,
            statuses: Vec::new(),
            failures: Vec::new(),
            progress_seen: false,
            batches: 0,
            upserts: Vec::new(),
            removals: Vec::new(),
            replacements: Vec::new(),
        }
    }

    fn receive(&mut self, timeout: Duration) {
        match self.handle.receiver.recv_timeout(timeout) {
            Ok(LiveEvent::Progress(_)) => self.progress_seen = true,
            Ok(LiveEvent::Ready(view)) => self.view = Some(*view),
            Ok(LiveEvent::Changes(changes)) => {
                self.batches += 1;
                for change in &changes {
                    match change {
                        Change::Upsert { path, .. } => self.upserts.push(path.clone()),
                        Change::ReplaceSubtree { path, .. } => self.replacements.push(path.clone()),
                        Change::Remove(path) => self.removals.push(path.clone()),
                    }
                }
                if let Some(view) = &mut self.view {
                    view.apply(&changes);
                } else {
                    panic!("incremental changes arrived before the initial tree");
                }
            }
            Ok(LiveEvent::Status { watching, message }) => {
                self.statuses
                    .push(format!("watching={watching}: {message}"));
            }
            Ok(LiveEvent::Failed(message)) => self.failures.push(message),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                panic!(
                    "live worker disconnected: statuses={:?}, failures={:?}",
                    self.statuses, self.failures
                );
            }
        }
    }

    fn until(&mut self, label: &str, mut matches: impl FnMut(&Self) -> bool) {
        let deadline = Instant::now() + TIMEOUT;
        while !matches(self) {
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(
                !remaining.is_zero(),
                "timed out waiting for {label}; total={:?}, batches={}, statuses={:?}, failures={:?}",
                self.view.as_ref().map(|view| view.tree.total_size()),
                self.batches,
                self.statuses,
                self.failures
            );
            self.receive(remaining.min(Duration::from_millis(100)));
        }
    }

    fn ready(&mut self) {
        self.until("initial scan", |session| session.view.is_some());
        assert!(self.failures.is_empty(), "{:?}", self.failures);
    }

    fn view(&self) -> &LiveTree {
        self.view.as_ref().unwrap()
    }

    fn agrees_with_disk(&mut self, path: &Path) {
        let expected = scan_path(path, &AtomicBool::new(false)).unwrap();
        let expected_inventory = inventory(&expected);
        self.until("the exact fresh-scan inventory", |session| {
            session.view.as_ref().is_some_and(|view| {
                view.tree.total_size() == expected.total_size()
                    && inventory(&view.tree) == expected_inventory
                    && view.tree.stats.files == expected.stats.files
                    && view.tree.stats.directories == expected.stats.directories
                    && view.tree.stats.symlinks == expected.stats.symlinks
            })
        });
        for path in expected_inventory.into_keys() {
            let id = self.view().find(&path).expect("reachable node is indexed");
            assert!(self.view().contains(id));
        }
    }
}

#[test]
fn append_and_truncate_update_filter_weights_and_preserve_unrelated_ids() {
    let directory = Directory::new();
    let unchanged = directory.child("keep/report.txt");
    let changed = directory.child("changing/data.txt");
    file(&unchanged, 20);
    file(&directory.child("keep/ignored.bin"), 7);
    file(&changed, 10);
    let mut session = Session::new(&directory.0);
    session.ready();
    session.agrees_with_disk(&directory.0);
    let selected = session.view().find(&directory.child("keep")).unwrap();
    let selected_file = session.view().find(&unchanged).unwrap();
    session
        .view
        .as_mut()
        .unwrap()
        .set_filter("*.txt;>8B")
        .unwrap();
    assert_eq!(session.view().weights[session.view().tree.root], 30);
    session.upserts.clear();
    session.replacements.clear();

    {
        let mut output = OpenOptions::new().append(true).open(&changed).unwrap();
        output.write_all(&[1; 11]).unwrap();
        output.flush().unwrap();
    }
    session.agrees_with_disk(&directory.0);
    assert!(
        session.upserts.contains(&changed),
        "expected {changed:?}; upserts={:?}; replacements={:?}",
        session.upserts,
        session.replacements
    );
    assert!(
        session.replacements.is_empty(),
        "ordinary append must not rescan a subtree: {:?}",
        session.replacements
    );
    assert_eq!(session.view().weights[session.view().tree.root], 41);
    assert_eq!(
        session.view().find(&directory.child("keep")),
        Some(selected)
    );
    assert_eq!(session.view().find(&unchanged), Some(selected_file));

    session.upserts.clear();
    session.replacements.clear();
    OpenOptions::new()
        .write(true)
        .open(&changed)
        .unwrap()
        .set_len(3)
        .unwrap();
    session.agrees_with_disk(&directory.0);
    assert!(
        session.upserts.contains(&changed),
        "expected {changed:?}; upserts={:?}; replacements={:?}",
        session.upserts,
        session.replacements
    );
    assert!(
        session.replacements.is_empty(),
        "ordinary truncation must not rescan a subtree: {:?}",
        session.replacements
    );
    assert_eq!(session.view().weights[session.view().tree.root], 20);
    assert_eq!(
        session.view().find(&directory.child("keep")),
        Some(selected)
    );
    assert_eq!(session.view().find(&unchanged), Some(selected_file));
}

#[test]
fn ordinary_create_and_delete_stay_incremental_with_a_thousand_root_siblings() {
    let directory = Directory::new();
    for index in 0..1000 {
        file(&directory.child(&format!("existing-{index:04}.bin")), 3);
    }
    let unchanged = directory.child("existing-0500.bin");
    let changed = directory.child("created.bin");
    let mut session = Session::new(&directory.0);
    session.ready();
    session.agrees_with_disk(&directory.0);
    let selected = session.view().find(&unchanged).unwrap();
    session.upserts.clear();
    session.removals.clear();
    session.replacements.clear();

    file(&changed, 101);
    session.agrees_with_disk(&directory.0);
    assert!(session.upserts.contains(&changed));
    assert!(
        session.replacements.is_empty(),
        "ordinary create must not rescan a subtree: {:?}",
        session.replacements
    );
    assert_eq!(session.view().find(&unchanged), Some(selected));
    let created = session.view().find(&changed).unwrap();
    session.upserts.clear();
    session.removals.clear();
    session.replacements.clear();

    fs::remove_file(&changed).unwrap();
    session.agrees_with_disk(&directory.0);
    assert!(session.removals.contains(&changed));
    assert!(
        session.replacements.is_empty(),
        "ordinary delete must not rescan a subtree: {:?}",
        session.replacements
    );
    assert!(!session.view().contains(created));
    assert_eq!(session.view().find(&unchanged), Some(selected));
}

#[test]
fn populated_folder_import_rename_and_nested_delete_match_fresh_scans() {
    let directory = Directory::new();
    let root = directory.child("watched");
    fs::create_dir(&root).unwrap();
    file(&root.join("keep/selected.txt"), 19);
    let staging = directory.child("staging");
    file(&staging.join("deep/a.txt"), 31);
    file(&staging.join("deep/more/b.bin"), 43);
    file(&staging.join("empty.dat"), 0);
    let mut session = Session::new(&root);
    session.ready();
    let selected = session.view().find(&root.join("keep")).unwrap();

    let incoming = root.join("incoming");
    fs::rename(&staging, &incoming).unwrap();
    session.agrees_with_disk(&root);
    assert!(
        session
            .view()
            .find(&incoming.join("deep/more/b.bin"))
            .is_some()
    );

    let renamed = root.join("renamed");
    fs::rename(&incoming, &renamed).unwrap();
    session.agrees_with_disk(&root);
    assert!(session.view().find(&incoming).is_none());
    assert!(session.view().find(&renamed.join("deep/a.txt")).is_some());
    assert_eq!(session.view().find(&root.join("keep")), Some(selected));

    let deleted = renamed.join("deep");
    let deleted_id = session.view().find(&deleted).unwrap();
    fs::remove_dir_all(&deleted).unwrap();
    session.agrees_with_disk(&root);
    assert!(!session.view().contains(deleted_id));
    assert!(session.view().find(&deleted).is_none());
    assert_eq!(session.view().tree.total_size(), 19);
}

#[test]
fn deleting_and_recreating_the_watched_root_recovers_automatically() {
    let directory = Directory::new();
    let root = directory.child("watched");
    file(&root.join("before/nested.bin"), 57);
    let mut session = Session::new(&root);
    session.ready();
    let old_child = session
        .view()
        .find(&root.join("before/nested.bin"))
        .unwrap();
    fs::remove_dir_all(&root).unwrap();
    session.until("root deletion", |session| {
        let view = session.view();
        view.tree.total_size() == 0 && !view.contains(old_child)
    });

    file(&root.join("after/new.txt"), 83);
    file(&root.join("after/deeper/second.bin"), 11);
    session.agrees_with_disk(&root);
    assert!(session.view().find(&root.join("before")).is_none());
    assert_eq!(session.view().tree.total_size(), 94);
}

#[test]
fn file_roots_survive_editor_style_replace_and_keep_siblings_out() {
    let directory = Directory::new();
    let root = directory.child("document.txt");
    file(&root, 13);
    file(&directory.child("unrelated.bin"), 999);
    let mut session = Session::new(&root);
    session.ready();
    session.agrees_with_disk(&root);

    for size in [47, 5] {
        let temporary = directory.child("document.tmp");
        let backup = directory.child("document.old");
        file(&temporary, size);
        fs::rename(&root, &backup).unwrap();
        fs::rename(&temporary, &root).unwrap();
        fs::remove_file(&backup).unwrap();
        session.agrees_with_disk(&root);
        assert_eq!(session.view().tree.total_size(), size);
        assert_eq!(inventory(&session.view().tree).len(), 1);
        assert!(
            session
                .view()
                .find(&directory.child("unrelated.bin"))
                .is_none()
        );
    }
}

#[test]
fn writes_during_the_initial_scan_are_reconciled_without_a_manual_rescan() {
    let directory = Directory::new();
    // Enough distinct directories to exercise scanning and watcher handoff,
    // while keeping this workflow quick on native Windows filesystems.
    for folder in 0..32 {
        for entry in 0..16 {
            file(
                &directory.child(&format!("d{folder:02}/f{entry:02}.bin")),
                3,
            );
        }
    }
    let mut session = Session::new(&directory.0);
    session.until("first scan progress", |session| session.progress_seen);
    let changed_root = directory.0.clone();
    let writer = std::thread::spawn(move || {
        for folder in 0..32 {
            file(
                &changed_root.join(format!("d{folder:02}/f00.bin")),
                100 + folder,
            );
            fs::remove_file(changed_root.join(format!("d{folder:02}/f01.bin"))).unwrap();
            file(&changed_root.join(format!("new/deep/f{folder:02}.txt")), 17);
        }
    });
    session.ready();
    writer.join().unwrap();
    session.agrees_with_disk(&directory.0);
    assert_eq!(session.view().tree.stats.files, 512);
}

#[test]
fn switching_roots_drops_the_old_watch_and_isolates_the_new_tree() {
    let directory = Directory::new();
    let first = directory.child("first");
    let second = directory.child("second");
    file(&first.join("old.bin"), 7);
    file(&second.join("new.bin"), 11);

    let mut old = Session::new(&first);
    old.ready();
    let old_cancel = old.handle.cancel.clone();
    drop(old);
    assert!(
        old_cancel.load(Ordering::Relaxed),
        "dropping a live session must cancel it"
    );

    let mut current = Session::new(&second);
    current.ready();
    file(&first.join("old.bin"), 1000);
    file(&first.join("old-only.bin"), 2000);
    file(&second.join("new.bin"), 23);
    session_assert_new_root(&mut current, &second, &first);
}

fn session_assert_new_root(session: &mut Session, root: &Path, old_root: &Path) {
    session.agrees_with_disk(root);
    assert_eq!(session.view().tree.total_size(), 23);
    assert!(
        inventory(&session.view().tree)
            .keys()
            .all(|path| path.starts_with(root) && !path.starts_with(old_root))
    );
}
