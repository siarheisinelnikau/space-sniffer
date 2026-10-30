//! Windows CPU rasterization and GDI presentation; no OpenGL or DirectX context.
// Native host integration adapted from Internet Uptime Monitor (MIT).
// Portions copyright (c) 2026 siarheisinelnikau; see LICENSE.
use crate::app::App;
use eframe::{Storage, egui};
use egui_software_backend::{BufferMutRef, ColorFieldOrder, EguiSoftwareRender};
use std::{
    collections::HashMap,
    fs,
    io::Write,
    num::NonZeroU32,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};
use winit::{
    application::ApplicationHandler,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop, OwnedDisplayHandle},
    window::{Window, WindowId},
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
// Coalesce scan/input bursts rather than letting CPU rasterization monopolize a core.
const FRAME_INTERVAL: Duration = Duration::from_micros(16_667);

struct Repaint {
    when: Instant,
    pass: u64,
    #[cfg(feature = "diagnostics")]
    command: Option<egui::ViewportCommand>,
}

#[derive(Default)]
struct FrameSchedule {
    next: Option<Instant>,
    last: Option<Instant>,
}

impl FrameSchedule {
    fn request(&mut self, when: Instant) {
        let when = self
            .last
            .map_or(when, |last| when.max(last + FRAME_INTERVAL));
        self.next = Some(self.next.map_or(when, |next| next.min(when)));
    }

    fn control_flow(&self) -> ControlFlow {
        self.next.map_or(ControlFlow::Wait, ControlFlow::WaitUntil)
    }
}

struct SoftwareWindow {
    app: App,
    surface: softbuffer::Surface<OwnedDisplayHandle, Arc<Window>>,
    _context: softbuffer::Context<OwnedDisplayHandle>,
    window: Arc<Window>,
    input: egui_winit::State,
    info: egui::ViewportInfo,
    renderer: EguiSoftwareRender,
    pending_textures: egui::TexturesDelta,
    has_frame: bool,
    shown: bool,
}

struct SoftwareHost {
    context: egui::Context,
    root: Option<PathBuf>,
    window: Option<SoftwareWindow>,
    schedule: FrameSchedule,
    storage: FileStore,
    error: Option<Box<dyn std::error::Error>>,
    trace: Option<FrameTrace>,
    #[cfg(feature = "screenshots")]
    screenshot: Option<PathBuf>,
    #[cfg(feature = "screenshots")]
    capture_frames: u8,
}

impl SoftwareHost {
    fn initialize(&mut self, event_loop: &ActiveEventLoop) -> Result<()> {
        if let Some(memory) = eframe::get_value::<egui::Memory>(&self.storage, "egui") {
            self.context.memory_mut(|current| *current = memory);
        }
        let mut builder = egui::ViewportBuilder::default()
            .with_title("Space Sniffer")
            .with_inner_size([1280.0, 760.0])
            .with_min_inner_size([800.0, 520.0])
            .with_icon(crate::app_icon())
            .with_visible(false);
        if let Some(mut settings) =
            eframe::get_value::<egui_winit::WindowSettings>(&self.storage, "window")
        {
            let largest = event_loop
                .available_monitors()
                .map(|monitor| {
                    let size = monitor.size().to_logical::<f32>(monitor.scale_factor());
                    egui::vec2(size.width, size.height)
                })
                .fold(egui::vec2(800.0, 520.0), |a, b| a.max(b));
            settings.clamp_size_to_sane_values(largest);
            settings.clamp_position_to_monitors(self.context.zoom_factor(), event_loop);
            builder = settings.initialize_viewport_builder(
                self.context.zoom_factor(),
                event_loop,
                builder,
            );
        }
        let window = Arc::new(egui_winit::create_window(
            &self.context,
            event_loop,
            &builder,
        )?);
        let context = softbuffer::Context::new(event_loop.owned_display_handle())
            .map_err(|error| format!("Could not create the GDI drawing context: {error}"))?;
        let surface = softbuffer::Surface::new(&context, window.clone())
            .map_err(|error| format!("Could not create the GDI bitmap: {error}"))?;
        let input = egui_winit::State::new(
            self.context.clone(),
            egui::ViewportId::ROOT,
            window.as_ref(),
            Some(window.scale_factor() as f32),
            window.theme(),
            None,
        );
        let app = App::new(&self.context, Some(&self.storage), self.root.take());
        self.window = Some(SoftwareWindow {
            app,
            surface,
            _context: context,
            window,
            input,
            info: Default::default(),
            renderer: EguiSoftwareRender::new(ColorFieldOrder::Bgra),
            pending_textures: Default::default(),
            has_frame: false,
            shown: false,
        });
        self.schedule.request(Instant::now());
        Ok(())
    }

