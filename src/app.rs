use eframe::egui::{
    self, Align2, Color32, FontId, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, Vec2,
};
use space_sniffer::{
    export,
    filter::Filter,
    format_bytes,
    model::{Node, ScanTree},
    scanner::{self, ScanEvent, ScanHandle, ScanProgress},
    treemap,
};
use std::{
    path::PathBuf,
    sync::atomic::Ordering,
    time::{Duration, Instant},
};

const BG: Color32 = Color32::from_rgb(17, 22, 32);
const PANEL: Color32 = Color32::from_rgb(23, 30, 42);
const MUTED: Color32 = Color32::from_rgb(137, 153, 176);
const ACCENT: Color32 = Color32::from_rgb(100, 207, 187);

pub struct App {
    tree: ScanTree,
    weights: Vec<u64>,
    scan: Option<ScanHandle>,
    progress: Option<ScanProgress>,
    started: Instant,
    path: String,
    query: String,
    applied: String,
    filter_error: Option<String>,
    error: Option<String>,
    notice: String,
    selected: usize,
    zoom: usize,
    back: Vec<usize>,
    forward: Vec<usize>,
    depth: usize,
    classic: bool,
    demo: bool,
    help: bool,
    revision: u64,
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
            style.spacing.item_spacing = Vec2::new(10.0, 8.0);
            style.spacing.button_padding = Vec2::new(13.0, 8.0);
            style.spacing.slider_width = 70.0;
            style
                .text_styles
                .insert(egui::TextStyle::Body, FontId::proportional(14.0));
            style
                .text_styles
                .insert(egui::TextStyle::Button, FontId::proportional(14.0));
        });
        let mut app = Self::demo_state();
        if let Some(path) = path {
            app.begin_scan(path);
        }
        app
    }

    fn demo_state() -> Self {
        let tree = crate::demo::tree();
        let weights = tree.nodes.iter().map(|n| n.size).collect();
        Self {
            tree,
            weights,
            scan: None,
            progress: None,
            started: Instant::now(),
            path: String::new(),
            query: String::new(),
            applied: String::new(),
            filter_error: None,
            error: None,
            notice: String::new(),
            selected: 0,
            zoom: 0,
            back: Vec::new(),
            forward: Vec::new(),
            depth: 3,
            classic: false,
            demo: true,
            help: false,
            revision: 0,
            map_cache: None,
            list_cache: None,
        }
    }

    fn begin_scan(&mut self, path: PathBuf) {
        if let Some(scan) = self.scan.take() {
            scan.cancel.store(true, Ordering::Relaxed);
        }
        self.path = path.to_string_lossy().into_owned();
        self.scan = Some(scanner::start_scan(path));
        self.progress = None;
        self.error = None;
        self.notice.clear();
        self.started = Instant::now();
    }

    fn choose_folder(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .set_title("Choose a folder or drive to scan")
            .pick_folder()
        {
            self.begin_scan(path);
        }
    }

    fn poll_scan(&mut self, ctx: &egui::Context) {
        let mut finished = None;
        if let Some(scan) = &self.scan {
            loop {
                match scan.receiver.try_recv() {
                    Ok(ScanEvent::Progress(progress)) => self.progress = Some(progress),
                    Ok(ScanEvent::Finished(result)) => {
                        finished = Some(result);
                        break;
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        finished = Some(Err("Scan worker stopped unexpectedly".into()));
                        break;
                    }
                }
            }
            ctx.request_repaint_after(Duration::from_millis(80));
        }
        if let Some(result) = finished {
            self.scan = None;
            match result {
                Ok(tree) => {
                    self.selected = tree.root;
                    self.zoom = tree.root;
                    self.tree = tree;
                    // A newly scanned tree needs new weights even when the
                    // query currently being edited is invalid.
                    self.weights = Filter::parse(&self.applied)
                        .unwrap_or_default()
                        .weights(&self.tree);
                    self.revision = self.revision.wrapping_add(1);
                    self.demo = false;
                    self.back.clear();
                    self.forward.clear();
                    self.apply_filter();
                    self.notice = "Scan complete".into();
                }
                Err(error) if error == "Scan cancelled" => {
                    self.notice = "Scan cancelled. Previous view retained.".into()
                }
                Err(error) => self.error = Some(error),
            }
        }
    }

    fn apply_filter(&mut self) {
        match Filter::parse(&self.query) {
            Ok(filter) => {
                self.weights = filter.weights(&self.tree);
                self.applied = self.query.clone();
                self.filter_error = None;
                self.revision = self.revision.wrapping_add(1);
            }
            Err(error) => self.filter_error = Some(error),
        }
    }

    fn navigate(&mut self, id: usize) {
        if id != self.zoom && self.tree.nodes[id].is_dir() {
            self.back.push(self.zoom);
            self.forward.clear();
            self.zoom = id;
            self.selected = id;
        }
    }

    fn export(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .set_title("Export disk usage report")
            .set_file_name("space-sniffer.csv")
            .add_filter("CSV report", &["csv"])
            .save_file()
        {
            match export::write_csv(&path, &self.tree, self.zoom, &self.weights) {
                Ok(()) => self.notice = format!("Report saved to {}", path.display()),
                Err(error) => self.error = Some(format!("Could not save report: {error}")),
            }
        }
    }

    fn header(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("header")
            .frame(
                egui::Frame::new()
                    .fill(PANEL)
                    .inner_margin(egui::Margin::symmetric(22, 14)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(
                            RichText::new("SPACE / SNIFFER")
                                .size(23.0)
                                .strong()
                                .color(Color32::WHITE),
                        );
                        ui.label(
                            RichText::new("A little perspective on your disk.")
                                .color(MUTED)
                                .size(12.0),
                        );
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("?  Help").clicked() {
                            self.help = true;
                        }
                        if ui.button("Export CSV").clicked() {
                            self.export();
                        }
                        if ui
                            .add(
                                egui::Button::new(
                                    RichText::new("+  Open folder").color(BG).strong(),
                                )
                                .fill(ACCENT),
                            )
                            .clicked()
                        {
                            self.choose_folder();
                        }
                    });
                });
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    let input = ui.add_sized(
                        [ui.available_width() - 100.0, 30.0],
                        egui::TextEdit::singleline(&mut self.path)
                            .hint_text("Enter a folder or drive, e.g. C:\\")
                            .margin(Vec2::new(10.0, 7.0)),
                    );
                    let enter = input.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if ui
                        .add_enabled(!self.path.trim().is_empty(), egui::Button::new("Scan / F5"))
                        .clicked()
                        || enter
                    {
                        self.begin_scan(PathBuf::from(self.path.trim()));
                    }
                });
            });
    }

    fn status(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("status")
            .frame(
                egui::Frame::new()
                    .fill(PANEL)
                    .inner_margin(egui::Margin::symmetric(20, 9)),
            )
            .show(ctx, |ui| {
                if self.scan.is_some() {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        if let Some(p) = &self.progress {
                            ui.label(format!(
                                "Scanning  ·  {} files  ·  {} found  ·  {:.1}s",
                                p.files,
                                format_bytes(p.bytes),
                                self.started.elapsed().as_secs_f32()
                            ));
                        } else {
                            ui.label("Starting scan…");
                        }
                        if ui.button("Cancel").clicked()
                            && let Some(scan) = &self.scan
                        {
                            scan.cancel.store(true, Ordering::Relaxed);
                        }
                    });
                    if let Some(p) = &self.progress {
                        ui.add(
                            egui::Label::new(
                                RichText::new(p.current_path.to_string_lossy())
                                    .small()
                                    .color(MUTED),
                            )
                            .truncate(),
                        );
                    }
                } else {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(if self.demo {
                                "●  DEMO DATA"
                            } else {
                                "●  READY"
                            })
                            .color(ACCENT)
                            .size(11.0)
                            .strong(),
                        );
                        ui.label(
                            RichText::new(format!(
                                "{} files  /  {} folders  /  {} skipped  /  {} links  /  {:.2}s",
                                self.tree.stats.files,
                                self.tree.stats.directories,
                                self.tree.stats.skipped,
                                self.tree.stats.symlinks,
                                self.tree.stats.elapsed.as_secs_f64()
                            ))
                            .small()
                            .color(MUTED),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(
                                RichText::new("Logical size · links not followed")
                                    .small()
                                    .color(MUTED),
                            );
                        });
                    });
                }
            });
    }

    fn inspector(&mut self, ctx: &egui::Context) {
        egui::SidePanel::right("inspector")
            .default_width(264.0)
            .min_width(220.0)
            .max_width(400.0)
            .resizable(true)
            .frame(egui::Frame::new().fill(PANEL).inner_margin(18))
            .show(ctx, |ui| {
                ui.label(RichText::new("SELECTION").small().strong().color(MUTED));
                ui.add_space(10.0);
                let node = &self.tree.nodes[self.selected];
                ui.label(RichText::new(&node.name).size(21.0).strong());
                ui.label(
                    RichText::new(format_bytes(node.size))
                        .size(30.0)
                        .color(ACCENT),
                );
                let root_size = self.tree.nodes[self.zoom].size;
                let fraction = if root_size > 0 {
                    node.size as f64 / root_size as f64
                } else {
                    0.0
                };
                ui.add(
                    egui::ProgressBar::new(fraction.min(1.0) as f32)
                        .fill(ACCENT)
                        .desired_height(5.0),
                );
                ui.label(
                    RichText::new(format!("{:.1}% of this view", fraction * 100.0))
                        .small()
                        .color(MUTED),
                );
                ui.add_space(12.0);
                ui.label(
                    RichText::new(node.path.to_string_lossy())
                        .small()
                        .color(MUTED),
                );
                ui.add_space(8.0);
                if !self.applied.is_empty() {
                    ui.label(format!(
                        "Matching: {}",
                        format_bytes(self.weights[self.selected])
                    ));
                }
                let is_dir = node.is_dir();
                let copy_path = node.path.to_string_lossy().into_owned();
                ui.horizontal_wrapped(|ui| {
                    if ui
                        .add_enabled(
                            is_dir && self.selected != self.zoom,
                            egui::Button::new("Zoom in"),
                        )
                        .clicked()
                    {
                        self.navigate(self.selected);
                    }
                    if ui.button("Copy path").clicked() {
                        ctx.copy_text(copy_path.clone());
                    }
                    if ui
                        .add_enabled(!self.demo, egui::Button::new("Reveal"))
                        .clicked()
                        && let Err(error) = reveal(&self.tree.nodes[self.selected])
                    {
                        self.error = Some(error);
                    }
                });
                ui.add_space(16.0);
                ui.separator();
                ui.add_space(8.0);
                ui.label(
                    RichText::new("LARGEST IN THIS FOLDER")
                        .small()
                        .strong()
                        .color(MUTED),
                );
                ui.add_space(8.0);
                let parent = if self.tree.nodes[self.selected].is_dir() {
                    self.selected
                } else {
                    self.tree.nodes[self.selected].parent.unwrap_or(self.zoom)
                };
                if self
                    .list_cache
                    .as_ref()
                    .is_none_or(|cache| cache.0 != parent || cache.1 != self.revision)
                {
                    let visible = self.tree.nodes[parent]
                        .children
                        .iter()
                        .copied()
                        .filter(|&id| self.weights[id] > 0 || self.tree.nodes[id].size == 0);
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
                let (_, _, ids, child_count) = self.list_cache.as_ref().unwrap().clone();
                egui::ScrollArea::vertical()
                    .id_salt("largest")
                    .show(ui, |ui| {
                        for id in ids.iter().copied().take(100) {
                            let node = &self.tree.nodes[id];
                            if self.weights[id] == 0 && node.size > 0 {
                                continue;
                            }
                            let response = ui.selectable_label(
                                self.selected == id,
                                format!("{}  {}", if node.is_dir() { "+" } else { "·" }, node.name),
                            );
                            ui.label(
                                RichText::new(format_bytes(self.weights[id]))
                                    .small()
                                    .color(MUTED),
                            );
                            if response.clicked() {
                                self.selected = id;
                            }
                            if response.double_clicked() {
                                self.navigate(id);
                            }
                            ui.add_space(5.0);
                        }
                        if ids.is_empty() {
                            ui.label(RichText::new("No child entries").color(MUTED));
                        }
                        if child_count > 100 {
                            ui.label(
                                RichText::new("Showing the 100 largest entries")
                                    .small()
                                    .color(MUTED),
                            );
                        }
                    });
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
                || cache.revision != self.revision
        }) {
            let mut tiles = Vec::new();
            if self.weights[self.zoom] > 0 {
                collect_tiles(
                    &self.tree,
                    &self.weights,
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
                revision: self.revision,
                tiles,
            });
        }
        let tiles = &self.map_cache.as_ref().unwrap().tiles;
        let hovered = response.hover_pos().and_then(|pos| {
            tiles
                .iter()
                .rev()
                .find(|tile| tile.rect.contains(pos))
                .map(|tile| tile.id)
        });
        for tile in tiles {
            let node = &self.tree.nodes[tile.id];
            let rect = tile.rect.shrink(1.0);
            let mut color = color(node, self.classic, tile.level);
            if hovered == Some(tile.id) {
                color = color.gamma_multiply(1.17);
            }
            painter.rect_filled(rect, 2.0, color);
            painter.rect_stroke(
                rect,
                2.0,
                Stroke::new(1.0, color.gamma_multiply(1.3)),
                StrokeKind::Inside,
            );
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
                        format!("{}  ·  {}", node.name, format_bytes(self.weights[tile.id])),
                        FontId::proportional(12.0),
                        Color32::from_rgb(227, 235, 246),
                    );
                } else if text_rect.height() > 36.0 {
                    clipped.text(
                        text_rect.center() - Vec2::new(0.0, 8.0),
                        Align2::CENTER_CENTER,
                        &node.name,
                        FontId::proportional(if rect.width() > 180.0 { 15.0 } else { 12.0 }),
                        Color32::WHITE,
                    );
                    clipped.text(
                        text_rect.center() + Vec2::new(0.0, 12.0),
                        Align2::CENTER_CENTER,
                        format_bytes(self.weights[tile.id]),
                        FontId::proportional(11.0),
                        Color32::from_white_alpha(190),
                    );
                } else {
                    clipped.text(
                        text_rect.left_top(),
                        Align2::LEFT_TOP,
                        &node.name,
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
                Stroke::new(2.0, Color32::WHITE),
                StrokeKind::Inside,
            );
        }
        if tiles.is_empty() {
            painter.text(
                area.center(),
                Align2::CENTER_CENTER,
                "No matching bytes\nTry another filter or open a different folder.",
                FontId::proportional(18.0),
                MUTED,
            );
        }
        if let Some(id) = hovered {
            let node = &self.tree.nodes[id];
            response.clone().on_hover_ui(|ui| {
                ui.strong(&node.name);
                ui.label(format_bytes(node.size));
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
                    self.tree.nodes[self.selected]
                        .path
                        .to_string_lossy()
                        .into_owned(),
                );
                ui.close();
            }
            if ui
                .add_enabled(
                    self.tree.nodes[self.selected].is_dir(),
                    egui::Button::new("Zoom into selection"),
                )
                .clicked()
            {
                self.navigate(self.selected);
                ui.close();
            }
            if ui.button("Back to root").clicked() {
                self.navigate(self.tree.root);
                ui.close();
            }
        });
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_scan(ctx);
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, egui::Key::O)) {
            self.choose_folder();
        }
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::F5))
            && !self.path.trim().is_empty()
        {
            self.begin_scan(PathBuf::from(self.path.trim()));
        }
        if !ctx.wants_keyboard_input()
            && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Backspace))
            && let Some(parent) = self.tree.nodes[self.zoom].parent
        {
            self.navigate(parent);
        }
        self.header(ctx);
        self.status(ctx);
        self.inspector(ctx);
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(BG)
                    .inner_margin(egui::Margin::symmetric(20, 14)),
            )
            .show(ctx, |ui| {
                if let Some(error) = self.error.clone() {
                    ui.horizontal_wrapped(|ui| {
                        ui.colored_label(Color32::LIGHT_RED, error);
                        if ui.small_button("Dismiss").clicked() {
                            self.error = None;
                        }
                    });
                }
                if !self.notice.is_empty() {
                    ui.add(
                        egui::Label::new(RichText::new(&self.notice).small().color(ACCENT))
                            .truncate(),
                    );
                }
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(!self.back.is_empty(), egui::Button::new("<"))
                        .clicked()
                        && let Some(id) = self.back.pop()
                    {
                        self.forward.push(self.zoom);
                        self.zoom = id;
                        self.selected = id;
                    }
                    if ui
                        .add_enabled(!self.forward.is_empty(), egui::Button::new(">"))
                        .clicked()
                        && let Some(id) = self.forward.pop()
                    {
                        self.back.push(self.zoom);
                        self.zoom = id;
                        self.selected = id;
                    }
                    let parent = self.tree.nodes[self.zoom].parent;
                    if ui
                        .add_enabled(parent.is_some(), egui::Button::new("Up"))
                        .clicked()
                        && let Some(id) = parent
                    {
                        self.navigate(id);
                    }
                    egui::ScrollArea::horizontal()
                        .id_salt("crumbs")
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                let mut crumbs = vec![self.zoom];
                                let mut cursor = self.zoom;
                                while let Some(parent) = self.tree.nodes[cursor].parent {
                                    crumbs.push(parent);
                                    cursor = parent;
                                }
                                for id in crumbs.into_iter().rev() {
                                    if ui
                                        .selectable_label(
                                            id == self.zoom,
                                            &self.tree.nodes[id].name,
                                        )
                                        .clicked()
                                    {
                                        self.navigate(id);
                                    }
                                    ui.label(RichText::new("/").color(MUTED));
                                }
                            });
                        });
                });
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(format_bytes(self.weights[self.zoom]))
                            .size(32.0)
                            .strong(),
                    );
                    ui.vertical(|ui| {
                        ui.label(RichText::new("IN THIS VIEW").small().color(MUTED));
                        ui.label(
                            RichText::new(if self.demo {
                                "Sample workspace · open a folder to begin"
                            } else {
                                "Click to inspect · double-click to explore"
                            })
                            .small()
                            .color(MUTED),
                        );
                    });
                });
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Filter").color(MUTED));
                    let response = ui.add_sized(
                        [(ui.available_width() - 350.0).max(100.0), 28.0],
                        egui::TextEdit::singleline(&mut self.query)
                            .hint_text("*.zip; >100mb; |backup*"),
                    );
                    if response.changed() {
                        self.apply_filter();
                    }
                    if ui.small_button("Clear").clicked() {
                        self.query.clear();
                        self.apply_filter();
                    }
                    ui.label(RichText::new("Depth").small().color(MUTED));
                    ui.add(
                        egui::Slider::new(&mut self.depth, 1..=6)
                            .show_value(true)
                            .clamping(egui::SliderClamping::Always),
                    );
                    ui.checkbox(&mut self.classic, "Classic");
                });
                if let Some(error) = &self.filter_error {
                    ui.colored_label(
                        Color32::LIGHT_RED,
                        format!("{error} · showing previous valid filter"),
                    );
                }
                ui.horizontal(|ui| {
                    for (label, color) in [
                        ("Code", Color32::from_rgb(64, 111, 161)),
                        ("Media", Color32::from_rgb(167, 120, 62)),
                        ("Images", Color32::from_rgb(51, 128, 122)),
                        ("Archives", Color32::from_rgb(156, 84, 108)),
                        ("Apps", Color32::from_rgb(113, 94, 157)),
                        ("Other", Color32::from_rgb(76, 99, 128)),
                    ] {
                        ui.label(RichText::new("■").color(color));
                        ui.label(RichText::new(label).size(11.0).color(MUTED));
                    }
                });
                ui.add_space(5.0);
                self.canvas(ui);
            });
        egui::Window::new("Welcome to Space Sniffer")
            .open(&mut self.help)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label("Open a folder or enter a drive path to scan it.");
                ui.label("Rectangle area represents logical file size.");
                ui.label("Click: inspect. Double-click: zoom into a folder.");
                ui.label("Backspace: parent. Ctrl+O: open. F5: rescan.");
                ui.separator();
                ui.label("Filters combine with semicolons (AND):");
                ui.monospace("*.zip; >100mb; |backup*");
                ui.label("Substring, * and ? wildcards, exclusions with |, < and > sizes.");
                ui.label("MB = 1,000,000 bytes. MiB = 1,048,576 bytes.");
                ui.separator();
                ui.label("Symlinks and Windows reparse points are never followed.");
                ui.label("Hard links count per path. Unreadable entries are skipped.");
                ui.label("Zero-byte entries appear in the list, without map area.");
                ui.label("CSV includes every entry in this view, with total and matching bytes.");
                ui.label("No telemetry. No file deletion. MIT licensed.");
            });
    }
}

