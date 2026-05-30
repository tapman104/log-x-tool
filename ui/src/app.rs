use std::path::PathBuf;
use std::sync::{mpsc, Arc};

// load_file is deprecated in favour of engine::index_file for large files;
// the UI migration will happen in a follow-up step.
#[allow(deprecated)]
use engine::{AppError, LogLevel};

// ── Background loader ──────────────────────────────────────────────────────

enum LoadProgress {
    Progress { scanned: u64, total: u64 },
    Done(Result<(engine::ParsedIndex, [usize; 6], u64), AppError>),
}

/// Handle to an in-progress file-load spawned on a worker thread.
struct LoadTask {
    /// Path being loaded (shown in spinner / title bar).
    path: PathBuf,
    rx: mpsc::Receiver<LoadProgress>,
}

struct SearchTask {
    /// Receives batches of matching indices as the search progresses.
    /// Indices are sent as `u32` (RoaringBitmap's native element type).
    rx: mpsc::Receiver<Vec<u32>>,
    /// Sending a value to this channel cancels the worker.
    cancel_tx: mpsc::SyncSender<()>,
}

// ── App state ──────────────────────────────────────────────────────────────

pub struct LogViewerApp {
    /// Last successfully loaded index.
    parsed: Option<Arc<engine::ParsedIndex>>,
    level_counts: [usize; 6], // indexed by LogLevel as usize
    /// Present while a background load is running.
    load_task: Option<LoadTask>,
    /// Most-recent error shown in the status bar.
    last_error: Option<String>,
    /// `true` = dark, `false` = light.
    dark_mode: bool,
    /// Last opened directory
    last_dir: Option<PathBuf>,
    /// Recent files list
    recent_files: Vec<PathBuf>,
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
    /// Request focus on search input.
    focus_search: bool,
    
    // Caches
    last_search: String,
    last_level_filter: Option<LogLevel>,
    /// Compressed bitmap of row indices that pass the current filter.
    /// Uses ~12 MB for 100 M consecutive entries vs ~800 MB for Vec<usize>.
    filtered_bitmap: roaring::RoaringBitmap,
    /// Cached length so the UI never calls `.len()` (O(n)) on the bitmap per frame.
    filtered_count: u64,
    search_counts: (usize, usize, usize, usize, usize),
    needs_filter_update: bool,
    search_task: Option<SearchTask>,
    search_complete: bool,
    search_debounce: std::time::Instant,

    load_progress: Option<(u64, u64)>,
    parse_duration: Option<std::time::Duration>,
    file_size_bytes: u64,
    match_cursor: Option<usize>,
    load_start_time: Option<std::time::Instant>,
}

impl Default for LogViewerApp {
    fn default() -> Self {
        let mut app = Self {
            parsed: None,
            level_counts: [0; 6],
            load_task: None,
            last_error: None,
            dark_mode: true,
            search: String::new(),
            level_filter: None,
            jump_input: String::new(),
            scroll_to_line: None,
            focus_jump: false,
            focus_search: false,
            last_search: String::new(),
            last_level_filter: None,
            filtered_bitmap: roaring::RoaringBitmap::new(),
            filtered_count: 0,
            search_counts: (0, 0, 0, 0, 0),
            needs_filter_update: false,
            search_task: None,
            search_complete: false,
            search_debounce: std::time::Instant::now(),
            load_progress: None,
            parse_duration: None,
            file_size_bytes: 0,
            match_cursor: None,
            load_start_time: None,
            last_dir: None,
            recent_files: Vec::new(),
        };
        app.load_config();
        app
    }
}

impl LogViewerApp {
    fn config_path() -> Option<PathBuf> {
        dirs::config_dir().map(|mut p| {
            p.push("log-viewer");
            p.push("recent_files.txt");
            p
        })
    }

