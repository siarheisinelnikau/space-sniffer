use eframe::egui::{self, RichText};
use space_sniffer::{
    format_bytes,
    updater::{self, Asset, Download, Release},
};
use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver},
    },
    time::{Duration, Instant},
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Status {
    Idle,
    Checking,
    Current,
    Available,
    Downloading,
    Ready,
    Installing,
    Installed,
}

enum Event {
    Checked(Result<Option<Release>, String>),
    Downloaded(Result<Download, String>),
    Installed(Result<PathBuf, String>),
}

pub(super) struct Updates {
    pub open: bool,
    check_on_startup: bool,
    status: Status,
    release: Option<Release>,
    download: Option<Download>,
    error: Option<String>,
    receiver: Option<Receiver<Event>>,
    cancelled: Arc<AtomicBool>,
    transferred: Arc<AtomicU64>,
    executable: Option<PathBuf>,
}

impl Default for Updates {
    fn default() -> Self {
        Self {
            open: false,
            check_on_startup: true,
            status: Status::Idle,
            release: None,
            download: None,
            error: None,
            receiver: None,
            cancelled: Arc::new(AtomicBool::new(false)),
            transferred: Arc::new(AtomicU64::new(0)),
            executable: None,
        }
    }
}

impl Updates {
    const STARTUP_CHECK_KEY: &str = "check-for-updates-on-startup";

    pub fn load(storage: Option<&dyn eframe::Storage>) -> Self {
        let mut updates = Self::default();
        if let Some(enabled) =
            storage.and_then(|storage| eframe::get_value(storage, Self::STARTUP_CHECK_KEY))
        {
            updates.check_on_startup = enabled;
        }
        updates
    }

