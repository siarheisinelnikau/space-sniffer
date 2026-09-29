use eframe::egui::{
    self, Align2, Color32, FontId, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, Vec2,
};
use space_sniffer::{
    export, format_bytes,
    live::{self, Change, LiveEvent, LiveHandle},
    live_tree::LiveTree,
    model::{EntryKind, Node, ScanStats, ScanTree},
    scanner::ScanProgress,
    treemap,
};
use std::{
    path::{Path, PathBuf},
    sync::atomic::Ordering,
    time::{Duration, Instant},
};

const BG: Color32 = Color32::from_rgb(17, 22, 32);
const PANEL: Color32 = Color32::from_rgb(23, 30, 42);
const MUTED: Color32 = Color32::from_rgb(137, 153, 176);
const ACCENT: Color32 = Color32::from_rgb(100, 207, 187);

pub struct App {
    ctx: egui::Context,
    model: LiveTree,
    live: Option<LiveHandle>,
    loaded: bool,
    loading: bool,
    watching: bool,
    watch_status: String,
    progress: Option<ScanProgress>,
    started: Instant,
    path: String,
    query: String,
    applied: String,
    query_changed: Option<Instant>,
    filter_error: Option<String>,
    error: Option<String>,
    notice: String,
    selected: usize,
    zoom: usize,
    back: Vec<PathBuf>,
    forward: Vec<PathBuf>,
    depth: usize,
    classic: bool,
    help: bool,
    revision: u64,
    map_revision: u64,
    updates: u64,
    drives: Vec<PathBuf>,
    map_cache: Option<MapCache>,
    list_cache: Option<(usize, u64, Vec<usize>, usize)>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, path: Option<PathBuf>) -> Self {
        let mut visuals = egui::Visuals::dark();
        visuals.panel_fill = PANEL;
        visuals.window_fill = PANEL;
        visuals.extreme_bg_color = BG;
        visuals.selection.bg_fill = Color32::from_rgb(41, 90, 90);
        visuals.widgets.active.bg_fill = Color32::from_rgb(47, 97, 97);
        cc.egui_ctx.set_visuals(visuals);
        cc.egui_ctx.style_mut(|style| {
            style.spacing.item_spacing = Vec2::new(6.0, 5.0);
            style.spacing.button_padding = Vec2::new(8.0, 5.0);
            style.spacing.slider_width = 95.0;
            style
                .text_styles
                .insert(egui::TextStyle::Body, FontId::proportional(13.0));
            style
                .text_styles
                .insert(egui::TextStyle::Button, FontId::proportional(13.0));
        });
        let mut app = Self::empty(cc.egui_ctx.clone());
        if let Some(path) = path {
            app.begin_scan(path);
        }
        // Development-only capture waits for real scan data before eframe's
        // second-frame screenshot. Normal builds never block startup here.
        #[cfg(feature = "screenshots")]
        if std::env::var_os("EFRAME_SCREENSHOT_TO").is_some() && app.loading {
            let deadline = Instant::now() + Duration::from_secs(30);
            while app.loading && Instant::now() < deadline {
                app.poll_live();
                if app.loading {
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
        }
        app
    }

    fn empty(ctx: egui::Context) -> Self {
        let tree = ScanTree {
            nodes: vec![Node {
                name: String::new(),
                path: PathBuf::new(),
                kind: EntryKind::Directory,
                size: 0,
                children: Vec::new(),
                parent: None,
                modified: None,
            }],
            root: 0,
            stats: ScanStats::default(),
        };
        Self {
            ctx,
            model: LiveTree::new(tree),
            live: None,
            loaded: false,
            loading: false,
            watching: false,
            watch_status: String::new(),
            progress: None,
            started: Instant::now(),
            path: String::new(),
            query: String::new(),
            applied: String::new(),
            query_changed: None,
            filter_error: None,
            error: None,
            notice: String::new(),
            selected: 0,
            zoom: 0,
            back: Vec::new(),
            forward: Vec::new(),
            depth: 4,
            classic: false,
            help: false,
            revision: 0,
            map_revision: 0,
            updates: 0,
            drives: crate::drives::roots(),
            map_cache: None,
            list_cache: None,
        }
    }

    fn begin_scan(&mut self, path: PathBuf) {
        self.stop();
        let same_root = self.loaded
            && std::path::absolute(&path).ok().as_deref()
                == Some(self.model.tree.nodes[self.model.tree.root].path.as_path());
        if !same_root {
            self.loaded = false;
            self.selected = self.model.tree.root;
            self.zoom = self.model.tree.root;
            self.back.clear();
            self.forward.clear();
        }
        self.path = path.to_string_lossy().into_owned();
        let ctx = self.ctx.clone();
        self.live = Some(live::start_live_with_wake(path, move || {
            ctx.request_repaint()
        }));
        self.loading = true;
        self.watching = false;
        self.watch_status = "Starting…".into();
        self.progress = None;
        self.error = None;
        self.notice.clear();
        self.updates = 0;
        self.started = Instant::now();
        self.ctx.request_repaint();
    }

    fn stop(&mut self) {
        if let Some(live) = self.live.take() {
            live.cancel.store(true, Ordering::Relaxed);
        }
        self.loading = false;
        self.watching = false;
    }

    fn choose_folder(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .set_title("Choose any drive or folder to watch")
            .pick_folder()
        {
            self.begin_scan(path);
        }
    }

    fn poll_live(&mut self) {
        let budget = Instant::now();
        while let Some(live) = &self.live {
            let result = live.receiver.try_recv();
            let event = match result {
                Ok(event) => event,
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.stop();
                    self.watch_status = "Monitoring stopped".into();
                    break;
                }
            };
            match event {
                LiveEvent::Progress(progress) => self.progress = Some(progress),
                LiveEvent::Ready(model) => self.install_model(*model),
                LiveEvent::Changes(changes) => self.apply_changes(&changes),
                LiveEvent::Status { watching, message } => {
                    self.watching = watching;
                    self.watch_status = message;
                }
                LiveEvent::Failed(error) => {
                    self.loading = false;
                    self.error = Some(error);
                }
            }
            if budget.elapsed() > Duration::from_millis(8) {
                self.ctx.request_repaint();
                break;
            }
        }
    }

    fn install_model(&mut self, model: LiveTree) {
        let (selected, zoom) = if self.loaded {
            (
                self.model.tree.nodes[self.selected].path.clone(),
                self.model.tree.nodes[self.zoom].path.clone(),
            )
        } else {
            (
                model.tree.nodes[model.tree.root].path.clone(),
                model.tree.nodes[model.tree.root].path.clone(),
            )
        };
        self.model = model;
        // The worker already built unfiltered weights. Avoid traversing the
        // whole tree on the UI thread for the common empty-filter case.
        if !self.applied.is_empty() {
            let _ = self.model.set_filter(&self.applied);
        }
        self.loaded = true;
        self.loading = false;
        self.error = None;
        self.restore_paths(&selected, &zoom);
        if self.query != self.applied {
            self.apply_filter();
        } else {
            self.query_changed = None;
            self.filter_error = None;
        }
        self.invalidate();
    }

    fn apply_changes(&mut self, changes: &[Change]) {
        if !self.loaded {
            return;
        }
        let selected = self.model.tree.nodes[self.selected].path.clone();
        let zoom = self.model.tree.nodes[self.zoom].path.clone();
        let visual_change = changes
            .iter()
            .any(|change| self.change_affects_appearance(change));
        let affects_view = changes.iter().any(|change| {
            if !self.change_affects_appearance(change) {
                return false;
            }
            let path = match change {
                Change::Upsert { path, .. }
                | Change::ReplaceSubtree { path, .. }
                | Change::Remove(path) => path,
            };
            path.ancestors()
                .find_map(|path| self.model.find(path))
                .is_none_or(|id| self.is_ancestor(id, self.zoom) || self.is_ancestor(self.zoom, id))
        });
        let update = self.model.apply(changes);
        self.restore_paths(&selected, &zoom);
        if update.changed > 0 {
            self.updates += update.changed as u64;
            if visual_change {
                self.revision = self.revision.wrapping_add(1);
            }
            if affects_view || self.model.tree.nodes[self.zoom].path != zoom {
                self.map_revision = self.map_revision.wrapping_add(1);
            }
        }
    }

    fn change_affects_appearance(&self, change: &Change) -> bool {
        if let Change::Upsert {
            path, kind, size, ..
        } = change
            && let Some(id) = self.model.find(path)
        {
            let node = &self.model.tree.nodes[id];
            let same_name = path
                .file_name()
                .is_none_or(|name| node.name == name.to_string_lossy());
            return node.kind != *kind
                || (node.kind == EntryKind::File && node.size != *size)
                || !same_name;
        }
        true
    }

    fn restore_paths(&mut self, selected: &Path, zoom: &Path) {
        self.zoom = zoom
            .ancestors()
            .find_map(|path| {
                self.model
                    .find(path)
                    .filter(|&id| self.model.tree.nodes[id].is_dir())
            })
            .unwrap_or(self.model.tree.root);
        self.selected = selected
            .ancestors()
            .find_map(|path| self.model.find(path))
            .unwrap_or(self.zoom);
    }

    fn is_ancestor(&self, ancestor: usize, mut id: usize) -> bool {
        loop {
            if ancestor == id {
                return true;
            }
            let Some(parent) = self.model.tree.nodes[id].parent else {
                return false;
            };
            id = parent;
        }
    }

    fn invalidate(&mut self) {
        self.revision = self.revision.wrapping_add(1);
        self.map_revision = self.map_revision.wrapping_add(1);
    }

    fn apply_filter(&mut self) {
        match self.model.set_filter(&self.query) {
            Ok(()) => {
                self.applied = self.query.clone();
                self.filter_error = None;
                self.invalidate();
            }
            Err(error) => self.filter_error = Some(error),
        }
        self.query_changed = None;
    }

    fn navigate(&mut self, id: usize) {
        if id != self.zoom && self.model.contains(id) && self.model.tree.nodes[id].is_dir() {
            self.back
                .push(self.model.tree.nodes[self.zoom].path.clone());
            self.forward.clear();
            self.zoom = id;
            self.selected = id;
        }
    }

    fn history(&mut self, forward: bool) {
        loop {
            let path = if forward {
                self.forward.pop()
            } else {
                self.back.pop()
            };
            let Some(path) = path else {
                break;
            };
            if let Some(id) = self
                .model
                .find(&path)
                .filter(|&id| self.model.tree.nodes[id].is_dir())
            {
                let current = self.model.tree.nodes[self.zoom].path.clone();
                if forward {
                    self.back.push(current);
                } else {
                    self.forward.push(current);
                }
                self.zoom = id;
                self.selected = id;
                break;
            }
        }
    }

    fn export(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .set_title("Export disk usage report")
            .set_file_name("space-sniffer.csv")
            .add_filter("CSV report", &["csv"])
            .save_file()
        {
            match export::write_csv(&path, &self.model.tree, self.zoom, &self.model.weights) {
                Ok(()) => self.notice = format!("Saved {}", path.display()),
                Err(error) => self.error = Some(format!("Could not save report: {error}")),
            }
        }
    }

    fn sidebar(&mut self, ctx: &egui::Context) {
        egui::SidePanel::left("sidebar")
            .default_width(224.0)
            .width_range(204.0..=320.0)
            .resizable(true)
            .frame(egui::Frame::new().fill(PANEL).inner_margin(12))
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("sidebar_scroll")
                    .show(ui, |ui| {
                        ui.label(RichText::new("SPACE / SNIFFER").size(17.0).strong());
                        ui.add_space(6.0);
                        if ui
                            .add_sized(
                                [ui.available_width(), 30.0],
                                egui::Button::new(RichText::new("Open folder…").strong().color(BG))
                                    .fill(ACCENT),
                            )
                            .clicked()
                        {
                            self.choose_folder();
                        }
                        ui.horizontal_wrapped(|ui| {
                            for drive in self.drives.clone() {
                                let label = drive.to_string_lossy();
                                let selected = self.loaded
                                    && self.model.tree.nodes[self.model.tree.root].path == drive;
                                if ui
                                    .selectable_label(selected, label)
                                    .on_hover_text(drive.to_string_lossy())
                                    .clicked()
                                {
                                    self.begin_scan(drive);
                                }
                            }
                        });
                        ui.label(RichText::new("ROOT PATH").small().color(MUTED));
                        let response = ui.add_sized(
                            [ui.available_width(), 27.0],
                            egui::TextEdit::singleline(&mut self.path)
                                .hint_text("Any drive, folder or network path"),
                        );
                        let enter =
                            response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                        ui.horizontal(|ui| {
                            if (ui
                                .add_enabled(
                                    !self.path.trim().is_empty(),
                                    egui::Button::new("Watch root"),
                                )
                                .clicked()
                                || enter)
                                && !self.path.trim().is_empty()
                            {
                                self.begin_scan(PathBuf::from(self.path.trim()));
                            }
                            if self.live.is_some() && ui.button("Stop").clicked() {
                                self.stop();
                                self.watch_status = "Stopped".into();
                            }
                        });
                        ui.separator();
                        ui.horizontal(|ui| {
                            if self.loading {
                                ui.spinner();
                                ui.label(RichText::new("Scanning…").color(ACCENT));
                            } else if self.live.is_some() {
                                let (rect, _) =
                                    ui.allocate_exact_size(Vec2::splat(8.0), Sense::hover());
                                ui.painter().circle_filled(rect.center(), 3.0, ACCENT);
                                ui.label(
                                    RichText::new(if self.watching { "LIVE" } else { "AUTO" })
                                        .strong()
                                        .color(ACCENT),
                                );
                            } else {
                                ui.label(
                                    RichText::new(if self.loaded {
                                        "Stopped"
                                    } else {
                                        "Choose a root"
                                    })
                                    .color(MUTED),
                                );
                            }
                        });
                        if !self.watch_status.is_empty() {
                            ui.add(
                                egui::Label::new(
                                    RichText::new(&self.watch_status).small().color(MUTED),
                                )
                                .wrap(),
                            );
                        }
                        if self.loading {
                            if let Some(progress) = &self.progress {
                                ui.label(format!(
                                    "{} files · {}",
                                    progress.files,
                                    format_bytes(progress.bytes)
                                ));
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(progress.current_path.to_string_lossy())
                                            .small()
                                            .color(MUTED),
                                    )
                                    .truncate(),
                                );
                            }
                        } else if self.loaded {
                            ui.label(
                                RichText::new(format_bytes(self.model.tree.total_size()))
                                    .size(25.0)
                                    .color(ACCENT),
                            );
                            ui.label(
                                RichText::new(format!(
                                    "{} files · {} folders",
                                    self.model.tree.stats.files, self.model.tree.stats.directories
                                ))
                                .small()
                                .color(MUTED),
                            );
                        }
                        ui.separator();
                        ui.label(RichText::new("FILTER").small().color(MUTED));
                        if ui
                            .add_sized(
                                [ui.available_width(), 27.0],
                                egui::TextEdit::singleline(&mut self.query)
                                    .hint_text("*.zip; >100mb; |backup*"),
                            )
                            .changed()
                        {
                            self.query_changed = Some(Instant::now());
                        }
                        if !self.query.is_empty() && ui.small_button("Clear filter").clicked() {
                            self.query.clear();
                            self.apply_filter();
                        }
                        if let Some(error) = &self.filter_error {
                            ui.label(RichText::new(error).small().color(Color32::LIGHT_RED));
                        }
                        ui.horizontal(|ui| {
                            ui.label("Depth");
                            ui.add(egui::Slider::new(&mut self.depth, 1..=6));
                        });
                        ui.checkbox(&mut self.classic, "Classic colors");
                        if self.loaded {
                            ui.separator();
                            self.selection(ui);
                            ui.add_space(5.0);
                            egui::CollapsingHeader::new("Largest entries")
                                .show(ui, |ui| self.largest(ui));
                        }
                        ui.separator();
                        ui.horizontal(|ui| {
                            if ui
                                .add_enabled(self.loaded, egui::Button::new("Export CSV"))
                                .clicked()
                            {
                                self.export();
                            }
                            if ui.button("Help").clicked() {
                                self.help = true;
                            }
                        });
                    });
            });
    }

    fn selection(&mut self, ui: &mut egui::Ui) {
        let node = &self.model.tree.nodes[self.selected];
        ui.label(RichText::new("SELECTED").small().color(MUTED));
        ui.add(egui::Label::new(RichText::new(&node.name).strong()).truncate())
            .on_hover_text(node.path.to_string_lossy());
        ui.label(RichText::new(format_bytes(node.size)).size(20.0));
        if !self.applied.is_empty() {
            ui.label(
                RichText::new(format!(
                    "Matching {}",
                    format_bytes(self.model.weights[self.selected])
                ))
                .small()
                .color(MUTED),
            );
        }
        let is_dir = node.is_dir();
        let path = node.path.to_string_lossy().into_owned();
        ui.horizontal_wrapped(|ui| {
            if ui.button("Copy path").clicked() {
                ui.ctx().copy_text(path.clone());
            }
            if ui.button("Reveal").clicked()
                && let Err(error) = reveal(&self.model.tree.nodes[self.selected])
            {
                self.error = Some(error);
            }
            if ui
                .add_enabled(
                    is_dir && self.selected != self.zoom,
                    egui::Button::new("Zoom"),
                )
                .clicked()
            {
                self.navigate(self.selected);
            }
        });
    }

    fn largest(&mut self, ui: &mut egui::Ui) {
        let parent = if self.model.tree.nodes[self.selected].is_dir() {
            self.selected
        } else {
            self.model.tree.nodes[self.selected]
                .parent
                .unwrap_or(self.zoom)
        };
        if self
            .list_cache
            .as_ref()
            .is_none_or(|cache| cache.0 != parent || cache.1 != self.revision)
        {
            let visible = self.model.tree.nodes[parent]
                .children
                .iter()
                .copied()
                .filter(|&id| self.model.weights[id] > 0 || self.model.tree.nodes[id].size == 0);
            let mut ids = Vec::new();
            let mut count = 0;
            for id in visible {
                count += 1;
                if ids.len() < 100 {
                    ids.push(id);
                }
            }
            self.list_cache = Some((parent, self.revision, ids, count));
        }
        let (_, _, ids, count) = self.list_cache.as_ref().unwrap().clone();
        egui::ScrollArea::vertical()
            .id_salt("largest")
            .max_height(210.0)
            .show(ui, |ui| {
                for id in ids {
                    let node = &self.model.tree.nodes[id];
                    let label = format!("{}  {}", if node.is_dir() { "+" } else { "·" }, node.name);
                    let response =
                        ui.add(egui::Button::selectable(id == self.selected, label).frame(false));
                    ui.label(
                        RichText::new(format_bytes(self.model.weights[id]))
                            .small()
                            .color(MUTED),
                    );
                    if response.clicked() {
                        self.selected = id;
                    }
                    if response.double_clicked() {
                        self.navigate(id);
                    }
                }
                if count == 0 {
                    ui.small("No entries");
                } else if count > 100 {
                    ui.small("Showing 100 largest entries");
                }
            });
    }

    fn status(&self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("status")
            .exact_height(25.0)
            .frame(
                egui::Frame::new()
                    .fill(PANEL)
                    .inner_margin(egui::Margin::symmetric(10, 4)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if self.loading {
                        ui.label(
                            RichText::new(format!(
                                "Scanning · {:.1}s",
                                self.started.elapsed().as_secs_f32()
                            ))
                            .small()
                            .color(MUTED),
                        );
                    } else if self.loaded {
                        ui.label(
                            RichText::new(format!(
                                "{} updates · {} skipped · {} links · initial scan {:.2}s",
                                self.updates,
                                self.model.tree.stats.skipped,
                                self.model.tree.stats.symlinks,
                                self.model.tree.stats.elapsed.as_secs_f64()
                            ))
                            .small()
                            .color(MUTED),
                        );
                    } else {
                        ui.label(
                            RichText::new("Select a drive or folder to start watching.")
                                .small()
                                .color(MUTED),
                        );
                    }
                    if !self.notice.is_empty() {
                        ui.add(
                            egui::Label::new(RichText::new(&self.notice).small().color(ACCENT))
                                .truncate(),
                        );
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(RichText::new("Logical bytes").small().color(MUTED));
                    });
                });
            });
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui
                .add_enabled(!self.back.is_empty(), egui::Button::new("<"))
                .clicked()
            {
                self.history(false);
            }
            if ui
                .add_enabled(!self.forward.is_empty(), egui::Button::new(">"))
                .clicked()
            {
                self.history(true);
            }
            if ui
                .add_enabled(
                    self.model.tree.nodes[self.zoom].parent.is_some(),
                    egui::Button::new("Up"),
                )
                .clicked()
                && let Some(parent) = self.model.tree.nodes[self.zoom].parent
            {
                self.navigate(parent);
            }
            let mut crumbs = vec![self.zoom];
            let mut cursor = self.zoom;
            while let Some(parent) = self.model.tree.nodes[cursor].parent {
                crumbs.push(parent);
                cursor = parent;
            }
            let width = (ui.available_width() - 120.0).max(100.0);
            ui.allocate_ui_with_layout(
                Vec2::new(width, 25.0),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    egui::ScrollArea::horizontal()
                        .id_salt("crumbs")
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                for id in crumbs.into_iter().rev() {
                                    if ui
                                        .selectable_label(
                                            id == self.zoom,
                                            &self.model.tree.nodes[id].name,
                                        )
                                        .clicked()
                                    {
                                        self.navigate(id);
                                    }
                                    ui.label(RichText::new("/").color(MUTED));
                                }
                            });
                        });
                },
            );
            ui.label(
                RichText::new(format_bytes(self.model.weights[self.zoom]))
                    .strong()
                    .color(ACCENT),
            );
        });
    }

    fn welcome(&mut self, ui: &mut egui::Ui) {
        ui.vertical_centered(|ui| {
            ui.add_space((ui.available_height() * 0.32).max(15.0));
            if self.loading {
                ui.spinner();
                ui.heading("Scanning your files");
                if let Some(progress) = &self.progress {
                    ui.label(format!(
                        "{} files · {} found",
                        progress.files,
                        format_bytes(progress.bytes)
                    ));
                }
            } else {
                ui.heading("Choose a disk or folder");
                ui.label(
                    RichText::new("The map will follow file changes automatically.").color(MUTED),
                );
                if ui.button("Open folder…").clicked() {
                    self.choose_folder();
                }
                ui.label(
                    RichText::new("Or select a drive in the sidebar.")
                        .small()
                        .color(MUTED),
                );
            }
        });
    }

    fn canvas(&mut self, ui: &mut egui::Ui) {
        let (area, response) = ui.allocate_exact_size(ui.available_size(), Sense::click());
        let painter = ui.painter_at(area);
        painter.rect_filled(area, 5.0, BG);
        if self.map_cache.as_ref().is_none_or(|cache| {
            cache.root != self.zoom
                || cache.depth != self.depth
                || cache.area != area
                || cache.revision != self.map_revision
        }) {
            let mut tiles = Vec::new();
            if self.model.weights[self.zoom] > 0 {
                collect_tiles(
                    &self.model.tree,
                    &self.model.weights,
                    self.zoom,
                    area.shrink(2.0),
                    self.depth,
                    0,
                    &mut tiles,
                );
            }
            self.map_cache = Some(MapCache {
                root: self.zoom,
                depth: self.depth,
                area,
                revision: self.map_revision,
                tiles,
            });
        }
        let tiles = &self.map_cache.as_ref().unwrap().tiles;
        let hovered = response
            .hover_pos()
            .and_then(|pos| tiles.iter().rposition(|tile| tile.rect.contains(pos)));
        for (index, tile) in tiles.iter().enumerate() {
            let node = &self.model.tree.nodes[tile.id];
            let rect = tile.rect.shrink(1.0);
            let mut color = color(node, self.classic, tile.level);
            if hovered == Some(index) {
                color = color.gamma_multiply(1.17);
            }
            painter.rect_filled(rect, 2.0, color);
            painter.rect_stroke(
                rect,
                2.0,
                Stroke::new(1.0_f32, color.gamma_multiply(1.3)),
                StrokeKind::Inside,
            );
            let label = if tile.collapsed > 0 {
                format!("{} smaller entries", tile.collapsed)
            } else {
                node.name.clone()
            };
            let text_rect = if tile.container {
                Rect::from_min_size(
                    rect.min + Vec2::new(7.0, 2.0),
                    Vec2::new((rect.width() - 14.0).max(0.0), 19.0),
                )
            } else {
                rect.shrink(7.0)
            };
            if text_rect.width() > 40.0 && text_rect.height() > 12.0 {
                let clipped = painter.with_clip_rect(text_rect);
                if tile.container {
                    clipped.text(
                        text_rect.left_top(),
                        Align2::LEFT_TOP,
                        format!("{}  ·  {}", label, format_bytes(tile.bytes)),
                        FontId::proportional(12.0),
                        Color32::from_rgb(227, 235, 246),
                    );
                } else if text_rect.height() > 36.0 {
                    clipped.text(
                        text_rect.center() - Vec2::new(0.0, 8.0),
                        Align2::CENTER_CENTER,
                        &label,
                        FontId::proportional(if rect.width() > 180.0 { 15.0 } else { 12.0 }),
                        Color32::WHITE,
                    );
                    clipped.text(
                        text_rect.center() + Vec2::new(0.0, 12.0),
                        Align2::CENTER_CENTER,
                        format_bytes(tile.bytes),
                        FontId::proportional(11.0),
                        Color32::from_white_alpha(190),
                    );
                } else {
                    clipped.text(
                        text_rect.left_top(),
                        Align2::LEFT_TOP,
                        &label,
                        FontId::proportional(11.0),
                        Color32::WHITE,
                    );
                }
            }
        }
        if let Some(tile) = tiles.iter().find(|tile| tile.id == self.selected) {
            painter.rect_stroke(
                tile.rect.shrink(1.0),
                2.0,
                Stroke::new(2.0_f32, Color32::WHITE),
                StrokeKind::Inside,
            );
        }
        if tiles.is_empty() {
            painter.text(
                area.center(),
                Align2::CENTER_CENTER,
                if self.applied.is_empty() {
                    "No file bytes here yet.\nNew files will appear automatically."
                } else {
                    "No files match this filter."
                },
                FontId::proportional(18.0),
                MUTED,
            );
        }
        if let Some(index) = hovered {
            let tile = &tiles[index];
            let id = tile.id;
            let node = &self.model.tree.nodes[id];
            response.clone().on_hover_ui(|ui| {
                if tile.collapsed > 0 {
                    ui.strong(format!("{} smaller entries", tile.collapsed));
                    ui.small("Zoom into this folder or filter to inspect smaller files.");
                } else {
                    ui.strong(&node.name);
                }
                ui.label(format_bytes(tile.bytes));
                ui.label(node.path.to_string_lossy());
                if node.is_dir() {
                    ui.small("Double-click to zoom in");
                }
            });
            if response.clicked() || response.secondary_clicked() {
                self.selected = id;
            }
            if response.double_clicked() {
                self.navigate(id);
            }
        }
        response.context_menu(|ui| {
            if ui.button("Copy selected path").clicked() {
                ui.ctx().copy_text(
                    self.model.tree.nodes[self.selected]
                        .path
                        .to_string_lossy()
                        .into_owned(),
                );
                ui.close();
            }
            if ui
                .add_enabled(
                    self.model.tree.nodes[self.selected].is_dir(),
                    egui::Button::new("Zoom into selection"),
                )
                .clicked()
            {
                self.navigate(self.selected);
                ui.close();
            }
            if ui.button("Back to root").clicked() {
                self.navigate(self.model.tree.root);
                ui.close();
            }
        });
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_live();
        if let Some(changed) = self.query_changed {
            let elapsed = changed.elapsed();
            if elapsed >= Duration::from_millis(150) {
                self.apply_filter();
            } else {
                ctx.request_repaint_after(Duration::from_millis(150) - elapsed);
            }
        }
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, egui::Key::O)) {
            self.choose_folder();
        }
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::F5))
            && !self.path.trim().is_empty()
        {
            self.begin_scan(PathBuf::from(self.path.trim()));
        }
        if self.loaded
            && !ctx.wants_keyboard_input()
            && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Backspace))
            && let Some(parent) = self.model.tree.nodes[self.zoom].parent
        {
            self.navigate(parent);
        }
        self.sidebar(ctx);
        self.status(ctx);
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(BG).inner_margin(6))
            .show(ctx, |ui| {
                if let Some(error) = self.error.clone() {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new(error).small().color(Color32::LIGHT_RED));
                        if ui.small_button("Dismiss").clicked() {
                            self.error = None;
                        }
                    });
                }
                if self.loaded {
                    self.toolbar(ui);
                    self.canvas(ui);
                } else {
                    self.welcome(ui);
                }
            });
        egui::Window::new("Space Sniffer")
            .open(&mut self.help)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label("Choose any drive or folder. Changes update the map automatically.");
                ui.label("Click: inspect. Double-click: zoom. Backspace: parent.");
                ui.label("Ctrl+O: choose root. F5: optional full rescan.");
                ui.separator();
                ui.monospace("*.zip; >100mb; |backup*");
                ui.label("Combine filename and size filters with semicolons.");
                ui.label("MB uses decimal bytes; MiB uses binary bytes.");
                ui.separator();
                ui.label("Logical file sizes; hard links count per path.");
                ui.label("Links and reparse points are not traversed.");
                ui.label("Watcher errors trigger automatic recovery.");
                ui.label("MIT licensed · No telemetry · No deletion actions");
            });
    }
}

