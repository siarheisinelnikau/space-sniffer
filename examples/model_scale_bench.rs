//! Synthetic model costs, without filesystem I/O or fixture construction.
//! cargo run --release --example model_scale_bench -- [files=1000000] [samples=3]
use space_sniffer::{
    live_tree::LiveTree,
    model::{EntryKind, Node, ScanStats, ScanTree},
};
use std::{
    hint::black_box,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

fn main() {
    let files = std::env::args()
        .nth(1)
        .map(|value| value.parse().unwrap())
        .unwrap_or(1_000_000);
    let samples = std::env::args()
        .nth(2)
        .map(|value| value.parse().unwrap())
        .unwrap_or(3_usize);
    let root = std::env::temp_dir().join("space-sniffer-model-scale");
    let baseline = flat_tree(&root, files);
    let mut build = Vec::new();
    let mut filter = Vec::new();
    let mut clear = Vec::new();
    for _ in 0..samples {
        let source = baseline.clone();
        let started = Instant::now();
        let mut model = black_box(LiveTree::new(source));
        build.push(started.elapsed());
        let started = Instant::now();
        model.set_filter("*.bin;>512B").unwrap();
        filter.push(started.elapsed());
        assert_eq!(model.weights[0], files as u64 * 1024);
        let started = Instant::now();
        model.set_filter("").unwrap();
        clear.push(started.elapsed());
        black_box(model);
    }
    report("model_new", files, build);
    report("filter_glob_size", files, filter);
    report("filter_clear", files, clear);
    drop(baseline);

    let mut model = LiveTree::new(flat_tree(&root, 0));
    let mut append = Vec::new();
    let mut worst = (Duration::ZERO, 0, 0, 0);
    for first in (0..files).step_by(8192) {
        let start = first + 1;
        let incoming = (first..(first + 8192).min(files))
            .map(|index| file(&root, index + 1))
            .collect();
        let before_capacity = model.tree.nodes.capacity();
        let started = Instant::now();
        let summary = model.append_scan(start, incoming).unwrap();
        let elapsed = started.elapsed();
        if elapsed > worst.0 {
            worst = (elapsed, start, before_capacity, model.tree.nodes.capacity());
        }
        assert!(summary.inspected <= summary.changed * 2 + 1);
        append.push(elapsed);
    }
    report("stream_append8192", files, append);
    println!(
        "worst_append start={} milliseconds={:.3} arena_capacity_before={} after={}",
        worst.1,
        ms(worst.0),
        worst.2,
        worst.3
    );
    assert_eq!(model.tree.total_size(), files as u64 * 1024);
}

fn file(root: &Path, id: usize) -> Node {
    let name = format!("f{id:08}.bin");
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

fn flat_tree(root: &Path, files: usize) -> ScanTree {
    let mut nodes = Vec::with_capacity(files + 1);
    nodes.push(Node {
        name: "space-sniffer-model-scale".into(),
        path: PathBuf::from(root),
        kind: EntryKind::Directory,
        size: files as u64 * 1024,
        children: (1..=files).collect(),
        parent: None,
        modified: None,
    });
    nodes.extend((1..=files).map(|id| file(root, id)));
    ScanTree {
        nodes,
        root: 0,
        stats: ScanStats {
            files: files as u64,
            directories: 1,
            ..ScanStats::default()
        },
    }
}

fn report(label: &str, files: usize, mut timings: Vec<Duration>) {
    timings.sort_unstable();
    println!(
        "{label} files={files} samples={} median_ms={:.3} p95_ms={:.3} max_ms={:.3}",
        timings.len(),
        ms(timings[timings.len() / 2]),
        ms(timings[(timings.len() * 95).div_ceil(100) - 1]),
        ms(*timings.last().unwrap())
    );
}

fn ms(value: Duration) -> f64 {
    value.as_secs_f64() * 1000.0
}