    fn load_config(&mut self) {
        if let Some(path) = Self::config_path() {
            if let Ok(content) = std::fs::read_to_string(&path) {
                for line in content.lines() {
                    let p = PathBuf::from(line);
                    if p.exists() {
                        self.recent_files.push(p.clone());
                    }
                }
                if let Some(first) = self.recent_files.first() {
                    if let Some(parent) = first.parent() {
                        self.last_dir = Some(parent.to_path_buf());
                    }
                }
            }
        }
    }

    fn save_config(&self) {
        if let Some(path) = Self::config_path() {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let content = self.recent_files.iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("\n");
            let _ = std::fs::write(&path, content);
        }
    }

    fn export_filtered(&self) {
        if let Some(ref parsed) = self.parsed {
            if let Some(path) = rfd::FileDialog::new()
                .set_title("Export filtered log")
                .save_file()
            {
                // Collect line strings off the UI thread; iterate the bitmap directly
                // rather than cloning it into a Vec<usize>.
                let lines: Vec<String> = self.filtered_bitmap
                    .iter()
                    .map(|i| parsed.lines.line_str(i as usize).to_owned())
                    .collect();
                std::thread::spawn(move || {
                    if let Ok(file) = std::fs::File::create(path) {
                        use std::io::Write;
                        let mut writer = std::io::BufWriter::new(file);
                        for line in &lines {
                            let _ = writeln!(writer, "{}", line);
                        }
                    }
                });
            }
        }
    }
}

// ── Logic ──────────────────────────────────────────────────────────────────

impl LogViewerApp {
    /// Spawn a worker thread to load `path` and store its channel handle.
    /// Any previous task / error is discarded; the old file stays visible
    /// until the new one arrives, avoiding a blank flash.
    fn spawn_load(&mut self, path: PathBuf) {
        let (tx, rx) = mpsc::channel::<LoadProgress>();
        let path_clone = path.clone();
        
        self.load_start_time = Some(std::time::Instant::now());
        self.load_progress = None;

        std::thread::spawn(move || {
            // ── Fast path: sidecar cache hit ─────────────────────────────────
            // try_load validates file_size + mtime, so a stale cache is
            // automatically rejected.  A cache hit skips the full mmap scan.
            let res: Result<(engine::ParsedIndex, [usize; 6], u64), engine::AppError> =
                if let Some(parsed) = engine::cache::try_load(&path_clone) {
                    let mut counts = [0usize; 6];
                    for r in &parsed.records {
                        let c_idx: usize = r.level.into();
                        counts[c_idx] += 1;
                    }
                    let file_size_bytes =
                        std::fs::metadata(&path_clone).map(|m| m.len()).unwrap_or(0);
                    Ok((parsed, counts, file_size_bytes))
                } else {
                    // ── Slow path: full scan + parse ─────────────────────────
                    let tx_clone = tx.clone();
                    engine::index_file_with_progress(&path_clone, move |scanned, total| {
                        let _ = tx_clone.send(LoadProgress::Progress { scanned, total });
                    })
                    .map(|idx| {
                        let parsed = engine::parse_index(idx);
                        // Persist the cache so the next open is instant.
                        engine::cache::save(&parsed, &path_clone);
                        let mut counts = [0usize; 6];
                        for r in &parsed.records {
                            let c_idx: usize = r.level.into();
                            counts[c_idx] += 1;
                        }
                        let file_size_bytes =
                            std::fs::metadata(&path_clone).map(|m| m.len()).unwrap_or(0);
                        (parsed, counts, file_size_bytes)
                    })
                };

            let _ = tx.send(LoadProgress::Done(res));
        });
        self.load_task = Some(LoadTask { path, rx });
        self.last_error = None;
    }

    /// Show the native OS file-picker and spawn a load.
    fn pick_and_open(&mut self) {
        let mut dialog = rfd::FileDialog::new().set_title("Open log file");
        if let Some(ref dir) = self.last_dir {
            dialog = dialog.set_directory(dir);
        }
        if let Some(path) = dialog.pick_file() {
            self.spawn_load(path);
        }
    }

