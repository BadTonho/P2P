use super::toolbar::{remote_audio_volume_context_menu, show_screen_audio_and_fullscreen_toolbar};
use crate::app::{ClientUi, RoomMode, ScreenShareRole, ScreenShareSession};
use eframe::egui;
use signaling_protocol::ParticipantInfo;

impl ClientUi {
    pub(super) fn show_screen_stage(
        &mut self,
        ui: &mut egui::Ui,
        participants: &[ParticipantInfo],
    ) {
        self.show_group_watch_controls(ui, participants);

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
                direct_volume_change =
                    self.show_direct_screen_stage(stage, stage_height, direct_remote_volume);
            }
        });

        if let Some(volume) = direct_volume_change {
            if let Some(session) = self.screen_share_session.as_ref() {
                session.set_remote_audio_volume_percent(volume);
            }
            self.remote_audio_default_volume_percent = volume;
        }
    }

    fn show_direct_screen_stage(
        &mut self,
        stage: &mut egui::Ui,
        stage_height: f32,
        direct_remote_volume: u8,
    ) -> Option<u8> {
        let mut direct_volume_change = None;
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
                        ui.add_space(((available_video_height - image_size.y) * 0.5).max(0.0));
                        let response = ui.add(
                            egui::Image::new((texture.id(), source))
                                .fit_to_exact_size(image_size)
                                .sense(egui::Sense::click()),
                        );
                        direct_volume_change =
                            remote_audio_volume_context_menu(response, direct_remote_volume);
                        ui.add_space(6.0);
                        let (bar_volume, toggle_fs) = show_screen_audio_and_fullscreen_toolbar(
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
                        ui.label("A imagem aparecerá aqui quando o primeiro quadro chegar.");
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
                            egui::Image::new((texture.id(), source)).fit_to_exact_size(image_size),
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
        direct_volume_change
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

    pub(in crate::app::ui) fn show_fullscreen_video(&mut self, ui: &mut egui::Ui) {
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
