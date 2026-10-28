# Architecture

Space Sniffer is a native Rust application. eframe hosts the egui interface and uses the Glow/OpenGL renderer. rfd provides platform file dialogs. The filesystem and geometry code is exposed through the library crate so tests and the command-line mode can use it without opening a window.

## Modules

| Module | Responsibility |
| --- | --- |
| `src/main.rs` | Parse GUI/CLI arguments and start the native window or synchronous summary scan |
| `src/app.rs` | Interface state, scan progress, navigation, inspector, filtering, treemap drawing, and platform reveal commands |
| `src/demo.rs` | Build synthetic data for the initial demo view |
| `src/lib.rs` | Export core modules and format byte values using binary units |
| `src/model.rs` | Node arena, entry kinds, parent/child indices, and scan statistics |
| `src/scanner.rs` | Scan filesystem metadata, report progress, handle cancellation, and aggregate directory sizes |
| `src/filter.rs` | Parse filename/size clauses, match leaves, and compute filtered ancestor sizes |
| `src/treemap.rs` | Compute squarified rectangles independently of egui |
| `src/export.rs` | Validate and export a subtree as CSV |

## Scan and model

`ScanTree` holds a `Vec<Node>`, a root index, and `ScanStats`. A `NodeId` is an index into that arena. Nodes carry their path, name, entry kind, logical size, optional modification time, children, and optional parent. Parents are inserted before children. Scanner and filter code rely on that ordering to aggregate sizes by walking the arena backwards without recursive calls.

The GUI starts one named worker thread with `scanner::start_scan`. The scanner uses an explicit stack of pending directories and `std::fs` metadata calls. It scans serially; there is no pool of workers or parallel subscan feature. Children are sorted by descending size with deterministic name/path tie breaks after aggregation.

Progress is throttled to roughly 100 ms and sent through a channel bounded to two events. Intermediate progress can be dropped when the queue is full; completion is delivered unless the receiver has gone away. An atomic flag requests cancellation. Cancellation discards the partial result. The GUI polls the receiver, keeps the previous tree visible, and replaces it only on success. Starting another scan requests cancellation of the previous one.

The synchronous `scanner::scan_path` uses the same implementation for `--scan PATH` and tests. The CLI prints totals and up to 20 root children, without creating an egui window.

Sizes use `Metadata::len` for regular files. Directory values sum descendant regular files. Hard links count once per path. Symbolic links and Windows reparse points are zero-byte leaves and are not traversed. Queued directories are checked again before reading them. Root errors fail the scan; inaccessible, disappearing, or unsupported descendant entries increment the skipped count and allow a partial result.

The result is held entirely in memory and is not an atomic filesystem snapshot. There is no filesystem watcher or incremental refresh. A new scan replaces the old result; no on-disk index is maintained.

## Filtering and geometry

`Filter::parse` produces AND-connected clauses from semicolon-separated input. Name clauses are case-insensitive substrings or whole-name `*`/`?` globs, optionally excluded with a leading `|`. Strict `<` and `>` clauses accept decimal and IEC byte quantities. Invalid input produces a readable error; the GUI retains its last valid set of weights.

`Filter::weights` returns one `u64` per node. Matching leaves contribute their original size, and their ancestors receive the sum. An empty filter preserves scanned sizes exactly. The original `ScanTree` stays unchanged. Both scan and filter accumulation use saturating addition to avoid integer overflow.

`treemap::layout` accepts IDs and byte weights with a rectangular bound. It sorts nonzero items by descending weight and computes a squarified layout. Geometry uses `f64` internally and returns guarded `f32` rectangles. Zero-byte items have no area; extreme ratios may produce subpixel tiles. The GUI nests layouts to the selected Depth, draws file type or Classic colors, and maps hit tests back to node IDs.

## Navigation and exports

The GUI stores selection and zoom IDs separately, plus back/forward stacks. Breadcrumbs follow parent indices. A successful replacement scan resets navigation to its root. File manager reveal uses `explorer.exe` on Windows, `open -R` on macOS, and `xdg-open` on other desktop platforms.

`export::write_csv` receives the current zoom root and weights. It validates the root, weight count, child bounds, and duplicate/cyclic subtree links before creating the output file. Traversal is iterative. Every subtree entry is exported, including hidden and zero-byte entries, with `path`, `kind`, `bytes`, and `visible_bytes` columns. Directory rows are aggregates and must not be summed together with their descendants.

CSV is UTF-8 with CRLF record endings and proper quote escaping. Non-UTF-8 platform paths are represented lossily. Potential spreadsheet formulas in path cells receive a leading apostrophe. Export is the only feature that intentionally writes user data; scanning does not modify the scanned files. No telemetry is implemented.

## Verification and packaging

Unit tests cover scanner aggregation and filesystem cases, filter syntax and Unicode globs, CSV quoting/formula safety, and geometry invariants. CI runs formatting, Clippy, tests, and release builds on Windows, Linux, and macOS. Only Windows has been verified locally so far; CI configuration is not a claim of desktop testing on the other platforms.

The tag-triggered release workflow builds Windows x86-64 MSVC, Linux x86-64 GNU on Ubuntu 22.04, and macOS Apple Silicon on macOS 14. It packages binaries and documentation, then uses GitHub CLI to publish archives and checksums. It does not use a third-party publishing action or produce an installer, signed executable, or macOS app bundle.

This project is an independent implementation inspired by [Uderzo Software's SpaceSniffer](https://www.uderzo.it/main_products/space_sniffer/), without affiliation or reuse of its source code or assets. The repository is licensed under [MIT](../LICENSE).