    fn draw(&mut self, event_loop: &ActiveEventLoop) -> Result<()> {
        let started = Instant::now();
        if let Some(trace) = &self.trace {
            trace.counters[0].fetch_add(1, Ordering::Relaxed);
        }
        self.schedule.last = Some(started);
        self.schedule.next = None;
        let Some(state) = &mut self.window else {
            return Ok(());
        };
        egui_winit::update_viewport_info(&mut state.info, &self.context, &state.window, false);
        let close_requested = state.info.close_requested();
        let mut input = state.input.take_egui_input(&state.window);
        input
            .viewports
            .insert(egui::ViewportId::ROOT, state.info.clone());
        state.info.events.clear();
        let hidden = state.shown && !state.window.is_visible().unwrap_or(true);
        let minimized = state.window.is_minimized() == Some(true);
        if let Some(trace) = &self.trace {
            trace.window_state(&state.window);
        }
        let mut output = self.context.run(input, |ctx| {
            if hidden || minimized {
                state.app.tick(ctx);
            } else {
                state.app.draw(ctx);
            }
        });
        // Hidden/minimized frames can allocate font textures. Keep their uploads until paint.
        state.pending_textures.append(output.textures_delta);
        state
            .input
            .handle_platform_output(&state.window, output.platform_output);
        let ui_time = started.elapsed();
        let mut close = close_requested;
        let mut commands = Vec::new();
        let mut screenshots = Vec::new();
        if let Some(viewport) = output.viewport_output.get_mut(&egui::ViewportId::ROOT) {
            if let Some(when) = started.checked_add(viewport.repaint_delay) {
                self.schedule.request(when);
            }
            for command in viewport.commands.drain(..) {
                match command {
                    egui::ViewportCommand::Close => close = true,
                    egui::ViewportCommand::CancelClose => close = false,
                    egui::ViewportCommand::Screenshot(data) => screenshots.push(data),
                    command => commands.push(command),
                }
            }
        }
        let mut actions = Vec::new();
        egui_winit::process_viewport_commands(
            &self.context,
            &mut state.info,
            commands,
            &state.window,
            &mut actions,
        );
        for action in actions {
            let event = match action {
                egui_winit::ActionRequested::Cut => egui::Event::Cut,
                egui_winit::ActionRequested::Copy => egui::Event::Copy,
                egui_winit::ActionRequested::Paste => {
                    if let Some(text) = state.input.clipboard_text() {
                        egui::Event::Paste(text)
                    } else {
                        continue;
                    }
                }
                egui_winit::ActionRequested::Screenshot(data) => {
                    screenshots.push(data);
                    continue;
                }
            };
            state.input.egui_input_mut().events.push(event);
            self.schedule.request(Instant::now());
        }
        if close {
            event_loop.exit();
            return Ok(());
        }
        let size = state.window.inner_size();
        let hidden = state.shown && !state.window.is_visible().unwrap_or(true);
        let minimized = state.window.is_minimized() == Some(true);
        if let Some(trace) = &self.trace {
            trace.window_state(&state.window);
        }
        if hidden || minimized || size.width == 0 || size.height == 0 {
            self.schedule.next = None;
            return Ok(());
        }
        let raster_started = Instant::now();
        state
            .surface
            .resize(
                NonZeroU32::new(size.width).unwrap(),
                NonZeroU32::new(size.height).unwrap(),
            )
            .map_err(|error| format!("Could not resize the GDI bitmap: {error}"))?;
        let primitives = self
            .context
            .tessellate(output.shapes, output.pixels_per_point);
        let mut buffer = state
            .surface
            .buffer_mut()
            .map_err(|error| error.to_string())?;
        buffer.fill(0);
        state.renderer.render(
            &mut BufferMutRef::new(
                bytemuck::cast_slice_mut(&mut buffer),
                size.width as usize,
                size.height as usize,
            ),
            &primitives,
            &state.pending_textures,
            output.pixels_per_point,
        );
        state.pending_textures = Default::default();
        let raster_time = raster_started.elapsed();
        if !screenshots.is_empty() {
            let image = Arc::new(egui::ColorImage::new(
                [size.width as usize, size.height as usize],
                buffer
                    .iter()
                    .map(|pixel| {
                        egui::Color32::from_rgb(
                            (pixel >> 16) as u8,
                            (pixel >> 8) as u8,
                            *pixel as u8,
                        )
                    })
                    .collect(),
            ));
            for data in screenshots {
                state
                    .input
                    .egui_input_mut()
                    .events
                    .push(egui::Event::Screenshot {
                        viewport_id: egui::ViewportId::ROOT,
                        user_data: data,
                        image: image.clone(),
                    });
            }
            self.schedule.request(Instant::now());
        }
        #[cfg(feature = "screenshots")]
        if self.screenshot.is_some() {
            self.capture_frames = self.capture_frames.saturating_add(1);
            if self.capture_frames < 2 {
                // Native areas such as Updates need a settling frame to become visible.
                self.schedule.request(started + FRAME_INTERVAL);
            } else if let Some(path) = self.screenshot.take() {
                let bytes = buffer
                    .iter()
                    .flat_map(|pixel| [(pixel >> 16) as u8, (pixel >> 8) as u8, *pixel as u8, 255])
                    .collect::<Vec<_>>();
                image::save_buffer(
                    path,
                    &bytes,
                    size.width,
                    size.height,
                    image::ColorType::Rgba8,
                )?;
                event_loop.exit();
            }
        }
        let present_started = Instant::now();
        buffer
            .present()
            .map_err(|error| format!("Could not display the GDI bitmap: {error}"))?;
        state.has_frame = true;
        let show = !event_loop.exiting();
        #[cfg(feature = "screenshots")]
        let show = show && self.screenshot.is_none();
        if !state.shown && show {
            state.window.set_visible(true);
            state.shown = true;
        }
        if let Some(trace) = &mut self.trace {
            trace.window_state(&state.window);
            trace.record(
                ui_time,
                raster_time,
                present_started.elapsed(),
                started.elapsed(),
            );
        }
        Ok(())
    }

