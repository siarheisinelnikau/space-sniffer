//! In-memory million-entry scanner finalization benchmark, without disk writes.
//!
//! rustc --edition=2024 -O examples/scanner_scale_bench.rs -o scanner-scale.exe
//! scanner-scale.exe [FILES=1000000] [REPEATS=5]
//! scanner-scale.exe --path EXISTING_DIRECTORY [REPEATS=7]
//!
//! Synthetic timings exclude fixture construction/destruction, filesystem I/O,
//! preview copying, and UI work. Files' direct-directory bytes are already
//! accumulated, as they are during the scanner's normal directory-batch merge.
//! --path measures complete read-only scans and does not create any files.

#[allow(dead_code)]
#[path = "../src/model.rs"]
mod model;
#[allow(dead_code)]
#[path = "../src/scanner.rs"]
mod scanner;

use model::{EntryKind, Node, NodeId, ScanStats, ScanTree};
use std::{hint::black_box, path::PathBuf, sync::atomic::AtomicBool, time::Instant};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.first().is_some_and(|arg| arg == "--path") {
        let path = args.get(1).ok_or("--path needs an existing directory")?;
        let repeats = args
            .get(2)
            .map(|arg| arg.to_string_lossy().parse::<usize>())
            .transpose()?
            .unwrap_or(7)
            .clamp(1, 100);
        return real_scans(PathBuf::from(path), repeats).map_err(Into::into);
    }
    let files = args
        .first()
        .map(|arg| arg.to_string_lossy().parse::<usize>())
        .transpose()?
        .unwrap_or(1_000_000)
        .max(1);
    let repeats = args
        .get(1)
        .map(|arg| arg.to_string_lossy().parse::<usize>())
        .transpose()?
        .unwrap_or(5)
        .clamp(1, 100);
    for (name, broad, random) in [
        ("flat-uniform", false, false),
        ("flat-random", false, true),
        ("broad-random", true, true),
    ] {
        let mut samples = [Vec::new(), Vec::new()];
        for round in 0..repeats {
            for offset in 0..2 {
                let observed = (round + offset) % 2 != 0;
                let (mut tree, directories, expected) = synthetic(files, broad, random);
                let cancel = AtomicBool::new(false);
                let started = Instant::now();
                if observed {
                    scanner::finalize_tree(
                        &mut tree,
                        &directories,
                        &cancel,
                        &mut scanner::ScanObserver::<true, _>::new(|_| {}),
                        |_| {},
                    )?;
                } else {
                    scanner::finalize_tree(
                        &mut tree,
                        &directories,
                        &cancel,
                        &mut scanner::ScanObserver::<false, _>::new(|_| {}),
                        |_| {},
                    )?;
                }
                let milliseconds = started.elapsed().as_secs_f64() * 1000.0;
                assert_eq!(tree.total_size(), expected);
                samples[usize::from(observed)].push(milliseconds);
                black_box(tree);
            }
        }
        for (observed, values) in samples.iter_mut().enumerate() {
            values.sort_by(f64::total_cmp);
            println!(
                "{name} files={files} observe={} finalization_median_ms={:.3} min_ms={:.3} max_ms={:.3} repeats={repeats}",
                observed != 0,
                values[values.len() / 2],
                values[0],
                values[values.len() - 1]
            );
        }
    }
    Ok(())
}

fn synthetic(files: usize, broad: bool, random: bool) -> (ScanTree, Vec<NodeId>, u64) {
    let count = if broad {
        (files / 1000).clamp(1, 1000)
    } else {
        0
    };
    let root = PathBuf::from("synthetic/long-parent-prefix-for-space-sniffer-profile");
    let mut nodes = Vec::with_capacity(files + count + 1);
    nodes.push(Node {
        name: "root".into(),
        path: root.clone(),
        kind: EntryKind::Directory,
        size: 0,
        children: Vec::with_capacity(if broad { count } else { files }),
        parent: None,
        modified: None,
    });
    let mut directories = vec![0];
    for directory in 0..count {
        let name = format!("d{directory:04}");
        nodes.push(Node {
            path: root.join(&name),
            name,
            kind: EntryKind::Directory,
            size: 0,
            children: Vec::with_capacity(files / count + 1),
            parent: Some(0),
            modified: None,
        });
        nodes[0].children.push(directory + 1);
        directories.push(directory + 1);
    }
    let mut state = 12345_u64;
    let mut expected = 0_u64;
    for file in 0..files {
        let parent = if broad { file % count + 1 } else { 0 };
        let id = nodes.len();
        let name = format!("file-{file:07}.bin");
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
        let size = if random {
            state % 1_000_000_000 + 1
        } else {
            1024
        };
        nodes.push(Node {
            path: nodes[parent].path.join(&name),
            name,
            kind: EntryKind::File,
            size,
            children: Vec::new(),
            parent: Some(parent),
            modified: None,
        });
        nodes[parent].children.push(id);
        nodes[parent].size = nodes[parent].size.saturating_add(size);
        expected = expected.saturating_add(size);
    }
    (
        ScanTree {
            nodes,
            root: 0,
            stats: ScanStats::default(),
        },
        directories,
        expected,
    )
}

fn real_scans(path: PathBuf, repeats: usize) -> Result<(), String> {
    let cancel = AtomicBool::new(false);
    let mut samples = [Vec::new(), Vec::new()];
    let mut expected = None;
    for round in 0..repeats {
        for offset in 0..2 {
            let observed = (round + offset) % 2 != 0;
            let started = Instant::now();
            let tree = if observed {
                scanner::scan_with_observer(&path, &cancel, |_| {}, |_| {})?
            } else {
                scanner::scan_with_progress(&path, &cancel, |_| {})?
            };
            samples[usize::from(observed)].push(started.elapsed().as_secs_f64() * 1000.0);
            let totals = (
                tree.nodes.len(),
                tree.total_size(),
                tree.stats.files,
                tree.stats.directories,
                tree.stats.skipped,
                tree.stats.symlinks,
            );
            if let Some(expected) = expected {
                assert_eq!(totals, expected);
            } else {
                expected = Some(totals);
            }
            black_box(tree);
        }
    }
    for (observed, values) in samples.iter_mut().enumerate() {
        values.sort_by(f64::total_cmp);
        println!(
            "read_only path={} observe={} scan_median_ms={:.3} min_ms={:.3} max_ms={:.3} totals={:?} repeats={repeats}",
            path.display(),
            observed != 0,
            values[values.len() / 2],
            values[0],
            values[values.len() - 1],
            expected.unwrap()
        );
    }
    Ok(())
}
