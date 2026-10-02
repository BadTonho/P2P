use super::*;

impl ClientUi {
    pub(super) fn enter_room(&mut self, code: String) {
        self.room_code = Some(code);
        self.code_copied_until = None;
        self.handoff_error = None;
        self.microphone_error = None;
        self.connection_error = None;
        self.screen_status = None;
        self.screen_share_status = None;
    }

    pub(super) fn start_hosting(&mut self) {
        if self.signaling.is_some() || self.connecting || self.update_blocks_room_actions() {
            return;
        }
        tracing::info!(room_mode = ?self.create_room_mode, "Iniciando criação de sala");
        self.turn_server = None;
        self.turn_room_config = None;
        self.turn_config_received = false;
        self.turn_config_wait_started_at = None;
        if self.create_room_mode == RoomMode::InternetTest {
            if let Err(error) = signaling_ws_url(&self.public_server_url) {
                tracing::error!(reason = %error, "Configuração do endereço Internet inválida para criar sala");
                self.connection_error = Some(format!(
                    "Configure um IPv4 público ou nome DDNS válido em Configurações > Conexão: {error}"
                ));
                return;
            }
            if let Err(error) = screen_sharing::validate_stun_uri(&self.stun_server_url) {
                tracing::error!(reason = %error, "Configuração STUN inválida para criar sala");
                self.connection_error = Some(error);
                return;
            }
            tracing::info!(
                signaling_endpoint = %safe_signaling_endpoint(&self.public_server_url),
                stun_endpoint = %safe_stun_endpoint(&self.stun_server_url),
                "Configuração de rede do teste Internet validada"
            );
            if self.use_turn_on_create {
                let external_ipv4 = match resolve_public_ipv4(&self.public_server_url) {
                    Ok(address) => address,
                    Err(error) => {
                        self.connection_error = Some(format!(
                            "Não foi possível configurar o TURN. Confira o IPv4 público ou DDNS em Configurações > Conexão: {error}"
                        ));
                        return;
                    }
                };
                match TurnRelayServer::start(external_ipv4) {
                    Ok((server, credentials)) => {
                        tracing::info!(
                            "TURN local pronto em UDP 3478; credenciais temporárias omitidas"
                        );
                        self.turn_server = Some(server);
                        self.turn_room_config = Some(TurnRoomConfig {
                            turn: Some(credentials),
                        });
                        self.turn_config_received = true;
                    }
                    Err(error) => {
                        tracing::error!(error = %error, "Não foi possível iniciar o TURN local");
                        self.connection_error = Some(error);
                        return;
                    }
                }
            } else {
                self.turn_room_config = Some(TurnRoomConfig { turn: None });
                self.turn_config_received = true;
            }
        }
        if self.create_room_mode == RoomMode::Local {
            tracing::info!(
                signaling_endpoint = ?self.selected_signaling_address(),
                "Endereço local escolhido para anunciar a sala"
            );
        }
        self.refresh_host_addresses();
        self.room_mode = self.create_room_mode;
        self.connection_error = None;
        self.connection_status = Some("Iniciando servidor de sala neste computador…".to_owned());
        self.connecting = true;
        self.starting_host = true;
        self.peer_connected = false;
        self.diagnostic_status = None;
        self.room_code = None;
        self.hosting_locally = false;

        let participant = self.local_participant_info();
        match SignalingClient::start_host_with_participant(participant, self.room_mode) {
            Ok(client) => self.signaling = Some(client),
            Err(error) => {
                tracing::error!(error = %error, "Falha ao iniciar sala hospedada localmente");
                self.connecting = false;
                self.starting_host = false;
                self.connection_status = Some("Desconectado.".to_owned());
                self.connection_error = Some(error);
                self.turn_server = None;
                self.turn_room_config = None;
                self.turn_config_received = false;
            }
        }
    }