    pub fn save(&self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, Self::STARTUP_CHECK_KEY, &self.check_on_startup);
    }

    pub fn startup(&mut self, ctx: &egui::Context) {
        if self.check_on_startup {
            self.check(ctx);
        }
    }

    fn busy(&self) -> bool {
        matches!(
            self.status,
            Status::Checking | Status::Downloading | Status::Installing
        )
    }

    fn start(&mut self, ctx: &egui::Context, task: impl FnOnce() -> Event + Send + 'static) {
        let (sender, receiver) = mpsc::sync_channel(1);
        let ctx = ctx.clone();
        self.error = None;
        match std::thread::Builder::new()
            .name("github-update".into())
            .spawn(move || {
                let event = task();
                let _ = sender.send(event);
                ctx.request_repaint();
            }) {
            Ok(_) => self.receiver = Some(receiver),
            Err(error) => {
                self.error = Some(format!("Could not start update worker: {error}"));
                self.status = if self.release.is_some() {
                    Status::Available
                } else {
                    Status::Idle
                };
            }
        }
    }

    pub fn check(&mut self, ctx: &egui::Context) {
        if self.busy() || self.status == Status::Installed {
            return;
        }
        self.download = None;
        self.release = None;
        self.status = Status::Checking;
        self.start(ctx, || Event::Checked(updater::check()));
    }

    /// Returns true once an installed update is ready for the automatic restart.
    pub fn poll(&mut self) -> bool {
        let event = match self.receiver.as_ref().map(Receiver::try_recv) {
            Some(Ok(event)) => event,
            Some(Err(mpsc::TryRecvError::Disconnected)) => {
                self.receiver = None;
                self.status = Status::Idle;
                self.error =
                    Some("The update worker stopped unexpectedly. Please try again.".into());
                return false;
            }
            _ => return false,
        };
        self.receiver = None;
        match event {
            Event::Checked(Ok(release)) => {
                self.status = if release.is_some() {
                    Status::Available
                } else {
                    Status::Current
                };
                self.release = release;
            }
            Event::Downloaded(Ok(download)) => {
                self.download = Some(download);
                self.status = Status::Ready;
                self.open = true;
            }
            Event::Installed(Ok(executable)) => {
                self.executable = Some(executable);
                self.status = Status::Installed;
                return true;
            }
            Event::Checked(Err(error)) => {
                self.status = Status::Idle;
                self.error = Some(error);
            }
            Event::Downloaded(Err(error)) => {
                self.status = Status::Available;
                self.error = Some(error);
            }
            Event::Installed(Err(error)) => {
                self.status = Status::Available;
                self.error = Some(error);
            }
        }
        false
    }

    pub fn button(&mut self, ui: &mut egui::Ui) {
        let label = if self.release.is_some() {
            "Update available"
        } else {
            "Updates"
        };
        ui.horizontal(|ui| {
            ui.small(format!("v{}", updater::CURRENT_VERSION));
            if ui.small_button(label).clicked() {
                self.open = true;
            }
        });
    }

    pub fn restart(&mut self, ctx: &egui::Context, root: Option<&Path>) {
        let Some(executable) = &self.executable else {
            return;
        };
        let mut command = Command::new(executable);
        if let Some(root) = root {
            command.arg(root);
        }
        match command.spawn() {
            Ok(_) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            Err(error) => {
                self.open = true;
                self.error = Some(format!(
                    "The update is installed, but restart failed: {error}. Close and reopen the app, or try Restart now."
                ));
            }
        }
    }

    fn begin_download(&mut self, ctx: &egui::Context, asset: Asset) {
        self.cancelled = Arc::new(AtomicBool::new(false));
        self.transferred.store(0, Ordering::Relaxed);
        let cancelled = self.cancelled.clone();
        let transferred = self.transferred.clone();
        let wake = ctx.clone();
        self.status = Status::Downloading;
        self.start(ctx, move || {
            let mut painted = Instant::now();
            Event::Downloaded(updater::download(&asset, &cancelled, |bytes| {
                transferred.store(bytes, Ordering::Relaxed);
                if painted.elapsed() >= Duration::from_millis(100) {
                    wake.request_repaint();
                    painted = Instant::now();
                }
            }))
        });
    }

    /// Returns true when the user requests another restart attempt.
    pub fn window(&mut self, ctx: &egui::Context) -> bool {
        let mut open = self.open;
        let mut restart = false;
        egui::Window::new("App updates")
            .open(&mut open)
            .default_width(420.0)
            .resizable(true)
            .show(ctx, |ui| {
                ui.label(format!("Space Sniffer {}", updater::CURRENT_VERSION));
                ui.small("Check the latest stable release on GitHub. Download and installation require your action.");
                ui.checkbox(&mut self.check_on_startup, "Check for updates on startup");
                ui.separator();
                match self.status {
                    Status::Idle => {
                        ui.label("Check GitHub for a newer version.");
                    }
                    Status::Checking => {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label("Checking GitHub…");
                        });
                    }
                    Status::Current => {
                        ui.label("You are using the latest version.");
                    }
                    Status::Available => {
                        ui.label("A newer version is available.");
                    }
                    Status::Downloading => {
                        let received = self.transferred.load(Ordering::Relaxed);
                        let total = self.release.as_ref().and_then(|r| r.asset.as_ref()).map_or(1, |a| a.size);
                        ui.add(egui::ProgressBar::new(received as f32 / total as f32)
                            .text(format!("{} / {}", format_bytes(received), format_bytes(total))));
                        if ui.button("Cancel download").clicked() {
                            self.cancelled.store(true, Ordering::Relaxed);
                        }
                    }
                    Status::Ready => {
                        ui.label("Download verified. Ready to install.");
                    }
                    Status::Installing => {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label("Installing update…");
                        });
                    }
                    Status::Installed => {
                        ui.label("Update installed. Restart to use the new version.");
                        restart = ui.button("Restart now").clicked();
                    }
                }
                if let Some(release) = &self.release {
                    ui.label(RichText::new(format!("Version {}", release.version)).strong());
                    ui.hyperlink_to("View release on GitHub", &release.page_url);
                    if !updater::CAN_INSTALL {
                        ui.label("In-app installation is available for Windows x86-64. Build this platform from the release source.");
                    } else if !release.asset.as_ref().is_some_and(|a| a.is_verifiable()) {
                        ui.label("This release has no verified Windows executable. Use the release page to update manually.");
                    } else if self.status == Status::Available && ui.button("Download and install").clicked() {
                        let asset = release.asset.clone().unwrap();
                        self.begin_download(ctx, asset);
                    }
                    if self.status == Status::Ready {
                        ui.small("The app will replace its executable and restart. The current root will be scanned again.");
                        if ui.button("Install and restart").clicked()
                            && let Some(download) = self.download.take()
                        {
                            self.status = Status::Installing;
                            self.start(ctx, move || Event::Installed(download.install()));
                        }
                    }
                    if let Some(release) = &self.release {
                        egui::CollapsingHeader::new("Release notes").show(ui, |ui| {
                            egui::ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
                                ui.add(egui::Label::new(&release.notes).wrap());
                            });
                        });
                    }
                }
                if let Some(error) = &self.error {
                    ui.add(egui::Label::new(RichText::new(error).color(egui::Color32::LIGHT_RED)).wrap());
                }
                ui.separator();
                if ui.add_enabled(!self.busy() && self.status != Status::Installed, egui::Button::new("Check for updates")).clicked() {
                    self.check(ctx);
                }
                ui.hyperlink_to("GitHub Releases", updater::RELEASES_URL);
            });
        self.open = open;
        // Do not terminate the process in the middle of executable replacement.
        if self.status == Status::Installing
            && ctx.input(|input| input.viewport().close_requested())
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        }
        restart
    }
}

