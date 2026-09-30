use super::*;
use base64::Engine as _;

impl ClientUi {
    pub(super) fn local_participant_info(&mut self) -> ParticipantInfo {
        if self.participant_id.is_empty() {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            self.participant_id = format!("{}-{nonce:x}", std::process::id());
        }
        let control_address = if self.room_mode == RoomMode::InternetTest {
            String::new()
        } else {
            self.host_addresses
                .get(self.selected_host_address)
                .map(|address| format!("{}:9001", address.ipv4))
                .unwrap_or_default()
        };
        let order = self
            .participants
            .iter()
            .find(|participant| participant.id == self.participant_id)
            .map_or(0, |participant| participant.order);
        ParticipantInfo {
            id: self.participant_id.clone(),
            display_name: self.profile_display_name.clone(),
            order,
            may_host: self.room_mode == RoomMode::Local && self.may_host,
            control_address,
            avatar_jpeg_base64: if self.room_mode == RoomMode::Local {
                self.profile_avatar_jpeg
                    .as_deref()
                    .map(|bytes| base64::engine::general_purpose::STANDARD.encode(bytes))
            } else {
                None
            },
            supports_group_screen_share: self.room_mode == RoomMode::Local,
        }
    }

    pub(super) fn update_room_roster(
        &mut self,
        participants: Vec<ParticipantInfo>,
        leader_id: String,
        room_mode: RoomMode,
    ) {
        self.room_mode = room_mode;
        self.participants = participants;
        let member_ids = self
            .participants
            .iter()
            .map(|participant| participant.id.clone())
            .collect::<HashSet<_>>();
        self.participant_avatar_textures
            .retain(|id, _| member_ids.contains(id));
        self.current_leader_id = leader_id.clone();
        self.group_available_shares
            .retain(|id| member_ids.contains(id));
        self.group_watched_shares
            .retain(|id| member_ids.contains(id));
        self.group_auto_focus_pending
            .retain(|id| member_ids.contains(id));
        self.group_outbound_sessions
            .retain(|id, _| member_ids.contains(id));
        self.group_inbound_sessions
            .retain(|id, _| member_ids.contains(id));
        self.group_outbound_ports
            .retain(|id, _| member_ids.contains(id));
        self.group_inbound_ports
            .retain(|id, _| member_ids.contains(id));
        self.group_remote_textures
            .retain(|id, _| member_ids.contains(id));
        self.group_remote_sequences
            .retain(|id, _| member_ids.contains(id));
        if self
            .focused_group_screen
            .as_ref()
            .is_some_and(|id| id != "__local" && !member_ids.contains(id))
        {
            self.focused_group_screen = None;
        }
        self.group_peer_status
            .retain(|id, _| member_ids.contains(id));
        if self.room_mode != RoomMode::Local
            && (self.group_local_sharing
                || !self.group_watched_shares.is_empty()
                || !self.group_inbound_sessions.is_empty()
                || !self.group_outbound_sessions.is_empty())
        {
            self.stop_group_media(false);
        } else if self.room_mode == RoomMode::Local
            && !self.group_sharing_compatible()
            && (self.group_local_sharing
                || !self.group_watched_shares.is_empty()
                || !self.group_inbound_sessions.is_empty()
                || !self.group_outbound_sessions.is_empty())
        {
            self.stop_group_media(true);
            self.screen_share_status = Some(
                "O compartilhamento em grupo foi encerrado: todos os participantes precisam de uma versão compatível."
                    .to_owned(),
            );
        }
        if self.participants.len() != 2 && !matches!(&self.screen_share_role, ScreenShareRole::Idle)
        {
            self.stop_screen_share(true);
            self.screen_share_status = Some(
                "O compartilhamento foi encerrado porque esta sala não tem exatamente duas pessoas."
                    .to_owned(),
            );
        }
        if self.room_mode == RoomMode::InternetTest {
            self.control_mesh = None;
            self.control_queue.clear();
            self.control_status = Some(
                "Malha TCP 9001 e sucessão automática desativadas nesta sala de Internet."
                    .to_owned(),
            );
            return;
        }
        let Some(local) = self
            .participants
            .iter()
            .find(|participant| participant.id == self.participant_id)
            .cloned()
        else {
            return;
        };
        let code = self.room_code.clone().unwrap_or_default();
        let leader_address = self
            .participants
            .iter()
            .find(|participant| participant.id == leader_id)
            .map(|participant| signaling_address_for_control(&participant.control_address))
            .unwrap_or_default();
        if let Some(mesh) = &self.control_mesh {
            mesh.update_roster(self.participants.clone(), leader_id, leader_address);
        } else if !code.is_empty() {
            match ControlMesh::start(local, code, leader_id.clone(), leader_address.clone()) {
                Ok(mesh) => {
                    mesh.update_roster(self.participants.clone(), leader_id, leader_address);
                    self.control_mesh = Some(mesh);
                    self.control_mesh_failed = false;
                    self.control_status =
                        Some("Conectando a malha direta na porta 9001…".to_owned());
                }
                Err(error) => self.control_status = Some(error),
            }
        }
        if self.group_local_sharing && self.group_sharing_compatible() {
            let _ = self.send_screen_share_signal(SignalKind::ScreenShareAvailable, String::new());
        }
    }

