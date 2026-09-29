use super::super::{ClientUi, RoomMode, ScreenShareRole, signaling_ws_url};
use crate::screen_capture::ScreenCapture;
use eframe::egui;
use signaling_protocol::ParticipantInfo;
use std::sync::{Arc, atomic::Ordering};

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

        egui::CollapsingHeader::new("Detalhes da sala")
            .default_open(false)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(format!("Código: {}", self.room_code.as_deref().unwrap_or("")));
                    if ui.button("Copiar código").clicked() {
                        if let Some(code) = &self.room_code {
                            ui.ctx().copy_text(code.clone());
                            self.code_copied = true;
                        }
                    }
                });
                if self.code_copied {
                    ui.small("Código copiado para a área de transferência.");
                }
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
                    if let Some(signaling) = &self.signaling {
                        if let Err(error) = signaling.send_diagnostic() {
                            self.diagnostic_status = Some(error);
                        }
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
                            egui::Frame::group(ui.style()).show(ui, |ui| {
                                ui.set_min_width(150.0);
                                ui.label(&participant.display_name);
                            });
                        }
                    });
                });
        }

        self.show_group_watch_controls(ui, &participants);

        ui.add_space(8.0);
        let stage_width = ui.available_width();
        let stage_height = ui.available_height().clamp(280.0, 680.0);
        egui::Frame::group(ui.style()).show(ui, |stage| {
            stage.set_min_size(egui::vec2(stage_width, stage_height));
            if self.group_sharing_compatible() {
                self.show_group_screen_stage(stage, stage_height);
            } else {
                match &self.screen_share_role {
                    ScreenShareRole::Receiving { .. } => {
                        if let Some(texture) = &self.remote_screen_texture {
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

        if let Some(reason) = self
            .screen_capture
            .as_ref()
            .and_then(ScreenCapture::fallback_reason)
        {
            Self::show_notice(ui, "Fallback da captura:", &reason);
        }
    }

    pub(super) fn show_room_toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
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
                let selected = self
                    .available_monitors
                    .iter()
                    .position(|monitor| {
                        self.selected_monitor_id.as_deref() == Some(monitor.device_id.as_str())
                    })
                    .unwrap_or(0);
                let selected_monitor = &self.available_monitors[selected];
                egui::ComboBox::from_id_salt("room-monitor-source")
                    .selected_text(selected_monitor.label(selected + 1))
                    .show_ui(ui, |ui| {
                        for (index, monitor) in self.available_monitors.iter().enumerate() {
                            ui.selectable_value(
                                &mut self.selected_monitor_id,
                                Some(monitor.device_id.clone()),
                                monitor.label(index + 1),
                            );
                        }
                    });
                if ui.button("Capturar monitor por DXGI").clicked() {
                    self.dxgi_capture_error = None;
                    let selected_device_id = self
                        .selected_monitor_id
                        .as_deref()
                        .unwrap_or(&selected_monitor.device_id)
                        .to_owned();
                    match ScreenCapture::start_monitor(
                        &selected_device_id,
                        ui.ctx().clone(),
                        Arc::clone(&self.capture_preview_enabled),
                    ) {
                        Ok(capture) => {
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

    fn show_group_watch_controls(&mut self, ui: &mut egui::Ui, participants: &[ParticipantInfo]) {
        if !self.group_sharing_compatible() {
            if self.room_mode == RoomMode::Local && participants.len() > 2 {
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
        });
    }

    fn show_group_screen_stage(&mut self, ui: &mut egui::Ui, stage_height: f32) {
        let focused = self.focused_group_screen.clone();
        if let Some(id) = focused.as_deref() {
            if let Some(texture) = self.group_remote_textures.get(id) {
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
            if id == "__local" {
                if let Some(texture) = self
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
                    egui::Frame::group(ui.style()).show(ui, |tile| {
                        tile.set_min_size(egui::vec2(300.0, 195.0));
                        tile.label(name);
                        if let Some(texture) = self.group_remote_textures.get(&peer_id) {
                            tile.add(
                                egui::Image::new((texture.id(), texture.size_vec2()))
                                    .fit_to_exact_size(egui::vec2(280.0, 158.0)),
                            );
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

    pub(super) fn show_handoff_panel(&mut self, ui: &mut egui::Ui, context: &egui::Context) {
        if let Some((code, _)) = &self.incoming_transfer {
            let code = code.clone();
            let accepting = self.pending_signaling.is_some();
            ui.add_space(10.0);
            ui.group(|ui| {
                ui.heading("Pedido para assumir a hospedagem");
                ui.label(format!("O anfitrião quer sair da sala {code} e está pedindo que você inicie o servidor neste computador."));
                if accepting {
                    ui.label("Iniciando o novo servidor e aguardando confirmação…");
                } else {
                    ui.horizontal(|ui| {
                        if ui.button("Aceitar e assumir").clicked() {
                            self.accept_incoming_transfer();
                        }
                        if ui.button("Recusar").clicked() {
                            self.reject_incoming_transfer();
                            if self.close_after_transfer {
                                self.leave_room();
                                self.allow_window_close = true;
                                context.send_viewport_cmd(egui::ViewportCommand::Close);
                            } else {
                                self.handoff_error = Some("Transferência recusada; você continua na sala.".to_owned());
                            }
                        }
                    });
                }
            });
        }

        if self.outgoing_transfer.is_some() {
            ui.add_space(10.0);
            ui.group(|ui| {
                ui.heading("Transferindo a hospedagem");
                ui.label("A sala continua ativa neste computador enquanto o outro participante aceita e inicia o novo servidor.");
                ui.horizontal(|ui| {
                    let can_cancel = self.outgoing_transfer.as_ref().is_some_and(|(_, token)| !token.is_empty());
                    if ui.add_enabled(can_cancel, egui::Button::new("Cancelar transferência")).clicked() {
                        if let (Some(signaling), Some((_, token))) = (&self.signaling, &self.outgoing_transfer) {
                            if let Err(error) = signaling.cancel_host_transfer(token.clone()) {
                                self.handoff_error = Some(error);
                            }
                        }
                    }
                    if ui.button("Encerrar sala").clicked() {
                        self.end_room_explicitly(context);
                    }
                });
            });
        }

        if let Some(error) = self.handoff_error.clone() {
            ui.add_space(10.0);
            ui.group(|ui| {
                Self::show_notice(ui, "Transferência:", &error);
                ui.horizontal(|ui| {
                    if self.hosting_locally
                        && self.peer_connected
                        && ui.button("Tentar novamente").clicked()
                    {
                        self.request_host_transfer();
                    }
                    if ui
                        .button(if self.close_after_transfer {
                            "Cancelar fechamento e ficar"
                        } else {
                            "Continuar na sala"
                        })
                        .clicked()
                    {
                        if let (Some(signaling), Some((_, token))) =
                            (&self.signaling, &self.outgoing_transfer)
                        {
                            let _ = signaling.cancel_host_transfer(token.clone());
                        }
                        self.outgoing_transfer = None;
                        self.handoff_error = None;
                        self.close_after_transfer = false;
                    }
                    if self.hosting_locally
                        && ui
                            .button(if self.close_after_transfer {
                                "Encerrar sala e fechar"
                            } else {
                                "Encerrar sala e sair"
                            })
                            .clicked()
                    {
                        self.end_room_explicitly(context);
                    }
                });
            });
        }
    }
}
