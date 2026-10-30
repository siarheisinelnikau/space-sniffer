# Performance

## Windows CPU/GDI rendering in 0.4.2

Windows now uses egui-winit, `egui_software_backend` 0.0.2 (egui 0.33.3), and softbuffer 0.4.8's GDI bitmap surface. The application does not create an OpenGL or DirectX rendering context. Linux/macOS keep their existing eframe graphics backend.

The host uses `Wait`/`WaitUntil` when there is no work, coalesces active requests to at most 60 frames/s, and reuses its bitmap for native expose events. Scanning and checking updates no longer animate a spinner over the entire map. A background result uses a single delayed repaint instead of egui 0.33's two zero-delay settling frames. Minimized scans run bounded model work without building widgets, tessellating, or presenting new bitmaps. Windows filesystem workers use background CPU, I/O, and memory scheduling; the UI keeps normal scheduling.

Local Windows release-mode checks on 2026-10-30/31:

- An unchanged project map had identical UI-frame and presentation counters across repeated one-second activity samples. Quiet process CPU samples were 0% of one logical core at the timer's resolution; minimize/restore and worker wakeups produced small nonzero samples. This does not establish zero CPU use under every condition.
- Minimize/restore testing showed flat presentation counters while minimized, including during a real C: scan. The native window continued responding and closed automatically after an 18-second bounded test. That was an interrupted initial scan, not a completed or atomic whole-drive result.
- No `opengl32`, `d3d*`, `dxgi`, or Vulkan graphics modules were found in the sampled CPU-rendered process. The inspected filesystem worker threads had a lower base priority than the UI.
- A synthetic 8,000-tile 1920×1080 benchmark, including geometry, egui tessellation and CPU rasterization, measured **10.420 ms median**, **11.627 ms p95**, and **11.789 ms maximum** across 25 measured frames after five warm-up frames. It excludes GDI presentation and filesystem work; previous geometry-only numbers below are a different measurement.
- In the final bounded C: run, active windows typically presented about 20 frames/s as progress/preview results arrived, with no new presentations while minimized. Full frame timing includes GDI presentation and varied with tree growth: interval medians ranged roughly **9–34 ms**, with a measured maximum around **107 ms**. The old OpenGL path was not rerun on C: because the user reported desktop-wide freezing. Cache conditions differ across local runs, so these are verification observations, not a controlled whole-drive throughput comparison or a claim that every interaction stays below 16 ms.

Reproduce CPU rasterization timing:

```sh
cargo test --locked --release --bin space-sniffer profile_dense_software_frames -- --ignored --nocapture
```

For opt-in native diagnostics, set `SPACE_SNIFFER_RENDER_TRACE` to a JSONL output path. A bounded writer records layout/raster/presentation timing and periodically samples cumulative activity counters without waking the UI. Build `--features diagnostics` to additionally enable `SPACE_SNIFFER_TEST_SECONDS` (1–120), `SPACE_SNIFFER_TEST_MINIMIZE`, and `SPACE_SNIFFER_TEST_DATA` for isolated native smoke checks. These test controls are absent from the normal release executable. Screenshots come directly from the CPU bitmap with `--features screenshots` and `EFRAME_SCREENSHOT_TO`; `SPACE_SNIFFER_SCREENSHOT_VIEW=updates` captures the real Updates window after its settling frame.

