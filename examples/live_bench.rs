//! Run with `cargo run --release --example live_bench -- [directory]`, or
//! `cargo run --release --example live_bench -- --watch-latency`.
//! The optional directory is read only. Synthetic incremental timings do not
//! include filesystem I/O and are intended to show model-update scaling.

use space_sniffer::{
    filter::Filter,
    live::{Change, LiveEvent, LiveHandle, start_live},
    live_tree::LiveTree,
    model::{EntryKind, Node, ScanStats, ScanTree},
    scanner::scan_path,
};
use std::{
    fs::{self, File, OpenOptions},
    hint::black_box,
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

fn main() {
    if let Some(path) = std::env::args_os().nth(1) {
        if path == "--watch-latency" {
            watch_latency();
            return;
        }
        let path = PathBuf::from(path);
        let started = Instant::now();
        let tree = scan_path(&path, &AtomicBool::new(false)).expect("directory scan failed");
        println!(
            "full_scan nodes={} files={} bytes={} milliseconds={:.3}",
            tree.nodes.len(),
            tree.stats.files,
            tree.total_size(),
            started.elapsed().as_secs_f64() * 1000.0
        );
        let started = Instant::now();
        black_box(LiveTree::new(tree));
        println!(
            "live_model_build milliseconds={:.3}",
            started.elapsed().as_secs_f64() * 1000.0
        );
    }

    let tree = synthetic_tree();
    let filter = Filter::parse("*.bin").unwrap();
    let started = Instant::now();
    for _ in 0..100 {
        black_box(filter.weights(black_box(&tree)));
    }
    println!(
        "full_weights nodes={} average_microseconds={:.3}",
        tree.nodes.len(),
        started.elapsed().as_secs_f64() * 1_000_000.0 / 100.0
    );

    let changed_path = tree.nodes[101].path.clone();
    let mut live = LiveTree::new(tree);
    live.set_filter("*.bin").unwrap();
    let mut timings = Vec::with_capacity(1000);
    let mut inspected = 0;
    for index in 0..1000 {
        let change = Change::Upsert {
            path: changed_path.clone(),
            kind: EntryKind::File,
            size: if index % 2 == 0 { 4096 } else { 1024 },
            modified: None,
        };
        let started = Instant::now();
        let summary = black_box(live.apply(black_box(&[change])));
        timings.push(started.elapsed());
        inspected += summary.inspected;
    }
    timings.sort_unstable();
    println!(
        "incremental_one_file nodes={} average_inspected={:.1} median_microseconds={:.3} p95_microseconds={:.3}",
        live.tree.nodes.len(),
        inspected as f64 / timings.len() as f64,
        timings[timings.len() / 2].as_secs_f64() * 1_000_000.0,
        timings[timings.len() * 95 / 100].as_secs_f64() * 1_000_000.0
    );
}

struct BenchDirectory(PathBuf);

impl BenchDirectory {
    fn new() -> Self {
        let timestamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
        let path = std::env::temp_dir().join(format!(
            "space-sniffer-live-bench-{}-{}",
            std::process::id(),
            timestamp.as_nanos()
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for BenchDirectory {
    fn drop(&mut self) {
        if let (Ok(target), Ok(temp)) = (self.0.canonicalize(), std::env::temp_dir().canonicalize())
            && target.parent() == Some(temp.as_path())
            && target.file_name().is_some_and(|name| {
                name.to_string_lossy()
                    .starts_with("space-sniffer-live-bench-")
            })
        {
            let _ = fs::remove_dir_all(target);
        }
    }
}

struct NativeSession {
    handle: LiveHandle,
    model: Option<LiveTree>,
    watching: bool,
}

impl NativeSession {
    fn new(root: &Path) -> Self {
        let mut result = Self {
            handle: start_live(root.to_owned()),
            model: None,
            watching: false,
        };
        result.until(|model| model.tree.total_size() == 0);
        assert!(
            result.watching,
            "latency benchmark requires a native watcher"
        );
        result
    }

    fn until(&mut self, mut matches: impl FnMut(&LiveTree) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !self.watching || !self.model.as_ref().is_some_and(&mut matches) {
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(
                !remaining.is_zero(),
                "native watcher/model update timed out"
            );
            match self
                .handle
                .receiver
                .recv_timeout(remaining.min(Duration::from_millis(100)))
            {
                Ok(LiveEvent::Ready(model)) => self.model = Some(*model),
                Ok(LiveEvent::Changes(changes)) => {
                    self.model.as_mut().unwrap().apply(&changes);
                }
                Ok(LiveEvent::Status { watching, .. }) => self.watching = watching,
                Ok(LiveEvent::Progress(_)) => {}
                Ok(LiveEvent::Failed(message)) => panic!("live scan failed: {message}"),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(error) => panic!("native watcher disconnected: {error}"),
            }
        }
    }

    fn bytes(&mut self, path: &Path, expected: Option<u64>) {
        self.until(|model| {
            let actual = model.find(path).map(|id| model.tree.nodes[id].size);
            actual == expected && model.tree.total_size() == expected.unwrap_or(0)
        });
        assert!(self.watching, "native watcher fell back while benchmarking");
    }
}

fn watch_latency() {
    let directory = BenchDirectory::new();
    let mut session = NativeSession::new(&directory.0);
    let path = directory.0.join("changing.bin");
    let mut created = Vec::new();
    let mut appended = Vec::new();
    let mut truncated = Vec::new();
    let mut deleted = Vec::new();
    for iteration in 0..10 {
        let size = 100 + iteration;
        let started = Instant::now();
        File::create(&path).unwrap().set_len(size).unwrap();
        session.bytes(&path, Some(size));
        created.push(started.elapsed());

        let started = Instant::now();
        {
            let mut output = OpenOptions::new().append(true).open(&path).unwrap();
            output.write_all(&[1; 11]).unwrap();
            output.flush().unwrap();
        }
        session.bytes(&path, Some(size + 11));
        appended.push(started.elapsed());

        let started = Instant::now();
        OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(3 + iteration)
            .unwrap();
        session.bytes(&path, Some(3 + iteration));
        truncated.push(started.elapsed());

        let started = Instant::now();
        fs::remove_file(&path).unwrap();
        session.bytes(&path, None);
        deleted.push(started.elapsed());
    }
    println!("native_watcher_end_to_end samples_per_operation=10 includes_filesystem_io=true");
    for (operation, mut timings) in [
        ("create", created),
        ("append", appended),
        ("truncate", truncated),
        ("delete", deleted),
    ] {
        timings.sort_unstable();
        println!(
            "{operation} median_milliseconds={:.3} min_milliseconds={:.3} max_milliseconds={:.3}",
            timings[timings.len() / 2].as_secs_f64() * 1000.0,
            timings[0].as_secs_f64() * 1000.0,
            timings.last().unwrap().as_secs_f64() * 1000.0,
        );
    }
}

fn synthetic_tree() -> ScanTree {
    const DIRECTORIES: usize = 100;
    const FILES: usize = 100_000;
    let root = PathBuf::from("space-sniffer-benchmark");
    let mut nodes = Vec::with_capacity(1 + DIRECTORIES + FILES);
    nodes.push(Node {
        name: "space-sniffer-benchmark".into(),
        path: root.clone(),
        kind: EntryKind::Directory,
        size: FILES as u64 * 1024,
        children: (1..=DIRECTORIES).collect(),
        parent: None,
        modified: None,
    });
    for directory in 0..DIRECTORIES {
        let name = format!("d{directory:03}");
        nodes.push(Node {
            path: root.join(&name),
            name,
            kind: EntryKind::Directory,
            size: (FILES / DIRECTORIES) as u64 * 1024,
            children: Vec::with_capacity(FILES / DIRECTORIES),
            parent: Some(0),
            modified: None,
        });
    }
    for index in 0..FILES {
        let parent = 1 + index % DIRECTORIES;
        let id = nodes.len();
        let name = format!("f{index:06}.bin");
        let path = nodes[parent].path.join(&name);
        nodes.push(Node {
            name,
            path,
            kind: EntryKind::File,
            size: 1024,
            children: Vec::new(),
            parent: Some(parent),
            modified: None,
        });
        nodes[parent].children.push(id);
    }
    ScanTree {
        nodes,
        root: 0,
        stats: ScanStats {
            files: FILES as u64,
            directories: (DIRECTORIES + 1) as u64,
            skipped: 0,
            symlinks: 0,
            elapsed: Duration::ZERO,
        },
    }
}
