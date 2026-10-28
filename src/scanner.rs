//! Filesystem scanning without recursive calls or link traversal.
//!
//! The worker reports logical bytes, rather than allocated disk space. Hard
//! links count once per path. Symbolic links and all Windows reparse points
//! (including junctions) are represented as zero-byte leaves and never opened.

use std::collections::VecDeque;
use std::fs::{self, Metadata};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::time::{Duration, Instant};

use crate::model::{EntryKind, Node, NodeId, ScanStats, ScanTree};

const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);
const CANCELLED: &str = "Scan cancelled";
const DIRECTORY_BATCH_SIZE: usize = 512;
const MAX_SCAN_WORKERS: usize = 8;

#[derive(Clone, Debug, Default)]
pub struct ScanProgress {
    pub files: u64,
    pub directories: u64,
    pub skipped: u64,
    pub bytes: u64,
    pub current_path: PathBuf,
}

#[derive(Debug)]
pub enum ScanEvent {
    Progress(ScanProgress),
    Finished(Result<ScanTree, String>),
}

pub struct ScanHandle {
    pub receiver: Receiver<ScanEvent>,
    pub cancel: Arc<AtomicBool>,
}

/// Start a scan on a named worker thread.
///
/// Progress is throttled and bounded to two queued events, so a busy UI cannot
/// accumulate a backlog. The final event is always delivered unless the
/// receiver has been dropped. Set `cancel` to true to request cancellation.
pub fn start_scan(path: PathBuf) -> ScanHandle {
    let (sender, receiver) = mpsc::sync_channel(2);
    let cancel = Arc::new(AtomicBool::new(false));
    let worker_cancel = Arc::clone(&cancel);
    let failure_sender = sender.clone();

    let worker = std::thread::Builder::new()
        .name("filesystem-scan".into())
        .spawn(move || {
            let result = scan_with_progress(&path, &worker_cancel, |progress| {
                // Intermediate progress may be dropped; completion may not.
                if let Err(TrySendError::Disconnected(_)) =
                    sender.try_send(ScanEvent::Progress(progress))
                {
                    worker_cancel.store(true, Ordering::Relaxed);
                }
            });
            let _ = sender.send(ScanEvent::Finished(result));
        });

    if let Err(error) = worker {
        let _ = failure_sender.try_send(ScanEvent::Finished(Err(format!(
            "Unable to start the scan: {error}"
        ))));
    }

    ScanHandle { receiver, cancel }
}

/// Scan synchronously, for command-line callers and tests.
///
/// Regular-file roots are supported. Root links, unsupported roots, and root
/// access failures return an error. Unreadable descendants produce a partial
/// result with `stats.skipped` incremented. Cancellation returns
/// `Err("Scan cancelled")` and discards the partial result.
pub fn scan_path(path: &Path, cancel: &AtomicBool) -> Result<ScanTree, String> {
    scan_with_progress(path, cancel, |_| {})
}

/// Scan with bounded directory parallelism and progress on the calling thread.
///
/// The callback is never invoked by directory workers, so it need not be Send.
/// Up to eight workers overlap directory I/O. SSDs and independent directories
/// benefit most; parallel access is not always faster on spinning disks.
pub fn scan_with_progress(
    path: &Path,
    cancel: &AtomicBool,
    report: impl FnMut(ScanProgress),
) -> Result<ScanTree, String> {
    let workers = std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1)
        .min(MAX_SCAN_WORKERS);
    scan_with_worker_count(path, cancel, workers, report)
}

