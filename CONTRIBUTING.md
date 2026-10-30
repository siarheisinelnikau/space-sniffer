# Contributing

Bug reports, focused fixes, performance improvements, and platform testing are welcome. Please write issues, pull requests, interface text, and documentation in English.

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

For application updates, check newer/equal/older versions, prerelease exclusion, offline and request-limit errors, missing assets/digests, cancellation, and rejected size/checksum mismatches. Verify Windows replacement and restart only with a disposable copy of the executable in a writable temporary folder. Keep all real release publishing and tagging separate from local updater tests. The release tag must match the package version embedded in the executable so update checks do not repeatedly offer the same build.

For interface changes, check the empty startup screen and a real scan. Verify arbitrary roots, drive shortcuts, the compact resizable left sidebar, folder zoom, breadcrumbs and history, `Ctrl+O` (`Cmd+O` on macOS), optional `F5`, `Backspace`, Depth, Classic colors, filtering, and Stop. Create, write, rename, and delete files while the app is open; the view should update automatically. Keep a selection inside a changed or removed folder and check navigation recovery. Include a screenshot when it helps review a visual change.

For scanner changes, check cancellation, unreadable entries, zero-byte files, and directory totals. Preserve the existing treatment of logical sizes, hard links, symbolic links, and Windows reparse points unless the change explicitly revises that contract and its documentation.

For progressive-scan changes, verify the root seed and first nonempty batch arrive before final completion. Preview chunks must contain at most 8,192 new nodes, form a contiguous nonrepeating arena prefix, and preserve parent-before-child IDs. A full output queue must not block directory traversal or advance the publication cursor. No repeated whole-tree cloning or rebuilding of the existing path index is allowed. Check observer thread affinity, cancellation after an observer callback, and long final sorting.

While a large first scan is running, inspect, zoom, and filter the partial map. Check discovered counts and discovery-order entries, Stop retaining a marked partial map, CSV staying disabled, and final completion automatically installing the full sorted model while preserving valid selection/zoom/filter. Native changes must start after the final model is installed. The [preview benchmark](examples/preview_bench.rs) distinguishes initial drawable geometry from final model readiness; neither measures actual operating-system window painting.

For filter or export changes, verify a combined query such as `*.zip; >100MB; |backup*`, invalid input retaining the previous filter, and an exported subtree containing both original and visible byte counts. CSV must keep its quoting and spreadsheet formula protections.

For responsiveness changes, run the ignored release-mode `profile_million` and `profile_dense` binary tests, plus the map/model/scanner scale examples listed in [performance notes](docs/performance.md). Record median, tail latency, fixture shape, and what the timer includes. Check filters commit atomically, queued changes resume without another file notification, and every top-level folder remains represented at the tile cap. Keep cold-cache, filesystem traversal, CPU geometry/tessellation, and OS painting claims separate.

Compare one scanner worker with multiple workers on flat, broad, and deep trees. Progress callbacks must stay on the coordinator thread, queues must remain bounded, and cancellation must release workers blocked by backpressure. The [scanner benchmark](examples/scan_bench.rs) and [performance notes](docs/performance.md) describe reproducible runs. State cache conditions and storage type: creating a fixture does not produce a cold cache, and parallel access is not always faster on a hard drive.

For live-update changes, verify totals, matching weights, file/directory/link counts, rename pairs, new subdirectories, root replacement, watcher overflow, and recovery. Avoid full-tree work for a single file update. Removed arena slots can be reused: preserve UI selection by path, and do not assume live parents precede children. Watcher events should wake the UI; an unchanged idle session should not request continuous redraws.

## Reports and pull requests

Include your operating system, Rust version, reproduction steps, expected result, and actual result. For scan issues, describe the directory structure and relevant permissions or link types. Share a minimal example rather than a report containing private paths.

Keep pull requests focused. Describe the user-visible behavior, why it changed, and the checks performed. Keep the interface and documentation in English. Update the README when changing controls, filter syntax, installation steps, or size semantics. [Architecture notes](docs/architecture.md) explain the module boundaries.

### macOS contributions

Use `main` as both your starting point and your PR base, including for macOS-specific work. Keep the scanner, model, filters, live-update pipeline, and interface shared; isolate native desktop behavior in `src/platform/` and use narrow conditional compilation where an OS backend requires it. Follow the [macOS guide](docs/macos.md) for building and packaging. The maintainer has no Mac for manual desktop testing, so a successful build or CI run must not be described as verified GUI support. Please include macOS version, hardware architecture, commands run, observed results, and remaining problems. GUI verification, mounted-volume behavior, permissions, Finder integration, and app packaging/signing contributions are welcome.

## Releases

The release workflow runs on pushed tags matching `v*`, for example `v0.4.2`. Before tagging, update `Cargo.toml` and `Cargo.lock` together when changing the package version, run the checks above, and verify a real GUI scan and the Updates settings on the supported local platform. The tag must exactly match `v` followed by the package version; the workflow rejects mismatches.

Name a standalone executable **`SpaceSniffer-vVERSION.exe`**, for example `SpaceSniffer-v0.4.2.exe`. The current release contains one Windows x86-64 variant, so omit `-x64`. Add `-x64`, `-x86`, or `-arm64` only when the same release offers multiple executable architecture variants. Source archives, checksums, symbols, and documentation do not count as architecture variants. Cargo's internal output remains `space-sniffer.exe`; the release workflow stages and uploads the versioned name. Keep asset names, download references, and updater matching consistent. Do not rename assets on historical releases as part of adopting this rule.

Before tagging, add **`docs/release-notes/vVERSION.md`** with readable English notes describing verified changes since the preceding published release on that channel. Use [the release-notes template](docs/release-notes/TEMPLATE.md), include concrete improvements and fixes, and state relevant platform details or upgrade steps. A compare link can supplement the prose. For a first release, describe initial capabilities instead. The workflow requires the version's notes file and publishes it as the release description; auto-generated commit lists are not a substitute.

CI checks Windows, Ubuntu 22.04, and macOS 15 on Apple Silicon and Intel. The macOS jobs run the shared Rust checks and validate local `.app` bundles without uploading them. The release workflow tests and builds Windows x86-64, then publishes only `SpaceSniffer-vVERSION.exe`, with no archive, installer, or extra release assets. After verifying the uploaded asset, it removes older GitHub release entries and assets; Git tags and commit history are retained. A rerun updates the executable and notes on the tagged release. Executables are not signed.

Use the connected GitHub plugin for remote repository operations and uploads during assisted development. Local Git inspection, staging, and commits are allowed. Do not fall back to remote Git commands, `gh`, authenticated shell HTTP, or credential/login setup unless the user explicitly requests another authentication method. The existing Actions workflow uses its own scoped `github.token` to publish authorized tags. A source push does not itself authorize a new release or changes to historical releases.

The README must include real screenshots of the application and its settings. Refresh them when the interface changes, keep image paths working, and use English captions and alt text. Application releases must retain manual update checks, user-initiated in-app installation, and the persistent startup-check preference.

Contributions are distributed under the [MIT license](LICENSE).
