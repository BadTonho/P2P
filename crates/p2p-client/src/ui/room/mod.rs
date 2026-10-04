mod handoff;
mod toolbar;

use self::toolbar::{remote_audio_volume_context_menu, show_screen_audio_and_fullscreen_toolbar};
use super::super::{ClientUi, RoomMode, ScreenShareRole, ScreenShareSession, signaling_ws_url};
use crate::screen_capture::ScreenCapture;
use eframe::egui;
use signaling_protocol::ParticipantInfo;

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

        self.show_group_watch_controls(ui, &participants);

        ui.add_space(8.0);
        let stage_width = ui.available_width();
        let stage_height = ui.available_height().clamp(280.0, 680.0);
        let direct_remote_volume = self
            .screen_share_session
            .as_ref()
            .map(ScreenShareSession::remote_audio_volume_percent)
            .unwrap_or(self.remote_audio_default_volume_percent);
        let mut direct_volume_change = None;
        egui::Frame::group(ui.style()).show(ui, |stage| {
            stage.set_min_size(egui::vec2(stage_width, stage_height));
            if self.group_sharing_compatible() {
                self.show_group_screen_stage(stage, stage_height);
            } else {
                match &self.screen_share_role {
                    ScreenShareRole::Receiving { .. } => {
                        if let Some(texture) = self.remote_screen_texture.clone() {
                            let available = stage.available_size();
                            let source = texture.size_vec2();
                            let toolbar_height = 36.0;
                            let available_video_height = (stage_height - toolbar_height).max(100.0);
                            let scale = (available.x / source.x)
                                .min(available_video_height / source.y)
                                .min(1.0);
                            let image_size = source * scale.max(0.01);
                            stage.vertical_centered(|ui| {
                                ui.add_space(
                                    ((available_video_height - image_size.y) * 0.5).max(0.0),
                                );
                                let response = ui.add(
                                    egui::Image::new((texture.id(), source))
                                        .fit_to_exact_size(image_size)
                                        .sense(egui::Sense::click()),
                                );
                                direct_volume_change = remote_audio_volume_context_menu(
                                    response,
                                    direct_remote_volume,
                                );
                                ui.add_space(6.0);
                                let (bar_volume, toggle_fs) =
                                    show_screen_audio_and_fullscreen_toolbar(
                                        ui,
                                        direct_remote_volume,
                                        self.fullscreen_video,
                                    );
                                if bar_volume.is_some() {
                                    direct_volume_change = bar_volume;
                                }
                                if toggle_fs {
                                    self.toggle_fullscreen(ui.ctx());
                                }
                            });
                        } else {
                            stage.vertical_centered(|ui| {
                                ui.add_space(stage_height * 0.4);
                                ui.heading("Conectando à tela do participante…");
                                ui.label(
                                    "A imagem aparecerá aqui quando o primeiro quadro chegar.",
                                );
                            });
                        }
                    }
                    ScreenShareRole::Sending { .. } if self.show_local_preview => {
                        if let Some(texture) = &self.screen_texture {
                            let available = stage.available_size();
                            let source = texture.size_vec2();
                            let scale = (available.x / source.x)
                                .min(available.y / source.y)
                                .min(1.0);
                            let image_size = source * scale.max(0.01);
                            stage.vertical_centered(|ui| {
                                ui.add_space(((stage_height - image_size.y) * 0.5).max(0.0));
                                ui.add(
                                    egui::Image::new((texture.id(), source))
                                        .fit_to_exact_size(image_size),
                                );
                            });
                        } else {
                            stage.vertical_centered(|ui| {
                                ui.add_space(stage_height * 0.4);
                                ui.heading("Você está compartilhando");
                                ui.label("Preparando sua prévia local…");
                            });
                        }
                    }
                    ScreenShareRole::Sending { .. } => {
                        stage.vertical_centered(|ui| {
                            ui.add_space(stage_height * 0.4);
                            ui.heading("Você está compartilhando");
                            ui.label("Prévia local desativada para economizar recursos.");
                        });
                    }
                    ScreenShareRole::Requesting { .. } => {
                        stage.vertical_centered(|ui| {
                            ui.add_space(stage_height * 0.4);
                            ui.heading("Solicitação enviada");
                            ui.label("Aguardando resposta do outro participante…");
                        });
                    }
                    ScreenShareRole::Idle if self.peer_connected => {
                        stage.vertical_centered(|ui| {
                            ui.add_space(stage_height * 0.4);
                            ui.heading("Sala pronta");
                            ui.label("Compartilhe sua tela para começar.");
                        });
                    }
                    ScreenShareRole::Idle => {
                        stage.vertical_centered(|ui| {
                            ui.add_space(stage_height * 0.4);
                            ui.heading("Aguardando seu amigo");
                            ui.label("A tela compartilhada aparecerá aqui.");
                        });
                    }
                }
            }
        });

        if let Some(volume) = direct_volume_change {
            if let Some(session) = self.screen_share_session.as_ref() {
                session.set_remote_audio_volume_percent(volume);
            }
            self.remote_audio_default_volume_percent = volume;
        }

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

    fn show_group_watch_controls(&mut self, ui: &mut egui::Ui, participants: &[ParticipantInfo]) {
        if !self.group_sharing_compatible() {
            if self.group_sharing_upgrade_required() {
                Self::show_notice(
                    ui,
                    "Compartilhamento em grupo indisponível:",
                    "Todos os participantes precisam da versão 1.1.3 para correlacionar cada transmissão com segurança.",
                );
            } else if self.room_mode == RoomMode::Local && participants.len() > 2 {
                Self::show_notice(
                    ui,
                    "Compartilhamento em grupo indisponível:",
                    "Todos precisam usar uma versão que ofereça suporte ao compartilhamento em grupo.",
                );
            }
            return;
        }
        let mut peers = self
            .group_available_shares
            .iter()
            .filter_map(|id| participants.iter().find(|p| &p.id == id))
            .cloned()
            .collect::<Vec<_>>();
        peers.sort_by_key(|participant| participant.order);
        if peers.is_empty() && !self.group_local_sharing {
            return;
        }
        ui.group(|ui| {
            ui.heading("Telas compartilhadas");
            ui.horizontal_wrapped(|ui| {
                for peer in peers {
                    let watching = self.group_watched_shares.contains(&peer.id);
                    let label = if watching {
                        format!("Parar de assistir {}", peer.display_name)
                    } else {
                        format!("Assistir {}", peer.display_name)
                    };
                    if ui.button(label).clicked() {
                        self.toggle_group_watch(&peer.id);
                    }
                }
                if self.group_local_sharing {
                    ui.label("Sua tela está disponível");
                }
            });
            for (peer_id, status) in &self.group_audio_status {
                let participant_name = participants
                    .iter()
                    .find(|participant| participant.id == *peer_id)
                    .map(|participant| participant.display_name.as_str())
                    .unwrap_or("participante");
                ui.small(format!("Áudio de {participant_name}: {status}"));
            }
        });
    }

    fn show_group_screen_stage(&mut self, ui: &mut egui::Ui, stage_height: f32) {
        let focused = self.focused_group_screen.clone();
        if let Some(id) = focused.as_deref() {
            if let Some(texture) = self.group_remote_textures.get(id).cloned() {
                let current_volume = self
                    .group_inbound_sessions
                    .get(id)
                    .map(ScreenShareSession::remote_audio_volume_percent)
                    .unwrap_or(self.remote_audio_default_volume_percent);
                let mut volume_change = None;
                let source = texture.size_vec2();
                let available = ui.available_size();
                let toolbar_height = 36.0;
                let available_video_height = (stage_height - toolbar_height).max(100.0);
                let scale = (available.x / source.x)
                    .min(available_video_height / source.y)
                    .min(1.0);
                ui.vertical_centered(|ui| {
                    ui.add_space(
                        ((available_video_height - source.y * scale.max(0.01)) * 0.5).max(0.0),
                    );
                    let response = ui.add(
                        egui::Image::new((texture.id(), source))
                            .fit_to_exact_size(source * scale.max(0.01))
                            .sense(egui::Sense::click()),
                    );
                    volume_change = remote_audio_volume_context_menu(response, current_volume);
                    ui.add_space(6.0);
                    let (bar_volume, toggle_fs) = show_screen_audio_and_fullscreen_toolbar(
                        ui,
                        current_volume,
                        self.fullscreen_video,
                    );
                    if bar_volume.is_some() {
                        volume_change = bar_volume;
                    }
                    if toggle_fs {
                        self.toggle_fullscreen(ui.ctx());
                    }
                    if ui.button("Voltar à grade").clicked() {
                        self.focused_group_screen = None;
                    }
                });
                if let Some(volume) = volume_change {
                    if let Some(session) = self.group_inbound_sessions.get(id) {
                        session.set_remote_audio_volume_percent(volume);
                    }
                    self.remote_audio_default_volume_percent = volume;
                }
                return;
            }
            if id == "__local"
                && let Some(texture) = self
                    .screen_texture
                    .as_ref()
                    .filter(|_| self.show_local_preview)
            {
                let source = texture.size_vec2();
                let available = ui.available_size();
                let scale = (available.x / source.x)
                    .min(available.y / source.y)
                    .min(1.0);
                ui.vertical_centered(|ui| {
                    ui.add_space(((stage_height - source.y * scale.max(0.01)) * 0.5).max(0.0));
                    ui.add(
                        egui::Image::new((texture.id(), source))
                            .fit_to_exact_size(source * scale.max(0.01)),
                    );
                    if ui.button("Voltar à grade").clicked() {
                        self.focused_group_screen = None;
                    }
                });
                return;
            }
            self.focused_group_screen = None;
        }
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                if self.group_local_sharing && self.show_local_preview {
                    egui::Frame::group(ui.style()).show(ui, |tile| {
                        tile.set_min_size(egui::vec2(300.0, 195.0));
                        tile.label("Você (prévia local)");
                        if let Some(texture) = &self.screen_texture {
                            tile.add(
                                egui::Image::new((texture.id(), texture.size_vec2()))
                                    .fit_to_exact_size(egui::vec2(280.0, 158.0)),
                            );
                        } else {
                            tile.label("Preparando prévia…");
                        }
                        if tile.button("Ampliar").clicked() {
                            self.focused_group_screen = Some("__local".to_owned());
                        }
                    });
                }
                let watched = self
                    .group_watched_shares
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>();
                for peer_id in watched {
                    let name = self
                        .participants
                        .iter()
                        .find(|p| p.id == peer_id)
                        .map(|p| p.display_name.clone())
                        .unwrap_or_else(|| "Participante".to_owned());
                    let current_volume = self
                        .group_inbound_sessions
                        .get(&peer_id)
                        .map(ScreenShareSession::remote_audio_volume_percent)
                        .unwrap_or(self.remote_audio_default_volume_percent);
                    let mut volume_change = None;
                    egui::Frame::group(ui.style()).show(ui, |tile| {
                        tile.set_min_size(egui::vec2(300.0, 195.0));
                        tile.label(name);
                        if let Some(texture) = self.group_remote_textures.get(&peer_id) {
                            let response = tile.add(
                                egui::Image::new((texture.id(), texture.size_vec2()))
                                    .fit_to_exact_size(egui::vec2(280.0, 158.0))
                                    .sense(egui::Sense::click()),
                            );
                            volume_change =
                                remote_audio_volume_context_menu(response, current_volume);
                            if tile.button("Ampliar").clicked() {
                                self.focused_group_screen = Some(peer_id.clone());
                            }
                        } else {
                            tile.label(
                                self.group_peer_status
                                    .get(&peer_id)
                                    .map(String::as_str)
                                    .unwrap_or("Aguardando a tela…"),
                            );
                        }
                    });
                    if let Some(volume) = volume_change {
                        if let Some(session) = self.group_inbound_sessions.get(&peer_id) {
                            session.set_remote_audio_volume_percent(volume);
                        }
                        self.remote_audio_default_volume_percent = volume;
                    }
                }
                if !self.group_local_sharing && self.group_watched_shares.is_empty() {
                    ui.vertical_centered(|ui| {
                        ui.add_space(stage_height * 0.35);
                        ui.heading("Sala pronta");
                        ui.label("Escolha uma transmissão e pressione Assistir.");
                    });
                }
            });
        });
    }

    pub(super) fn show_fullscreen_video(&mut self, ui: &mut egui::Ui) {
        let available = ui.available_size();
        let toolbar_height = 42.0;
        let available_video_height = (available.y - toolbar_height).max(100.0);

        let (texture_opt, current_volume, is_group, group_id) =
            if let Some(id) = self.focused_group_screen.clone() {
                let tex = self.group_remote_textures.get(&id).cloned();
                let vol = self
                    .group_inbound_sessions
                    .get(&id)
                    .map(ScreenShareSession::remote_audio_volume_percent)
                    .unwrap_or(self.remote_audio_default_volume_percent);
                (tex, vol, true, Some(id))
            } else if let Some(texture) = &self.remote_screen_texture {
                let vol = self
                    .screen_share_session
                    .as_ref()
                    .map(ScreenShareSession::remote_audio_volume_percent)
                    .unwrap_or(self.remote_audio_default_volume_percent);
                (Some(texture.clone()), vol, false, None)
            } else {
                self.set_fullscreen(ui.ctx(), false);
                return;
            };

        if let Some(texture) = texture_opt {
            let source = texture.size_vec2();
            let scale = (available.x / source.x).min(available_video_height / source.y);
            let image_size = source * scale.max(0.01);

            ui.vertical_centered(|ui| {
                ui.add_space(((available_video_height - image_size.y) * 0.5).max(0.0));
                ui.add(egui::Image::new((texture.id(), source)).fit_to_exact_size(image_size));

                ui.add_space(8.0);
                let (new_vol, toggle_fs) =
                    show_screen_audio_and_fullscreen_toolbar(ui, current_volume, true);
                if let Some(vol) = new_vol {
                    if let Some(ref gid) = group_id {
                        if let Some(session) = self.group_inbound_sessions.get(gid) {
                            session.set_remote_audio_volume_percent(vol);
                        }
                    } else if let Some(session) = &self.screen_share_session {
                        session.set_remote_audio_volume_percent(vol);
                    }
                    self.remote_audio_default_volume_percent = vol;
                }
                if toggle_fs {
                    self.set_fullscreen(ui.ctx(), false);
                }
                if is_group && ui.button("Voltar à grade").clicked() {
                    self.focused_group_screen = None;
                    self.set_fullscreen(ui.ctx(), false);
                }
            });
        } else {
            self.set_fullscreen(ui.ctx(), false);
        }
    }
}