// Crate-visible for the standalone benchmark, without a second public scan API.
pub(crate) fn scan_with_worker_count(
    path: &Path,
    cancel: &AtomicBool,
    workers: usize,
    mut report: impl FnMut(ScanProgress),
) -> Result<ScanTree, String> {
    let started = Instant::now();
    check_cancelled(cancel)?;
    // Make paths suitable for reveal/export without resolving root symlinks.
    let absolute_path = std::path::absolute(path)
        .map_err(|error| format!("Unable to resolve {}: {error}", path.display()))?;
    let path = absolute_path.as_path();
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("Unable to inspect {}: {error}", path.display()))?;
    if is_link(&metadata) {
        return Err(format!(
            "{} is a link or reparse point. Select its target directly.",
            path.display()
        ));
    }
    let root_kind = if metadata.is_dir() {
        EntryKind::Directory
    } else if metadata.is_file() {
        EntryKind::File
    } else {
        return Err(format!(
            "{} is not a regular file or directory.",
            path.display()
        ));
    };
    let mut tree = ScanTree {
        nodes: vec![make_node(path.to_owned(), root_kind, &metadata, None)],
        root: 0,
        stats: ScanStats::default(),
    };
    let mut bytes = tree.nodes[0].size;
    if root_kind == EntryKind::Directory {
        tree.stats.directories = 1;
    } else {
        tree.stats.files = 1;
    }
    report(progress(&tree.stats, bytes, path));
    let mut last_report = Instant::now();
    if root_kind == EntryKind::Directory {
        scan_directories(
            &mut tree,
            &mut bytes,
            cancel,
            workers.clamp(1, MAX_SCAN_WORKERS),
            &mut report,
            &mut last_report,
        )?;
    }
    // Children are inserted after parents, so reverse aggregation never recurses.
    for node_id in (0..tree.nodes.len()).rev() {
        check_cancelled(cancel)?;
        if last_report.elapsed() >= PROGRESS_INTERVAL {
            report(progress(&tree.stats, bytes, path));
            last_report = Instant::now();
        }
        let mut children = std::mem::take(&mut tree.nodes[node_id].children);
        children.sort_unstable_by(|&left, &right| {
            tree.nodes[right]
                .size
                .cmp(&tree.nodes[left].size)
                .then_with(|| tree.nodes[left].name.cmp(&tree.nodes[right].name))
                .then_with(|| tree.nodes[left].path.cmp(&tree.nodes[right].path))
        });
        tree.nodes[node_id].children = children;
        if let Some(parent) = tree.nodes[node_id].parent {
            tree.nodes[parent].size = tree.nodes[parent]
                .size
                .saturating_add(tree.nodes[node_id].size);
        }
    }
    check_cancelled(cancel)?;
    tree.stats.elapsed = started.elapsed();
    report(progress(&tree.stats, bytes, path));
    Ok(tree)
}

struct DirectoryJob {
    id: NodeId,
    path: PathBuf,
}
struct DirectoryBatch {
    worker: usize,
    directory: NodeId,
    nodes: Vec<Node>,
    skipped: u64,
    done: bool,
    replaced_by_link: bool,
    error: Option<String>,
}
impl DirectoryBatch {
    fn new(worker: usize, directory: NodeId) -> Self {
        Self {
            worker,
            directory,
            nodes: Vec::new(),
            skipped: 0,
            done: false,
            replaced_by_link: false,
            error: None,
        }
    }
}

