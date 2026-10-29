# Architecture

Space Sniffer 0.4 is a native Rust disk explorer with a progressively populated initial map, parallel scanning, and automatic filesystem updates. eframe hosts egui with Glow/OpenGL; rfd supplies file dialogs. The GUI starts empty until the user chooses a drive, folder, or root path. Controls and selection details share a compact, resizable left sidebar.

## Modules

| Module | Responsibility |
| --- | --- |
| `src/main.rs` | GUI/CLI arguments and native window startup |
| `src/app.rs` | Sidebar, live events, navigation, cached geometry, filtering, drawing, and reveal commands |
| `src/platform/` | Isolated Windows/macOS/Unix adapters for root shortcuts, file-manager reveal, and console attachment |
| `src/lib.rs` | Core library exports and binary-unit byte formatting |
| `src/model.rs` | Node arena, entry kinds, parent/child IDs, and scan statistics |
| `src/scanner.rs` | Parallel metadata traversal, bounded progress, cancellation, sorting, and initial totals |
| `src/live.rs` | Bounded scan-preview publication, watcher lifecycle, change batches, and recovery |
| `src/live_tree.rs` | Validated preview appends, path index, incremental totals/statistics/filter weights, and arena slot reuse |
| `src/filter.rs` | Filename/size clause parsing, leaf matching, and static scan weights |
| `src/treemap.rs` | Squarified geometry independent of egui |
| `src/map_items.rs` | Visible-item selection and aggregation of tiny siblings while preserving bytes |
| `src/export.rs` | Validated iterative CSV subtree export |

## Initial scan

`ScanTree` contains a `Vec<Node>`, root `NodeId`, and `ScanStats`. Nodes store path, name, kind, logical size, optional modification time, children, and parent. Initial scans insert parents before children, allowing reverse aggregation without recursion.

`scanner::scan_with_progress(&Path, &AtomicBool, impl FnMut(ScanProgress))` is the shared scan entry point. Its callback runs on the calling coordinator thread and need not implement `Send`. `scan_path` wraps it for synchronous callers and `--scan PATH`; `start_scan` wraps it in a background thread for channel-based callers.

`scanner::scan_with_observer(&Path, &AtomicBool, report, observe)` adds a borrowed `&ScanTree` preview hook on that same calling thread. It emits the root seed, the first nonempty directory batch immediately, roughly 100 ms updates during traversal/finalization, and the final tree. Arena IDs remain append-only with parents before children. File metadata is accurate when inspected; directory sizes can be unaggregated and child lists partial or unsorted. Observers must be quick and nonblocking. The scanner makes no preview clones and checks cancellation after each observer call. Existing no-observer APIs retain a separately optimized fast path; CLI output is unchanged.

The coordinator owns the arena and statistics. Up to `min(available_parallelism, 8)` scoped workers enumerate independent directories and construct batches of at most 512 entries. Each worker has a one-job channel; a shared results channel holds at most two batches per worker. Metadata collection needs no per-file mutex. Only the coordinator merges nodes, schedules directories, and calls progress callbacks.

`DirEntry::metadata()` preserves no-follow semantics and reuses cached enumeration metadata on Windows, avoiding an extra filesystem call per file. Queued directories are checked for links/reparse points again before enumeration. File bytes are accumulated into their direct directory during batch merging; finalization walks only discovered directories in reverse order. Children are sorted by descending size, then name/path. Wide already-ordered lists skip sorting; wide unsorted sorts cache compact size/ID entries and observed sorts retain cancellation checkpoints. Worker order can change arena IDs; visible ordering and totals remain deterministic.

Progress is throttled to roughly 100 ms, including waits for directory I/O. `start_scan` bounds its public channel to two events, drops excess intermediate progress, and reliably sends completion while the receiver exists. Atomic cancellation discards the scanner's partial return value; the GUI can retain an already published preview. Job and result channels disconnect before joins on cancellation/error, releasing workers blocked by backpressure. Operating system calls already in progress cannot be interrupted. Exceptionally wide observed child lists sort in bounded runs with merge checkpoints so long final sorting remains observable and cancellable.

## Progressive preview transport

The live worker first publishes a one-node root seed through `LiveEvent::Preview { start, nodes }`. Subsequent chunks contain at most 8,192 previously unpublished nodes from the scanner's dense arena prefix. Node names, paths, parent IDs, kinds, file lengths, and modification times are copied; source child vectors are never copied, and directory sizes are reset to zero. The GUI reconstructs children and discovered totals as it appends nodes.

