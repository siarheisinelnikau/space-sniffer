//! `cargo run --release --example preview_bench -- --append-only`
//! `cargo run --release --example preview_bench -- --startup [existing-directory]`
//!
//! With no arguments, run both benchmarks. Startup creates and removes its own
//! temporary 20,000-file fixture unless an existing directory is supplied; a
//! supplied directory is read only. Fixture/model construction is not timed.

use space_sniffer::{
    live::{LiveEvent, start_live},
    live_tree::LiveTree,
    model::{EntryKind, Node, ScanStats, ScanTree},
};
use std::{
    fs::{self, File},
    hint::black_box,
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

fn main() {
    let mut arguments = std::env::args_os().skip(1);
    match arguments.next().as_deref() {
        Some(mode) if mode == "--append-only" => append_benchmark(),
        Some(mode) if mode == "--startup" => startup_benchmark(arguments.next().map(PathBuf::from)),
        None => {
            append_benchmark();
            startup_benchmark(None);
        }
        _ => panic!("use --append-only or --startup [existing-directory]"),
    }
}

fn file_node(root: &Path, id: usize) -> Node {
    let name = format!("f{id:06}.bin");
    Node {
        path: root.join(&name),
        name,
        kind: EntryKind::File,
        size: 1024,
        children: Vec::new(),
        parent: Some(0),
        modified: None,
    }
}

fn append_benchmark() {
    const FILES: usize = 100_000;
    const CHUNK: usize = 8192;
    const SAMPLES: usize = 20;
    let root = std::env::temp_dir().join("space-sniffer-preview-model");
    let mut nodes = Vec::with_capacity(FILES + 1);
    nodes.push(Node {
        name: "space-sniffer-preview-model".into(),
        path: root.clone(),
        kind: EntryKind::Directory,
        size: FILES as u64 * 1024,
        children: (1..=FILES).collect(),
        parent: None,
        modified: None,
    });
    nodes.extend((1..=FILES).map(|id| file_node(&root, id)));
    let baseline = ScanTree {
        nodes,
        root: 0,
        stats: ScanStats {
            files: FILES as u64,
            directories: 1,
            ..ScanStats::default()
        },
    };
    let mut timings = Vec::with_capacity(SAMPLES);
    let mut inspected = 0;
    for _ in 0..SAMPLES {
        // The existing model and incoming chunk are constructed before timing;
        // append_scan itself includes allocation growth, validation and indexing.
        let mut model = LiveTree::new(baseline.clone());
        let start = model.tree.nodes.len();
        let incoming = (start..start + CHUNK)
            .map(|id| file_node(&root, id))
            .collect();
        let started = Instant::now();
        let summary = black_box(model.append_scan(start, incoming).unwrap());
        timings.push(started.elapsed());
        inspected += summary.inspected;
        assert_eq!(summary.changed, CHUNK);
        assert_eq!(summary.inspected, CHUNK * 2 + 1);
        assert_eq!(model.tree.total_size(), (FILES + CHUNK) as u64 * 1024);
        assert_eq!(model.find(&root.join("f050000.bin")), Some(50_000));
        black_box(model);
    }
    timings.sort_unstable();
    println!(
        "append_scan existing_nodes={} appended_nodes={CHUNK} samples={SAMPLES} average_inspected={:.0} median_milliseconds={:.3} p95_milliseconds={:.3}",
        baseline.nodes.len(),
        inspected as f64 / SAMPLES as f64,
        timings[SAMPLES / 2].as_secs_f64() * 1000.0,
        timings[(SAMPLES * 95).div_ceil(100) - 1].as_secs_f64() * 1000.0,
    );
}

struct Directory(PathBuf);

impl Directory {
    fn new() -> Self {
        let timestamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
        let path = std::env::temp_dir().join(format!(
            "space-sniffer-preview-bench-{}-{}",
            std::process::id(),
            timestamp.as_nanos()
        ));
        fs::create_dir(&path).unwrap();
        #[cfg(target_os = "macos")]
        let path = path.canonicalize().unwrap();
        for index in 0..20_000 {
            File::create(path.join(format!("f{index:05}.bin")))
                .unwrap()
                .set_len(17)
                .unwrap();
        }
        Self(path)
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        if let (Ok(target), Ok(temp)) = (self.0.canonicalize(), std::env::temp_dir().canonicalize())
            && target.parent() == Some(temp.as_path())
            && target.file_name().is_some_and(|name| {
                name.to_string_lossy()
                    .starts_with("space-sniffer-preview-bench-")
            })
        {
            let _ = fs::remove_dir_all(target);
        }
    }
}

fn seed(mut root: Node) -> LiveTree {
    let mut stats = ScanStats::default();
    match root.kind {
        EntryKind::Directory => {
            root.size = 0;
            stats.directories = 1;
        }
        EntryKind::File => stats.files = 1,
        EntryKind::Symlink => stats.symlinks = 1,
    }
    LiveTree::new(ScanTree {
        nodes: vec![root],
        root: 0,
        stats,
    })
}

fn has_map_tiles(model: &LiveTree) -> bool {
    let root = &model.tree.nodes[model.tree.root];
    let items =
        space_sniffer::map_items::visible_items(&root.children, &model.weights, 1_000_000.0, 1024);
    space_sniffer::treemap::layout(
        &items.items,
        space_sniffer::treemap::Rect {
            x: 0.0,
            y: 0.0,
            width: 1000.0,
            height: 1000.0,
        },
    )
    .iter()
    .any(|tile| tile.rect.width > 0.0 && tile.rect.height > 0.0)
}

fn startup_benchmark(existing: Option<PathBuf>) {
    let fixture = existing.is_none().then(Directory::new);
    let path = existing
        .as_deref()
        .unwrap_or_else(|| &fixture.as_ref().unwrap().0);
    let started = Instant::now();
    let handle = start_live(path.to_owned());
    let mut model = None;
    let mut first_root = None;
    let mut first_tiles = None;
    let mut first_tile_nodes = 0;
    let mut first_tile_bytes = 0;
    let mut chunks = 0;
    let mut max_chunk = 0;
    let mut cursor = 0;
    let deadline = started + Duration::from_secs(60);
    let final_tree = loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        assert!(!remaining.is_zero(), "live startup timed out");
        match handle
            .receiver
            .recv_timeout(remaining.min(Duration::from_millis(500)))
        {
            Ok(LiveEvent::Preview { start, nodes }) => {
                assert_eq!(start, cursor, "preview prefixes must never repeat");
                assert!(!nodes.is_empty() && nodes.len() <= 8192);
                cursor += nodes.len();
                chunks += 1;
                max_chunk = max_chunk.max(nodes.len());
                if start == 0 {
                    assert_eq!(nodes.len(), 1);
                    model = Some(seed(nodes.into_iter().next().unwrap()));
                    first_root = Some(started.elapsed());
                } else {
                    model.as_mut().unwrap().append_scan(start, nodes).unwrap();
                }
                if first_tiles.is_none() && has_map_tiles(model.as_ref().unwrap()) {
                    first_tiles = Some(started.elapsed());
                    first_tile_nodes = model.as_ref().unwrap().tree.nodes.len();
                    first_tile_bytes = model.as_ref().unwrap().tree.total_size();
                }
            }
            Ok(LiveEvent::Ready(tree)) => break tree,
            Ok(LiveEvent::Progress(_) | LiveEvent::Status { .. }) => {}
            Ok(LiveEvent::Changes(_)) => panic!("native changes must follow Ready"),
            Ok(LiveEvent::Failed(message)) => panic!("startup failed: {message}"),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(error) => panic!("live startup disconnected: {error}"),
        }
    };
    let ready = started.elapsed();
    assert!(cursor <= final_tree.tree.nodes.len());
    if fixture.is_some() {
        assert_eq!(final_tree.tree.stats.files, 20_000);
        assert_eq!(final_tree.tree.total_size(), 20_000 * 17);
        assert!(first_tiles.is_some());
    }
    println!(
        "live_startup final_nodes={} final_bytes={} preview_chunks={chunks} max_chunk_nodes={max_chunk} delivered_preview_nodes={cursor} repeated_prefix_nodes=0 first_root_milliseconds={} first_tiles_milliseconds={} first_tile_nodes={first_tile_nodes} first_tile_bytes={first_tile_bytes} ready_milliseconds={:.3}",
        final_tree.tree.nodes.len(),
        final_tree.tree.total_size(),
        milliseconds(first_root),
        milliseconds(first_tiles),
        ready.as_secs_f64() * 1000.0,
    );
}

fn milliseconds(duration: Option<Duration>) -> String {
    duration.map_or_else(
        || "none".into(),
        |duration| format!("{:.3}", duration.as_secs_f64() * 1000.0),
    )
}