    /// Non-blocking poll of the background task.
    /// Integrates the result into `log_file` / `last_error` when ready.
    fn poll_load_task(&mut self) {
        let mut final_outcome = None;
        if let Some(ref t) = self.load_task {
            while let Ok(msg) = t.rx.try_recv() {
                match msg {
                    LoadProgress::Progress { scanned, total } => {
                        self.load_progress = Some((scanned, total));
                    }
                    LoadProgress::Done(res) => {
                        final_outcome = Some(res);
                        break;
                    }
                }
            }
        }

        if let Some(outcome) = final_outcome {
            let path = self.load_task.take().unwrap().path;
            if let Some(start) = self.load_start_time.take() {
                self.parse_duration = Some(start.elapsed());
            }
            match outcome {
                Ok((parsed, counts, file_size)) => {
                    if let Some(parent) = path.parent() {
                        self.last_dir = Some(parent.to_path_buf());
                    }
                    self.recent_files.retain(|p| p != &path);
                    self.recent_files.insert(0, path);
                    self.recent_files.truncate(5);
                    self.save_config();

                    self.level_counts = counts;
                    self.file_size_bytes = file_size;
                    self.needs_filter_update = true;

                    self.parsed = Some(Arc::new(parsed));
                    self.last_error = None;
                }
                Err(e) => {
                    self.last_error = Some(e.to_string());
                }
            }
        }
    }

    fn spawn_search(&mut self, parsed: Arc<engine::ParsedIndex>, query: String, level: Option<LogLevel>) {
        // Cancel any in-flight search
        if let Some(ref t) = self.search_task {
            let _ = t.cancel_tx.try_send(());
        }
        self.filtered_bitmap.clear();
        self.filtered_count = 0;
        self.search_complete = false;

        let (result_tx, result_rx) = mpsc::channel::<Vec<u32>>();
        let (cancel_tx, cancel_rx) = mpsc::sync_channel::<()>(1);

        std::thread::spawn(move || {
            let query_lower = query.to_lowercase();
            let query_bytes = query_lower.as_bytes();
            let mut batch: Vec<u32> = Vec::with_capacity(4096);

            for i in 0..parsed.lines.len() {
                if cancel_rx.try_recv().is_ok() { return; }

                // RoaringBitmap indices are u32; files with ≥ 2^32 lines are not
                // supported at current hardware limits.
                assert!(i < u32::MAX as usize, "line index exceeds u32::MAX");

                let passes_level = match &level {
                    Some(lvl) => parsed.level_of(i) == *lvl,
                    None => true,
                };
                let passes_search = query_bytes.is_empty()
                    || memchr::memmem::find(parsed.lines.line_bytes(i), query_bytes).is_some();

                if passes_level && passes_search {
                    batch.push(i as u32);
                    if batch.len() == 4096 {
                        if result_tx.send(std::mem::take(&mut batch)).is_err() { return; }
                    }
                }
            }
            if !batch.is_empty() {
                let _ = result_tx.send(batch);
            }
        });

        self.search_task = Some(SearchTask { rx: result_rx, cancel_tx });
    }

