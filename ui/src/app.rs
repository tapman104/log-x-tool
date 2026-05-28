use std::path::PathBuf;
use std::sync::mpsc;

use engine::{load_file, parse_file, AppError, LogFile, LogLevel};

// ── Background loader ──────────────────────────────────────────────────────

/// Handle to an in-progress file-load spawned on a worker thread.
struct LoadTask {
    /// Path being loaded (shown in spinner / title bar).
    path: PathBuf,
    /// Receives `Ok(LogFile)` or `Err(AppError)` from the worker thread.
    rx: mpsc::Receiver<Result<LogFile, AppError>>,
}

// ── App state ──────────────────────────────────────────────────────────────

pub struct LogViewerApp {
    /// Last successfully loaded file.
    log_file: Option<LogFile>,
    err_count: usize,
    warn_count: usize,
    info_count: usize,
    debug_count: usize,
    /// Present while a background load is running.
    load_task: Option<LoadTask>,
    /// Most-recent error shown in the status bar.
    last_error: Option<String>,
    /// `true` = dark, `false` = light.
    dark_mode: bool,
    /// Current search string — filters and highlights the log table.
    search: String,
    /// Current log level filter.
    level_filter: Option<LogLevel>,
    /// Jump to line input.
    jump_input: String,
    /// Pending line jump request.
    scroll_to_line: Option<usize>,
    /// Request focus on jump input.
    focus_jump: bool,
}

impl Default for LogViewerApp {
    fn default() -> Self {
        Self {
            log_file: None,
            err_count: 0,
            warn_count: 0,
            info_count: 0,
            debug_count: 0,
            load_task: None,
            last_error: None,
            dark_mode: true,
            search: String::new(),
            level_filter: None,
            jump_input: String::new(),
            scroll_to_line: None,
            focus_jump: false,
        }
    }
}

// ── Logic ──────────────────────────────────────────────────────────────────

impl LogViewerApp {
    /// Spawn a worker thread to load `path` and store its channel handle.
    /// Any previous task / error is discarded; the old file stays visible
    /// until the new one arrives, avoiding a blank flash.
    fn spawn_load(&mut self, path: PathBuf) {
        let (tx, rx) = mpsc::channel::<Result<LogFile, AppError>>();
        let path_clone = path.clone();
        std::thread::spawn(move || {
            // Send the full AppError — no string conversion on the worker side.
            let _ = tx.send(load_file(&path_clone));
        });
        self.load_task = Some(LoadTask { path, rx });
        self.last_error = None;
    }

    /// Show the native OS file-picker and spawn a load.
    fn pick_and_open(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .set_title("Open log file")
            .pick_file()
        {
            self.spawn_load(path);
        }
    }

    /// Non-blocking poll of the background task.
    /// Integrates the result into `log_file` / `last_error` when ready.
    fn poll_load_task(&mut self) {
        // Two-step: extract result (drops immutable borrow), then mutate.
        let result = if let Some(ref t) = self.load_task {
            t.rx.try_recv().ok()
        } else {
            None
        };

        if let Some(outcome) = result {
            self.load_task = None;
            match outcome {
                Ok(mut lf) => {
                    parse_file(&mut lf);
                    
                    let mut errs = 0;
                    let mut warns = 0;
                    let mut infos = 0;
                    let mut debugs = 0;
                    for e in &lf.entries {
                        match e.level {
                            Some(LogLevel::Error) => errs += 1,
                            Some(LogLevel::Warn) => warns += 1,
                            Some(LogLevel::Info) => infos += 1,
                            Some(LogLevel::Debug) => debugs += 1,
                            _ => {}
                        }
                    }
                    self.err_count = errs;
                    self.warn_count = warns;
                    self.info_count = infos;
                    self.debug_count = debugs;

                    self.log_file = Some(lf);
                    self.last_error = None;
                }
                Err(e) => {
                    self.last_error = Some(e.to_string());
                }
            }
        }
    }
}