fn scan_directories(
    tree: &mut ScanTree,
    bytes: &mut u64,
    cancel: &AtomicBool,
    worker_count: usize,
    report: &mut impl FnMut(ScanProgress),
    last_report: &mut Instant,
) -> Result<(), String> {
    let stop = AtomicBool::new(false);
    std::thread::scope(|scope| {
        // Two bounded result batches per worker, one job per idle worker.
        let (results, receiver) = mpsc::sync_channel(worker_count * 2);
        let mut jobs = Vec::with_capacity(worker_count);
        for worker in 0..worker_count {
            let (sender, inbox) = mpsc::sync_channel(1);
            let results = results.clone();
            let stop = &stop;
            let spawned = std::thread::Builder::new()
                .name(format!("scan-directory-{worker}"))
                .spawn_scoped(scope, move || {
                    while let Ok(job) = inbox.recv() {
                        if cancel.load(Ordering::Relaxed) || stop.load(Ordering::Relaxed) {
                            break;
                        }
                        if !read_directory(worker, job, cancel, stop, &results) {
                            break;
                        }
                    }
                });
            match spawned {
                Ok(_) => jobs.push(sender),
                Err(error) if jobs.is_empty() => {
                    return Err(format!("Unable to start directory scanning: {error}"));
                }
                Err(_) => break,
            }
        }
        drop(results);
        let outcome = (|| {
            let mut pending = VecDeque::from([tree.root]);
            let mut idle: Vec<_> = (0..jobs.len()).rev().collect();
            let mut active = 0;
            let mut current_path = tree.nodes[tree.root].path.clone();
            while !pending.is_empty() || active > 0 {
                check_cancelled(cancel)?;
                while !pending.is_empty() && !idle.is_empty() {
                    let directory = pending.pop_front().unwrap();
                    let worker = idle.pop().unwrap();
                    jobs[worker]
                        .send(DirectoryJob {
                            id: directory,
                            path: tree.nodes[directory].path.clone(),
                        })
                        .map_err(|_| "Directory scan worker stopped unexpectedly".to_string())?;
                    active += 1;
                }
                if last_report.elapsed() >= PROGRESS_INTERVAL {
                    report(progress(&tree.stats, *bytes, &current_path));
                    *last_report = Instant::now();
                    check_cancelled(cancel)?;
                }
                let batch = match receiver.recv_timeout(PROGRESS_INTERVAL) {
                    Ok(batch) => batch,
                    Err(RecvTimeoutError::Timeout) => continue,
                    Err(RecvTimeoutError::Disconnected) => {
                        return Err("Directory scan workers stopped unexpectedly".into());
                    }
                };
                current_path.clone_from(&tree.nodes[batch.directory].path);
                if let Some(error) = batch.error {
                    if batch.directory == tree.root {
                        return Err(error);
                    }
                    tree.stats.skipped = tree.stats.skipped.saturating_add(1);
                }
                if batch.replaced_by_link {
                    if batch.directory == tree.root {
                        return Err(format!(
                            "{} became a link or reparse point during the scan.",
                            current_path.display()
                        ));
                    }
                    tree.nodes[batch.directory].kind = EntryKind::Symlink;
                    tree.stats.directories = tree.stats.directories.saturating_sub(1);
                    tree.stats.symlinks = tree.stats.symlinks.saturating_add(1);
                }
                tree.stats.skipped = tree.stats.skipped.saturating_add(batch.skipped);
                tree.nodes[batch.directory]
                    .children
                    .reserve(batch.nodes.len());
                for node in batch.nodes {
                    let node_id = tree.nodes.len();
                    match node.kind {
                        EntryKind::Directory => {
                            tree.stats.directories = tree.stats.directories.saturating_add(1);
                            pending.push_back(node_id);
                        }
                        EntryKind::File => {
                            tree.stats.files = tree.stats.files.saturating_add(1);
                            *bytes = bytes.saturating_add(node.size);
                        }
                        EntryKind::Symlink => {
                            tree.stats.symlinks = tree.stats.symlinks.saturating_add(1);
                        }
                    }
                    tree.nodes.push(node);
                    tree.nodes[batch.directory].children.push(node_id);
                }
                if batch.done {
                    active -= 1;
                    idle.push(batch.worker);
                }
            }
            Ok(())
        })();
        // Disconnect channels before joining, including cancellation/error paths.
        // This unblocks workers publishing to a full results queue.
        stop.store(true, Ordering::Relaxed);
        drop(jobs);
        drop(receiver);
        outcome
    })
}