impl Drop for Updates {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct MemoryStorage(std::collections::HashMap<String, String>);

    impl eframe::Storage for MemoryStorage {
        fn get_string(&self, key: &str) -> Option<String> {
            self.0.get(key).cloned()
        }
        fn set_string(&mut self, key: &str, value: String) {
            self.0.insert(key.to_owned(), value);
        }
        fn flush(&mut self) {}
    }

    #[test]
    fn disabling_startup_checks_survives_restart_and_starts_no_worker() {
        let mut storage = MemoryStorage::default();
        let mut updates = Updates::load(None);
        assert!(updates.check_on_startup);
        updates.check_on_startup = false;
        updates.save(&mut storage);
        let mut restarted = Updates::load(Some(&storage));
        restarted.startup(&egui::Context::default());
        assert!(!restarted.check_on_startup);
        assert!(restarted.receiver.is_none());
        assert!(restarted.status == Status::Idle);
        // The preference gates startup only; the manual check remains enabled.
        assert!(!restarted.busy());
    }

    #[test]
    fn a_failed_download_cannot_offer_installation_and_an_installed_update_restarts_once() {
        let (sender, receiver) = mpsc::sync_channel(1);
        let mut updates = Updates::default();
        updates.status = Status::Downloading;
        updates.receiver = Some(receiver);
        sender
            .send(Event::Downloaded(Err("checksum mismatch".into())))
            .unwrap();
        assert!(!updates.poll());
        assert!(updates.status == Status::Available);
        assert!(updates.download.is_none());
        assert_eq!(updates.error.as_deref(), Some("checksum mismatch"));

        let (sender, receiver) = mpsc::sync_channel(1);
        updates.status = Status::Installing;
        updates.receiver = Some(receiver);
        sender
            .send(Event::Installed(Ok(PathBuf::from("application.exe"))))
            .unwrap();
        assert!(updates.poll());
        assert!(!updates.poll());
        assert!(updates.status == Status::Installed);
    }

    #[test]
    fn closing_the_app_is_deferred_only_during_executable_replacement() {
        for status in [
            Status::Installing,
            Status::Downloading,
            Status::Ready,
            Status::Installed,
        ] {
            let ctx = egui::Context::default();
            let mut updates = Updates::default();
            updates.status = status;
            let mut input = egui::RawInput::default();
            input
                .viewports
                .get_mut(&egui::ViewportId::ROOT)
                .unwrap()
                .events
                .push(egui::ViewportEvent::Close);
            let output = ctx.run(input, |ctx| {
                updates.window(ctx);
            });
            let commands = &output.viewport_output[&egui::ViewportId::ROOT].commands;
            assert_eq!(
                commands.contains(&egui::ViewportCommand::CancelClose),
                status == Status::Installing
            );
        }
    }
}
