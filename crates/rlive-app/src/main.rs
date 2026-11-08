//! rLive — a session-oriented DAW in Rust.

fn main() -> eframe::Result {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // The app doubles as its own out-of-process plugin scanner.
    if let Some(code) = rlive_plugin_host::scanner::run_scanner_main(&args) {
        std::process::exit(code);
    }
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let open = args.first().map(std::path::PathBuf::from);

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("rLive")
            .with_inner_size([1400.0, 860.0])
            .with_min_inner_size([900.0, 600.0]),
        ..Default::default()
    };
    eframe::run_native(
        "rLive",
        options,
        Box::new(move |cc| Ok(Box::new(rlive_app::ui::RliveApp::new(cc, open)))),
    )
}
