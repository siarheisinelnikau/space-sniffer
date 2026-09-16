# Space Sniffer

[Русская версия](README.ru.md)

A native disk usage explorer written in Rust with [eframe / egui](https://github.com/emilk/egui). Scan a folder or drive, find large files in a nested treemap, and explore the results without leaving the app.

![Space Sniffer showing a nested disk usage treemap](docs/screenshot.png)

This is an independent implementation inspired by [SpaceSniffer by Uderzo Software](https://www.uderzo.it/main_products/space_sniffer/). It is not affiliated with Uderzo Software and does not use the original application's source code or assets.

## What works

- Background scanning with progress, cancellation, and a responsive interface.
- A squarified treemap with nested folders, file type colors, adjustable depth, and a Classic palette.
- Folder zoom, breadcrumbs, back/forward navigation, and a selection inspector with the largest child entries.
- Filename and size filters, path copying, and opening an entry in the system file manager.
- CSV export of the current subtree, with original and filtered byte counts.
- A command-line scan summary and synthetic demo data when launched without a path.

Scanning reads filesystem metadata. The app has no file deletion feature or telemetry. Export writes only the report you choose to save.

**Platform status:** built and tested locally on Windows. Linux and macOS build/test jobs are configured in CI; their desktop behavior has not yet been verified locally.

## Build and run

Install [Rust](https://www.rust-lang.org/tools/install) **1.88 or newer** with Cargo. The repository includes `Cargo.lock`; use `--locked` for reproducible dependency selection.

### Windows

Use the MSVC Rust toolchain, plus Visual Studio Build Tools with **Desktop development with C++** and a Windows SDK. The [Rust Windows setup guide](https://rust-lang.github.io/rustup/installation/windows.html) describes the prerequisites.

From the repository directory in PowerShell:

```powershell
cargo run --locked --release
cargo run --locked --release -- "C:\Users"
cargo run --locked --release -- --scan "C:\Users"
```

The first command opens the demo. The second opens the app and scans that path. `--scan PATH` prints totals and up to 20 of the root's largest children without opening a window. A regular file is also a valid scan root.

To build the executable without launching it:

```powershell
cargo build --locked --release
.\target\release\space-sniffer.exe
```

### Linux

For Debian/Ubuntu, install a C/C++ toolchain and the graphics development packages used by eframe's X11/Wayland backends:

```sh
sudo apt-get update
sudo apt-get install -y build-essential pkg-config libx11-dev \
  libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev \
  libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev \
  libgl1-mesa-dev libegl1-mesa-dev libssl-dev
cargo run --locked --release -- "$HOME"
```

See the [eframe Linux prerequisites](https://docs.rs/crate/eframe/0.33.0). Running the GUI requires a graphical desktop and working OpenGL drivers. File dialogs use rfd's default [XDG Desktop Portal backend](https://docs.rs/rfd/0.15.4/rfd/#linux--bsd-backends), which needs `xdg-desktop-portal` and a backend for your desktop, such as `xdg-desktop-portal-gtk` or `xdg-desktop-portal-kde`. GTK development headers are not required by this app's default dialog backend.

### macOS

Install Rust and Xcode Command Line Tools (`xcode-select --install`), then run:

```sh
cargo run --locked --release -- "$HOME"
```

The release workflow packages a Windows x86-64 ZIP, a Linux x86-64 GNU tarball, and a macOS Apple Silicon tarball when a `v*` tag is pushed, and publishes them to [GitHub Releases](https://github.com/siarheisinelnikau/space-sniffer/releases). These are executable archives, not installers or macOS app bundles.

## Explore a scan

Choose **Open folder**, enter a path and press Enter, or pass a path on the command line. During a scan, the previous view stays available until a complete result replaces it. Cancelling retains that view.

| Control | Action |
| --- | --- |
| Click a tile or list entry | Inspect it |
| Double-click a folder | Zoom into it |
| Breadcrumbs, back/forward arrows | Navigate the scan |
| `Ctrl+O` | Choose a folder |
| `F5` | Scan the path currently in the path field |
| `Backspace` | Go to the parent folder when not editing text |
| **Depth** | Show 1–6 nested levels |
| **Classic** | Switch the treemap palette |
| **Copy path** / **Reveal** | Copy the selection's path / open the system file manager |

Zero-byte entries are available in the list but have no treemap area. The inspector lists up to 100 of a folder's largest children.

## Filters

Filters apply to file basenames, ignoring case. Plain text matches a substring; patterns containing `*` or `?` match the whole name. `*` matches any number of characters and `?` matches one character. Join clauses with `;` to require all of them. Prefix a name pattern with `|` to exclude it.

| Query | Meaning |
| --- | --- |
| `report` | Names containing `report` |
| `*.zip` | Names ending in `.zip` |
| `photo?.jpg` | Names such as `photo1.jpg` |
| `\|backup*` | Exclude names beginning with `backup` |
| `>100MB` | Files strictly larger than 100,000,000 bytes |
| `<2GiB` | Files strictly smaller than 2,147,483,648 bytes |
| `*.zip; >100MB; \|backup*` | ZIP files over 100 MB, excluding names beginning with `backup` |

`KB`, `MB`, `GB`, `TB`, `PB`, and `EB` use powers of 1000. `KiB`, `MiB`, `GiB`, `TiB`, `PiB`, and `EiB` use powers of 1024. Bare numbers mean bytes; decimal quantities such as `>1.5MiB` must resolve to whole bytes. Filters do not include date or tag syntax. An invalid query displays an error and leaves the previous valid filter active.

**Export CSV** saves every entry in the currently zoomed subtree, including entries hidden by a filter and zero-byte entries. Columns are `path`, `kind`, `bytes`, and `visible_bytes`. Directory rows contain aggregate sizes, so summing every row would count descendants repeatedly. CSV paths are escaped and protected against spreadsheet formula interpretation.

## Size semantics and current limits

- Sizes are logical file lengths, not allocated disk space. Compression, sparse files, filesystem metadata, and free space are not measured.
- Hard links count separately for every path; their shared storage is not deduplicated.
- Symbolic links and Windows reparse points, including junctions, are not traversed and contribute zero bytes. Selecting a link as the scan root produces an error.
- Unreadable or disappearing descendants are skipped and counted. An unreadable root produces an error. A scan can therefore be partial; it does not bypass permissions.
- Each scan uses one worker thread. Parallel subscans and a live filesystem watcher are not implemented. Use `F5` after files change; a scan is not an atomic filesystem snapshot.
- The full scan is held in memory. Very large trees can take time to scan, filter, and draw. Cancellation is cooperative and cannot interrupt an operating system call already in progress.

## Contributing and license

See [CONTRIBUTING.md](CONTRIBUTING.md) for checks and manual verification, and [docs/architecture.md](docs/architecture.md) for the module layout.

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets
cargo build --locked --release
```

Released under the [MIT license](LICENSE).