    fn fail(&mut self, event_loop: &ActiveEventLoop, error: Box<dyn std::error::Error>) {
        self.error = Some(error);
        event_loop.exit();
    }
}

impl ApplicationHandler<Repaint> for SoftwareHost {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_none()
            && let Err(error) = self.initialize(event_loop)
        {
            self.fail(event_loop, error);
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        let Some(state) = &mut self.window else {
            return;
        };
        if state.window.id() != id {
            return;
        }
        if matches!(
            event,
            WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. }
        ) {
            state.has_frame = false;
        }
        if matches!(event, WindowEvent::RedrawRequested) && state.has_frame {
            // OS expose events reuse the retained bitmap; they do not create a UI loop.
            if let Err(error) = state
                .surface
                .buffer_mut()
                .and_then(|buffer| buffer.present())
            {
                self.fail(event_loop, error.to_string().into());
            }
            if let Some(trace) = &self.trace {
                trace.counters[1].fetch_add(1, Ordering::Relaxed);
            }
            return;
        }
        if matches!(event, WindowEvent::CloseRequested) {
            state.info.events.push(egui::ViewportEvent::Close);
        }
        let response = state.input.on_window_event(&state.window, &event);
        if response.repaint
            || matches!(
                event,
                WindowEvent::RedrawRequested | WindowEvent::CloseRequested
            )
        {
            self.schedule.request(Instant::now());
        }
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, repaint: Repaint) {
        #[cfg(feature = "diagnostics")]
        if let Some(command) = repaint.command {
            if let Some(trace) = &mut self.trace {
                trace.flush();
            }
            self.context.send_viewport_cmd(command);
            self.schedule.request(Instant::now());
            return;
        }
        let pass = self.context.cumulative_pass_nr();
        if pass == repaint.pass || pass == repaint.pass.saturating_add(1) {
            self.schedule.request(repaint.when);
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self
            .schedule
            .next
            .is_some_and(|when| when <= Instant::now())
            && let Err(error) = self.draw(event_loop)
        {
            self.fail(event_loop, error);
        }
        // Always restore Wait when the last repaint is consumed. Never busy-poll.
        event_loop.set_control_flow(self.schedule.control_flow());
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        self.context.set_request_repaint_callback(|_| {});
        if let Some(state) = &self.window {
            state.app.save(&mut self.storage);
            self.context
                .memory(|memory| eframe::set_value(&mut self.storage, "egui", memory));
            eframe::set_value(
                &mut self.storage,
                "window",
                &egui_winit::WindowSettings::from_window(self.context.zoom_factor(), &state.window),
            );
        }
        self.storage.flush();
        // App::drop requests scan/watcher cancellation before native resources disappear.
        self.window = None;
    }
}

/// Same RON map and location as the previous eframe host, including unknown keys.
struct FileStore {
    path: Option<PathBuf>,
    values: HashMap<String, String>,
    dirty: bool,
}