    pub(super) fn start_join(&mut self, code: String) {
        if self.signaling.is_some() || self.connecting || self.update_blocks_room_actions() {
            return;
        }
        let server_url = match signaling_ws_url(self.join_address()) {
            Ok(url) => url,
            Err(error) => {
                tracing::error!(reason = %error, "Endereço do anfitrião inválido ao entrar em sala");
                self.connection_error = Some(format!(
                    "Escolha um anfitrião salvo ou informe o endereço manual na tela inicial: {error}"
                ));
                return;
            }
        };
        tracing::info!(endpoint = %server_url, "Conectando para entrar em sala; código omitido");
        self.turn_server = None;
        self.turn_room_config = None;
        self.turn_config_received = false;
        self.turn_config_wait_started_at = Some(Instant::now());
        self.room_mode = RoomMode::Local;
        self.connection_error = None;
        self.connection_status = Some("Conectando ao anfitrião…".to_owned());
        self.connecting = true;
        self.starting_host = false;
        self.hosting_locally = false;
        self.peer_connected = false;
        self.diagnostic_status = None;
        self.room_code = None;

        let participant = self.local_participant_info();
        match SignalingClient::join_with_participant(server_url, code, participant) {
            Ok(client) => self.signaling = Some(client),
            Err(error) => {
                tracing::error!(error = %error, "Falha ao iniciar a conexão de entrada na sala");
                self.connecting = false;
                self.connection_status = Some("Desconectado.".to_owned());
                self.connection_error = Some(error);
            }
        }
    }