impl Drop for App {
    fn drop(&mut self) {
        self.stop();
    }
}

struct DrawTile {
    id: usize,
    rect: Rect,
    container: bool,
    level: usize,
    bytes: u64,
    collapsed: usize,
}

struct MapCache {
    root: usize,
    depth: usize,
    area: Rect,
    revision: u64,
    tiles: Vec<DrawTile>,
}

fn collect_tiles(
    tree: &ScanTree,
    weights: &[u64],
    id: usize,
    rect: Rect,
    remaining: usize,
    level: usize,
    out: &mut Vec<DrawTile>,
) {
    if out.len() >= 8000 || rect.width() < 2.0 || rect.height() < 2.0 {
        return;
    }
    let node = &tree.nodes[id];
    let container = node.is_dir()
        && remaining > 0
        && rect.width() > 60.0
        && rect.height() > 44.0
        && !node.children.is_empty();
    out.push(DrawTile {
        id,
        rect,
        container,
        level,
        bytes: weights[id],
        collapsed: 0,
    });
    if !container {
        return;
    }
    let inner = Rect::from_min_max(rect.min + Vec2::new(3.0, 20.0), rect.max - Vec2::splat(4.0));
    let items = space_sniffer::map_items::visible_items(
        &node.children,
        weights,
        inner.area(),
        1024.min(8000 - out.len()),
    );
    for tile in treemap::layout(
        &items.items,
        treemap::Rect {
            x: inner.min.x,
            y: inner.min.y,
            width: inner.width(),
            height: inner.height(),
        },
    ) {
        if out.len() >= 8000 {
            break;
        }
        let bounds = Rect::from_min_size(
            Pos2::new(tile.rect.x, tile.rect.y),
            Vec2::new(tile.rect.width, tile.rect.height),
        );
        if tile.id == space_sniffer::map_items::COLLAPSED_ID {
            out.push(DrawTile {
                id,
                rect: bounds,
                container: false,
                level: level + 1,
                bytes: items.collapsed_bytes,
                collapsed: items.collapsed_count,
            });
            continue;
        }
        collect_tiles(
            tree,
            weights,
            tile.id,
            bounds,
            remaining - 1,
            level + 1,
            out,
        );
    }
}

