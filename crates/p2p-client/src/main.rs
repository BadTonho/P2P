#![windows_subsystem = "windows"]

use eframe::egui;

fn main() -> eframe::Result {
    eframe::run_ui_native(
        "P2P - Voz e tela",
        eframe::NativeOptions::default(),
        |ui, _frame| {
            egui::CentralPanel::default().show(ui, |ui| {
                ui.heading("P2P - Voz e tela");
                ui.label("Estrutura inicial do aplicativo.");
            });
        },
    )
}
