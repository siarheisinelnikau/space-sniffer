//! Standalone scanner benchmark (no window or graphics startup).
//!
//! Build: rustc --edition=2024 -O examples/scan_bench.rs -o scan-bench.exe
//! Run: scan-bench.exe PATH [REPEATS]
//! Or: scan-bench.exe --generate [REPEATS]
//! Observer overhead: scan-bench.exe --observe --generate [REPEATS]
//! Large flat fixture: scan-bench.exe --observe --generate-flat [REPEATS]
//!
//! --generate contains 20,000 files across flat, broad and deep trees;
//! --generate-flat contains 100,000 files in one directory.
//! First-observed and repeated warm medians are reported separately. Creating
//! files warms filesystem metadata: neither measurement is a true cold-cache
//! result. Storage, antivirus, cache state and worker scheduling affect timings.

#[allow(dead_code)]
#[path = "../src/model.rs"]
mod model;
#[allow(dead_code)]
#[path = "../src/scanner.rs"]
mod scanner;

use std::fs;
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

const WORKERS: &[usize] = &[1, 2, 4, 8];

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments: Vec<_> = std::env::args_os().skip(1).collect();
    let compare_observers = arguments.first().is_some_and(|arg| arg == "--observe");
    if compare_observers {
        arguments.remove(0);
    }
    let Some(input) = arguments.first() else {
        return Err(
            "Usage: scan-bench [--observe] PATH|--generate|--generate-flat [REPEATS]".into(),
        );
    };
    let repeats = arguments
        .get(1)
        .map(|value| value.to_string_lossy().parse::<usize>())
        .transpose()?
        .unwrap_or(7)
        .clamp(1, 100);
    let generated = if input == "--generate" {
        Some(Fixture::create()?)
    } else if input == "--generate-flat" {
        Some(Fixture::flat(100_000)?)
    } else {
        None
    };
    let root = generated
        .as_ref()
        .map(|fixture| fixture.0.clone())
        .unwrap_or_else(|| PathBuf::from(input));
    println!("Root: {}", root.display());
    println!("Timing includes traversal, metadata and sorting; excludes fixture creation.");
    println!("First-observed is not cold-cache. Warm results use rotated configuration order.");
    if compare_observers {
        benchmark_observers(&root, repeats)?;
    } else {
        benchmark(&root, repeats)?;
    }
    if generated.is_some() && input == "--generate" && !compare_observers {
        for child in ["flat", "broad", "deep"] {
            benchmark(&root.join(child), repeats)?;
        }
    }
    Ok(())
}

fn benchmark_observers(path: &Path, repeats: usize) -> Result<(), String> {
    const MODES: [&str; 3] = ["progress", "observer-noop", "observer-copy8192"];
    let cancel = AtomicBool::new(false);
    let mut samples: Vec<Vec<f64>> = vec![Vec::new(); MODES.len()];
    let mut expected = None;
    for round in 0..=repeats {
        for offset in 0..MODES.len() {
            let mode = (offset + round) % MODES.len();
            let mut copied = 0;
            let mut calls = 0;
            let started = Instant::now();
            let tree = match mode {
                0 => scanner::scan_with_progress(path, &cancel, |_| {})?,
                1 => scanner::scan_with_observer(
                    path,
                    &cancel,
                    |_| {},
                    |_| {
                        calls += 1;
                    },
                )?,
                _ => scanner::scan_with_observer(
                    path,
                    &cancel,
                    |_| {},
                    |tree| {
                        calls += 1;
                        let end = (copied + 8192).min(tree.nodes.len());
                        let chunk: Vec<_> = tree.nodes[copied..end]
                            .iter()
                            .map(|node| model::Node {
                                name: node.name.clone(),
                                path: node.path.clone(),
                                kind: node.kind,
                                size: if node.is_dir() { 0 } else { node.size },
                                parent: node.parent,
                                modified: node.modified,
                                children: Vec::new(),
                            })
                            .collect();
                        copied = end;
                        black_box(chunk);
                    },
                )?,
            };
            let milliseconds = started.elapsed().as_secs_f64() * 1000.0;
            let totals = (tree.total_size(), tree.stats.files, tree.stats.directories);
            if let Some(expected) = expected {
                assert_eq!(totals, expected);
            } else {
                expected = Some(totals);
            }
            if round == 0 {
                println!(
                    "{} first: {milliseconds:.3} ms; {} nodes, {calls} callbacks, {copied} copied",
                    MODES[mode],
                    tree.nodes.len()
                );
            } else {
                samples[mode].push(milliseconds);
            }
        }
    }
    for (mode, values) in samples.iter_mut().enumerate() {
        values.sort_by(f64::total_cmp);
        println!(
            "{} warm median: {:.3} ms ({} runs)",
            MODES[mode],
            values[values.len() / 2],
            values.len()
        );
    }
    Ok(())
}

