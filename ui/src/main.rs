#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod log_panel;

fn main() -> eframe::Result<()> {
    let mut options = eframe::NativeOptions::default();
    options.viewport.icon = eframe::icon_data::from_png_bytes(&include_bytes!("../assets/icon.png")[..])
        .ok()
        .map(std::sync::Arc::new);
    eframe::run_native(
        "Log Viewer",
        options,
        Box::new(|_cc| Ok(Box::new(app::LogViewerApp::default()))),
    )
}