impl Drop for App {
    fn drop(&mut self) {
        if let Some(scan) = &self.scan {
            scan.cancel.store(true, Ordering::Relaxed);
        }
    }
}

struct DrawTile {
    id: usize,
    rect: Rect,
    container: bool,
    level: usize,
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
        && rect.height() > 52.0
        && !node.children.is_empty();
    out.push(DrawTile {
        id,
        rect,
        container,
        level,
    });
    if !container {
        return;
    }
    let inner = Rect::from_min_max(rect.min + Vec2::new(4.0, 24.0), rect.max - Vec2::splat(4.0));
    let items: Vec<_> = node
        .children
        .iter()
        .map(|&child| (child, weights[child]))
        .filter(|&(_, weight)| weight > 0)
        .collect();
    for tile in treemap::layout(
        &items,
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
    use space_sniffer::model::EntryKind;
    use std::sync::{Arc, atomic::AtomicBool, mpsc};

    #[test]
    fn new_scan_with_invalid_query_rebuilds_weights_for_the_new_tree() {
        let mut app = App::demo_state();
        app.query = "*.zip".into();
        app.apply_filter();
        app.query = ">oops".into();
        app.apply_filter();
        let mut tree = crate::demo::tree();
        let root = tree.root;
        for i in 0..100 {
            let id = tree.nodes.len();
            tree.nodes.push(Node {
                name: format!("extra-{i}.zip"),
                path: PathBuf::from(format!("extra-{i}.zip")),
                kind: EntryKind::File,
                size: 100,
                children: vec![],
                parent: Some(root),
                modified: None,
            });
            tree.nodes[root].children.push(id);
            tree.nodes[root].size += 100;
        }
        let expected = Filter::parse("*.zip").unwrap().weights(&tree);
        let (sender, receiver) = mpsc::channel();
        sender.send(ScanEvent::Finished(Ok(tree))).unwrap();
        app.scan = Some(ScanHandle {
            receiver,
            cancel: Arc::new(AtomicBool::new(false)),
        });
        app.poll_scan(&egui::Context::default());
        assert_eq!(app.weights, expected);
        assert_eq!(app.weights.len(), app.tree.nodes.len());
        assert!(app.filter_error.is_some());
        assert_eq!(app.applied, "*.zip");
    }

    #[test]
    fn nested_map_respects_depth_and_only_uses_valid_nodes() {
        let tree = crate::demo::tree();
        let weights = tree.nodes.iter().map(|node| node.size).collect::<Vec<_>>();
        let bounds = Rect::from_min_size(Pos2::ZERO, Vec2::new(1000.0, 600.0));
        let mut tiles = Vec::new();
        collect_tiles(&tree, &weights, tree.root, bounds, 2, 0, &mut tiles);
        assert!(tiles.len() > tree.nodes[tree.root].children.len());
        for tile in tiles {
            assert!(tile.id < tree.nodes.len());
            assert!(tile.level <= 2);
            assert!(bounds.contains_rect(tile.rect));
        }
    }
}
