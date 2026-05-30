use egui_extras::{Column, TableBuilder};
use engine::LogLevel;

/// Row height in logical pixels (fixed — required for O(1) virtual-scroll math).
const ROW_HEIGHT: f32 = 18.0;
/// Header height.
const HEADER_HEIGHT: f32 = 22.0;
/// Width of the line-number column.
const LINE_COL_WIDTH: f32 = 64.0;
/// Width of the timestamp column.
const TS_COL_WIDTH: f32 = 185.0;

// ── Colour helpers ────────────────────────────────────────────────────────────

/// Map a log level to its display colour.
fn level_color(level: &Option<LogLevel>, default: egui::Color32) -> egui::Color32 {
    match level {
        Some(LogLevel::Error)               => egui::Color32::from_rgb(255, 100, 100),
        Some(LogLevel::Warn)                => egui::Color32::from_rgb(255, 200,  80),
        Some(LogLevel::Info)                => default,
        Some(LogLevel::Debug)
        | Some(LogLevel::Trace)             => egui::Color32::from_gray(120),
        Some(LogLevel::Unknown) | None      => default,
    }
}

/// Append `text` to `job`, colouring every occurrence of `needle`
/// (case-insensitive) with a yellow highlight.
/// When `needle` is empty the text is appended with `base_fmt` unchanged.
fn append_highlighted(
    job:      &mut egui::text::LayoutJob,
    text:     &str,
    needle:   &str,
    base_fmt: egui::text::TextFormat,
) {
    if needle.is_empty() {
        job.append(text, 0.0, base_fmt);
        return;
    }

    let hi_fmt = egui::text::TextFormat {
        color:      egui::Color32::from_rgb(20, 20, 20),   // dark text …
        background: egui::Color32::from_rgb(255, 210, 50), // … on amber bg
        ..base_fmt.clone()
    };

    let text_lower   = text.to_lowercase();
    let needle_lower = needle.to_lowercase();
    let match_len    = needle_lower.len();
    let mut cursor   = 0usize;

    while cursor <= text_lower.len() {
        match text_lower[cursor..].find(&needle_lower) {
            None => {
                if cursor < text.len() {
                    job.append(&text[cursor..], 0.0, base_fmt);
                }
                break;
            }
            Some(rel) => {
                let abs = cursor + rel;
                if abs > cursor {
                    job.append(&text[cursor..abs], 0.0, base_fmt.clone());
                }
                let end = (abs + match_len).min(text.len());
                job.append(&text[abs..end], 0.0, hi_fmt.clone());
                cursor = abs + match_len;
            }
        }
    }
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Render a virtual-scroll table of log entries.
///
/// `search` — when non-empty, only rows whose `raw` contains the string
/// (case-insensitive) are shown, and every match is highlighted in amber.
pub fn show_log_panel(
    ui: &mut egui::Ui,
    parsed: &engine::ParsedIndex,
    search: &str,
    level_filter: &mut Option<LogLevel>,
    scroll_to_line: &mut Option<usize>,
    filtered_bitmap: &roaring::RoaringBitmap,
    search_counts: (usize, usize, usize, usize, usize),
) {
    let default_col  = ui.visuals().text_color();
    let font_id      = egui::TextStyle::Monospace.resolve(ui.style());

    // ── Filter Buttons ───────────────────────────────────────────────────────
    ui.horizontal(|ui| {
        let mut add_btn = |label: &str, lvl: Option<LogLevel>, count: usize| {
            let text = format!("{} ({})", label, count);
            let is_selected = *level_filter == lvl;
            if ui.selectable_label(is_selected, text).clicked() {
                if is_selected {
                    *level_filter = None;
                } else {
                    *level_filter = lvl;
                }
            }
        };

        add_btn("ALL", None, search_counts.0);
        add_btn("ERROR", Some(LogLevel::Error), search_counts.1);
        add_btn("WARN", Some(LogLevel::Warn), search_counts.2);
        add_btn("INFO", Some(LogLevel::Info), search_counts.3);
        add_btn("DEBUG", Some(LogLevel::Debug), search_counts.4);
    });
    ui.add_space(4.0);

    let num_rows = filtered_bitmap.len() as usize;

    // ── Table ────────────────────────────────────────────────────────────────
    let mut builder = TableBuilder::new(ui)
        .striped(true)
        .resizable(true)
        .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
        .column(Column::exact(LINE_COL_WIDTH))
        .column(Column::exact(TS_COL_WIDTH))
        .column(Column::remainder().clip(true));

    if let Some(n) = *scroll_to_line {
        builder = builder.scroll_to_row(n - 1, Some(egui::Align::Center));
        *scroll_to_line = None;
    }

    builder
        .header(HEADER_HEIGHT, |mut header| {
            header.col(|ui| { ui.strong("Line"); });
            header.col(|ui| { ui.strong("Timestamp"); });
            header.col(|ui| {
                if search.is_empty() {
                    ui.strong("Text");
                } else {
                    ui.strong(format!("Text  ({num_rows} match{})",
                        if num_rows == 1 { "" } else { "es" }));
                }
            });
        })
        .body(|body| {
            body.rows(ROW_HEIGHT, num_rows, |mut row| {
                // select(rank) translates a virtual row index to an absolute line index.
                // O(log n) on RoaringBitmap — fast enough for row rendering.
                let i = filtered_bitmap
                    .select(row.index() as u32)
                    .unwrap() as usize;
                let level = parsed.level_of(i);
                let text_color = level_color(&Some(level), default_col);
                let bg_color   = match level {
                    LogLevel::Error => Some(egui::Color32::from_rgba_unmultiplied(255, 50, 50, 25)),
                    LogLevel::Warn  => Some(egui::Color32::from_rgba_unmultiplied(255, 180, 50, 20)),
                    _ => None,
                };

                // ── Line-number cell ─────────────────────────────────────────
                row.col(|ui| {
                    if let Some(bg) = bg_color {
                        ui.painter().rect_filled(ui.max_rect(), 0.0, bg);
                    }
                    ui.with_layout(
                        egui::Layout::right_to_left(egui::Align::Center),
                        |ui| {
                            ui.label(
                                egui::RichText::new((i + 1).to_string())
                                    .color(egui::Color32::from_gray(110))
                                    .monospace(),
                            );
                        },
                    );
                });

                // ── Timestamp cell ───────────────────────────────────────────
                row.col(|ui| {
                    if let Some(bg) = bg_color {
                        ui.painter().rect_filled(ui.max_rect(), 0.0, bg);
                    }
                    let ts = parsed.timestamp_of(i).unwrap_or("");
                    if !ts.is_empty() {
                        ui.label(
                            egui::RichText::new(ts)
                                .color(egui::Color32::from_gray(130))
                                .monospace(),
                        );
                    }
                });

                // ── Raw-text cell ────────────────────────────────────────────
                // Uses LayoutJob so level colour and search highlight can coexist
                // in a single widget without extra allocations.
                row.col(|ui| {
                    if let Some(bg) = bg_color {
                        ui.painter().rect_filled(ui.max_rect(), 0.0, bg);
                    }
                    let base_fmt = egui::text::TextFormat {
                        font_id: font_id.clone(),
                        color:   text_color,
                        ..Default::default()
                    };
                    let mut job = egui::text::LayoutJob::default();
                    let raw = parsed.lines.line_str(i);
                    append_highlighted(&mut job, raw, search, base_fmt);
                    let response = ui.add(egui::Label::new(job).truncate().sense(egui::Sense::click()));
                    response.context_menu(|ui| {
                        if ui.button("Copy line").clicked() {
                            ui.ctx().copy_text(raw.to_owned());
                            ui.close_menu();
                        }
                    });
                });
            });
        });
}