    pub(super) fn refresh_signaling(&mut self, context: &egui::Context) {
        let pending_events = self
            .pending_signaling
            .as_ref()
            .map(|client| std::iter::from_fn(|| client.try_recv()).collect::<Vec<_>>())
            .unwrap_or_default();
        for event in pending_events {
            match event {
                SignalingEvent::RoomAdopted(code) => {
                    tracing::info!(
                        "Servidor informou que a sala transferida foi adotada; código omitido"
                    );
                    if let Some(epoch) = self.pending_election_epoch.take() {
                        self.pending_room_adopted = true;
                        self.signaling = self.pending_signaling.take();
                        self.pending_election_reconnect = false;
                        self.hosting_locally = true;
                        self.peer_connected = false;
                        self.enter_room(code);
                        if let Some(address) = self.selected_signaling_address() {
                            if let Some(mesh) = &self.control_mesh {
                                mesh.publish_leader(address, epoch);
                            }
                            self.control_status = Some(
                                "Servidor pronto; anunciando o novo anfitrião aos participantes."
                                    .to_owned(),
                            );
                        } else {
                            self.control_status = Some(
                                "Servidor iniciado, mas nenhum IPv4 pode ser anunciado.".to_owned(),
                            );
                            if let Some(mesh) = &self.control_mesh {
                                mesh.candidate_failed(epoch);
                            }
                        }
                        continue;
                    }
                    self.pending_room_adopted = true;
                    if let (Some(signaling), Some((_, token))) =
                        (&self.signaling, &self.incoming_transfer)
                    {
                        if let Err(error) = signaling.confirm_host_transfer(code, token.clone()) {
                            self.handoff_error = Some(error);
                            self.pending_signaling = None;
                            self.pending_room_adopted = false;
                        } else {
                            self.connection_status = Some(
                                "Novo servidor pronto; confirmando a transferência…".to_owned(),
                            );
                        }
                    }
                }
                SignalingEvent::RoomJoined(code) if self.pending_election_reconnect => {
                    tracing::info!(
                        "Reconexão à sala concluída após eleição de anfitrião; código omitido"
                    );
                    self.signaling = self.pending_signaling.take();
                    self.pending_election_reconnect = false;
                    self.pending_election_epoch = None;
                    self.pending_room_adopted = false;
                    self.hosting_locally = false;
                    self.peer_connected = true;
                    self.enter_room(code);
                    self.connection_status = Some(
                        "Reconectado ao novo anfitrião; a identidade e a ordem foram mantidas."
                            .to_owned(),
                    );
                }
                SignalingEvent::Error(error) | SignalingEvent::ServerError(error) => {
                    tracing::error!(error = %error, "Erro recebido durante conexão ou transferência de sala");
                    if let Some(epoch) = self.pending_election_epoch.take() {
                        if !self.pending_election_reconnect {
                            if let Some(mesh) = &self.control_mesh {
                                mesh.candidate_failed(epoch);
                            }
                        }
                        self.pending_signaling = None;
                        self.pending_election_reconnect = false;
                        self.control_status = Some(format!(
                            "A tentativa de mudança de anfitrião falhou: {error}"
                        ));
                        continue;
                    }
                    if let (Some(signaling), Some((_, token))) =
                        (&self.signaling, &self.incoming_transfer)
                    {
                        let _ = signaling.reject_host_transfer(token.clone());
                    }
                    self.pending_signaling = None;
                    self.pending_room_adopted = false;
                    self.handoff_error =
                        Some(format!("Não foi possível assumir a hospedagem: {error}"));
                }
                SignalingEvent::Disconnected => {
                    tracing::warn!("Conexão pendente de sala foi desconectada");
                    if let Some(epoch) = self.pending_election_epoch.take() {
                        if !self.pending_election_reconnect {
                            if let Some(mesh) = &self.control_mesh {
                                mesh.candidate_failed(epoch);
                            }
                        }
                        self.pending_signaling = None;
                        self.pending_election_reconnect = false;
                        self.control_status = Some(
                            "A tentativa de mudança de anfitrião foi desconectada.".to_owned(),
                        );
                        continue;
                    }
                    self.pending_signaling = None;
                    self.pending_room_adopted = false;
                    self.handoff_error =
                        Some("A tentativa de iniciar o novo servidor foi encerrada.".to_owned());
                }
                _ => {}
            }
        }

        let events = self
            .signaling
            .as_ref()
            .map(|client| std::iter::from_fn(|| client.try_recv()).collect::<Vec<_>>())
            .unwrap_or_default();
        let mut disconnect = false;
        let mut acknowledge_diagnostic = false;
        for event in events {
            match event {
                SignalingEvent::RoomCreated(code) => {
                    tracing::info!(room_mode = ?self.room_mode, "Sala criada no servidor integrado; código omitido");
                    self.connecting = false;
                    self.starting_host = false;
                    self.hosting_locally = true;
                    self.connection_status =
                        Some("Sala hospedada neste computador; aguardando seu amigo.".to_owned());
                    self.enter_room(code);
                }
                SignalingEvent::RoomJoined(code) => {
                    tracing::info!("Entrada na sala concluída; código omitido");
                    self.connecting = false;
                    self.peer_connected = true;
                    self.connection_status = Some("Você entrou na sala do anfitrião.".to_owned());
                    self.enter_room(code);
                }
                SignalingEvent::RoomAdopted(_) => {
                    tracing::info!("Sala adotada pelo participante local após transferência");
                    self.room_mode = RoomMode::Local;
                }
                SignalingEvent::PeerJoined => {
                    tracing::info!("Outro participante entrou na sala");
                    self.connecting = false;
                    self.peer_connected = true;
                    self.connection_status = Some("Seu amigo entrou na sala.".to_owned());
                    if self.hosting_locally && self.room_mode == RoomMode::InternetTest {
                        self.send_turn_room_config();
                    }
                }
                SignalingEvent::PeerLeft => {
                    tracing::warn!("Outro participante saiu ou desconectou da sala");
                    self.stop_screen_share(false);
                    self.peer_connected = false;
                    self.connection_status =
                        Some("Seu amigo desconectou; a sala aguarda outra conexão.".to_owned());
                    self.diagnostic_status = None;
                    if self.outgoing_transfer.is_some() {
                        self.outgoing_transfer = None;
                        self.handoff_error = Some(
                            "O outro participante desconectou antes da transferência.".to_owned(),
                        );
                    }
                }
                SignalingEvent::RoomRoster {
                    participants,
                    leader_id,
                    room_mode,
                } => {
                    tracing::info!(participants = participants.len(), room_mode = ?room_mode, "Lista de participantes atualizada");
                    self.update_room_roster(participants, leader_id, room_mode);
                }
                SignalingEvent::HostTransferPending { code, token } => {
                    tracing::info!(
                        "Transferência de hospedagem solicitada; códigos e tokens omitidos"
                    );
                    self.outgoing_transfer = Some((code, token));
                    self.handoff_error = None;
                }
                SignalingEvent::HostTransferRequested { code, token } => {
                    tracing::info!("Pedido de transferência recebido; códigos e tokens omitidos");
                    self.incoming_transfer = Some((code, token));
                    self.handoff_error = None;
                }
                SignalingEvent::HostTransferComplete(code) => {
                    tracing::info!("Transferência de hospedagem concluída; código omitido");
                    if self.hosting_locally {
                        let should_close = self.close_after_transfer;
                        self.leave_room();
                        self.connection_status =
                            Some(format!("A hospedagem da sala {code} foi transferida."));
                        if should_close {
                            self.allow_window_close = true;
                            context.send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                    } else if self.pending_signaling.is_some() && self.pending_room_adopted {
                        self.signaling = None;
                        self.signaling = self.pending_signaling.take();
                        self.pending_room_adopted = false;
                        self.incoming_transfer = None;
                        self.handoff_error = None;
                        self.hosting_locally = true;
                        self.peer_connected = false;
                        self.refresh_host_addresses();
                        self.enter_room(code);
                        self.connection_status = Some("Você assumiu a hospedagem. A sala está pronta para outro participante.".to_owned());
                    }
                }
                SignalingEvent::HostTransferCanceled(message) => {
                    tracing::warn!(reason = %message, "Transferência de hospedagem cancelada");
                    if self.outgoing_transfer.is_some()
                        || self.incoming_transfer.is_some()
                        || self.pending_signaling.is_some()
                    {
                        self.outgoing_transfer = None;
                        self.incoming_transfer = None;
                        self.pending_signaling = None;
                        self.pending_room_adopted = false;
                        self.handoff_error = Some(message);
                    }
                }
                SignalingEvent::ServerError(error) => {
                    tracing::error!(error = %error, "Servidor recusou a operação solicitada");
                    if self.connecting {
                        self.connecting = false;
                        self.starting_host = false;
                        self.hosting_locally = false;
                        self.peer_connected = false;
                        self.room_code = None;
                        self.connection_status = Some("Desconectado.".to_owned());
                        self.connection_error = Some(error);
                        disconnect = true;
                    } else if self.pending_signaling.is_some() {
                        if let (Some(signaling), Some((_, token))) =
                            (&self.signaling, &self.incoming_transfer)
                        {
                            let _ = signaling.reject_host_transfer(token.clone());
                        }
                        self.pending_signaling = None;
                        self.pending_room_adopted = false;
                        self.incoming_transfer = None;
                        self.handoff_error = Some(error);
                    } else if self.outgoing_transfer.is_some() || self.incoming_transfer.is_some() {
                        if self
                            .outgoing_transfer
                            .as_ref()
                            .is_some_and(|(_, token)| token.is_empty())
                        {
                            self.outgoing_transfer = None;
                        }
                        self.handoff_error = Some(error);
                    } else if !matches!(&self.screen_share_role, ScreenShareRole::Idle) {
                        self.stop_screen_share(false);
                        self.screen_share_status = Some(error);
                    } else if self
                        .diagnostic_status
                        .as_deref()
                        .is_some_and(|status| status.starts_with("Enviando sinal"))
                    {
                        self.diagnostic_status = Some(error);
                    } else {
                        self.connection_error = Some(error);
                    }
                }
                SignalingEvent::Signal {
                    from_participant_id: _,
                    stream_id: _,
                    kind: SignalKind::Diagnostic,
                    payload,
                } => {
                    if let Some(serialized) = payload.strip_prefix(TURN_CONFIG_SIGNAL_PREFIX) {
                        match serde_json::from_str::<TurnRoomConfig>(serialized) {
                            Ok(config) => {
                                let turn_enabled = config.turn.is_some();
                                self.turn_room_config = Some(config);
                                self.turn_config_received = true;
                                self.turn_config_wait_started_at = None;
                                tracing::info!(
                                    turn_enabled,
                                    "Configuração ICE recebida do anfitrião; credenciais omitidas"
                                );
                                self.connection_status = Some(if turn_enabled {
                                    "Configuração recebida; TURN está disponível como alternativa."
                                        .to_owned()
                                } else {
                                    "Configuração recebida; esta sala usa somente conexão direta."
                                        .to_owned()
                                });
                            }
                            Err(error) => {
                                tracing::error!(error = %error, "Configuração ICE da sala inválida");
                                self.connection_error = Some(
                                    "O anfitrião enviou uma configuração de mídia inválida."
                                        .to_owned(),
                                );
                            }
                        }
                    } else {
                        tracing::debug!("Sinal de diagnóstico recebido; payload omitido");
                        match payload.as_str() {
                            "diagnostic-ping-v1" => {
                                self.diagnostic_status = Some(
                                    "Sinal recebido; enviando confirmação ao seu amigo.".to_owned(),
                                );
                                acknowledge_diagnostic = true;
                            }
                            "diagnostic-pong-v1" => {
                                self.diagnostic_status = Some(
                                    "Seu amigo confirmou o recebimento do sinal de diagnóstico."
                                        .to_owned(),
                                );
                            }
                            _ => {}
                        }
                    }
                }
                SignalingEvent::Signal {
                    from_participant_id,
                    stream_id,
                    kind,
                    payload,
                } => {
                    tracing::debug!(signal_kind = ?kind, payload_bytes = payload.len(), "Sinal de negociação recebido; payload omitido");
                    if matches!(
                        kind,
                        SignalKind::Offer
                            | SignalKind::Answer
                            | SignalKind::IceCandidate
                            | SignalKind::ScreenShareRequest
                            | SignalKind::ScreenShareAccept
                            | SignalKind::ScreenShareBusy
                            | SignalKind::ScreenShareStopped
                            | SignalKind::ScreenShareAvailable
                            | SignalKind::ScreenShareUnavailable
                            | SignalKind::ScreenShareWatch
                            | SignalKind::ScreenShareUnwatch
                    ) {
                        self.handle_screen_share_signal(
                            from_participant_id.clone(),
                            stream_id.clone(),
                            kind,
                            payload.clone(),
                            context,
                        );
                    } else if matches!(
                        kind,
                        SignalKind::Offer | SignalKind::Answer | SignalKind::IceCandidate
                    ) {
                        self.handle_group_peer_signal(
                            from_participant_id,
                            stream_id,
                            kind,
                            payload,
                            context,
                        );
                    } else {
                        self.connection_status = Some("Sinal de conexão recebido.".to_owned());
                    }
                }
                SignalingEvent::Error(error) => {
                    tracing::error!(error = %error, "Conexão de sinalização falhou durante a sala");
                    if self.control_mesh.is_some() && self.room_code.is_some() {
                        self.connecting = false;
                        self.starting_host = false;
                        self.signaling = None;
                        self.connection_status = Some("Servidor de sinalização desconectado; mantendo os canais diretos para eleger outro anfitrião.".to_owned());
                        self.connection_error = Some(error);
                        continue;
                    }
                    self.connecting = false;
                    self.starting_host = false;
                    self.hosting_locally = false;
                    self.peer_connected = false;
                    self.room_code = None;
                    self.connection_status = Some("Desconectado.".to_owned());
                    self.connection_error = Some(error);
                    disconnect = true;
                }
                SignalingEvent::Disconnected => {
                    tracing::warn!("Conexão de sinalização encerrada durante a sala");
                    if self.control_mesh.is_some() && self.room_code.is_some() {
                        self.connecting = false;
                        self.signaling = None;
                        self.connection_status = Some("Servidor de sinalização desconectado; aguardando a eleição pela malha direta.".to_owned());
                        continue;
                    }
                    self.connecting = false;
                    self.starting_host = false;
                    self.hosting_locally = false;
                    self.peer_connected = false;
                    self.room_code = None;
                    self.connection_status = Some("Conexão com o servidor encerrada.".to_owned());
                    disconnect = true;
                }
            }
        }

        if acknowledge_diagnostic {
            if let Some(signaling) = &self.signaling {
                if let Err(error) = signaling.acknowledge_diagnostic() {
                    self.diagnostic_status = Some(error);
                }
            }
        }

        if disconnect {
            self.stop_screen_share(false);
            self.signaling = None;
            self.turn_server = None;
            self.turn_room_config = None;
            self.turn_config_received = false;
            self.turn_config_wait_started_at = None;
            self.pending_signaling = None;
            self.screen_picker = None;
            if let Some(mut capture) = self.screen_capture.take() {
                let _ = capture.stop();
            }
            self.clear_local_preview();
        }
    }

    pub(super) fn refresh_turn_state(&mut self) {
        let failure = self
            .turn_server
            .as_mut()
            .and_then(TurnRelayServer::try_failure);
        if let Some(error) = failure {
            tracing::error!(error = %error, "Servidor TURN integrado encerrou com erro");
            self.turn_server = None;
            self.turn_room_config = Some(TurnRoomConfig { turn: None });
            self.turn_config_received = true;
            self.send_turn_room_config();
            self.connection_error = Some(format!(
                "O servidor TURN parou: {error}. O compartilhamento poderá usar somente conexão direta."
            ));
            if self.screen_share_session.is_some() {
                self.stop_screen_share(false);
                self.screen_share_status = Some(
                    "O servidor TURN parou. Inicie novamente para tentar uma conexão direta."
                        .to_owned(),
                );
            }
        }

        if self.room_mode == RoomMode::InternetTest
            && !self.hosting_locally
            && !self.turn_config_received
            && self
                .turn_config_wait_started_at
                .is_some_and(|started| started.elapsed() >= Duration::from_secs(5))
        {
            tracing::warn!(
                "O anfitrião não enviou configuração TURN em 5 segundos; mantendo tentativa direta"
            );
            self.turn_room_config = Some(TurnRoomConfig { turn: None });
            self.turn_config_received = true;
            self.turn_config_wait_started_at = None;
            self.connection_status = Some(
                "O anfitrião não enviou configuração TURN; a tela tentará somente conexão direta."
                    .to_owned(),
            );
        }
    }

    pub(super) fn request_leave(&mut self, context: &egui::Context) {
        self.stop_screen_share(true);
        if self.hosting_locally && self.peer_connected && self.room_mode == RoomMode::InternetTest {
            self.leave_room();
            self.connection_status = Some(
                "Sala de Internet encerrada; o anfitrião saiu e ela não terá sucessor.".to_owned(),
            );
        } else if self.hosting_locally && self.peer_connected {
            if let Some(mesh) = self
                .control_mesh
                .as_ref()
                .filter(|_| !self.control_mesh_failed)
            {
                if !mesh.leave_normally() {
                    self.leave_room();
                    self.connection_status = Some(
                        "Voce saiu; nao foi possivel iniciar a eleicao automatica.".to_owned(),
                    );
                    if self.close_after_transfer {
                        self.allow_window_close = true;
                        context.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    return;
                }
                self.leave_after_handoff = true;
                self.connection_status = Some(
                    "Elegendo automaticamente o próximo anfitrião; esta sala continuará ativa…"
                        .to_owned(),
                );
            } else {
                self.leave_room();
                self.connection_status = Some(
                    "Você saiu. A malha de controle estava indisponível, então não foi possível transferir a hospedagem.".to_owned(),
                );
            }
        } else if self.incoming_transfer.is_some() {
            self.reject_incoming_transfer();
            self.leave_room();
        } else {
            self.leave_room();
        }
        if self.close_after_transfer && self.room_code.is_none() {
            self.allow_window_close = true;
            context.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    pub(super) fn end_room_explicitly(&mut self, context: &egui::Context) {
        self.stop_screen_share(true);
        self.ending_room_explicitly = true;
        let end_requested = self
            .control_mesh
            .as_ref()
            .filter(|_| !self.control_mesh_failed)
            .is_some_and(ControlMesh::end_room);
        if end_requested {
            self.connection_status =
                Some("Encerrando a sala para todos os participantes…".to_owned());
        } else {
            self.leave_room();
            self.connection_status = Some("Sala encerrada por você.".to_owned());
        }
        if self.close_after_transfer && self.room_code.is_none() {
            self.allow_window_close = true;
            context.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    pub(super) fn request_host_transfer(&mut self) {
        let Some(code) = self.room_code.clone() else {
            self.handoff_error = Some("Você não está em uma sala.".to_owned());
            return;
        };
        let Some(signaling) = &self.signaling else {
            self.handoff_error = Some("A conexão da sala não está ativa.".to_owned());
            return;
        };
        match signaling.request_host_transfer() {
            Ok(()) => {
                self.outgoing_transfer = Some((code, String::new()));
                self.handoff_error = None;
                self.connection_status = Some(
                    "Solicitação enviada; aguardando o aceite do outro participante.".to_owned(),
                );
            }
            Err(error) => self.handoff_error = Some(error),
        }
    }

    pub(super) fn accept_incoming_transfer(&mut self) {
        let Some((code, token)) = self.incoming_transfer.clone() else {
            return;
        };
        self.close_after_transfer = false;
        match SignalingClient::adopt_transfer(code, token) {
            Ok(client) => {
                self.pending_signaling = Some(client);
                self.pending_room_adopted = false;
                self.handoff_error = None;
                self.connection_status = Some("Iniciando o servidor neste computador…".to_owned());
            }
            Err(error) => self.handoff_error = Some(error),
        }
    }

    pub(super) fn reject_incoming_transfer(&mut self) {
        if let (Some(signaling), Some((_, token))) = (&self.signaling, &self.incoming_transfer) {
            let _ = signaling.reject_host_transfer(token.clone());
        }
        self.pending_signaling = None;
        self.pending_room_adopted = false;
        self.incoming_transfer = None;
    }

    pub(super) fn handle_window_close(&mut self, context: &egui::Context) {
        let close_requested = context.input(|input| input.viewport().close_requested());
        if !close_requested || self.allow_window_close {
            return;
        }

        if (self.hosting_locally && self.peer_connected)
            || self.outgoing_transfer.is_some()
            || self.incoming_transfer.is_some()
            || self.pending_signaling.is_some()
        {
            context.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.close_after_transfer = true;
            if self.hosting_locally && self.peer_connected && self.outgoing_transfer.is_none() {
                self.request_leave(context);
            }
        } else {
            self.allow_window_close = true;
        }
    }

    pub(super) fn leave_room(&mut self) {
        tracing::info!("Saindo da sala e liberando recursos locais");
        self.stop_microphone();
        self.stop_screen_share(true);
        self.turn_server = None;
        self.turn_room_config = None;
        self.turn_config_received = false;
        self.turn_config_wait_started_at = None;
        self.screen_picker = None;
        if let Some(mut capture) = self.screen_capture.take() {
            let _ = capture.stop();
        }
        self.clear_local_preview();
        self.room_code = None;
        self.participants.clear();
        self.current_leader_id.clear();
        self.room_mode = RoomMode::Local;
        self.control_queue.clear();
        self.control_mesh = None;
        self.pending_election_epoch = None;
        self.pending_election_reconnect = false;
        self.leave_after_handoff = false;
        self.ending_room_explicitly = false;
        self.join_code.clear();
        self.code_copied_until = None;
        self.microphone_error = None;
        self.screen_status = None;
        self.screen_share_status = None;
        self.pending_signaling = None;
        self.signaling = None;
        self.pending_room_adopted = false;
        self.hosting_locally = false;
        self.starting_host = false;
        self.incoming_transfer = None;
        self.outgoing_transfer = None;
        self.connecting = false;
        self.peer_connected = false;
        self.connection_status = Some("Desconectado.".to_owned());
        self.connection_error = None;
        self.diagnostic_status = None;
        self.handoff_error = None;
    }
}