Preview publication uses `try_send` on the existing bounded output queue and never waits for the UI. The publication cursor advances only after successful delivery, so chunks remain contiguous and never repeat a prefix. Slow or hidden UIs can fall behind scanning; observer cadence does not promise that every discovered entry is delivered before completion. There is no repeated whole-tree copy or full index rebuild per chunk.

`LiveTree::append_scan` validates a new chunk before mutation, appends its nodes, and updates the path index, ancestor sizes, matching weights, and discovered counters. Preview children remain in discovery order. The UI labels these data as **Discovered so far** and **Discovered entries**, rather than presenting a completed largest-first listing.

After aggregation and sorting, the worker builds the authoritative full `LiveTree` and publishes `Ready(Box<LiveTree>)`. This supersedes the partial prefix even when some nodes were never sent as previews. The GUI preserves its active filter and restores selected/zoomed paths where possible. Native change batches follow Ready, using the existing live-update path.

Stopping during the initial scan keeps an explicitly marked partial map; it does not manufacture a completed result. CSV export remains disabled until a full initial scan finishes. The current partial map stays inspectable and can be replaced by restarting the root.

## Watching and recovery

The GUI calls `live::start_live_with_wake`. The worker publishes bounded previews during scanning and builds the final index before `LiveEvent::Ready(Box<LiveTree>)`, keeping full path-index construction away from the UI. Other events carry progress, status, changes, or failures. Published events wake egui through `request_repaint`; there is no continuous idle repaint timer. Live updates after Ready retain the existing event-coalescing and recovery behavior.

The live worker uses notify's `RecommendedWatcher`: ReadDirectoryChangesW on Windows, inotify on Linux, and FSEvents on macOS. Link following is disabled. Nonrecursive watches on ancestors and a recursive directory-root watch are established before the initial scan. Ancestor watches and a 500 ms root metadata probe help recover deletion/recreation and file-root transitions. Root probes inspect the root itself, not the whole tree.

The raw notification queue is bounded to 1,024 events, output to eight events, and pending paths to 4,096. Changes coalesce after 100 ms of quiet, with a maximum 250 ms batch age during sustained writes. Access and access-time-only events are ignored to avoid loops triggered by scanning. These are batching intervals, not end-to-end latency guarantees.

The GUI applies at most one tree-bearing event per frame and yields after a cooperative 4 ms status/progress budget. This leaves input and drawing opportunities between preview chunks; an individual large allocation or subtree change can still take longer.

Ordinary file events inspect metadata and produce `Upsert` or `Remove`. Created, renamed, or uncertain directory events rescan only the affected subtree with `ReplaceSubtree`. Lost events, pending overflow, and explicit watcher rescan requests trigger root reconciliation.

Native setup failures or runtime errors publish a status change and switch to full snapshot polling every two seconds, retrying native watching every five seconds. Unchanged polling snapshots are suppressed. Polling is a recovery mode and can be expensive on a large root; notification support and network-share behavior depend on the platform/filesystem.

## Incremental model

`LiveTree` maintains a path-to-ID index, live-slot markers, reusable slots, the active filter, and visible-byte weights. Ordinary file changes update the leaf and its ancestors, adjust file/directory/link counters, and reposition siblings using binary search. Directory replacement walks the affected subtree. Editing a filter recomputes connected-node weights without restarting filesystem scanning.

The index is split into 1,024 small hash maps, storing randomized normalized-path hashes and node IDs rather than duplicate full-path strings. Hits are verified against the actual node path; explicit collision buckets preserve distinct paths even with identical hashes. Construction caches normalized directory prefixes and processes ordered initial arenas sequentially. Sharding spreads resizing work across small maps, although the contiguous node arena can still require a large reallocation during growth.

Removed slots are disconnected, zeroed, and recycled after an update batch. A live tree therefore does **not** preserve parent-before-child arena order. Live filtering traverses connected children rather than relying on static reverse-index aggregation. The UI restores selection and zoom by path because later batches can reuse removed IDs; deleted views return to a surviving ancestor.

An initial scan only supplies a global skipped count, initially attributed to the root. Later subtree refreshes track their own skipped counts; root reconciliation restores a fresh baseline. There is no persistent on-disk index.

## Sizes and boundaries

Regular files contribute `Metadata::len()` and directories aggregate descendants using saturating arithmetic. These are logical lengths, not allocated disk space. Hard-link paths count independently. Symbolic links and all Windows reparse points, including junctions, are zero-byte leaves and are never intentionally traversed. Link roots are rejected. Roots become absolute without resolving links, supporting reveal and exported paths.