fn color(node: &Node, classic: bool, level: usize) -> Color32 {
    if node.is_dir() {
        if classic {
            return Color32::from_rgb(145, 108, 70).gamma_multiply(1.0 + level as f32 * 0.06);
        }
        return Color32::from_rgb(42, 55, 75).gamma_multiply(1.0 + level as f32 * 0.10);
    }
    let extension = node
        .path
        .extension()
        .unwrap_or_default()
        .to_string_lossy()
        .to_ascii_lowercase();
    if classic {
        return if ["exe", "dll", "bin"].contains(&extension.as_str()) {
            Color32::from_rgb(81, 128, 169)
        } else {
            Color32::from_rgb(174, 133, 88)
        };
    }
    match extension.as_str() {
        "rs" | "js" | "ts" | "tsx" | "py" | "html" | "css" | "toml" | "json" | "c" | "cpp"
        | "h" | "md" => Color32::from_rgb(64, 111, 161),
        "mp4" | "mov" | "mkv" | "mp3" | "wav" | "flac" | "avi" => Color32::from_rgb(167, 120, 62),
        "jpg" | "jpeg" | "png" | "raw" | "webp" | "gif" | "svg" | "fig" => {
            Color32::from_rgb(51, 128, 122)
        }
        "zip" | "7z" | "gz" | "tar" | "rar" | "iso" => Color32::from_rgb(156, 84, 108),
        "exe" | "dll" | "msi" | "dmg" | "app" | "so" => Color32::from_rgb(113, 94, 157),
        _ => Color32::from_rgb(76, 99, 128),
    }
}

