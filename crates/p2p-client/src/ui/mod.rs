mod diagnostics;
mod home;
mod room;
mod settings;

use super::ClientUi;
use eframe::egui;

pub(super) fn show_home(app: &mut ClientUi, ui: &mut egui::Ui) {
    app.show_home(ui);
}

pub(super) fn show_room(app: &mut ClientUi, ui: &mut egui::Ui) {
    app.show_room(ui);
}

pub(super) fn show_fullscreen_video(app: &mut ClientUi, ui: &mut egui::Ui) {
    app.show_fullscreen_video(ui);
}

pub(super) fn show_room_toolbar(app: &mut ClientUi, ui: &mut egui::Ui) {
    app.show_room_toolbar(ui);
}

pub(super) fn show_handoff_panel(app: &mut ClientUi, ui: &mut egui::Ui, context: &egui::Context) {
    app.show_handoff_panel(ui, context);
}

pub(super) fn show_settings(app: &mut ClientUi, ui: &mut egui::Ui) {
    app.show_settings(ui);
}

pub(super) fn show_diagnostics(
    app: &mut ClientUi,
    ui: &mut egui::Ui,
    open_logs_directory: &mut bool,
    export_logs: &mut bool,
) {
    app.show_diagnostics(ui, open_logs_directory, export_logs);
}
