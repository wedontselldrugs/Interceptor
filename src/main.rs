mod config;
mod console_window;
mod elevation;
mod interceptor;
mod project;
mod tui;
mod updater;

use tui::MenuAction;

fn main() {
    match elevation::ensure_admin() {
        elevation::ElevationStatus::Ready => {}
        elevation::ElevationStatus::Relaunched | elevation::ElevationStatus::Failed => return,
    }

    let mut current_settings = None;
    while let MenuAction::Start(config) = tui::cli_menu(current_settings.take()) {
        current_settings = Some(config.clone());
        interceptor::run(config);
    }
}