impl FileStore {
    fn new() -> Self {
        let path = std::env::var_os("APPDATA")
            .map(|path| PathBuf::from(path).join("Space Sniffer/data/app.ron"));
        #[cfg(feature = "diagnostics")]
        let path = std::env::var_os("SPACE_SNIFFER_TEST_DATA")
            .map(|path| PathBuf::from(path).join("app.ron"))
            .or(path);
        let values = path
            .as_ref()
            .and_then(|path| fs::read_to_string(path).ok())
            .and_then(|text| ron::from_str(&text).ok())
            .unwrap_or_default();
        Self {
            path,
            values,
            dirty: false,
        }
    }
}

impl Storage for FileStore {
    fn get_string(&self, key: &str) -> Option<String> {
        self.values.get(key).cloned()
    }
    fn set_string(&mut self, key: &str, value: String) {
        if self.values.get(key) != Some(&value) {
            self.values.insert(key.to_owned(), value);
            self.dirty = true;
        }
    }
    fn flush(&mut self) {
        if !self.dirty {
            return;
        }
        let result = (|| -> Result<()> {
            let Some(path) = &self.path else {
                return Ok(());
            };
            let parent = path.parent().ok_or("Settings path has no parent")?;
            fs::create_dir_all(parent)?;
            let mut file = tempfile::NamedTempFile::new_in(parent)?;
            file.write_all(ron::to_string(&self.values)?.as_bytes())?;
            file.as_file().sync_all()?;
            file.persist(path)?;
            Ok(())
        })();
        if result.is_ok() {
            self.dirty = false;
        }
    }
}

/// Opt-in aggregated diagnostics; disk writes happen on a bounded background worker.
struct FrameTrace {
    sender: Option<mpsc::SyncSender<serde_json::Value>>,
    writer: Option<std::thread::JoinHandle<()>>,
    counters: Arc<[AtomicU64; 4]>,
    started: Instant,
    samples: Vec<[f64; 4]>,
}

