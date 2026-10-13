# Contributing

Bug reports, focused fixes, performance improvements, and platform testing are welcome. English and Russian reports are both welcome.

This is an independent Rust implementation inspired by [Uderzo Software's SpaceSniffer](https://www.uderzo.it/main_products/space_sniffer/), without affiliation. Contribute original code and assets or material whose license permits inclusion under the project's MIT license. Do not copy the original application's source or assets.

## Set up

Follow the platform prerequisites in [README.md](README.md). Use Rust 1.88 or newer and commit intentional `Cargo.lock` changes for dependency updates. The current local verification platform is Windows; reports from Linux and macOS are useful.

```sh
cargo run --locked --release
cargo run --locked --release -- PATH
cargo run --locked --release -- --scan PATH
```

Without a path, the GUI waits for a drive or folder selection and does not scan automatically. Use an isolated temporary directory with files of known lengths when checking scans and live changes.

## Before opening a pull request

Run the same checks used by CI:

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets
cargo build --locked --release
```

For behavior changes, add a focused test covering a real regression or invariant. Keep filesystem tests isolated in temporary directories. Link and permission behavior differs by platform; state which checks ran and which were skipped. The scanner, live model, watcher, filter, export, and treemap modules have tests independent of the GUI.

For interface changes, check the empty startup screen and a real scan. Verify arbitrary roots, drive shortcuts, the compact resizable left sidebar, folder zoom, breadcrumbs and history, `Ctrl+O`, optional `F5`, `Backspace`, Depth, Classic colors, filtering, and Stop. Create, write, rename, and delete files while the app is open; the view should update automatically. Keep a selection inside a changed or removed folder and check navigation recovery. Include a screenshot when it helps review a visual change.

For scanner changes, check cancellation, unreadable entries, zero-byte files, and directory totals. Preserve the existing treatment of logical sizes, hard links, symbolic links, and Windows reparse points unless the change explicitly revises that contract and its documentation.

For progressive-scan changes, verify the root seed and first nonempty batch arrive before final completion. Preview chunks must contain at most 8,192 new nodes, form a contiguous nonrepeating arena prefix, and preserve parent-before-child IDs. A full output queue must not block directory traversal or advance the publication cursor. No repeated whole-tree cloning or rebuilding of the existing path index is allowed. Check observer thread affinity, cancellation after an observer callback, and long final sorting.

While a large first scan is running, inspect, zoom, and filter the partial map. Check discovered counts and discovery-order entries, Stop retaining a marked partial map, CSV staying disabled, and final completion automatically installing the full sorted model while preserving valid selection/zoom/filter. Native changes must start after the final model is installed. The [preview benchmark](examples/preview_bench.rs) distinguishes initial drawable geometry from final model readiness; neither measures actual operating-system window painting.

For filter or export changes, verify a combined query such as `*.zip; >100MB; |backup*`, invalid input retaining the previous filter, and an exported subtree containing both original and visible byte counts. CSV must keep its quoting and spreadsheet formula protections.

Compare one scanner worker with multiple workers on flat, broad, and deep trees. Progress callbacks must stay on the coordinator thread, queues must remain bounded, and cancellation must release workers blocked by backpressure. The [scanner benchmark](examples/scan_bench.rs) and [performance notes](docs/performance.md) describe reproducible runs. State cache conditions and storage type: creating a fixture does not produce a cold cache, and parallel access is not always faster on a hard drive.

For live-update changes, verify totals, matching weights, file/directory/link counts, rename pairs, new subdirectories, root replacement, watcher overflow, and recovery. Avoid full-tree work for a single file update. Removed arena slots can be reused: preserve UI selection by path, and do not assume live parents precede children. Watcher events should wake the UI; an unchanged idle session should not request continuous redraws.

## Reports and pull requests

Include your operating system, Rust version, reproduction steps, expected result, and actual result. For scan issues, describe the directory structure and relevant permissions or link types. Share a minimal example rather than a report containing private paths.

Keep pull requests focused. Describe the user-visible behavior, why it changed, and the checks performed. Keep the interface and documentation in English. Update the README when changing controls, filter syntax, installation steps, or size semantics. [Architecture notes](docs/architecture.md) explain the module boundaries.

## Releases

The release workflow runs on pushed tags matching `v*`, for example `v0.3.0`. Before tagging, update `Cargo.toml` and `Cargo.lock` together when changing the package version, run the checks above, and verify a real GUI scan on the supported local platform.

The workflow builds native release binaries on Windows, Ubuntu 22.04, and macOS 15, packages the executable and documentation, and uploads the archives as workflow artifacts. Once every build succeeds, GitHub CLI creates the GitHub release and attaches the archives and `SHA256SUMS`. A rerun updates assets on an existing release. The workflow does not sign executables or produce installers or macOS app bundles.

Contributions are distributed under the [MIT license](LICENSE).
