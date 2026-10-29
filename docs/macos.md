# Experimental macOS port

macOS is developed on `main` alongside Windows and Linux, using the same Rust application and shared core. **The supported desktop release is FREE for Windows.** macOS remains experimental, with no published binary or claim of verified desktop support. The maintainer has no Mac available for manual testing. Pull requests and reports from Mac users are welcome; target `main` for all contributions.

## What is prepared

- Shared Rust scanner, model, filters, live-update pipeline, and bounded treemap rendering.
- An isolated macOS desktop adapter in `src/platform/macos.rs`, with `/`, home, and mounted-volume shortcuts, Finder reveal, and `Cmd+O`.
- The existing FSEvents backend and canonical watch-root handling for aliases such as `/var` and `/private/var`.
- Parent monitoring restricted to the nearest existing directory on macOS, avoiding incidental subscriptions to every ancestor through `/`; the root probe remains active for recovery.
- A minimal `.app` bundle template with English privacy descriptions and version metadata taken from `Cargo.toml`.
- A standard-library Python packager that checks the input Mach-O architecture, preserves executable permissions, includes the MIT license, and refuses to overwrite an existing app.
- The common CI workflow runs formatting, Clippy, Rust tests, release builds, and bundle checks on native Apple Silicon and Intel runners. Bundles are checked locally in CI and are not uploaded; automated tests do not replace interactive desktop validation.

## Build on a Mac

Install Rust 1.88+ with Cargo, Python 3.11+, and Apple's Command Line Tools (`xcode-select --install`). Clone the repository and build from `main`:

```sh
git switch main
rustup target add aarch64-apple-darwin
MACOSX_DEPLOYMENT_TARGET=11.0 cargo build --locked --release --target aarch64-apple-darwin
python3 scripts/package_macos.py --target aarch64-apple-darwin
open "target/macos-bundle/aarch64-apple-darwin/Space Sniffer.app"
```

On Intel, substitute `x86_64-apple-darwin` in every command and path. These are separate thin binaries, not a universal app. The template declares macOS 11.0 and the commands set the same deployment target; this is an intended build setting, **not a tested minimum macOS version**. The packager checks only the executable header and bundle layout, not OS compatibility or dependent libraries. To keep another local build, provide a fresh `--output DIRECTORY`; the script never replaces an existing `.app`.

For a direct development run or command-line summary:

```sh
cargo run --locked --release
cargo run --locked --release -- "$HOME"
cargo run --locked --release -- --scan "$HOME"
```

The app is unsigned and not notarized. Packaging does not install it, bypass Gatekeeper, grant filesystem access, or add entitlements. Signing, notarization, a bundle icon, distribution, and minimum-version verification remain contribution opportunities. The official release workflow continues to publish only the Windows `.exe`.

## Permissions and watcher limits

Scans use the current user's permissions. English usage descriptions explain access to Desktop, Documents, Downloads, network volumes, and removable volumes; they do not grant access. Protected or unreadable entries are skipped and counted. Do not require administrator access or silently change system privacy settings.

FSEvents may omit events for network shares or files owned by other users. Current automatic full-snapshot fallback starts when watching reports a setup/runtime error; a watcher that succeeds but stays silent is **not automatically detected**. Use an optional manual rescan when checking such a volume and include this limitation in test reports. Explicit reconciliation/polling controls are welcome future work; avoid unconditional whole-drive polling.

APFS can be case-sensitive or case-insensitive. Do not fix identity issues by lowercasing every Mac path. Validate case-only renames, including the selected root, and composed/decomposed Unicode names before claiming complete live-update support. The selected root's old spelling can remain accessible after a case-only rename while events use the new spelling; lexical event filters need particular scrutiny.

## Verification and pull requests

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets
python3 -m unittest discover -s scripts -p 'test_*.py'
```

Please report which of these checks you completed, your macOS version, Apple Silicon or Intel, Rust version, and whether you launched the bundled app or the Cargo executable. Share isolated test paths and expected/actual byte counts; avoid screenshots containing private filenames.

Manual validation is still needed for:

- Launching from Finder and Terminal, Retina scaling, resize/minimize/restore, dialogs, focus, and `Cmd+O`.
- Progressive first scans on large roots, responsive zoom/filtering, Stop, and memory use.
- Real create/write/truncate/rename/delete notifications, editor-style replacement, root/parent deletion and recovery, volume unmount/remount, and sleep/wake.
- Case-only file, folder, and selected-root renames; Unicode normalization; case-sensitive and case-insensitive APFS volumes.
- Permission-denied folders, protected user folders, symlinks, external/network volumes, and missed-event recovery.
- Finder reveal, clipboard, CSV export, keyboard navigation, signing, and notarization.

The general [contribution guide](../CONTRIBUTING.md) applies. Set the PR base to `main` and keep platform-independent changes in the shared modules. Explain what remains untested rather than treating a green CI job as proof of GUI support.

## References

[Apple's bundle structure guide](https://developer.apple.com/library/archive/documentation/CoreFoundation/Conceptual/CFBundles/BundleTypes/BundleTypes.html), [Apple's Documents permission description](https://developer.apple.com/documentation/bundleresources/information-property-list/nsdocumentsfolderusagedescription), [Rust's macOS target and deployment settings](https://doc.rust-lang.org/rustc/platform-support/apple-darwin.html), and [notify's documented backend limits](https://docs.rs/notify/8.2.0/notify/#known-problems).