    pub(super) fn refresh_control_mesh(&mut self, context: &egui::Context) {
        let pending = self
            .control_mesh
            .as_ref()
            .map(|mesh| std::iter::from_fn(|| mesh.try_recv()).collect::<Vec<_>>())
            .unwrap_or_default();
        for event in pending {
            match event {
                ControlEvent::Ready => {
                    tracing::info!("Malha de controle TCP pronta");
                    self.control_mesh_failed = false;
                    self.control_status = Some("Canal de controle ativo na porta 9001.".to_owned());
                }
                ControlEvent::Error(error) => {
                    tracing::error!(error = %error, "Erro na malha de controle");
                    self.control_mesh_failed = true;
                    self.control_status = Some(error);
                }
                ControlEvent::QueueUpdated(queue) => {
                    self.control_queue = queue;
                    if self
                        .last_control_metrics_log_at
                        .is_none_or(|last| last.elapsed() >= Duration::from_secs(10))
                    {
                        let summary = self
                            .control_queue
                            .iter()
                            .map(|entry| {
                                format!(
                                    "ordem {}: perda {:.1}%, jitter {:.1} ms, latência {:.1} ms, elegível {}",
                                    entry.participant.order,
                                    entry.loss_percent,
                                    entry.jitter_ms,
                                    entry.latency_ms,
                                    entry.eligible
                                )
                            })
                            .collect::<Vec<_>>()
                            .join("; ");
                        tracing::debug!(candidates = %summary, "Resumo periódico de estabilidade da fila");
                        self.last_control_metrics_log_at = Some(Instant::now());
                    }
                }
                ControlEvent::LinksUpdated { connected, total } => {
                    if self.last_control_link_state != Some((connected, total)) {
                        tracing::info!(
                            connected,
                            total,
                            "Quantidade de enlaces de controle conectados mudou"
                        );
                        self.last_control_link_state = Some((connected, total));
                    }
                    self.control_status = Some(if total == 0 {
                        "Canal de controle ativo; aguardando outros participantes.".to_owned()
                    } else if connected < total {
                        format!(
                            "Malha direta parcial ({connected}/{total}). Confira o IPv4 escolhido e permita a porta 9001 no firewall de todos."
                        )
                    } else {
                        format!(
                            "Malha direta completa ({connected}/{total} participantes conectados)."
                        )
                    });
                }
                ControlEvent::HostUnstable { loss_percent } => {
                    tracing::warn!(
                        loss_percent,
                        "Anfitrião marcado instável; iniciando sucessão"
                    );
                    self.control_status = Some(format!(
                        "Anfitrião instável ({loss_percent:.1}% de perda). Elegendo o próximo participante elegível…"
                    ));
                }
                ControlEvent::BecomeHost {
                    code,
                    epoch,
                    participants,
                } => {
                    tracing::warn!(epoch, "Participante local eleito para hospedar a sala");
                    let participant = self.local_participant_info();
                    match SignalingClient::start_elected_host(code, participant, participants) {
                        Ok(client) => {
                            self.pending_signaling = Some(client);
                            self.pending_election_epoch = Some(epoch);
                            self.pending_election_reconnect = false;
                            self.control_status = Some("Você foi escolhido para assumir; iniciando o servidor na porta 9000…".to_owned());
                        }
                        Err(error) => {
                            tracing::error!(error = %error, epoch, "Candidato eleito não conseguiu iniciar servidor");
                            self.control_status =
                                Some(format!("Não foi possível iniciar o servidor: {error}"));
                            if let Some(mesh) = &self.control_mesh {
                                mesh.candidate_failed(epoch);
                            }
                        }
                    }
                }
                ControlEvent::LeaderChanged {
                    participant_id,
                    address,
                    epoch,
                } => {
                    tracing::warn!(
                        epoch,
                        is_local_leader = participant_id == self.participant_id,
                        "Liderança da sala mudou"
                    );
                    self.current_leader_id = participant_id.clone();
                    if participant_id == self.participant_id {
                        self.hosting_locally = true;
                        self.peer_connected = false;
                        self.control_status =
                            Some("Este computador está hospedando a sala.".to_owned());
                        continue;
                    }
                    if self.leave_after_handoff {
                        let should_close = self.close_after_transfer;
                        self.leave_room();
                        self.connection_status =
                            Some("A hospedagem foi transferida; você saiu da sala.".to_owned());
                        if should_close {
                            self.allow_window_close = true;
                            context.send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                        continue;
                    }
                    let code = self.room_code.clone().unwrap_or_default();
                    let participant = self.local_participant_info();
                    match SignalingClient::join_with_participant(address, code, participant) {
                        Ok(client) => {
                            self.pending_signaling = Some(client);
                            self.pending_election_epoch = Some(epoch);
                            self.pending_election_reconnect = true;
                            self.hosting_locally = false;
                            self.connection_status = Some(
                                "A sala mudou de anfitrião; reconectando ao novo servidor…"
                                    .to_owned(),
                            );
                        }
                        Err(error) => {
                            self.control_status =
                                Some(format!("Falha ao reconectar ao novo anfitrião: {error}"))
                        }
                    }
                }
                ControlEvent::RoomEnded => {
                    tracing::warn!("Malha de controle informou que a sala terminou");
                    let should_close = self.close_after_transfer;
                    let ended_explicitly = self.ending_room_explicitly;
                    let has_authorized_successor = self.participants.iter().any(|participant| {
                        participant.id != self.current_leader_id && participant.may_host
                    });
                    self.leave_room();
                    self.connection_status = Some(if ended_explicitly {
                        "Sala encerrada por você.".to_owned()
                    } else if has_authorized_successor {
                        "A sala foi encerrada porque o participante autorizado não conseguiu assumir a hospedagem. Confira a conexão direta pela porta TCP 9001 e se a porta TCP 9000 está livre no computador escolhido.".to_owned()
                    } else {
                        "A sala foi encerrada porque nenhum participante restante autorizou a hospedagem. Para manter a sala ativa, marque essa opção antes de entrar na próxima vez.".to_owned()
                    });
                    if should_close {
                        self.allow_window_close = true;
                        context.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                }
            }
        }
    }
}
