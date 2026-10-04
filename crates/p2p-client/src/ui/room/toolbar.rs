use crate::app::{ClientUi, RoomMode, ScreenShareRole};
use crate::screen_capture::ScreenCapture;
use eframe::egui;
use std::sync::{Arc, atomic::Ordering};

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
            let sharing = self.group_local_sharing
                || matches!(&self.screen_share_role, ScreenShareRole::Sending { .. });
            let audio_changed = ui
                .add_enabled(
                    !sharing,
                    egui::Checkbox::new(
                        &mut self.include_system_audio,
                        "Incluir som do computador",
                    ),
                )
                .changed();
            if audio_changed {
                self.save_preferences();
                tracing::info!(
                    include_system_audio = self.include_system_audio,
                    "Preferência de áudio do sistema alterada"
                );
            }
            self.show_screen_source_menu(ui);
            if self.group_sharing_compatible() {
                let can_share = self.peer_connected
                    && self.screen_capture.is_some()
                    && self.screen_picker.is_none();
                if self.group_local_sharing {
                    if ui.button("Parar compartilhamento").clicked() {
                        self.set_group_share(false);
                    }
                } else if ui
                    .add_enabled(can_share, egui::Button::new("Compartilhar minha tela"))
                    .clicked()
                {
                    self.request_screen_share(ui.ctx());
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
            } else {
                let share_role = self.screen_share_role.clone();
                match share_role {
                    ScreenShareRole::Idle => {
                        let allowed = self.participants.len() == 2
                            && self.peer_connected
                            && self.turn_configuration_ready()
                            && self.screen_capture.is_some()
                            && self.screen_share_session.is_none();
                        if ui
                            .add_enabled(allowed, egui::Button::new("Compartilhar tela"))
                            .clicked()
                        {
                            self.request_screen_share(ui.ctx());
                        }
                    }
                    ScreenShareRole::Requesting { .. } => {
                        if ui.button("Cancelar pedido").clicked() {
                            self.stop_screen_share(true);
                            self.screen_share_status =
                                Some("Pedido de compartilhamento cancelado.".to_owned());
                        }
                    }
                    ScreenShareRole::Sending { .. } | ScreenShareRole::Receiving { .. } => {
                        if ui.button("Parar compartilhamento").clicked() {
                            self.stop_screen_share(true);
                        }
                    }
                }
            }

            if matches!(&self.screen_share_role, ScreenShareRole::Sending { .. }) {
                let preview_changed = ui
                    .checkbox(&mut self.show_local_preview, "Mostrar minha prévia")
                    .changed();
                if preview_changed {
                    self.clear_local_preview();
                    self.capture_preview_enabled
                        .store(self.show_local_preview, Ordering::Relaxed);
                    tracing::info!(
                        enabled = self.show_local_preview,
                        "Prévia local de compartilhamento alterada"
                    );
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

    fn show_screen_source_menu(&mut self, ui: &mut egui::Ui) {
        let label = if let Some(capture) = &self.screen_capture {
            format!("Fonte: {}", capture.backend_name())
        } else if self.screen_picker.is_some() {
            "Selecionando tela…".to_owned()
        } else {
            "Capturar tela".to_owned()
        };
        let menu_response = ui.menu_button(label, |ui| {
            if self.screen_capture.is_some() {
                if ui.button("Parar captura da tela").clicked() {
                    self.stop_screen_capture();
                    ui.close();
                }
                return;
            }
            if self.screen_picker.is_some() {
                ui.label("Aguardando o seletor do Windows…");
                return;
            }

            ui.horizontal_wrapped(|ui| {
                ui.label(format!(
                    "Monitores encontrados: {}",
                    self.available_monitors.len()
                ));
                if ui.button("Atualizar lista de monitores").clicked() {
                    self.refresh_monitors(true);
                }
            });
            if self.available_monitors.len() == 1 {
                ui.small(
                    "O Windows disponibilizou apenas um monitor. Se esperava dois, confirme que as telas estão em modo Estender nas configurações de vídeo.",
                );
            } else if self.available_monitors.is_empty() {
                ui.small(
                    "Confira se há monitores ativos e se o Windows está configurado para Estender as telas.",
                );
            }

            if !self.available_monitors.is_empty() {
                let monitor_choices = self
                    .available_monitors
                    .iter()
                    .enumerate()
                    .map(|(index, monitor)| {
                        (
                            monitor.device_id.clone(),
                            format!("Capturar {}", monitor.label(index + 1)),
                        )
                    })
                    .collect::<Vec<_>>();
                let mut selected_device_id = None;
                for (device_id, label) in monitor_choices {
                    if ui.button(label).clicked() {
                        selected_device_id = Some(device_id);
                    }
                }
                if let Some(selected_device_id) = selected_device_id {
                    self.dxgi_capture_error = None;
                    match ScreenCapture::start_monitor(
                        &selected_device_id,
                        ui.ctx().clone(),
                        Arc::clone(&self.capture_preview_enabled),
                    ) {
                        Ok(capture) => {
                            self.clear_local_preview();
                            self.screen_capture = Some(capture);
                            self.screen_status = Some(
                                "Captura DXGI ativa; o app não adiciona a borda de captura do Windows."
                                    .to_owned(),
                            );
                            tracing::info!(monitor_device_id = %selected_device_id, "Captura DXGI iniciada pela barra da sala");
                            ui.close();
                        }
                        Err(error) => {
                            tracing::error!(error = %error, monitor_device_id = %selected_device_id, "Falha ao iniciar captura DXGI; Windows Graphics Capture está disponível como alternativa");
                            self.dxgi_capture_error = Some(error.clone());
                            self.screen_status = Some(format!(
                                "Não foi possível capturar este monitor por DXGI: {error}"
                            ));
                        }
                    }
                }
            } else {
                ui.label("Nenhum monitor DXGI disponível.");
            }
            ui.small("A captura de janela usa a borda amarela de privacidade do Windows.");
            if ui.button("Selecionar janela ou monitor pelo Windows").clicked() {
                self.select_screen(ui.ctx());
                ui.close();
            }
            if self.dxgi_capture_error.is_some() {
                ui.small("O seletor do Windows pode ser usado como alternativa.");
            }
        });
        let menu_is_open = menu_response.inner.is_some();
        if menu_is_open && !self.monitor_menu_open {
            self.refresh_monitors(true);
            ui.ctx().request_repaint();
        }
        self.monitor_menu_open = menu_is_open;
    }
}