fn read_directory(
    worker: usize,
    job: DirectoryJob,
    cancel: &AtomicBool,
    stop: &AtomicBool,
    results: &SyncSender<DirectoryBatch>,
) -> bool {
    let mut batch = DirectoryBatch::new(worker, job.id);
    // Recheck queued paths before enumeration: they may have become junctions.
    let metadata = match fs::symlink_metadata(&job.path) {
        Ok(metadata) => metadata,
        Err(error) => {
            batch.done = true;
            batch.error = Some(format!("Unable to inspect {}: {error}", job.path.display()));
            return results.send(batch).is_ok();
        }
    };
    if is_link(&metadata) {
        batch.done = true;
        batch.replaced_by_link = true;
        return results.send(batch).is_ok();
    }
    if !metadata.is_dir() {
        batch.done = true;
        batch.error = Some(format!("{} is no longer a directory.", job.path.display()));
        return results.send(batch).is_ok();
    }
    let entries = match fs::read_dir(&job.path) {
        Ok(entries) => entries,
        Err(error) => {
            batch.done = true;
            batch.error = Some(format!("Unable to read {}: {error}", job.path.display()));
            return results.send(batch).is_ok();
        }
    };
    let mut visited = 0;
    for entry in entries {
        if cancel.load(Ordering::Relaxed) || stop.load(Ordering::Relaxed) {
            return false;
        }
        // No link traversal. Windows reuses cached enumeration metadata here;
        // Unix performs the equivalent of symlink_metadata for each entry.
        let node = entry.and_then(|entry| {
            let metadata = entry.metadata()?;
            let kind = if is_link(&metadata) {
                EntryKind::Symlink
            } else if metadata.is_dir() {
                EntryKind::Directory
            } else if metadata.is_file() {
                EntryKind::File
            } else {
                return Ok(None);
            };
            Ok(Some(make_node(entry.path(), kind, &metadata, Some(job.id))))
        });
        match node {
            Ok(Some(node)) => batch.nodes.push(node),
            Ok(None) | Err(_) => batch.skipped = batch.skipped.saturating_add(1),
        }
        visited += 1;
        if visited == DIRECTORY_BATCH_SIZE {
            if results.send(batch).is_err() {
                return false;
            }
            batch = DirectoryBatch::new(worker, job.id);
            visited = 0;
        }
    }
    batch.done = true;
    results.send(batch).is_ok()
}
fn make_node(path: PathBuf, kind: EntryKind, metadata: &Metadata, parent: Option<NodeId>) -> Node {
    Node {
        name: path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string_lossy().into_owned()),
        size: if kind == EntryKind::File {
            metadata.len()
        } else {
            0
        },
        modified: metadata.modified().ok(),
        path,
        kind,
        children: Vec::new(),
        parent,
    }
}

fn progress(stats: &ScanStats, bytes: u64, current_path: &Path) -> ScanProgress {
    ScanProgress {
        files: stats.files,
        directories: stats.directories,
        skipped: stats.skipped,
        bytes,
        current_path: current_path.to_owned(),
    }
}

fn check_cancelled(cancel: &AtomicBool) -> Result<(), String> {
    if cancel.load(Ordering::Relaxed) {
        Err(CANCELLED.into())
    } else {
        Ok(())
    }
}