    fn poll_search_task(&mut self) {
        let done = if let Some(ref t) = self.search_task {
            loop {
                match t.rx.try_recv() {
                    Ok(batch) => {
                        self.filtered_bitmap.extend(batch);
                        self.filtered_count = self.filtered_bitmap.len();
                    }
                    Err(mpsc::TryRecvError::Empty) => break false,
                    Err(mpsc::TryRecvError::Disconnected) => break true,
                }
            }
        } else { true };
        if done { self.search_task = None; self.search_complete = true; }
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

        self.poll_search_task();
        if self.search_task.is_some() {
            ctx.request_repaint();
        }
        if self.search_task.is_none()
            && self.search_debounce.elapsed() > std::time::Duration::from_millis(150)
            && self.needs_filter_update
        {
            self.needs_filter_update = false;
            if let Some(ref p) = self.parsed {
                self.spawn_search(Arc::clone(p), self.search.clone(), self.level_filter.clone());
            }
        }

        // ── Dynamic title bar ────────────────────────────────────────────────
        let title = if let Some(ref t) = self.load_task {
            format!(
                "Log Viewer — Loading {}…",
                t.path.file_name().unwrap_or_default().to_string_lossy()
            )
        } else if let Some(ref parsed) = self.parsed {
            format!(
                "Log Viewer — {}",
                parsed.lines.path.file_name().unwrap_or_default().to_string_lossy()
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
                    if !self.recent_files.is_empty() {
                        ui.separator();
                        ui.menu_button("Recent Files", |ui| {
                            // FIX 4: avoid cloning the whole Vec; collect only the
                            // one path the user clicked (if any) outside the iterator.
                            let mut to_open: Option<PathBuf> = None;
                            for path in self.recent_files.iter() {
                                if ui.button(path.display().to_string()).clicked() {
                                    ui.close_menu();
                                    to_open = Some(path.clone());
                                }
                            }
                            if let Some(p) = to_open {
                                self.spawn_load(p);
                            }
                        });
                    }
                    if self.filtered_count > 0 {
                        ui.separator();
                        if ui.button("Export filtered…").clicked() {
                            ui.close_menu();
                            self.export_filtered();
                        }
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
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, egui::Key::F)) {
            self.focus_search = true;
        }
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            self.search.clear();
            self.level_filter = None;
        }

        // ── Status bar (always visible) ──────────────────────────────────────
        if let Some(ref parsed) = self.parsed {
            egui::TopBottomPanel::bottom("status_bar").show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let format_str = if parsed.format.is_empty() { "Unknown" } else { &parsed.format };
                    
                    let lines = parsed.lines.len();
                    let s = lines.to_string();
                    let bytes = s.as_bytes();
                    let mut lines_fmt = String::with_capacity(s.len() + s.len() / 3);
                    for (i, &b) in bytes.iter().enumerate() {
                        if i > 0 && (bytes.len() - i) % 3 == 0 {
                            lines_fmt.push(',');
                        }
                        lines_fmt.push(b as char);
                    }
                    
                    let file_gb = self.file_size_bytes as f64 / 1_000_000_000.0;
                    let parsed_s = self.parse_duration.unwrap_or_default().as_secs_f64();
                    
                    ui.label(format!(
                        "Format: {}   |   Lines: {}   |   File: {:.1} GB   |   Parsed in {:.1} s",
                        format_str, lines_fmt, file_gb, parsed_s
                    ));
                });
            });
        }

        // ── Error bar (dismissible) ──────────────────────────────────────────
        if self.last_error.is_some() {
            let err = self.last_error.clone().unwrap();
            egui::TopBottomPanel::bottom("error_bar").show(ctx, |ui| {
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
                    if let Some((scanned, total)) = self.load_progress {
                        let pct = scanned as f32 / total as f32;
                        ui.add(egui::ProgressBar::new(pct)
                            .show_percentage()
                            .animate(true)
                            .text(format!("{:.1} / {:.1} GB",
                                scanned as f64 / 1e9, total as f64 / 1e9)));
                    } else {
                        ui.add(egui::Spinner::new().size(48.0));
                    }
                    ui.add_space(16.0);
                    ui.label(
                        egui::RichText::new(format!("Loading  {file_name}…"))
                            .size(16.0)
                            .color(egui::Color32::from_gray(160)),
                    );
                });
            } else if self.parsed.is_some() {
                // ── Loaded state ─────────────────────────────────────────────
                // Extract display info while releasing the borrow before the
                // mutable TextEdit borrow of self.search below.
                let (name, format_badge, entry_count, err_count, warn_count) = {
                    let parsed = self.parsed.as_ref().unwrap();
                    let name = parsed.lines
                        .path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("<unknown>")
                        .to_owned();
                    let badge = parsed.format.clone();

                    (name, badge, parsed.lines.len(), self.level_counts[0], self.level_counts[1])
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

                // Search bar
                ui.horizontal(|ui| {
                    ui.label("🔍");
                    
                    let mut next_clicked = false;
                    let mut prev_clicked = false;
                    
                    if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::F3)) {
                        next_clicked = true;
                    }
                    if ctx.input_mut(|i| i.consume_key(egui::Modifiers::SHIFT, egui::Key::F3)) {
                        prev_clicked = true;
                    }

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if self.filtered_count > 0 {
                            if ui.button("▼").clicked() {
                                next_clicked = true;
                            }
                            if ui.button("▲").clicked() {
                                prev_clicked = true;
                            }
                            let m = self.filtered_count as usize;
                            let n = self.match_cursor.unwrap_or(0) + 1;
                            ui.label(format!("{} / {}", n.min(m), m));
                        }

                        if !self.search.is_empty()
                            && ui.small_button("✕").on_hover_text("Clear search").clicked()
                        {
                            self.search.clear();
                        }

                        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                            let search_resp = ui.add(
                                egui::TextEdit::singleline(&mut self.search)
                                    .hint_text("Search…")
                                    .desired_width(ui.available_width()),
                            );
                            if self.focus_search {
                                search_resp.request_focus();
                                self.focus_search = false;
                            }
                        });
                    });

                    if next_clicked || prev_clicked {
                        let m = self.filtered_count as usize;
                        if m > 0 {
                            let mut cursor = self.match_cursor.unwrap_or(0);
                            if next_clicked {
                                cursor = (cursor + 1) % m;
                            } else {
                                cursor = cursor.checked_sub(1).unwrap_or(m - 1);
                            }
                            self.match_cursor = Some(cursor);
                            // select(rank) is O(log n) and gives the (rank)th set bit.
                            let line_idx = self.filtered_bitmap
                                .select(cursor as u32)
                                .expect("cursor within bitmap bounds") as usize;
                            self.scroll_to_line = Some(line_idx + 1);
                        }
                    }
                });

                ui.add_space(2.0);

                if let Some(ref parsed) = self.parsed {
                    let search_changed = self.search != self.last_search;
                    let level_changed = self.level_filter != self.last_level_filter;

                    if search_changed || level_changed {
                        self.last_search = self.search.clone();
                        self.last_level_filter = self.level_filter.clone();
                        self.needs_filter_update = true;
                        self.search_debounce = std::time::Instant::now();
                        self.match_cursor = None;
                    }

                    // With the background search task, we revert to showing global counts on the filter buttons.
                    let all = parsed.lines.len();
                    let err = self.level_counts[0];
                    let warn = self.level_counts[1];
                    let info = self.level_counts[2];
                    let debug = self.level_counts[3];
                    self.search_counts = (all, err, warn, info, debug);

                    crate::log_panel::show_log_panel(
                        ui,
                        parsed,
                        &self.search,
                        &mut self.level_filter,
                        &mut self.scroll_to_line,
                        &self.filtered_bitmap,
                        self.search_counts,
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

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    /// Verify that a fully-open filter over 100 M rows fits within the
    /// stated memory budget.  A dense range [0, 100_000_000) serialises to
    /// ~12 MB in roaring's native format — well under the 50 MB cap stated
    /// in the spec.
    #[test]
    fn bitmap_dense_100m_under_50mb() {
        use roaring::RoaringBitmap;

        const N: u32 = 100_000_000;
        const LIMIT_BYTES: usize = 50 * 1024 * 1024; // 50 MB

        let bitmap: RoaringBitmap = (0..N).collect();
        assert_eq!(bitmap.len(), N as u64, "all entries must be present");

        // Serialise to measure compressed size.
        let mut buf: Vec<u8> = Vec::new();
        bitmap
            .serialize_into(&mut buf)
            .expect("serialization must succeed");

        assert!(
            buf.len() < LIMIT_BYTES,
            "serialized size {} bytes exceeds {LIMIT_BYTES} byte limit",
            buf.len(),
        );
    }
}