fn reveal(node: &Node) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    let result = std::process::Command::new("explorer.exe")
        .arg(if node.is_dir() {
            node.path.as_os_str().to_os_string()
        } else {
            let mut arg = std::ffi::OsString::from("/select,");
            arg.push(&node.path);
            arg
        })
        .spawn();
    #[cfg(target_os = "macos")]
    let result = std::process::Command::new("open")
        .arg("-R")
        .arg(&node.path)
        .spawn();
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let result = std::process::Command::new("xdg-open")
        .arg(if node.is_dir() {
            node.path.as_path()
        } else {
            node.path.parent().unwrap_or(&node.path)
        })
        .spawn();
    result
        .map(|_| ())
        .map_err(|error| format!("Could not reveal entry: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(extra: usize) -> ScanTree {
        let root = std::path::absolute("test-root").unwrap();
        let nodes = vec![
            Node {
                name: "test-root".into(),
                path: root.clone(),
                kind: EntryKind::Directory,
                size: 30,
                children: vec![1],
                parent: None,
                modified: None,
            },
            Node {
                name: "nested".into(),
                path: root.join("nested"),
                kind: EntryKind::Directory,
                size: 30,
                children: vec![2, 3],
                parent: Some(0),
                modified: None,
            },
            Node {
                name: "one.zip".into(),
                path: root.join("nested/one.zip"),
                kind: EntryKind::File,
                size: 10,
                children: vec![],
                parent: Some(1),
                modified: None,
            },
            Node {
                name: "two.bin".into(),
                path: root.join("nested/two.bin"),
                kind: EntryKind::File,
                size: 20,
                children: vec![],
                parent: Some(1),
                modified: None,
            },
        ];
        let mut tree = ScanTree {
            nodes,
            root: 0,
            stats: ScanStats {
                files: 2,
                directories: 2,
                ..Default::default()
            },
        };
        for i in 0..extra {
            let id = tree.nodes.len();
            tree.nodes.push(Node {
                name: format!("extra{i}.zip"),
                path: root.join(format!("extra{i}.zip")),
                kind: EntryKind::File,
                size: 1,
                children: vec![],
                parent: Some(0),
                modified: None,
            });
            tree.nodes[0].children.push(id);
            tree.nodes[0].size += 1;
            tree.stats.files += 1;
        }
        tree
    }

    #[test]
    fn startup_has_no_demo_and_does_not_scan_without_a_root() {
        let app = App::empty(egui::Context::default());
        assert!(!app.loaded);
        assert!(!app.loading);
        assert!(app.live.is_none());
        assert!(app.path.is_empty());
        assert_eq!(app.model.tree.total_size(), 0);
    }

    #[test]
    fn updates_preserve_zoom_selection_and_valid_filter() {
        let mut app = App::empty(egui::Context::default());
        app.install_model(LiveTree::new(tree(0)));
        app.navigate(1);
        app.selected = 2;
        app.query = "*.zip".into();
        app.apply_filter();
        let path = app.model.tree.nodes[2].path.clone();
        app.apply_changes(&[Change::Upsert {
            path: path.clone(),
            kind: EntryKind::File,
            size: 50,
            modified: None,
        }]);
        assert_eq!(app.model.tree.nodes[app.zoom].name, "nested");
        assert_eq!(app.model.tree.nodes[app.selected].path, path);
        assert_eq!(app.model.weights[app.zoom], 50);
        assert_eq!(app.model.tree.total_size(), 70);
        app.apply_changes(&[Change::Remove(path)]);
        assert_eq!(app.model.tree.nodes[app.selected].name, "nested");
        assert_eq!(app.model.weights[app.zoom], 0);
    }

    #[test]
    fn resync_with_invalid_query_retains_last_valid_filter_and_navigation() {
        let mut app = App::empty(egui::Context::default());
        app.install_model(LiveTree::new(tree(0)));
        app.navigate(1);
        app.selected = 2;
        app.query = "*.zip".into();
        app.apply_filter();
        app.query = ">oops".into();
        app.apply_filter();
        app.install_model(LiveTree::new(tree(100)));
        assert_eq!(app.model.weights.len(), app.model.tree.nodes.len());
        assert_eq!(app.model.weights[app.model.tree.root], 110);
        assert!(app.filter_error.is_some());
        assert_eq!(app.applied, "*.zip");
        assert_eq!(app.model.tree.nodes[app.zoom].name, "nested");
    }

    #[test]
    fn deleting_current_folder_returns_to_a_live_ancestor() {
        let mut app = App::empty(egui::Context::default());
        app.install_model(LiveTree::new(tree(0)));
        app.navigate(1);
        let path = app.model.tree.nodes[1].path.clone();
        app.apply_changes(&[Change::Remove(path)]);
        assert_eq!(app.zoom, app.model.tree.root);
        assert_eq!(app.selected, app.model.tree.root);
        assert_eq!(app.model.tree.total_size(), 0);
    }

    #[test]
    fn content_writes_without_size_changes_keep_geometry_and_list_caches() {
        let mut app = App::empty(egui::Context::default());
        app.install_model(LiveTree::new(tree(0)));
        let revision = app.revision;
        let geometry = app.map_revision;
        let path = app.model.tree.nodes[2].path.clone();
        app.apply_changes(&[Change::Upsert {
            path,
            kind: EntryKind::File,
            size: 10,
            modified: Some(std::time::SystemTime::now()),
        }]);
        assert_eq!(app.map_revision, geometry);
        assert_eq!(app.revision, revision);
        assert!(app.updates > 0);
    }

    #[test]
    fn changes_outside_the_zoomed_folder_leave_geometry_cached() {
        let mut app = App::empty(egui::Context::default());
        app.install_model(LiveTree::new(tree(1)));
        app.navigate(1);
        let geometry = app.map_revision;
        let path = app.model.tree.nodes[4].path.clone();
        app.apply_changes(&[Change::Upsert {
            path,
            kind: EntryKind::File,
            size: 20,
            modified: None,
        }]);
        assert_eq!(app.map_revision, geometry);
        assert_eq!(app.model.tree.nodes[app.zoom].size, 30);
        assert_eq!(app.model.tree.total_size(), 50);
    }
}
