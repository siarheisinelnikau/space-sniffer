# Performance

Space Sniffer 0.2 reduces initial scan work and applies ordinary file changes incrementally. The scanner uses up to eight directory workers and bounded batch queues. On Windows, `DirEntry::metadata()` reuses enumeration metadata; [Rust documents this optimization and its no-follow behavior](https://doc.rust-lang.org/std/fs/struct.DirEntry.html#method.metadata).

## Measured initial scans

These are Windows results from an optimized standalone scanner harness (`rustc -O`, Rust 1.96.1), excluding GUI startup, fixture creation, watcher setup, and drawing. The baseline is unchanged serial scanner source captured before commit `026c056`; both implementations ran in the same process with identical optimization settings. The baseline executable was not rebuilt or timed.

The fixture contains 20,000 files, 204 directories, and 1,088,000 logical bytes: 10,000 32-byte files in a flat directory, 100 broad directories of 80 64-byte files, and a 100-level chain with 20 128-byte files per level. The combined root and its three subtrees were measured separately.

Values below are medians of seven runs, rotating implementation/worker order each round. Metadata was warm. Storage model and antivirus configuration were not recorded, so these describe this local run, not a hardware-independent expectation.

| Fixture | Previous serial | New, 1 worker | New, 2 workers | New, 4 workers | New, 8 workers |
| --- | ---: | ---: | ---: | ---: | ---: |
| Combined: 20,000 files, 204 directories | 292.89 ms | 21.06 ms | 14.35 ms | 11.36 ms | 10.61 ms |
| Flat: 10,000 files, 1 directory | 139.57 ms | 6.01 ms | 6.19 ms | 6.16 ms | 6.25 ms |
| Broad: 8,000 files, 101 directories | 118.82 ms | 9.35 ms | 5.44 ms | 3.34 ms | 2.81 ms |
| Deep: 2,000 files, 101 directories | 37.75 ms | 6.46 ms | 6.49 ms | 6.31 ms | 6.37 ms |

The combined fixture improved about 27.6 times over the previous implementation. Most Windows improvement comes from avoiding redundant metadata calls: one worker already improved substantially. Eight workers roughly halved the new combined scan time versus one worker and helped the broad tree most. The flat directory and deep chain offered little independent directory work; extra workers did not improve them.

No true cold-cache result was measured. Creating the fixture warms metadata. Combined-root first-observed timings were 303.09 ms for the old scanner and 21.12/14.54/11.69/9.43 ms for 1/2/4/8 new workers; these are also not cold-cache measurements. True cold tests require a documented cache-reset procedure and separate reporting. Parallel reads can be slower on spinning disks; network latency, permissions, antivirus, directory shape, and cache state can dominate full-drive scans.

## Reproduce current worker comparisons

The standalone [benchmark](../examples/scan_bench.rs) compiles without the GUI or extra packages. From the repository root in PowerShell:

```powershell
New-Item -ItemType Directory -Force target/scan-bench | Out-Null
rustc --edition=2024 -O examples/scan_bench.rs -o target/scan-bench/scan-bench.exe
./target/scan-bench/scan-bench.exe --generate 7
./target/scan-bench/scan-bench.exe "C:\path\to\representative-tree" 7
```

On Linux or macOS:

```sh
mkdir -p target/scan-bench
rustc --edition=2024 -O examples/scan_bench.rs -o target/scan-bench/scan-bench
./target/scan-bench/scan-bench --generate 7
./target/scan-bench/scan-bench /path/to/representative-tree 7
```

`--generate` creates the fixture in a temporary directory and removes it afterward. Passing a path scans existing data without modifying it. The optional positional number is the run count; the default is seven. The program validates identical totals across worker counts and reports first-observed timings separately from rotated warm medians.

These commands compare the current scanner's 1/2/4/8-worker modes. There is no `--baseline` argument. Reproducing the historical table additionally requires the captured previous scanner source in a comparison harness; timing a separate old executable includes startup and is a different measurement.

Record the commit, OS, Rust version, storage type, root size/shape, worker count, cache procedure, antivirus conditions, repetitions, and median/range. Separate initial scanning from watcher latency and UI drawing.

## Measured in-memory updates

The release-mode [live model benchmark](../examples/live_bench.rs) uses a synthetic tree with 100 directories containing 1,000 files each: 100,101 nodes including the root. Files initially contain 1,024 logical bytes each, totaling 102,400,000 bytes. The active filter is `*.bin`.

| Operation | Repetitions | Measured time | Model work |
| --- | ---: | --- | --- |
| Complete `Filter::weights` rebuild | 100 | 14,612.7 µs average | Entire 100,101-node tree |
| One-file incremental `Upsert` | 1,000 | 3.7 µs median; 3.9 µs p95 | 41 reported inspections per update on average |

Incremental updates alternate the same file between 4,096 and 1,024 bytes. Reported inspections include ancestor and sibling-order comparisons. These measurements cover pure in-memory model work, excluding initial index construction, native notification delivery, debounce, filesystem metadata I/O, and drawing. The average full-rebuild time and median/p95 update times describe different statistics and operations; they do not establish watcher latency or an end-to-end speedup.

Reproduce from the repository root:

```sh
cargo run --locked --release --example live_bench
```

An optional directory argument additionally measures a read-only initial scan and live-index construction on that path. Without an argument, the benchmark creates only synthetic in-memory data.

## Live updates and idle work

Windows native-watcher end-to-end measurements used ten samples per operation
on isolated temporary files, including filesystem writes, notification delivery,
debounce and model application. Medians were 143.858 ms for create, 133.410 ms
for append, 135.529 ms for truncate and 133.399 ms for delete; the observed
range across operations was 101.235–153.350 ms. These results do not include
UI painting and are not guarantees for other storage or operating systems.

```sh
cargo run --locked --release --example live_bench -- --watch-latency
```

The map aggregates subpixel entries instead of sorting millions of invisible
rectangles. A release-mode test with one million equal tiny siblings took
2.48 ms to select a single aggregate, preserving all bytes. Per-folder layout
receives at most 1,024 entries; zoom or filtering reveals smaller entries.

The initial scan builds its path index on a worker. Native notifications then identify changed paths. The live model updates affected nodes, ancestor totals, counters, and active filter weights; ordinary file writes do not require a root rescan. Directory discovery, invalidated subtrees, and lost events can require larger reconciliation.

The UI caches treemap geometry and the largest-entry list. Published watcher events wake the UI; an unchanged idle session does not redraw continuously. This reduces ongoing work but does not guarantee end-to-end latency. Large subtrees and event bursts need separate measurements. If native watching fails, recovery uses full snapshot polling every two seconds until native watching can resume, so fallback cost scales with root size.