impl FrameTrace {
    fn new() -> Option<Self> {
        let path = std::env::var_os("SPACE_SNIFFER_RENDER_TRACE")?;
        let file = fs::File::create(path).ok()?;
        let (sender, receiver) = mpsc::sync_channel::<serde_json::Value>(8);
        let counters = Arc::new(std::array::from_fn(|_| AtomicU64::new(0)));
        let observed = counters.clone();
        let writer = std::thread::Builder::new()
            .name("render-trace".into())
            .spawn(move || {
                let mut file = std::io::BufWriter::new(file);
                let _ = writeln!(file, "{{\"renderer\":\"CPU / Windows GDI\"}}");
                let clock = Instant::now();
                loop {
                    match receiver.recv_timeout(Duration::from_secs(1)) {
                        Ok(sample) => {
                            let _ = writeln!(file, "{sample}");
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => {
                            let _ = writeln!(
                                file,
                                "{}",
                                serde_json::json!({
                                    "kind": "activity", "seconds": clock.elapsed().as_secs_f64(),
                                    "ui_frames": observed[0].load(Ordering::Relaxed),
                                    "presents": observed[1].load(Ordering::Relaxed),
                                "visible": observed[2].load(Ordering::Relaxed) != 0,
                                "minimized": observed[3].load(Ordering::Relaxed) != 0,
                                })
                            );
                        }
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                    let _ = file.flush();
                }
                let _ = file.flush();
            })
            .ok()?;
        Some(Self {
            sender: Some(sender),
            writer: Some(writer),
            counters,
            started: Instant::now(),
            samples: Vec::new(),
        })
    }

    fn record(&mut self, ui: Duration, raster: Duration, present: Duration, total: Duration) {
        self.counters[1].fetch_add(1, Ordering::Relaxed);
        self.samples
            .push([ui, raster, present, total].map(|d| d.as_secs_f64() * 1000.0));
        if self.started.elapsed() >= Duration::from_secs(1) {
            self.flush();
        }
    }

    fn window_state(&self, window: &Window) {
        self.counters[2].store(
            u64::from(window.is_visible().unwrap_or(true)),
            Ordering::Relaxed,
        );
        self.counters[3].store(
            u64::from(window.is_minimized() == Some(true)),
            Ordering::Relaxed,
        );
    }

    fn flush(&mut self) {
        if self.samples.is_empty() {
            return;
        }
        let count = self.samples.len();
        let mut columns = [Vec::new(), Vec::new(), Vec::new(), Vec::new()];
        for row in &self.samples {
            for (index, &value) in row.iter().enumerate() {
                columns[index].push(value);
            }
        }
        for column in &mut columns {
            column.sort_unstable_by(f64::total_cmp);
        }
        if let Some(sender) = &self.sender {
            let _ = sender.try_send(serde_json::json!({
            "seconds": self.started.elapsed().as_secs_f64(), "frames": count,
            "ui_median_ms": columns[0][count / 2], "raster_median_ms": columns[1][count / 2],
            "present_median_ms": columns[2][count / 2], "frame_median_ms": columns[3][count / 2],
            "frame_p95_ms": columns[3][(count - 1) * 95 / 100], "frame_max_ms": columns[3][count - 1],
        }));
        }
        self.samples.clear();
        self.started = Instant::now();
    }
}

impl Drop for FrameTrace {
    fn drop(&mut self) {
        self.flush();
        self.sender.take();
        if let Some(writer) = self.writer.take() {
            let _ = writer.join();
        }
    }
}

pub fn run(root: Option<PathBuf>) -> Result<()> {
    let event_loop = EventLoop::<Repaint>::with_user_event().build()?;
    let context = egui::Context::default();
    let proxy = event_loop.create_proxy();
    #[cfg(feature = "diagnostics")]
    start_smoke_commands(proxy.clone());
    context.set_request_repaint_callback(move |info| {
        if let Some(when) = Instant::now().checked_add(info.delay) {
            let _ = proxy.send_event(Repaint {
                when,
                pass: info.current_cumulative_pass_nr,
                #[cfg(feature = "diagnostics")]
                command: None,
            });
        }
    });
    let mut host = SoftwareHost {
        context,
        root,
        window: None,
        schedule: Default::default(),
        storage: FileStore::new(),
        error: None,
        trace: FrameTrace::new(),
        #[cfg(feature = "screenshots")]
        screenshot: std::env::var_os("EFRAME_SCREENSHOT_TO").map(PathBuf::from),
        #[cfg(feature = "screenshots")]
        capture_frames: 0,
    };
    event_loop.run_app(&mut host)?;
    if let Some(error) = host.error {
        return Err(error);
    }
    Ok(())
}

/// Opt-in bounded native-window checks. Omitted from the normal release build.
#[cfg(feature = "diagnostics")]
fn start_smoke_commands(proxy: winit::event_loop::EventLoopProxy<Repaint>) {
    let Some(seconds) = std::env::var("SPACE_SNIFFER_TEST_SECONDS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| (1..=120).contains(value))
    else {
        return;
    };
    let minimize = std::env::var_os("SPACE_SNIFFER_TEST_MINIMIZE").is_some();
    let _ = std::thread::Builder::new()
        .name("render-smoke".into())
        .spawn(move || {
            let commands = if minimize {
                vec![
                    (seconds / 3, egui::ViewportCommand::Minimized(true)),
                    (seconds * 2 / 3, egui::ViewportCommand::Minimized(false)),
                    (seconds, egui::ViewportCommand::Close),
                ]
            } else {
                vec![(seconds, egui::ViewportCommand::Close)]
            };
            let start = Instant::now();
            for (second, command) in commands {
                std::thread::sleep(Duration::from_secs(second).saturating_sub(start.elapsed()));
                if proxy
                    .send_event(Repaint {
                        when: Instant::now(),
                        pass: 0,
                        command: Some(command),
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repaint_bursts_coalesce_and_the_loop_sleeps_after_the_last_frame() {
        let now = Instant::now();
        let mut schedule = FrameSchedule::default();
        assert_eq!(schedule.control_flow(), ControlFlow::Wait);
        schedule.request(now + Duration::from_secs(10));
        schedule.request(now);
        assert_eq!(schedule.next, Some(now));
        schedule.last = Some(now);
        schedule.next = None;
        for _ in 0..1000 {
            schedule.request(now);
        }
        assert_eq!(schedule.next, Some(now + FRAME_INTERVAL));
        assert!(matches!(schedule.control_flow(), ControlFlow::WaitUntil(_)));
        schedule.next = None;
        assert_eq!(schedule.control_flow(), ControlFlow::Wait);
    }

    #[test]
    fn settings_flush_preserves_existing_eframe_keys_and_update_preference() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("app.ron");
        let values = HashMap::from([("unknown-existing-key".to_owned(), "preserved".to_owned())]);
        fs::write(&path, ron::to_string(&values).unwrap()).unwrap();
        let mut store = FileStore {
            path: Some(path.clone()),
            values,
            dirty: false,
        };
        eframe::set_value(&mut store, "check-for-updates-on-startup", &false);
        store.flush();
        let loaded: HashMap<String, String> =
            ron::from_str(&fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(loaded["unknown-existing-key"], "preserved");
        assert_eq!(loaded["check-for-updates-on-startup"], "false");
        assert!(!store.dirty);
    }
}
