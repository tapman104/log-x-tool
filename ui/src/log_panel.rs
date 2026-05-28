use egui_extras::{Column, TableBuilder};
use engine::{LogFile, LogLevel};

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
pub fn show_log_panel(ui: &mut egui::Ui, log_file: &LogFile, search: &str) {
    let entries      = &log_file.entries;
    let default_col  = ui.visuals().text_color();
    let font_id      = egui::TextStyle::Monospace.resolve(ui.style());
    let search_lower = search.to_lowercase();

    // ── Filter ───────────────────────────────────────────────────────────────
    // Collect the indices of rows that match the search. When search is empty
    // every row passes. Using indices keeps us O(visible) during rendering.
    let indices: Vec<usize> = if search.is_empty() {
        (0..entries.len()).collect()
    } else {
        (0..entries.len())
            .filter(|&i| entries[i].raw.to_lowercase().contains(&search_lower))
            .collect()
    };
    let num_rows = indices.len();

    // ── Table ────────────────────────────────────────────────────────────────
    TableBuilder::new(ui)
        .striped(true)
        .resizable(true)
        .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
        .column(Column::exact(LINE_COL_WIDTH))
        .column(Column::exact(TS_COL_WIDTH))
        .column(Column::remainder().clip(true))
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
                let entry      = &entries[indices[row.index()]];
                let text_color = level_color(&entry.level, default_col);

                // ── Line-number cell ─────────────────────────────────────────
                row.col(|ui| {
                    ui.with_layout(
                        egui::Layout::right_to_left(egui::Align::Center),
                        |ui| {
                            ui.label(
                                egui::RichText::new(entry.line_number.to_string())
                                    .color(egui::Color32::from_gray(110))
                                    .monospace(),
                            );
                        },
                    );
                });

                // ── Timestamp cell ───────────────────────────────────────────
                row.col(|ui| {
                    let ts = entry.timestamp.as_deref().unwrap_or("");
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
                    let base_fmt = egui::text::TextFormat {
                        font_id: font_id.clone(),
                        color:   text_color,
                        ..Default::default()
                    };
                    let mut job = egui::text::LayoutJob::default();
                    append_highlighted(&mut job, &entry.raw, search, base_fmt);
                    ui.add(egui::Label::new(job).truncate());
                });
            });
        });
}
