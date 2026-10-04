mod handoff;
mod stage;
mod toolbar;

use super::super::{ClientUi, RoomMode, ScreenShareRole, signaling_ws_url};
use crate::screen_capture::ScreenCapture;
use eframe::egui;

impl ClientUi {
    pub(super) fn show_room(&mut self, ui: &mut egui::Ui) {
        let mut participants = self.participants.clone();
        participants.sort_by_key(|participant| participant.order);

        ui.horizontal(|ui| {
            ui.heading("Sala");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(if self.peer_connected {
                    "Conectado"
                } else {
                    "Aguardando participante"
                });
                ui.label(if self.room_mode == RoomMode::InternetTest {
                    "Internet · até 2 pessoas"
                } else {
                    "Rede local / Radmin · até 8 pessoas"
                });
            });
        });

        if self.room_mode == RoomMode::InternetTest {
            Self::show_notice(
                ui,
                "Teste controlado:",
                "A sinalização usa ws:// sem criptografia ou autenticação. Use apenas com pessoas conhecidas.",
            );
        }
        if let Some(error) = &self.connection_error {
            Self::show_notice(ui, "Erro de conexão:", error);
        }
        if let Some(status) = &self.screen_share_status {
            Self::show_notice(ui, "Compartilhamento:", status);
        }
        if let Some(status) = &self.screen_status {
            Self::show_notice(ui, "Captura:", status);
        }
        match &self.screen_share_role {
            ScreenShareRole::Sending { .. } => {
                ui.label(self.screen_route_status(
                    "transmitindo a tela.",
                    "Compartilhamento aceito; negociando a conexão WebRTC.",
                ));
            }
            ScreenShareRole::Receiving { .. } => {
                ui.label(self.screen_route_status(
                    "recebendo a tela.",
                    "Compartilhamento aceito; negociando a conexão WebRTC.",
                ));
            }
            ScreenShareRole::Requesting { .. } => {
                ui.label("Pedido de compartilhamento enviado; aguardando resposta.");
            }
            ScreenShareRole::Idle => {}
        }
        if self.screen_share_metrics.decode_errors > 0
            && matches!(&self.screen_share_role, ScreenShareRole::Receiving { .. })
        {
            Self::show_notice(
                ui,
                "Aviso de vídeo:",
                &format!(
                    "{} erros H.264 nesta sessão; veja Diagnóstico.",
                    self.screen_share_metrics.decode_errors
                ),
            );
        }

        ui.add_space(8.0);
        ui.heading("Participantes");
        if participants.is_empty() {
            ui.label("Aguardando participantes…");
        } else {
            egui::ScrollArea::horizontal()
                .id_salt("room-participants")
                .max_height(48.0)
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        for participant in &participants {
                            let avatar = self.participant_avatar_texture(ui.ctx(), participant);
                            egui::Frame::group(ui.style()).show(ui, |ui| {
                                ui.set_min_width(150.0);
                                ui.horizontal(|ui| {
                                    if let Some(texture) = avatar {
                                        ui.add(
                                            egui::Image::new((
                                                texture.id(),
                                                egui::vec2(40.0, 40.0),
                                            ))
                                            .fit_to_exact_size(egui::vec2(40.0, 40.0)),
                                        );
                                    }
                                    ui.label(&participant.display_name);
                                });
                            });
                        }
                    });
                });
        }

        self.show_screen_stage(ui, &participants);

        if let Some(reason) = self
            .screen_capture
            .as_ref()
            .and_then(ScreenCapture::fallback_reason)
        {
            Self::show_notice(ui, "Fallback da captura:", &reason);
        }

        egui::CollapsingHeader::new("Detalhes da sala")
            .default_open(false)
            .show(ui, |ui| {
                ui.label(
                    self.connection_status
                        .as_deref()
                        .unwrap_or("Conectado ao servidor."),
                );
                ui.label(if self.hosting_locally {
                    "Este computador está hospedando a sala."
                } else {
                    "Você entrou na sala hospedada por outro participante."
                });

                ui.separator();
                ui.heading("Convite");
                if self.hosting_locally && self.room_mode == RoomMode::InternetTest {
                    match signaling_ws_url(&self.public_server_url) {
                        Ok(url) => {
                            ui.horizontal_wrapped(|ui| {
                                ui.monospace(&url);
                                if ui.button("Copiar endereço").clicked() {
                                    ui.ctx().copy_text(url);
                                }
                            });
                        }
                        Err(_) => Self::show_notice(
                            ui,
                            "Endereço necessário:",
                            "Configure o IPv4 público ou DDNS em Configurações > Conexão.",
                        ),
                    }
                } else if self.hosting_locally {
                    ui.label("Escolha o endereço que seu amigo consegue alcançar.");
                    self.show_host_address_picker(ui);
                } else {
                    ui.label("Use o endereço informado pelo anfitrião em Configurações > Conexão.");
                }

                ui.separator();
                ui.heading("Participantes e sucessão");
                if participants.is_empty() {
                    ui.label("Aguardando participantes…");
                } else {
                    for participant in &participants {
                        ui.label(format!(
                            "{} — ordem {}, {}",
                            participant.display_name,
                            participant.order,
                            if self.room_mode == RoomMode::InternetTest {
                                "sucessão desativada no modo Internet"
                            } else if participant.id == self.current_leader_id {
                                "anfitrião atual"
                            } else if participant.may_host {
                                "autorizado a hospedar"
                            } else {
                                "não autorizado a assumir"
                            }
                        ));
                    }
                }
                if self.room_mode == RoomMode::InternetTest {
                    ui.small("Esta sala aceita duas pessoas e termina quando o anfitrião sai ou perde a conexão.");
                } else if participants.iter().all(|participant| !participant.may_host) {
                    ui.small("Ninguém autorizou hospedagem automática; a sala termina se o anfitrião sair.");
                }
                if self.room_mode == RoomMode::Local {
                    if let Some(status) = &self.control_status {
                        ui.small(status);
                    }
                    let mut queue = self.control_queue.clone();
                    queue.sort_by(|left, right| {
                        right
                            .eligible
                            .cmp(&left.eligible)
                            .then_with(|| left.loss_percent.total_cmp(&right.loss_percent))
                            .then_with(|| left.jitter_ms.total_cmp(&right.jitter_ms))
                            .then_with(|| left.latency_ms.total_cmp(&right.latency_ms))
                            .then_with(|| left.participant.order.cmp(&right.participant.order))
                    });
                    for (index, candidate) in queue.iter().enumerate() {
                        ui.label(format!(
                            "{}. {}{}",
                            index + 1,
                            candidate.participant.display_name,
                            if candidate.eligible { "" } else { " (inelegível)" }
                        ));
                    }
                }

                if self.peer_connected && ui.button("Testar sinalização").clicked() {
                    self.diagnostic_status = Some("Enviando sinal de diagnóstico…".to_owned());
                    if let Some(signaling) = &self.signaling
                        && let Err(error) = signaling.send_diagnostic() {
                            self.diagnostic_status = Some(error);
                        }
                }
                if let Some(status) = &self.diagnostic_status {
                    ui.small(status);
                }
                if self.hosting_locally && self.room_mode == RoomMode::Local {
                    ui.separator();
                    if ui
                        .add_enabled(
                            !self.ending_room_explicitly,
                            egui::Button::new(if self.ending_room_explicitly {
                                "Encerrando sala…"
                            } else {
                                "Encerrar sala sem sucessor"
                            }),
                        )
                        .clicked()
                    {
                        self.end_room_explicitly(ui.ctx());
                    }
                }
            });
    }
}