// ── UI ─────────────────────────────────────────────────────────────────────

impl eframe::App for LogViewerApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // ── Theme ────────────────────────────────────────────────────────────
        ctx.set_visuals(if self.dark_mode {
            egui::Visuals::dark()
        } else {
            egui::Visuals::light()
        });

        // ── Background task ──────────────────────────────────────────────────
        self.poll_load_task();
        if self.load_task.is_some() {
            // Keep repainting so the spinner animates and we catch the result
            // the frame it arrives.
            ctx.request_repaint();
        }

        // ── Dynamic title bar ────────────────────────────────────────────────
        let title = if let Some(ref t) = self.load_task {
            format!(
                "Log Viewer — Loading {}…",
                t.path.file_name().unwrap_or_default().to_string_lossy()
            )
        } else if let Some(ref lf) = self.log_file {
            format!(
                "Log Viewer — {}",
                lf.path.file_name().unwrap_or_default().to_string_lossy()
            )
        } else {
            "Log Viewer".to_string()
        };
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(title));

        // ── Drag-and-drop ────────────────────────────────────────────────────
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .filter_map(|f| f.path.clone())
                .collect()
        });
        if let Some(path) = dropped.into_iter().next() {
            self.spawn_load(path);
        }

        // ── Menu bar ─────────────────────────────────────────────────────────
        egui::TopBottomPanel::top("menu_bar").show(ctx, |ui| {
            egui::menu::bar(ui, |ui| {
                ui.menu_button("File", |ui| {
                    if ui.button("Open…   Ctrl+O").clicked() {
                        ui.close_menu();
                        self.pick_and_open();
                    }
                    ui.separator();
                    if ui.button("Quit").clicked() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });
                ui.menu_button("View", |ui| {
                    let label = if self.dark_mode { "☀  Light theme" } else { "🌙  Dark theme" };
                    if ui.button(label).clicked() {
                        self.dark_mode = !self.dark_mode;
                        ui.close_menu();
                    }
                });
                // Quick-toggle icon pinned to the right edge of the bar.
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let icon = if self.dark_mode { "☀" } else { "🌙" };
                    if ui
                        .button(egui::RichText::new(icon).size(15.0))
                        .on_hover_text(if self.dark_mode {
                            "Switch to light theme"
                        } else {
                            "Switch to dark theme"
                        })
                        .clicked()
                    {
                        self.dark_mode = !self.dark_mode;
                    }
                });
            });
        });

        // ── Keyboard shortcuts ────────────────────────────────────────────────
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, egui::Key::O)) {
            self.pick_and_open();
        }
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, egui::Key::G)) {
            self.focus_jump = true;
        }

        // ── Error / status bar (dismissible) ─────────────────────────────────
        if self.last_error.is_some() {
            let err = self.last_error.clone().unwrap();
            egui::TopBottomPanel::bottom("status_bar").show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.colored_label(egui::Color32::from_rgb(220, 80, 80), &err);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("✕  dismiss").clicked() {
                            self.last_error = None;
                        }
                    });
                });
            });
        }

        // ── Central panel ────────────────────────────────────────────────────
        egui::CentralPanel::default().show(ctx, |ui| {
            if let Some(ref t) = self.load_task {
                // ── Loading indicator ────────────────────────────────────────
                let file_name = t.path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned();
                ui.vertical_centered(|ui| {
                    ui.add_space(ui.available_height() / 3.0);
                    ui.add(egui::Spinner::new().size(48.0));
                    ui.add_space(16.0);
                    ui.label(
                        egui::RichText::new(format!("Loading  {file_name}…"))
                            .size(16.0)
                            .color(egui::Color32::from_gray(160)),
                    );
                });
            } else if self.log_file.is_some() {
                // ── Loaded state ─────────────────────────────────────────────
                // Extract display info while releasing the borrow before the
                // mutable TextEdit borrow of self.search below.
                let (name, format_badge, entry_count, err_count, warn_count) = {
                    let lf = self.log_file.as_ref().unwrap();
                    let name = lf
                        .path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("<unknown>")
                        .to_owned();
                    let badge = lf.format.clone().unwrap_or_default();

                    (name, badge, lf.entries.len(), self.err_count, self.warn_count)
                };

                // Header row — filename + format badge + total line count
                ui.horizontal(|ui| {
                    ui.heading(&name);
                    if !format_badge.is_empty() {
                        ui.label(
                            egui::RichText::new(format!("  {format_badge}"))
                                .size(12.0)
                                .color(egui::Color32::from_gray(120)),
                        );
                    }
                    if err_count > 0 || warn_count > 0 {
                        ui.add_space(8.0);
                        if err_count > 0 {
                            ui.label(
                                egui::RichText::new(format!("{} errors", err_count))
                                    .size(12.0)
                                    .color(egui::Color32::from_rgb(255, 100, 100)),
                            );
                        }
                        if err_count > 0 && warn_count > 0 {
                            ui.label(
                                egui::RichText::new("·")
                                    .size(12.0)
                                    .color(egui::Color32::from_gray(120)),
                            );
                        }
                        if warn_count > 0 {
                            ui.label(
                                egui::RichText::new(format!("{} warnings", warn_count))
                                    .size(12.0)
                                    .color(egui::Color32::from_rgb(255, 200, 80)),
                            );
                        }
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            egui::RichText::new(format!("{entry_count} lines"))
                                .color(egui::Color32::from_gray(140)),
                        );

                        let jump_resp = ui.add(
                            egui::TextEdit::singleline(&mut self.jump_input)
                                .hint_text("Go to line…")
                                .desired_width(110.0),
                        );

                        if self.focus_jump {
                            jump_resp.request_focus();
                            self.focus_jump = false;
                        }

                        if jump_resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                            if let Ok(n) = self.jump_input.trim().parse::<usize>() {
                                let clamped = n.max(1).min(entry_count);
                                self.scroll_to_line = Some(clamped);
                            }
                            self.jump_input.clear();
                        }
                    });
                });

                ui.separator();

                // Search bar — mutable borrow of self.search (different field
                // from self.log_file, so the borrow checker is happy).
                ui.horizontal(|ui| {
                    ui.label("🔍");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.search)
                            .hint_text("Search…")
                            .desired_width(f32::INFINITY),
                    );
                    if !self.search.is_empty()
                        && ui.small_button("✕").on_hover_text("Clear search").clicked()
                    {
                        self.search.clear();
                    }
                });

                ui.add_space(2.0);

                // Re-borrow log_file for the table (search TextEdit borrow released).
                if let Some(ref lf) = self.log_file {
                    crate::log_panel::show_log_panel(
                        ui,
                        lf,
                        &self.search,
                        &mut self.level_filter,
                        &mut self.scroll_to_line,
                        self.err_count,
                        self.warn_count,
                        self.info_count,
                        self.debug_count,
                    );
                }
            } else {
                // ── Empty state ──────────────────────────────────────────────
                ui.vertical_centered(|ui| {
                    ui.add_space(ui.available_height() / 3.0);
                    ui.label(egui::RichText::new("📂").size(52.0));
                    ui.add_space(12.0);
                    ui.label(
                        egui::RichText::new("Drop a log file here")
                            .size(20.0)
                            .color(egui::Color32::from_gray(160)),
                    );
                    ui.add_space(6.0);
                    ui.label(
                        egui::RichText::new("or")
                            .size(13.0)
                            .color(egui::Color32::from_gray(100)),
                    );
                    ui.add_space(10.0);
                    if ui
                        .button(egui::RichText::new("  Open a file…  ").size(14.0))
                        .clicked()
                    {
                        self.pick_and_open();
                    }
                });
            }
        });
    }
}
