// Hide the console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod install;
mod launcher;
mod manifest;
mod selfupdate;
mod updater;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("TechSouls Launcher")
            .with_inner_size([1280.0, 780.0])
            .with_min_inner_size([760.0, 480.0])
            // The launcher draws its own title bar.
            .with_decorations(false),
        ..Default::default()
    };

    eframe::run_native(
        "TechSouls Launcher",
        options,
        Box::new(|cc| Ok(Box::new(launcher::Launcher::new(cc)))),
    )
}
