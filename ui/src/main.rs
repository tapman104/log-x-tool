mod app;
mod log_panel;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions::default(); // DnD on by default in eframe 0.31
    eframe::run_native(
        "Log Viewer",
        options,
        Box::new(|_cc| Ok(Box::new(app::LogViewerApp::default()))),
    )
}