fn is_link(metadata: &Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;
    use std::time::{SystemTime, UNIX_EPOCH};

    static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let timestamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
            let path = std::env::temp_dir().join(format!(
                "space-sniffer-test-{}-{}-{sequence}",
                std::process::id(),
                timestamp.as_nanos()
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn scan(&self) -> ScanTree {
            scan_path(&self.0, &AtomicBool::new(false)).unwrap()
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            // Resolve and verify the exact temporary target before cleanup.
            if let (Ok(path), Ok(parent)) =
                (self.0.canonicalize(), std::env::temp_dir().canonicalize())
                && path.parent() == Some(parent.as_path())
                && path
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with("space-sniffer-test-"))
            {
                let _ = fs::remove_dir_all(path);
            }
        }
    }

    #[test]
    fn totals_and_children_are_correct_and_sorted() {
        let directory = TestDirectory::new();
        fs::create_dir(directory.0.join("nested")).unwrap();
        fs::create_dir(directory.0.join("empty")).unwrap();
        fs::write(directory.0.join("alpha.bin"), [0; 3]).unwrap();
        fs::write(directory.0.join("zeta.bin"), [0; 3]).unwrap();
        fs::write(directory.0.join("nested").join("data.bin"), [0; 9]).unwrap();

        let tree = directory.scan();
        assert_eq!(tree.total_size(), 15);
        assert_eq!(tree.stats.files, 3);
        assert_eq!(tree.stats.directories, 3);
        assert_eq!(tree.stats.skipped, 0);
        assert_eq!(tree.stats.symlinks, 0);
        let names: Vec<_> = tree.nodes[tree.root]
            .children
            .iter()
            .map(|&id| tree.nodes[id].name.as_str())
            .collect();
        assert_eq!(names, ["nested", "alpha.bin", "zeta.bin", "empty"]);
        for (node_id, node) in tree.nodes.iter().enumerate() {
            for &child_id in &node.children {
                assert_eq!(tree.nodes[child_id].parent, Some(node_id));
            }
            if node.is_dir() {
                assert_eq!(
                    node.size,
                    node.children.iter().map(|&id| tree.nodes[id].size).sum()
                );
            }
        }
    }

    #[test]
    fn empty_directory_is_a_valid_result() {
        let directory = TestDirectory::new();
        let tree = directory.scan();
        assert_eq!(tree.nodes.len(), 1);
        assert_eq!(tree.total_size(), 0);
        assert_eq!(tree.stats.files, 0);
        assert_eq!(tree.stats.directories, 1);
    }

    #[test]
    fn missing_root_reports_an_error() {
        let directory = TestDirectory::new();
        let result = scan_path(&directory.0.join("missing"), &AtomicBool::new(false));
        assert!(result.unwrap_err().contains("Unable to inspect"));
    }

    #[test]
    fn cancellation_discards_the_result() {
        let directory = TestDirectory::new();
        let result = scan_path(&directory.0, &AtomicBool::new(true));
        assert_eq!(result.unwrap_err(), "Scan cancelled");
    }

    #[test]
    fn cancellation_during_traversal_is_observed() {
        let directory = TestDirectory::new();
        fs::write(directory.0.join("data.bin"), [0; 4]).unwrap();
        let cancel = AtomicBool::new(false);
        let result = scan_with_progress(&directory.0, &cancel, |_| {
            cancel.store(true, Ordering::Relaxed);
        });
        assert_eq!(result.unwrap_err(), "Scan cancelled");
    }

    #[test]
    fn individual_file_roots_are_supported() {
        let directory = TestDirectory::new();
        let path = directory.0.join("data.bin");
        fs::write(&path, [0; 12]).unwrap();
        let tree = scan_path(&path, &AtomicBool::new(false)).unwrap();
        assert_eq!(tree.total_size(), 12);
        assert_eq!(tree.stats.files, 1);
        assert_eq!(tree.stats.directories, 0);
        assert_eq!(tree.nodes[tree.root].kind, EntryKind::File);
    }

    #[test]
    fn hard_links_count_each_path() {
        let directory = TestDirectory::new();
        let original = directory.0.join("original.bin");
        fs::write(&original, [0; 7]).unwrap();
        fs::hard_link(&original, directory.0.join("another.bin")).unwrap();
        let tree = directory.scan();
        assert_eq!(tree.total_size(), 14);
        assert_eq!(tree.stats.files, 2);
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn directory_links_are_zero_byte_leaves_and_root_links_are_rejected() {
        let directory = TestDirectory::new();
        fs::write(directory.0.join("data.bin"), [0; 5]).unwrap();
        let link = directory.0.join("loop");
        make_directory_link(&directory.0, &link).unwrap();

        let tree = directory.scan();
        assert_eq!(tree.total_size(), 5);
        assert_eq!(tree.stats.files, 1);
        assert_eq!(tree.stats.directories, 1);
        assert_eq!(tree.stats.symlinks, 1);
        let node = tree.nodes.iter().find(|node| node.path == link).unwrap();
        assert_eq!(node.kind, EntryKind::Symlink);
        assert_eq!(node.size, 0);
        assert!(node.children.is_empty());
        assert!(scan_path(&link, &AtomicBool::new(false)).is_err());
    }

    #[cfg(unix)]
    fn make_directory_link(target: &Path, link: &Path) -> std::io::Result<()> {
        std::os::unix::fs::symlink(target, link)
    }

    #[cfg(windows)]
    fn make_directory_link(target: &Path, link: &Path) -> std::io::Result<()> {
        // Junctions need no developer-mode or administrator permission and
        // exercise the reparse-point check used for Windows directory loops.
        let output = std::process::Command::new("cmd")
            .args(["/D", "/C", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .output()?;
        if output.status.success() {
            Ok(())
        } else {
            Err(std::io::Error::other(
                String::from_utf8_lossy(&output.stderr).into_owned(),
            ))
        }
    }

    #[test]
    fn worker_delivers_progress_and_a_finished_result() {
        let directory = TestDirectory::new();
        fs::write(directory.0.join("data.bin"), [0; 8]).unwrap();
        let handle = start_scan(directory.0.clone());
        loop {
            match handle
                .receiver
                .recv_timeout(Duration::from_secs(5))
                .unwrap()
            {
                ScanEvent::Progress(progress) => {
                    assert!(progress.bytes <= 8);
                    assert!(progress.current_path.starts_with(&directory.0));
                }
                ScanEvent::Finished(result) => {
                    assert_eq!(result.unwrap().total_size(), 8);
                    break;
                }
            }
        }
    }

    #[test]
    fn parallel_batches_preserve_serial_results_and_parent_order() {
        let directory = TestDirectory::new();
        // Exceed one batch in the root, with enough independent directories
        // to keep all workers occupied and results arriving in different orders.
        for index in 0..520 {
            fs::write(directory.0.join(format!("root-{index:04}.bin")), [0; 3]).unwrap();
        }
        for index in 0..12 {
            let child = directory.0.join(format!("child-{index:02}"));
            fs::create_dir(&child).unwrap();
            for file in 0..40 {
                fs::write(child.join(format!("file-{file:03}.bin")), [0; 7]).unwrap();
            }
        }
        let cancel = AtomicBool::new(false);
        let serial = scan_with_worker_count(&directory.0, &cancel, 1, |_| {}).unwrap();
        let parallel = scan_with_worker_count(&directory.0, &cancel, 4, |_| {}).unwrap();
        assert_eq!(serial.total_size(), 4_920);
        assert_eq!(serial.stats.files, 1_000);
        assert_eq!(parallel.stats.files, serial.stats.files);
        assert_eq!(parallel.stats.directories, serial.stats.directories);
        assert_eq!(parallel.total_size(), serial.total_size());
        for node in &serial.nodes {
            let other = parallel
                .nodes
                .iter()
                .find(|other| other.path == node.path)
                .unwrap();
            assert_eq!((other.kind, other.size), (node.kind, node.size));
            let paths = |tree: &ScanTree, node: &Node| {
                node.children
                    .iter()
                    .map(|&id| tree.nodes[id].path.clone())
                    .collect::<Vec<_>>()
            };
            assert_eq!(paths(&serial, node), paths(&parallel, other));
        }
        for (id, node) in parallel.nodes.iter().enumerate() {
            assert!(node.parent.is_none_or(|parent| parent < id));
        }
    }

    #[test]
    fn callbacks_stay_on_the_caller_and_need_not_be_send() {
        use std::cell::Cell;
        use std::rc::Rc;
        let directory = TestDirectory::new();
        let caller = std::thread::current().id();
        let calls = Rc::new(Cell::new(0));
        scan_with_worker_count(&directory.0, &AtomicBool::new(false), 4, |_| {
            assert_eq!(std::thread::current().id(), caller);
            calls.set(calls.get() + 1);
        })
        .unwrap();
        assert!(calls.get() >= 2);
    }

    #[test]
    fn cancellation_unblocks_a_worker_with_batches_in_flight() {
        let directory = TestDirectory::new();
        for index in 0..(DIRECTORY_BATCH_SIZE * 2 + 1) {
            fs::write(directory.0.join(format!("file-{index:04}")), []).unwrap();
        }
        let cancel = AtomicBool::new(false);
        let stop = AtomicBool::new(false);
        std::thread::scope(|scope| {
            let (sender, receiver) = mpsc::sync_channel(1);
            let cancel = &cancel;
            let stop = &stop;
            let path = directory.0.clone();
            let worker = scope.spawn(move || {
                read_directory(0, DirectoryJob { id: 0, path }, &cancel, &stop, &sender)
            });
            let batch = receiver.recv_timeout(Duration::from_secs(5)).unwrap();
            assert_eq!(batch.nodes.len(), DIRECTORY_BATCH_SIZE);
            assert!(!batch.done);
            cancel.store(true, Ordering::Relaxed);
            drop(receiver);
            assert!(!worker.join().unwrap());
        });
    }

    #[test]
    fn deep_directories_do_not_require_recursive_traversal() {
        let directory = TestDirectory::new();
        let mut leaf = directory.0.clone();
        for _ in 0..128 {
            leaf.push("d");
            fs::create_dir(&leaf).unwrap();
        }
        fs::write(leaf.join("leaf.bin"), [0; 11]).unwrap();
        let tree = directory.scan();
        assert_eq!(tree.stats.directories, 129);
        assert_eq!(tree.total_size(), 11);
    }
}