Space Sniffer 0.4 profiles large-tree CPU costs separately from filesystem I/O. The initial map fills progressively, scanning uses up to eight directory workers, and queues stay bounded. On Windows, `DirEntry::metadata()` reuses enumeration metadata; [Rust documents this optimization and its no-follow behavior](https://doc.rust-lang.org/std/fs/struct.DirEntry.html#method.metadata).

## Large-tree profiling in 0.4

The following optimized Windows measurements compare 0.3.1 with the 0.4 changes. Synthetic fixtures are built in memory, without creating a million files on disk. Times exclude fixture construction and destruction unless the operation explicitly measures replacement. They do not measure cold storage, GPU work, or operating-system painting.

| Operation | Before | After | Scope |
| --- | ---: | ---: | --- |
| Replace a million-entry UI model | 113.293 ms | 0.076 ms | UI call; old model destruction now runs on a background thread |
| Dense 8,000-tile frame, rebuild geometry | 18.211 ms | 4.994 ms | Median of 21 CPU frames at 1920 × 1080, including egui shapes and tessellation |
| Same dense map with cached geometry | 17.476 ms | 3.530 ms | Median of 21 CPU frames, including tessellation; not GPU presentation |
| Million-entry glob and size filter | 164 ms | 17.3 ms | Complete in-memory model filter, measured synchronously for comparison |
| Interactive million-entry filter | One synchronous call | 9 slices, longest 2.044 ms | Separate UI fixture; total filter CPU 17.890 ms; old weights remain visible until commit |
| Select from a million tiny siblings | 1.646 ms | 0.681 ms | Median of 31 calls, known parent total, one aggregate tile |
| Build a million-entry live index | 1,559 ms | 222.1 ms | Final run: median of 5 model constructions, excluding input cloning |
| Append 8,192 discovered nodes, p95 | 6.00 ms | 3.98 ms | Streaming to one million files, including model growth |
| Worst observed preview append | 125.24 ms | 16.46 ms | Same stream; old maximum was global index rehashing, new maximum was arena growth |

The UI uses one combined fill/border shape per tile, square corners for tiny rectangles, cached colors, and no filename allocation for tiles too small to label. Its 8,000-tile cap reserves space for remaining siblings, preventing one deep branch from hiding later folders. Per-folder selection receives the already-known parent weight, makes one child pass, and returns the order geometry needs. No full-million-item geometry is constructed by the GUI. The unfiltered sidebar takes its first 100 rows directly; sparse filtered lists inspect at most 4,096 children per frame.

The live path index stores randomized 64-bit hashes and node IDs in 1,024 independent shards. Actual node paths verify matches, with collision buckets preserving distinct identities. This removes the persistent extra normalized full-path allocation per entry and distributes index growth. Initial index construction reuses normalized parent prefixes. These are allocation/layout changes, not a measured total-process-memory claim. A contiguous `Vec<Node>` still occasionally grows: the 16.46 ms maximum copied the arena at 524,289 entries, from capacity 524,352 to 1,048,704. There is no hard upper bound on allocation latency or a promise that every frame stays below 16 ms. Only one tree-bearing event is applied per frame, keeping separate large chunks from accumulating in one UI turn.

Above 32,768 entries, interactive filters inspect at most 1,024 nodes per step and yield after roughly 2 ms. A complete weight buffer is installed atomically. Input is still debounced; the active map remains available for inspection and navigation. Live-event application pauses while filtering so its source stays stable, then explicitly wakes delivery of queued events. Final large models with an active filter are prepared on a worker while the existing preview stays visible. A slice time is a cooperative budget, not a hard real-time deadline; allocation, scheduling, and a single expensive filename match can exceed it.

The scanner now folds direct file bytes during batch merging, finalizes only directories, skips sorting already-ordered wide child lists, and caches compact size/ID sort entries for wide unsorted lists. Five rotated in-memory million-file runs gave these **finalization-phase medians**, excluding filesystem traversal and UI work:

| Shape, observed scanner | 0.3.1 | 0.4 |
| --- | ---: | ---: |
| Flat, equal sizes and ordered names | 62.342 ms | 8.712 ms |
| Flat, random sizes | 226.933 ms | 72.394 ms |
| 1,000 directories, random sizes | 59.123 ms | 33.831 ms |

Complete read-only scans of an existing Rust documentation tree (51,948 files, 1,447 directories, 657,292,993 logical bytes) improved more modestly: observed-scanner median **24.544 → 23.631 ms**, ordinary-scanner median **24.019 → 22.871 ms**, across eight rotated warm-metadata runs. Totals were identical. This is not a whole-drive result. Wide unsorted sorting uses temporary 16-byte-per-child buffers for ordinary scans or 32 bytes per child with observed merging; ordered wide directories avoid those buffers. Storage and antivirus settings were not controlled.

Reproduce the CPU measurements from the repository root:

```sh
cargo test --locked --release --bin space-sniffer profile_million -- --ignored --nocapture
cargo test --locked --release --bin space-sniffer profile_dense -- --ignored --nocapture
cargo run --locked --release --example map_scale_bench
cargo run --locked --release --example model_scale_bench -- 1000000 3
cargo run --locked --release --example scanner_scale_bench -- 1000000 5
cargo run --locked --release --example scanner_scale_bench -- --path EXISTING_DIRECTORY 8
```

The map benchmark includes a frozen 0.3.1 selector for same-process comparisons and checks byte/count equivalence. Dense GUI tests include shape creation and CPU tessellation; selector-only timings do not. The scanner benchmark measures finalization or a complete read-only scan depending on its arguments. Large subtree replacement, native-watcher setup, OS paint latency, true cold-cache disk throughput, and total process memory require separate measurement.

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
