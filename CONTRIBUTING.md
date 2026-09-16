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

Without a path, the GUI displays synthetic demo data. Use a small temporary directory with files of known lengths when checking real scan results.

## Before opening a pull request

Run the same checks used by CI:

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets
cargo build --locked --release
```

For behavior changes, add a focused test covering a real regression or invariant. Keep filesystem tests isolated in temporary directories. Link and permission behavior differs by platform; state which checks ran and which were skipped. The scanner, filter, export, and treemap modules have tests independent of the GUI.

For interface changes, check the demo and a real small scan. Verify folder zoom, breadcrumbs and history, `Ctrl+O`, `F5`, `Backspace`, the Depth slider, Classic colors, and filtering. Include a screenshot when it helps review a visual change.

For scanner changes, check cancellation, unreadable entries, zero-byte files, and directory totals. Preserve the existing treatment of logical sizes, hard links, symbolic links, and Windows reparse points unless the change explicitly revises that contract and its documentation.

For filter or export changes, verify a combined query such as `*.zip; >100MB; |backup*`, invalid input retaining the previous filter, and an exported subtree containing both original and visible byte counts. CSV must keep its quoting and spreadsheet formula protections.

## Reports and pull requests

Include your operating system, Rust version, reproduction steps, expected result, and actual result. For scan issues, describe the directory structure and relevant permissions or link types. Share a minimal example rather than a report containing private paths.

Keep pull requests focused. Describe the user-visible behavior, why it changed, and the checks performed. Update both READMEs when changing controls, filter syntax, installation steps, or size semantics. [Architecture notes](docs/architecture.md) explain the module boundaries.

## Releases

The release workflow runs on pushed tags matching `v*`, for example `v0.1.0`. Before tagging, update `Cargo.toml` and `Cargo.lock` together when changing the package version, run the checks above, and verify a real GUI scan on the supported local platform.

The workflow builds native release binaries on Windows, Ubuntu 22.04, and macOS 14, packages the executable and documentation, and uploads the archives as workflow artifacts. Once every build succeeds, GitHub CLI creates the GitHub release and attaches the archives and `SHA256SUMS`. A rerun updates assets on an existing release. The workflow does not sign executables or produce installers or macOS app bundles.

Contributions are distributed under the [MIT license](LICENSE).