fn benchmark(path: &Path, repeats: usize) -> Result<(), String> {
    println!("\n{}", path.display());
    let cancel = AtomicBool::new(false);
    let mut samples: Vec<Vec<f64>> = vec![Vec::new(); WORKERS.len()];
    let mut expected = None;
    for round in 0..=repeats {
        for offset in 0..WORKERS.len() {
            let index = (offset + round) % WORKERS.len();
            let workers = WORKERS[index];
            let started = Instant::now();
            let tree = scanner::scan_with_worker_count(path, &cancel, workers, |_| {})?;
            let milliseconds = started.elapsed().as_secs_f64() * 1000.0;
            let totals = (
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
            if round == 0 {
                println!(
                    "{workers} workers first: {milliseconds:.2} ms; {} files, {} dirs, {} bytes",
                    tree.stats.files,
                    tree.stats.directories,
                    tree.total_size()
                );
            } else {
                samples[index].push(milliseconds);
            }
        }
    }
    for (index, values) in samples.iter_mut().enumerate() {
        values.sort_by(f64::total_cmp);
        println!(
            "{} workers warm median: {:.2} ms ({} runs)",
            WORKERS[index],
            values[values.len() / 2],
            values.len()
        );
    }
    Ok(())
}

struct Fixture(PathBuf);

impl Fixture {
    fn flat(count: usize) -> std::io::Result<Self> {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "space-sniffer-bench-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir(&path)?;
        let fixture = Self(path);
        for index in 0..count {
            fs::write(fixture.0.join(format!("file-{index:06}.bin")), [0; 32])?;
        }
        Ok(fixture)
    }

    fn create() -> std::io::Result<Self> {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "space-sniffer-bench-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir(&path)?;
        let fixture = Self(path);
        let flat = fixture.0.join("flat");
        let broad = fixture.0.join("broad");
        let deep = fixture.0.join("deep");
        for path in [&flat, &broad, &deep] {
            fs::create_dir(path)?;
        }
        for index in 0..10_000 {
            fs::write(flat.join(format!("file-{index:05}.bin")), [0; 32])?;
        }
        for index in 0..100 {
            let directory = broad.join(format!("dir-{index:03}"));
            fs::create_dir(&directory)?;
            for file in 0..80 {
                fs::write(directory.join(format!("file-{file:03}.bin")), [0; 64])?;
            }
        }
        let mut directory = deep;
        for _ in 0..100 {
            directory.push("d");
            fs::create_dir(&directory)?;
            for file in 0..20 {
                fs::write(directory.join(format!("file-{file:03}.bin")), [0; 128])?;
            }
        }
        Ok(fixture)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let (Ok(path), Ok(parent)) = (self.0.canonicalize(), std::env::temp_dir().canonicalize())
            && path.parent() == Some(parent.as_path())
            && path
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("space-sniffer-bench-"))
        {
            let _ = fs::remove_dir_all(path);
        }
    }
}
