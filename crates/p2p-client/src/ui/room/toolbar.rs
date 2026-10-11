use crate::app::{ClientUi, RoomMode, ScreenShareRole};
use eframe::egui;
use std::sync::atomic::Ordering;

pub(super) fn remote_audio_volume_context_menu(
    response: egui::Response,
    current_percent: u8,
) -> Option<u8> {
    let mut changed_volume = None;
    response.context_menu(|ui| {
        ui.label("Volume da transmissão");
        let mut percent = f32::from(current_percent);
        if ui
            .add(
                egui::Slider::new(&mut percent, 0.0..=100.0)
                    .integer()
                    .suffix("%"),
            )
            .changed()
        {
            changed_volume = Some(percent.round().clamp(0.0, 100.0) as u8);
        }
    });
    changed_volume
}

pub(super) fn show_screen_audio_and_fullscreen_toolbar(
    ui: &mut egui::Ui,
    current_volume: u8,
    is_fullscreen: bool,
) -> (Option<u8>, bool) {
    let mut volume_change = None;
    let mut toggle_fullscreen = false;

    egui::Frame::new()
        .fill(egui::Color32::from_rgba_premultiplied(20, 22, 28, 225))
        .corner_radius(egui::CornerRadius::same(6))
        .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(46, 51, 64)))
        .inner_margin(egui::Margin::symmetric(10, 5))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;

                let volume_icon = if current_volume == 0 {
                    "🔇"
                } else if current_volume < 40 {
                    "🔉"
                } else {
                    "🔊"
                };

                if ui
                    .button(volume_icon)
                    .on_hover_text(if current_volume == 0 {
                        "Ativar áudio da transmissão"
                    } else {
                        "Silenciar áudio da transmissão"
                    })
                    .clicked()
                {
                    if current_volume > 0 {
                        volume_change = Some(0);
                    } else {
                        volume_change = Some(100);
                    }
                }

                ui.label("Volume:");
                let mut percent = f32::from(current_volume);
                if ui
                    .add(
                        egui::Slider::new(&mut percent, 0.0..=100.0)
                            .integer()
                            .suffix("%"),
                    )
                    .changed()
                {
                    volume_change = Some(percent.round().clamp(0.0, 100.0) as u8);
                }

                ui.separator();
                let fs_text = if is_fullscreen {
                    "🗗 Sair da tela cheia (F11)"
                } else {
                    "⛶ Tela cheia (F11)"
                };
                if ui.button(fs_text).clicked() {
                    toggle_fullscreen = true;
                }
            });
        });

    (volume_change, toggle_fullscreen)
}

impl ClientUi {
    pub(in crate::app::ui) fn show_room_toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            let is_sharing = self.group_local_sharing
                || matches!(&self.screen_share_role, ScreenShareRole::Sending { .. });

            if is_sharing {
                if ui
                    .button(egui::RichText::new("Parar compartilhamento").strong())
                    .clicked()
                {
                    self.stop_screen_capture();
                }

                let preview_changed = ui
                    .checkbox(&mut self.show_local_preview, "Mostrar minha prévia")
                    .changed();
                if preview_changed {
                    self.clear_local_preview();
                    self.capture_preview_enabled
                        .store(self.show_local_preview, Ordering::Relaxed);
                    self.save_preferences();
                }
            } else if matches!(&self.screen_share_role, ScreenShareRole::Requesting { .. }) {
                if ui.button("Cancelar pedido de tela").clicked() {
                    self.stop_screen_share(true);
                    self.screen_share_status =
                        Some("Pedido de compartilhamento cancelado.".to_owned());
                }
            } else {
                let can_share = if self.group_sharing_compatible() {
                    self.peer_connected || self.hosting_locally
                } else {
                    self.participants.len() == 2
                        && self.peer_connected
                        && self.turn_configuration_ready()
                        && self.screen_share_session.is_none()
                };

                let share_btn =
                    egui::Button::new(egui::RichText::new("Compartilhar tela").size(13.0).strong());
                if ui.add_enabled(can_share, share_btn).clicked() {
                    self.open_screen_share_modal();
                }
            }

            let label =
                if self.hosting_locally && self.peer_connected && self.room_mode == RoomMode::Local
                {
                    "Sair e transferir"
                } else if self.hosting_locally
                    && self.peer_connected
                    && self.room_mode == RoomMode::InternetTest
                {
                    "Encerrar sala"
                } else {
                    "Sair da sala"
                };
            if ui
                .add_enabled(
                    self.screen_picker.is_none() && self.outgoing_transfer.is_none(),
                    egui::Button::new(label),
                )
                .clicked()
            {
                self.request_leave(ui.ctx());
            }
        });
        if let Some(status) = self.audio_status.as_deref() {
            ui.small(status);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_toolbar_renders_share_screen_when_idle() {
        let context = egui::Context::default();
        let mut app = ClientUi::default();
        app.peer_connected = true;

        let mut output = context.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(800.0, 100.0),
                )),
                ..Default::default()
            },
            |ui| {
                app.show_room_toolbar(ui);
            },
        );
        output.textures_delta.clear();

        assert!(!app.group_local_sharing);
    }

    #[test]
    fn test_toolbar_renders_stop_sharing_when_sending() {
        let context = egui::Context::default();
        let mut app = ClientUi::default();
        app.group_local_sharing = true;

        let mut output = context.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(800.0, 100.0),
                )),
                ..Default::default()
            },
            |ui| {
                app.show_room_toolbar(ui);
            },
        );
        output.textures_delta.clear();

        assert!(app.group_local_sharing);
    }
}
