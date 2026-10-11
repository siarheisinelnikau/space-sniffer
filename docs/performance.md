# Performance

Space Sniffer 0.3 shows discovered data before the initial scan completes, while retaining parallel scanning and incremental live updates. The scanner uses up to eight directory workers and bounded batch queues. On Windows, `DirEntry::metadata()` reuses enumeration metadata; [Rust documents this optimization and its no-follow behavior](https://doc.rust-lang.org/std/fs/struct.DirEntry.html#method.metadata).

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

## Progressive observer overhead

The observer API was compared with the unchanged pre-observer parallel scanner in the same optimized standalone process. Windows metadata was warm; values are medians of 11 runs with configuration order rotated. The mixed 20,000-file fixture is the one described above; the flat fixture has 100,000 32-byte files. Neither set measures UI work or a cold cache.

| Fixture | Pre-observer progress API | Current progress API, no observer | No-op observer | Observer copying at most 8,192 new nodes per call |
| --- | ---: | ---: | ---: | ---: |
| Mixed 20,000 files | 9.210 ms | 9.274 ms | 8.972 ms | 10.263 ms |
| Flat 100,000 files | 60.069 ms | 59.572 ms | 59.431 ms | 60.455 ms |

The normal and no-op paths remained close to the prior implementation within local timing variation. Bounded node copying added about one millisecond in these runs. This copy benchmark clears child lists and zeroes directory sizes, without cloning whole trees. It excludes transport waiting, preview indexing, map geometry, and window painting; those costs need separate measurement.

Reproduce current no-observer/no-op/bounded-copy comparisons with the scanner benchmark built above:

```powershell
./target/scan-bench/scan-bench.exe --observe --generate 11
./target/scan-bench/scan-bench.exe --observe --generate-flat 11
```

On Linux/macOS, use the same arguments with `./target/scan-bench/scan-bench`. `--generate-flat` creates and later removes an isolated temporary 100,000-file directory. The current example does not include the pre-observer source baseline; that historical column used the preserved comparison harness.

## First drawable geometry before completion

The release-mode [preview benchmark](../examples/preview_bench.rs) creates an isolated temporary directory containing 20,000 17-byte files. In one Windows run with warm filesystem metadata, the root seed/model was available after **1.336 ms**, the first positive-area treemap geometry after **2.105 ms**, and the authoritative full model after **48.645 ms**. That first geometry represented 513 nodes and 8,704 bytes. Three nonoverlapping preview chunks delivered an 8,705-node prefix, with at most 8,192 nodes per chunk, before the complete 20,001-node, 340,000-byte model replaced it.

Timing begins before `start_live`. The first-geometry measurement includes preview delivery, model append, visible-item selection, and treemap layout. Final readiness includes construction of the authoritative `LiveTree`. Fixture creation is excluded. These are core and geometry measurements, **not** operating-system window paint, GPU, or displayed-frame latency; the run is not a whole-drive guarantee.

```sh
cargo run --locked --release --example preview_bench -- --startup
cargo run --locked --release --example preview_bench -- --append-only
```

`--startup` without a path creates and removes its own fixture. An optional existing-directory argument is read only. `--append-only` measures applying a bounded chunk to an existing synthetic model, separately from scanning and layout; no arguments runs both benchmarks.

In the separate model-only test, appending 8,192 nodes to an existing 100,001-node flat model took **5.179 ms median** and **5.642 ms p95** over 20 samples, with exactly 16,385 reported inspections per append. This includes chunk validation, path indexing, and arena allocation growth; baseline-model and incoming-chunk construction are excluded. It includes neither filesystem work nor window painting, and does not promise a particular frame time.

The root seed is sent first, the first nonempty scan batch is offered immediately, and subsequent observer opportunities occur roughly every 100 ms. Preview publication never waits for a full UI queue; contiguous prefix chunks can lag the scanner, and the final model supersedes the remaining prefix. Preview counts therefore describe data already delivered to the map, not the scanner's full discovered total.

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