Root access failures fail a scan. Unreadable, disappearing, or unsupported descendants are skipped and counted, producing a potentially partial result. Scans and live views observe a changing filesystem; they are not atomic snapshots. Filesystem permissions and notification limits still apply.

## Interface, filters, and geometry

The sidebar holds root selection, status, filters, depth/colors, selection details, discovered/largest entries, and export. Changing the root starts a new session; Stop cancels scanning/watching while retaining a marked partial map when applicable. `F5` is an optional full rescan. Startup waits for root selection. CSV is enabled only after initial completion.

Filters combine semicolon-separated clauses with AND. Names use case-insensitive substrings or whole-name `*`/`?` globs; `|` excludes a pattern. Strict `<` and `>` accept decimal or IEC quantities. Invalid queries retain the last valid filter. Input is debounced for 150 ms; live updates maintain weights for the active filter. ASCII matching avoids temporary lowercase/character buffers, with a Unicode fallback.

`LiveTree::begin_filter` creates a revision-guarded `FilterJob`; bounded `step` calls build a new weight buffer without changing active weights. `commit_filter` rejects stale/incomplete jobs and atomically installs a completed result. The GUI uses 1,024-node steps and a cooperative 2 ms budget for models above 32,768 entries, pausing live-event application until completion. Ordered scans aggregate backwards; models with recycled IDs use a child-cursor postorder traversal without pushing every sibling onto a stack. Final large models with an existing filter are prepared on a worker before replacement. Old large models and stopped live receivers are released on background threads so destruction cannot stall an interaction.

`treemap::layout` accepts IDs, weights, and bounds, borrowing already-ordered nonzero input when possible and computing in `f64` before returning guarded `f32` rectangles. `visible_items_with_total` uses the parent weight to select drawable children in one pass and aggregate the rest. The GUI nests layouts to the selected depth and caches geometry and colors by view revision, bounds, zoom, and depth. Changes outside the zoomed subtree update totals without invalidating that subtree's geometry. A global 8,000-tile limit reserves one slot for each pending sibling. Painting combines fill/border shapes and omits rounded corners and labels on tiny tiles. The largest-entry list caches at most 100 rows; sparse filtered lists advance through bounded child slices.

## Exports and platform actions

`export::write_csv` validates the root, weight count, child bounds, and duplicate/cyclic subtree links before writing. Iterative traversal exports connected subtree entries, including filtered and zero-byte entries, with `path`, `kind`, `bytes`, and `visible_bytes`. Directory rows are aggregates and must not be summed with descendants.

CSV uses UTF-8, CRLF records, quote escaping, and a leading apostrophe for potential spreadsheet formulas in paths. Non-UTF-8 paths are represented lossily. Reveal uses `explorer.exe`, `open -R`, or `xdg-open`. Export writes the chosen report; scanning/watching do not modify scanned files. No deletion action or telemetry is implemented.

Desktop integrations live behind `src/platform/mod.rs`, with separate Windows, macOS, and Unix implementations. Platform commands receive literal arguments without a shell. The GUI uses egui's platform-aware command modifier (`Ctrl+O` on Windows/Linux, `Cmd+O` on macOS). All platforms are developed on `main`, sharing the scanner, model, filters, live-update pipeline, geometry, and GUI. macOS-specific FSEvents parent-watch handling uses narrow conditional compilation in `src/live.rs`; app packaging lives in `scripts/package_macos.py` and `packaging/macos/Info.plist`. See the [macOS guide](macos.md); CI builds do not replace manual macOS GUI testing.

## Verification and packaging

Tests cover scan totals, serial/parallel equivalence, callback affinity, cancellation/backpressure, deep trees, hard links, junctions, incremental updates, watcher behavior, filters, CSV safety, navigation, and geometry. See [performance notes](performance.md) for reproducible timings and cache limits.

CI checks formatting, Clippy, tests, and release builds on Windows, Linux, and both Apple Silicon and Intel macOS runners. The same check job serves every platform, with additional macOS steps to test the Python packager, stage an unsigned `.app`, and inspect its plist, executable permissions, and linked libraries. These bundles are not uploaded. Local desktop verification is on Windows; configured CI jobs do not establish desktop behavior elsewhere. Tags publish only the standalone Windows x86-64 MSVC `space-sniffer.exe`. The workflow verifies its upload before deleting older GitHub release entries and their assets, retaining Git tags and commit history. The executable is unsigned; Linux and macOS remain buildable from source.

This independent implementation is inspired by [Uderzo Software's SpaceSniffer](https://www.uderzo.it/main_products/space_sniffer/) without affiliation or reuse of its code/assets. The project is [MIT licensed](../LICENSE).
