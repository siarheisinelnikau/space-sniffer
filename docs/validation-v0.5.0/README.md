# Space Sniffer 0.5.0 localization and appearance validation

Validation used isolated artificial filesystem fixtures and settings. No computer-use APIs, OS input injection, real update installation, or user-file changes were involved.

## Automated checks

- `cargo fmt --all -- --check`.
- `cargo clippy --locked --offline --all-targets --all-features -- -D warnings`.
- `cargo test --locked --offline --all-targets`: 88 library tests, 25 app/host tests, 10 live-workflow tests and 38 scanner example tests passed; three manual benchmarks were ignored.
- After the final localization and theme changes, all-feature library/app tests include the new 12-combination language/theme settling regression: 88 library and 26 app/host tests pass, with three manual benchmarks ignored.
- Catalog checks cover all six translations for every key, duplicate keys, matching parameter fields, safe formatting of braces in paths, existing-settings defaults, persisted language/theme selection, and preservation of the startup-check preference.

The UI source audit leaves product names, filesystem content, binary size units, filter examples, symbolic navigation buttons and upstream technical diagnostics in their original form. All consumer labels and known scan/filter/watcher message envelopes use the catalog, including already displayed statuses after changing language. Native file dialog captions are translated; native OS buttons follow Windows language settings.

## Real application images

The optimized Windows binary with `diagnostics,screenshots` uses the production egui components, CPU rasterizer and GDI presentation path. Capture mode validates language/theme/view options, uses isolated settings, suppresses startup update requests, saves its own framebuffer, and exits without persisting capture settings. All final named captures completed with exit code 0 under an external timeout.

Initial bounded captures exposed an existing Windows host shutdown race: winit enters its native wait after the AboutToWait callback, so requesting exit from a completed frame and then restoring an infinite Wait could postpone shutdown until another OS event. The host now supplies one expired WaitUntil deadline when exit has already been requested. This wakes only the final shutdown transition; normal idle scheduling is unchanged. Capture teardown also skips unnecessary settings writes. The final captures verify actual process completion, not just PNG creation.

- [`../screenshot.png`](../screenshot.png): English / dark, main interface, 1280 × 760 pixels.
- [`../light-russian.png`](../light-russian.png): Russian / light, same fixture and viewport.
- [`../updates.png`](../updates.png): English / dark, Updates settings including installed version and saved startup-check preference.
- [`../help-german.png`](../help-german.png): German / light, translated help and sidebar.

The actual PNGs were inspected for hierarchy, contrast, wrapping, clipped controls and consistency. The sidebar scrolls when its content exceeds the available height; the minimum 800 × 520 interface is covered by real-component idle tests in every language/theme combination. Captures establish client-area layout, not OS dialog interaction, accessibility traversal or keyboard/mouse behavior.

## Bounded Windows rendering check

The measured executable and SHA-256 are recorded in `diagnostic-binary.json`. Source inspection identifies `egui_software_backend` CPU rasterization, a `softbuffer` GDI bitmap, retained expose-event presentation, and an event-driven Wait/WaitUntil host. No OpenGL/D3D/DXGI/Vulkan module was loaded by the diagnostic process; module inspection is supporting evidence, not the renderer identification by itself.

The isolated 24-second light/Russian diagnostic started with no scan, minimized itself at eight seconds, restored at sixteen seconds, and closed at twenty-four seconds. `render-trace.jsonl` separates finite transition logic frames from rasterized/presented frames. Settled counters remained flat at 2 UI frames / 2 presents while visible, 4 / 2 while minimized, and 6 / 4 after restoration. Minimized transition logic did not rasterize or present the interface. A separate real-fixture capture exercised populated map rendering.

The final 20.3-second process sample was below CPU timer resolution in all 20 samples, reported as 0.0% of one logical core and 0.0% of the machine on 20 logical processors. The trace writer and sampler are diagnostic overhead. GPU/compositor telemetry and subjective pointer/input latency were not measured. This verifies finite rendering and recovery for the sampled workload; it is not a whole-drive or universal performance guarantee.

The unchanged application icon is checked separately from interface screenshots using resource-only native LoadImageW/DrawIconEx extraction from the final standalone executable. Space Sniffer has no tray interface. Physical Explorer caching, mixed-DPI OS integration, native file picker interaction, and Linux/macOS desktop behavior remain unverified.
