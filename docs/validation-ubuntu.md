# Ubuntu implementation validation — 2026-11-03

Environment: Ubuntu 25.04, x86-64, GNOME/Wayland, Rust 1.95.0. The local
machine exposes 16 CPUs. Dependencies use the existing Cargo lockfile and
the shared updater v0.3.1 revision `78e10609c97b27bf1220855b9a573f348590b095`.

## Completed checks

- `cargo fmt --all -- --check` passed.
- `cargo clippy --locked --all-targets -- -D warnings` passed; the final
  screenshot-enabled variant also passed with `--features screenshots`.
- `cargo test --locked --all-targets` passed: 93 library, 31 application,
  10 live-workflow tests, and 22 scanner/mount tests in each of two examples
  (178 executions). The two manual performance tests are ignored by default.
- `cargo build --locked --release` passed.
- `cargo test --locked --release --bin space-sniffer profile_ -- --ignored --nocapture`
  passed both manual benchmarks. These use in-memory generated trees, not
  physical-disk scans or operating-system presentation timings.
- Five Python packaging tests passed, covering both the existing macOS
  packager and the new Linux packager.
- `desktop-file-validate` passed without warnings.
- The final `.deb` was built with `dpkg-deb`; its control metadata, root-owned
  files, executable permissions, desktop entry, icon and licenses were inspected.
- The real CLI reported exactly 175 MiB, four files, five directories, no
  skipped entries and no links for the isolated smoke fixture.
- The real empty, volume-colored and Updates windows were captured and
  visually inspected using Xvfb/Mesa X11 and the current native Wayland
  desktop. Capture settings and data were isolated from the user's settings.

The application and the separate shared updater workspace pass Clippy with warnings denied. Fifteen offline release-publisher tests also pass. Updater tests cover Debian metadata, package digests, cancellation, private staging cleanup, release selection and restart handling for an exited parent.

## Volume behavior covered

Tests cover nested mounts, component boundaries, optional mountinfo fields,
escaped whitespace/backslashes, non-UTF-8 paths, malformed input, network and
virtual filesystem identities, file bind mounts, root ancestor aliases, and
the same color for bind aliases of a device. Mixed directory and grouped tiles
do not claim a single volume. UI tests cover persisted opt-in settings, geometry
reuse when toggling colors, and cache invalidation on refresh.

Mount parsing is shared by the desktop adapter and scan/watch scope. The
scanner, live model, filtering and treemap remain one cross-platform pipeline.
Refresh canonicalization runs on a background worker, and mount shortcuts do
not stat remote paths.

## Diagnostic timings

One local release-mode run, with warm in-memory fixtures:

| Fixture / measured work | Median | p95 |
| --- | ---: | ---: |
| Dense map, 8,000 tiles: rebuild and tessellate | 8.586 ms | 10.170 ms |
| Dense map: cached geometry and tessellate | 2.983 ms | 3.673 ms |
| Million-node egui frame: rebuild | 1.031 ms | 1.171 ms |
| Million-node egui frame: cached | 0.068 ms | 0.113 ms |

The million-node filter used 13 slices, with a longest UI call of 2.045 ms
and 26.003 ms total CPU work. Model replacement on the UI took 0.060 ms.
These observations are not before/after performance claims or disk benchmarks.

## Root scan regression

The original `/` scan included `/proc/kcore`, whose reported regular-file
length on this host is 140,737,471,598,592 bytes (nearly 128 TiB). Native recursive
watch setup also entered virtual filesystems. Separately, a permission error in
an unreadable log directory disabled the entire root watcher and triggered a
full root scan every two seconds.

The shared scope now excludes virtual filesystems before metadata inspection,
including in previews, callbacks, and live rescans. Watch setup partitions at
excluded mounts and inaccessible descendants. Bind mounts that lead back into
scanned storage are pruned. Large complete root updates build their index and
filter off the UI thread, retaining the previous map until ready.

Focused tests cover a sparse 1 TiB stand-in for kcore, serial/parallel scan
agreement, ordinary files preserving their logical sizes, real native events
under filtered watch boundaries, new directories, permission recovery, lost
notification recovery, and navigation/filter preservation across a root update.

On the actual host, the final CLI and native Wayland map both showed **44.0 GiB**,
348,230 files, 44,918 directories, 35,686 links, and 121 skipped entries. These
counts are a point-in-time observation of accessible logical file sizes, not
allocated disk usage. Warm-cache CLI traversal took approximately 0.37 seconds
in an earlier run; the final Wayland process reached its captured, complete
LIVE map and exited normally in 2.268 seconds, including watch setup and rendering.

An isolated Xvfb/Mesa run scanned `/`, sent three F5 rescans and 20 file writes,
and checked 100 `_NET_WM_PING` responses over 20 seconds. Response times were
1.175 ms median, 1.313 ms p95, and 11.324 ms maximum. Peak sampled RSS was
639,404 KiB during repeated rescans. This tests window-event responsiveness,
not physical display presentation latency. A separate 15-second live-worker
check established native watching and did one initial lost-event reconciliation;
it did not enter the previous repeated polling loop.

The installed normal release was then opened on `/` in the native Wayland
session. A ten-second observation after startup used 1.38 CPU-seconds, with
404,992 KiB RSS and a 458,536 KiB process high-water mark. This is a bounded
local observation; other filesystem activity affects idle resource use.

The map, model, scanner-finalization and preview scale examples also passed.
For five warm synthetic million-file samples, model construction measured
244.425 ms median / 273.775 ms p95 and combined glob/size filtering measured
22.615 ms / 25.117 ms. Streaming 8,192-node batches measured 3.033 ms median /
4.961 ms p95 (123 batches); the worst allocation-growth batch was 29.956 ms.
The generated flat/broad/deep scanner fixture passed with 1, 2, 4 and 8 workers.
These benchmarks exclude physical disk I/O except the generated scanner fixture,
which was created on tmpfs and has warm metadata.

## Ubuntu updater release checks

The separate updater v0.3.1 workspace passed Linux tests and Clippy locally. The
Sniffer v0.6.0 build passed all 178 default test executions, the five packaging
tests, fifteen release-publisher tests, and real X11/Wayland GUI smoke checks.
The normal release and Debian package were rebuilt after the capture build.

A local Sniffer 0.5.1 bootstrap package with the same updater was installed through
apt to allow a real upgrade to the public v0.6.0 release. Its installed binary
hash matched the tested bootstrap binary, and `dpkg -V` reported no differences.
The final interactive GitHub download, authorization and restart sequence is
reserved for the user to exercise after publication.

## Verification limits

The generated package targets this machine's ABI (`libc6 >= 2.39` and
`libssl3t64`, among its dependencies). It was installed system-wide through apt; `dpkg -V` passed and the installed
executable hash matched the tested release binary.
Ubuntu 22.04/24.04 jobs are configured to produce packages on their own release;
those remote CI jobs have not been run as part of this local change.

Native Wayland launch/rendering was checked, but interactive portal dialogs,
physical removable-device hotplug, real multi-disk/RAID layouts, and desktop interactions beyond the bounded root checks above were not
manually exercised. Multi-volume mapping was
tested with fixtures; the GUI sample files reside on one tmpfs volume. Windows
and macOS were not built or run locally. The same project and their CI jobs
remain in place.

The legacy eframe after-swap screenshot produced a black Wayland image. The
development capture now uses the viewport screenshot event before swapping
buffers, rejects a uniform image, and exits through the normal close path. Both
X11 and Wayland captures succeeded with it. Normal builds omit that capture code.
