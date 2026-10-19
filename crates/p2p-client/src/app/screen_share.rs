use super::*;

impl ClientUi {
    pub(super) fn handle_quality_feedback_signal(
        &mut self,
        from_participant_id: Option<&str>,
        stream_id: Option<&str>,
        payload: &str,
    ) -> bool {
        let Some(serialized) = payload.strip_prefix(screen_sharing::QUALITY_FEEDBACK_SIGNAL_PREFIX)
        else {
            return false;
        };
        let Some(feedback) = QualityFeedback::parse(serialized) else {
            tracing::warn!("Feedback adaptativo de vídeo inválido; payload descartado");
            return true;
        };
        let (Some(peer_id), Some(stream_id)) = (from_participant_id, stream_id) else {
            tracing::warn!("Feedback adaptativo sem participante ou sessão; payload descartado");
            return true;
        };

        if self.group_sharing_compatible() {
            let current_generation = self.group_share.outbound_generations.get(peer_id);
            if current_generation.map(String::as_str) != Some(stream_id)
                || !super::group_sharing::is_group_generation_for_peer(peer_id, stream_id)
            {
                tracing::debug!(peer = %peer_id, "Feedback adaptativo de uma sessão de grupo encerrada ou desconhecida foi descartado");
                return true;
            }
            if let Some(session) = self.group_share.outbound_sessions.get(peer_id) {
                let maximum = self
                    .screen_capture
                    .as_ref()
                    .map(ScreenCapture::quality)
                    .unwrap_or_default();
                match session.set_outbound_video_profile(feedback.profile, maximum) {
                    Ok(applied) => tracing::info!(
                        peer = %peer_id,
                        stream_id = %stream_id,
                        requested_profile = ?feedback.profile,
                        applied_profile = ?applied,
                        reason = ?feedback.reason,
                        "Feedback de qualidade aplicado à sessão do participante"
                    ),
                    Err(error) => {
                        tracing::debug!(peer = %peer_id, error = %error, "Feedback adaptativo recebido antes do início do encoder")
                    }
                }
            }
            return true;
        }

        let active_request = match &self.screen_share_role {
            ScreenShareRole::Sending { request_id } => Some(request_id.as_str()),
            _ => None,
        };
        if active_request != Some(stream_id)
            || !self
                .participants
                .iter()
                .any(|participant| participant.id == peer_id)
        {
            tracing::debug!(peer = %peer_id, stream_id = %stream_id, "Feedback adaptativo de outra sessão foi descartado");
            return true;
        }
        if let Some(session) = self.screen_share_session.as_ref() {
            match session.set_outbound_video_profile(feedback.profile, self.screen_share_quality) {
                Ok(applied) => tracing::info!(
                    peer = %peer_id,
                    stream_id = %stream_id,
                    requested_profile = ?feedback.profile,
                    applied_profile = ?applied,
                    reason = ?feedback.reason,
                    "Feedback de qualidade aplicado à sessão do participante"
                ),
                Err(error) => {
                    tracing::debug!(peer = %peer_id, error = %error, "Feedback adaptativo recebido antes do início do encoder")
                }
            }
        }
        true
    }

    pub(super) fn send_quality_feedback(
        &self,
        peer_id: &str,
        stream_id: String,
        feedback: QualityFeedback,
    ) {
        let serialized = match feedback.serialize() {
            Ok(payload) => payload,
            Err(error) => {
                tracing::error!(error = %error, "Não foi possível serializar feedback adaptativo");
                return;
            }
        };
        let payload = format!(
            "{}{serialized}",
            screen_sharing::QUALITY_FEEDBACK_SIGNAL_PREFIX
        );
        if let Err(error) = self.send_screen_share_signal_to_stream(
            peer_id,
            stream_id,
            SignalKind::Diagnostic,
            payload,
        ) {
            tracing::debug!(peer = %peer_id, error = %error, "Não foi possível enviar feedback adaptativo para a sessão");
        }
    }

    pub(super) fn log_minimum_profile_overload(
        &self,
        peer_id: &str,
        stream_id: &str,
        evidence: screen_sharing::QualityOverloadEvidence,
    ) {
        tracing::warn!(
            peer = %peer_id,
            stream_id = %stream_id,
            profile = "720p15",
            decoder_queue_drops = evidence.queue_drops,
            recovery_gate_drops = evidence.recovery_gate_drops,
            "O receptor continua abaixo da meta no perfil mínimo de vídeo"
        );
    }

    pub(super) fn select_screen(&mut self, _context: &egui::Context) {
        tracing::info!("Usuário abriu o seletor de tela ou janela do Windows");
        self.screen_status = None;
        match PendingScreenCapture::begin(
            Arc::clone(&self.capture_preview_enabled),
            self.screen_share_quality,
        ) {
            Ok(picker) => {
                self.screen_picker = Some(picker);
                self.screen_status =
                    Some("Selecione uma tela ou janela no seletor do Windows.".to_owned());
            }
            Err(error) => {
                self.screen_status = Some(format!("Não foi possível abrir o seletor: {error}"));
            }
        }
    }
    pub(super) fn refresh_screen(&mut self, context: &egui::Context) {
        let preview_active = self.show_local_preview
            && (self.group_share.local_sharing
                || matches!(&self.screen_share_role, ScreenShareRole::Sending { .. }));
        self.capture_preview_enabled
            .store(preview_active, Ordering::Relaxed);
        if !preview_active {
            self.clear_local_preview();
        }

        let picker_result = if self.settings_open {
            self.screen_picker = None;
            None
        } else {
            self.screen_picker
                .as_mut()
                .and_then(|picker| picker.poll(context.clone()))
        };
        if let Some(result) = picker_result {
            self.screen_picker = None;
            match result {
                Ok(Some(capture)) => {
                    self.stop_running_screen_capture();
                    self.clear_local_preview();
                    self.screen_capture = Some(capture);
                    self.screen_status =
                        Some("A prévia atualiza enquanto a captura estiver ativa.".to_owned());
                    if self.auto_start_share_after_picker {
                        self.auto_start_share_after_picker = false;
                        if self.group_sharing_compatible() {
                            self.set_group_share(true);
                        } else {
                            self.request_screen_share(context);
                        }
                    }
                }
                Ok(None) => {
                    self.auto_start_share_after_picker = false;
                    self.screen_status =
                        Some("Seleção cancelada; a captura permaneceu desligada.".to_owned());
                }
                Err(error) => {
                    self.auto_start_share_after_picker = false;
                    tracing::error!(error = %error, "Falha ao iniciar captura de tela após seleção");
                    self.screen_status = Some(format!(
                        "Não foi possível iniciar a captura da tela: {error}"
                    ));
                }
            }
        }

        let Some(capture) = self.screen_capture.as_mut() else {
            self.clear_local_preview();
            return;
        };

        let frame = capture.latest_frame();

        if capture.source_closed() {
            self.stop_screen_share(true);
            self.stop_screen_capture();
            self.screen_status =
                Some("A tela ou janela escolhida foi fechada; a captura terminou.".to_owned());
            return;
        }

        let was_dxgi = capture.backend_name() == "DXGI Desktop Duplication";
        if let Some(result) = capture.poll_finished() {
            self.stop_screen_share(true);
            self.screen_capture = None;
            self.clear_local_preview();
            self.screen_status = Some(match result {
                Ok(()) => "A captura da tela foi encerrada pelo Windows.".to_owned(),
                Err(error) => {
                    if was_dxgi {
                        self.dxgi_capture_error = Some(error.clone());
                    }
                    format!("A captura da tela falhou: {error}")
                }
            });
            return;
        }
        self.refresh_local_preview(context, preview_active, frame.as_deref());
    }

    pub(super) fn refresh_local_preview(
        &mut self,
        context: &egui::Context,
        preview_active: bool,
        frame: Option<&PreviewFrame>,
    ) {
        if !preview_active {
            self.clear_local_preview();
            return;
        }
        let Some(frame) = frame else {
            return;
        };
        let key = LocalPreviewFrameKey::from(frame);
        if (self.local_preview_frame_key == Some(key) && self.screen_texture.is_some())
            || self
                .local_preview_failure
                .as_ref()
                .is_some_and(|(failed_key, _)| *failed_key == key)
        {
            return;
        }
        let image = match local_preview_image(frame) {
            Ok(Some(image)) => image,
            Ok(None) => return,
            Err(error) => {
                self.clear_local_preview();
                let message = format!(
                    "Não foi possível mostrar a prévia local; a transmissão continua: {error}"
                );
                self.screen_status = Some(message.clone());
                self.local_preview_failure = Some((key, message));
                return;
            }
        };
        if let Some(texture) = self.screen_texture.as_mut() {
            texture.set(image, egui::TextureOptions::LINEAR);
        } else {
            self.screen_texture =
                Some(context.load_texture("screen-preview", image, egui::TextureOptions::LINEAR));
        }
        self.local_preview_frame_key = Some(key);
        self.clear_local_preview_failure();
    }

    pub(super) fn clear_local_preview_failure(&mut self) {
        if let Some((_, message)) = self.local_preview_failure.take()
            && self.screen_status.as_deref() == Some(message.as_str())
        {
            self.screen_status = None;
        }
    }

    pub(super) fn clear_local_preview(&mut self) {
        self.screen_texture = None;
        self.local_preview_frame_key = None;
        self.clear_local_preview_failure();
    }

    pub(super) fn stop_screen_capture(&mut self) {
        self.stop_screen_share(true);
        let result = self
            .screen_capture
            .take()
            .map_or(Ok(()), |mut capture| capture.stop());
        self.clear_local_preview();
        self.screen_status = Some(match result {
            Ok(()) => "Captura da tela parada.".to_owned(),
            Err(error) => {
                format!("A captura parou, mas houve um erro ao liberar o recurso: {error}")
            }
        });
    }

    pub(super) fn request_screen_share(&mut self, context: &egui::Context) {
        self.audio_status = None;
        if self.group_sharing_compatible() {
            self.set_group_share(true);
            context.request_repaint();
            return;
        }
        if self.participants.len() != 2
            || !self.peer_connected
            || self.screen_capture.is_none()
            || !matches!(&self.screen_share_role, ScreenShareRole::Idle)
        {
            return;
        }
        if self.room_mode == RoomMode::InternetTest
            && let Err(error) = screen_sharing::validate_stun_uri(&self.stun_server_url)
        {
            self.screen_share_status = Some(error);
            return;
        }
        self.screen_share_metrics = ScreenShareMetrics::default();
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let request_id = format!("{}-{nonce:x}", self.participant_id);
        let request = ScreenShareRequest {
            request_id: request_id.clone(),
            participant_id: self.participant_id.clone(),
        };
        let payload = match serde_json::to_string(&request) {
            Ok(payload) => payload,
            Err(error) => {
                self.screen_share_status =
                    Some(format!("Não foi possível preparar o pedido: {error}"));
                return;
            }
        };
        let result = self
            .signaling
            .as_ref()
            .ok_or_else(|| "A sala não está conectada ao servidor de sinalização.".to_owned())
            .and_then(|signaling| signaling.send_signal(SignalKind::ScreenShareRequest, payload));
        match result {
            Ok(()) => {
                self.screen_share_role = ScreenShareRole::Requesting { request_id };
                self.screen_share_status =
                    Some("Pedido enviado; a transmissão começa quando o amigo aceitar.".to_owned());
                context.request_repaint();
            }
            Err(error) => self.screen_share_status = Some(error),
        }
    }

    pub(super) fn handle_screen_share_signal(
        &mut self,
        from_participant_id: Option<String>,
        stream_id: Option<String>,
        kind: SignalKind,
        payload: String,
        context: &egui::Context,
    ) {
        match kind {
            SignalKind::ScreenShareAvailable => {
                if let Some(peer_id) = from_participant_id.filter(|id| id != &self.participant_id) {
                    if self.group_sharing_compatible() {
                        self.group_share.available_shares.insert(peer_id);
                    } else if self.group_sharing_upgrade_required() {
                        self.screen_share_status = Some(
                            "Este participante não anuncia correlação de sessão. Para compartilhar em grupo, todos precisam atualizar para a versão 1.1.3.".to_owned(),
                        );
                    }
                }
            }
            SignalKind::ScreenShareUnavailable => {
                if let Some(peer_id) = from_participant_id {
                    self.group_share.available_shares.remove(&peer_id);
                    self.group_share.watched_shares.remove(&peer_id);
                    self.group_share.auto_focus_pending.remove(&peer_id);
                    if let Some(session) = self.group_share.inbound_sessions.remove(&peer_id) {
                        group_sharing::log_final_group_session_diagnostics(
                            &session,
                            "receiver",
                            "remote_share_unavailable",
                        );
                    }
                    if let Some(generation) = self.group_share.inbound_generations.remove(&peer_id)
                    {
                        self.mark_group_generation_closed(&peer_id, generation);
                    }
                    self.group_share.audio_status.remove(&peer_id);
                    self.group_share.inbound_ports.remove(&peer_id);
                    self.group_share.remote_textures.remove(&peer_id);
                    self.group_share.remote_sequences.remove(&peer_id);
                    if self.group_share.focused_screen.as_deref() == Some(peer_id.as_str()) {
                        self.group_share.focused_screen = None;
                    }
                }
            }
            SignalKind::ScreenShareWatch => {
                let Some(viewer_id) = from_participant_id else {
                    return;
                };
                if !self.group_share.local_sharing || !self.group_sharing_compatible() {
                    return;
                }
                if self.group_share.outbound_sessions.contains_key(&viewer_id) {
                    return;
                }
                self.rebalance_group_outbound(context, Some(viewer_id));
            }
            SignalKind::ScreenShareUnwatch => {
                if let Some(viewer_id) = from_participant_id {
                    let generation = self.group_share.outbound_generations.remove(&viewer_id);
                    if let Some(generation) = generation.as_ref() {
                        let _ = self.send_screen_share_signal_to_stream(
                            &viewer_id,
                            generation.clone(),
                            SignalKind::ScreenShareStopped,
                            String::new(),
                        );
                    }
                    if let Some(session) = self.group_share.outbound_sessions.remove(&viewer_id) {
                        group_sharing::log_final_group_session_diagnostics(
                            &session,
                            "sender",
                            "viewer_unwatched",
                        );
                    }
                    if let Some(generation) = generation {
                        self.mark_group_generation_closed(&viewer_id, generation);
                    }
                    self.group_share.outbound_ports.remove(&viewer_id);
                    self.rebalance_group_outbound(context, None);
                }
            }
            SignalKind::ScreenShareRequest => {
                let request: ScreenShareRequest = match serde_json::from_str(&payload) {
                    Ok(request) => request,
                    Err(error) => {
                        self.screen_share_status =
                            Some(format!("Pedido de compartilhamento inválido: {error}"));
                        return;
                    }
                };
                let remote_order = self
                    .participants
                    .iter()
                    .find(|participant| participant.id == request.participant_id);
                let Some(remote_order) = remote_order.map(|participant| participant.order) else {
                    return;
                };
                if self.participants.len() != 2 {
                    let _ = self
                        .send_screen_share_signal(SignalKind::ScreenShareBusy, request.request_id);
                    self.screen_share_status =
                        Some("O compartilhamento ainda está limitado a duas pessoas.".to_owned());
                    return;
                }
                if self.room_mode == RoomMode::InternetTest && !self.turn_config_received {
                    let _ = self
                        .send_screen_share_signal(SignalKind::ScreenShareBusy, request.request_id);
                    self.screen_share_status = Some(
                        "Aguardando a configuração de mídia enviada pelo anfitrião.".to_owned(),
                    );
                    return;
                }

                let local_order = self
                    .participants
                    .iter()
                    .find(|participant| participant.id == self.participant_id)
                    .map(|participant| participant.order)
                    .unwrap_or(u8::MAX);
                match self.screen_share_role.clone() {
                    ScreenShareRole::Requesting { .. } if local_order < remote_order => {
                        // Quando os dois pedem ao mesmo tempo, vence quem entrou primeiro.
                        let _ = self.send_screen_share_signal(
                            SignalKind::ScreenShareBusy,
                            request.request_id,
                        );
                        return;
                    }
                    ScreenShareRole::Requesting { .. } if local_order > remote_order => {
                        // O pedido de quem entrou antes vence; aceitamos e descartamos o nosso.
                        self.screen_share_role = ScreenShareRole::Idle;
                    }
                    ScreenShareRole::Idle if self.screen_share_session.is_none() => {}
                    _ => {
                        let _ = self.send_screen_share_signal(
                            SignalKind::ScreenShareBusy,
                            request.request_id,
                        );
                        return;
                    }
                }

                let bind_ipv4 = match self.selected_media_ipv4() {
                    Ok(address) => address,
                    Err(error) => {
                        let _ = self.send_screen_share_signal(
                            SignalKind::ScreenShareBusy,
                            request.request_id,
                        );
                        self.screen_share_status = Some(error);
                        return;
                    }
                };

                match ScreenShareSession::new_with_audio_volume(
                    context.clone(),
                    bind_ipv4,
                    self.stun_server_for_room(),
                    self.turn_credentials_for_room(),
                    self.video_decoder_preference,
                    self.remote_audio_default_volume_percent,
                ) {
                    Ok(session) => {
                        let metrics = session.metrics();
                        tracing::info!(
                            screen_share_session = metrics.session_id,
                            role = "receiver",
                            "Receptor de compartilhamento preparado"
                        );
                        self.screen_share_session = Some(session);
                        self.screen_share_metrics = ScreenShareMetrics::default();
                        self.last_screen_metrics_log_at = Some(Instant::now());
                        self.screen_share_role = ScreenShareRole::Receiving {
                            request_id: request.request_id.clone(),
                        };
                        self.screen_share_status =
                            Some("Aceitando a tela do amigo; preparando a conexão P2P…".to_owned());
                        if let Err(error) = self.send_screen_share_signal(
                            SignalKind::ScreenShareAccept,
                            request.request_id,
                        ) {
                            self.stop_screen_share(false);
                            self.screen_share_status = Some(error);
                        }
                    }
                    Err(error) => {
                        let _ = self.send_screen_share_signal(
                            SignalKind::ScreenShareBusy,
                            request.request_id,
                        );
                        self.screen_share_status = Some(error);
                    }
                }
            }
            SignalKind::ScreenShareAccept => {
                let Some(request_id) = (match self.screen_share_role.clone() {
                    ScreenShareRole::Requesting { request_id } if request_id == payload => {
                        Some(request_id)
                    }
                    _ => None,
                }) else {
                    return;
                };
                let Some((source, quality)) = self
                    .screen_capture
                    .as_ref()
                    .map(|capture| (capture.frame_source(), capture.quality()))
                else {
                    self.screen_share_role = ScreenShareRole::Idle;
                    self.screen_share_status = Some(
                        "A captura local terminou antes do início do compartilhamento.".to_owned(),
                    );
                    return;
                };
                let bind_ipv4 = match self.selected_media_ipv4() {
                    Ok(address) => address,
                    Err(error) => {
                        self.screen_share_role = ScreenShareRole::Idle;
                        self.screen_share_status = Some(error);
                        return;
                    }
                };
                match ScreenShareSession::new_with_audio_volume(
                    context.clone(),
                    bind_ipv4,
                    self.stun_server_for_room(),
                    self.turn_credentials_for_room(),
                    self.video_decoder_preference,
                    self.remote_audio_default_volume_percent,
                ) {
                    Ok(session) => {
                        if let Some(capture) = self.screen_capture.as_ref() {
                            let _ = capture.take_performance_snapshot();
                        }
                        if let Err(error) = session
                            .start_sending_with_quality_and_options_and_exclusion(
                                source,
                                quality,
                                quality.individual_bitrate_bps(),
                                self.include_system_audio,
                                self.audio_exclusion_config(),
                            )
                        {
                            session.stop();
                            self.screen_share_role = ScreenShareRole::Idle;
                            self.screen_share_status = Some(error);
                            return;
                        }
                        let metrics = session.metrics();
                        tracing::info!(
                            screen_share_session = metrics.session_id,
                            role = "sender",
                            "Emissor de compartilhamento preparado"
                        );
                        self.screen_share_session = Some(session);
                        self.last_screen_metrics_log_at = Some(Instant::now());
                        self.screen_share_role = ScreenShareRole::Sending { request_id };
                        self.screen_share_status =
                            Some("Iniciando a codificação e a conexão direta…".to_owned());
                    }
                    Err(error) => {
                        self.screen_share_role = ScreenShareRole::Idle;
                        self.screen_share_status = Some(error);
                    }
                }
            }
            SignalKind::ScreenShareBusy => {
                if matches!(
                    &self.screen_share_role,
                    ScreenShareRole::Requesting { request_id } if request_id == &payload
                ) {
                    self.screen_share_role = ScreenShareRole::Idle;
                    self.screen_share_status = Some(
                        "O amigo já está compartilhando ou não pode receber outra solicitação agora."
                            .to_owned(),
                    );
                }
            }
            SignalKind::ScreenShareStopped => {
                if let Some(peer_id) = from_participant_id.as_deref().filter(|id| {
                    self.group_sharing_compatible()
                        && self.group_share.watched_shares.contains(*id)
                        && stream_id.as_deref()
                            == self
                                .group_share
                                .inbound_generations
                                .get(*id)
                                .map(String::as_str)
                }) {
                    self.group_share
                        .auto_focus_pending
                        .insert(peer_id.to_owned());
                    if let Some(session) = self.group_share.inbound_sessions.remove(peer_id) {
                        group_sharing::log_final_group_session_diagnostics(
                            &session,
                            "receiver",
                            "remote_share_reconfigured",
                        );
                    }
                    if let Some(generation) = self.group_share.inbound_generations.remove(peer_id) {
                        self.mark_group_generation_closed(peer_id, generation);
                    }
                    self.group_share.inbound_ports.remove(peer_id);
                    self.group_share.remote_textures.remove(peer_id);
                    self.group_share.remote_sequences.remove(peer_id);
                    self.group_share.peer_status.insert(
                        peer_id.to_owned(),
                        "A transmissão está sendo reconfigurada…".to_owned(),
                    );
                    self.group_share.audio_status.remove(peer_id);
                    return;
                }
                let request_id = payload;
                let active_request = match &self.screen_share_role {
                    ScreenShareRole::Requesting { request_id }
                    | ScreenShareRole::Sending { request_id }
                    | ScreenShareRole::Receiving { request_id } => Some(request_id),
                    ScreenShareRole::Idle => None,
                };
                if active_request.is_some_and(|active| active == &request_id) {
                    self.stop_screen_share(false);
                    self.screen_share_status =
                        Some("O compartilhamento de tela foi encerrado.".to_owned());
                }
            }
            SignalKind::Offer | SignalKind::Answer | SignalKind::IceCandidate => {
                if self.group_sharing_compatible() {
                    self.handle_group_peer_signal(
                        from_participant_id,
                        stream_id,
                        kind,
                        payload,
                        context,
                    );
                } else if let Some(session) = &self.screen_share_session
                    && let Err(error) = session.handle_signal(kind, payload)
                {
                    self.stop_screen_share(true);
                    self.screen_share_status = Some(error);
                }
            }
            SignalKind::Diagnostic => {}
        }
    }

    pub(super) fn send_screen_share_signal(
        &self,
        kind: SignalKind,
        payload: String,
    ) -> Result<(), String> {
        tracing::debug!(
            signal_kind = ?kind,
            payload_bytes = payload.len(),
            "Enviando sinal de compartilhamento; conteúdo omitido"
        );
        self.signaling
            .as_ref()
            .ok_or_else(|| "A conexão de sinalização não está disponível.".to_owned())?
            .send_signal(kind, payload)
    }

    pub(super) fn send_screen_share_signal_to(
        &self,
        participant_id: &str,
        kind: SignalKind,
        payload: String,
    ) -> Result<(), String> {
        self.signaling
            .as_ref()
            .ok_or_else(|| "A conexão de sinalização não está disponível.".to_owned())?
            .send_signal_to(participant_id.to_owned(), kind, payload)
    }

    pub(super) fn send_screen_share_signal_to_stream(
        &self,
        participant_id: &str,
        stream_id: String,
        kind: SignalKind,
        payload: String,
    ) -> Result<(), String> {
        self.signaling
            .as_ref()
            .ok_or_else(|| "A conexão de sinalização não está disponível.".to_owned())?
            .send_signal_to_stream(participant_id.to_owned(), stream_id, kind, payload)
    }

    pub(super) fn send_turn_room_config(&self) {
        if !self.hosting_locally {
            return;
        }
        let Some(config) = &self.turn_room_config else {
            return;
        };
        if !config.is_valid() {
            tracing::warn!("Configuração de mídia local inválida; envio de sinal ignorado");
            return;
        }
        let payload = match serde_json::to_string(config) {
            Ok(payload) => format!("{TURN_CONFIG_SIGNAL_PREFIX}{payload}"),
            Err(error) => {
                tracing::error!(error = %error, "Não foi possível serializar a configuração de mídia da sala");
                return;
            }
        };
        let result = self
            .signaling
            .as_ref()
            .ok_or_else(|| "A conexão de sinalização não está disponível.".to_owned())
            .and_then(|signaling| signaling.send_signal(SignalKind::Diagnostic, payload));
        match result {
            Ok(()) => tracing::info!(
                turn_enabled = config.turn.is_some(),
                "Configuração ICE da sala enviada ao outro participante; credenciais omitidas"
            ),
            Err(error) => tracing::error!(
                error = %error,
                "Não foi possível enviar a configuração ICE da sala"
            ),
        }
    }

    pub(super) fn turn_credentials_for_room(&self) -> Option<TurnCredentials> {
        if self.room_mode != RoomMode::InternetTest {
            return None;
        }
        let mut credentials = self.turn_room_config.as_ref()?.turn.clone()?;
        if self.hosting_locally {
            // The host must not depend on router NAT loopback to reach its own TURN service.
            // The TURN server still advertises the configured public relay address.
            credentials.url = "turn:127.0.0.1:3478?transport=udp".to_owned();
        }
        Some(credentials)
    }

    pub(super) fn turn_configuration_ready(&self) -> bool {
        self.room_mode != RoomMode::InternetTest || self.turn_config_received
    }

    pub(super) fn screen_route_status(&self, connected: &str, negotiating: &str) -> String {
        if !self.screen_share_metrics.p2p_connected {
            return negotiating.to_owned();
        }
        match self.screen_share_metrics.route {
            Some(screen_sharing::MediaRoute::Turn) => {
                format!("Conexão retransmitida via TURN; {connected}")
            }
            Some(screen_sharing::MediaRoute::Direct) => {
                format!("Conexão direta P2P estabelecida; {connected}")
            }
            None => format!("Conexão WebRTC estabelecida; {connected}"),
        }
    }

    pub(super) fn refresh_screen_share(&mut self, context: &egui::Context) {
        self.refresh_group_screen_shares(context);
        if let Some(session) = self.screen_share_session.as_ref() {
            self.screen_share_metrics = session.metrics();
            let should_log_metrics = self.log_performance_metrics
                && self
                    .last_screen_metrics_log_at
                    .is_none_or(|last| last.elapsed() >= Duration::from_secs(5));
            if should_log_metrics {
                let metrics = &self.screen_share_metrics;
                let logged_at = Instant::now();
                let interval = self
                    .last_screen_metrics_log_at
                    .map_or(Duration::from_secs(5), |last| {
                        logged_at.saturating_duration_since(last)
                    });
                let interval_seconds = interval.as_secs_f64().max(0.001);
                let performance = session.take_performance_snapshot();
                let capture_performance =
                    if matches!(&self.screen_share_role, ScreenShareRole::Sending { .. }) {
                        self.screen_capture
                            .as_ref()
                            .map(ScreenCapture::take_performance_snapshot)
                            .unwrap_or_default()
                    } else {
                        Default::default()
                    };
                let capture_fps = capture_performance.processed_frames as f64 / interval_seconds;
                let capture_examined_frames = capture_performance
                    .processed_frames
                    .saturating_add(capture_performance.unchanged_frames);
                let encoder_fps = performance.encoded_frames as f64 / interval_seconds;
                let send_fps = performance.sent_frames as f64 / interval_seconds;
                let receive_fps = performance.assembled_access_units as f64 / interval_seconds;
                let receive_packet_rate = performance.received_packets as f64 / interval_seconds;
                let decode_fps = performance.decoded_frames as f64 / interval_seconds;
                let publish_fps = performance.published_frames as f64 / interval_seconds;
                let backend = self
                    .screen_capture
                    .as_ref()
                    .map(ScreenCapture::backend_name)
                    .unwrap_or("sem captura local");
                let average_ms = |nanos: u64, samples: u64| {
                    if samples == 0 {
                        0.0
                    } else {
                        nanos as f64 / samples as f64 / 1_000_000.0
                    }
                };
                let capture_readback_ms =
                    average_ms(capture_performance.readback_nanos, capture_examined_frames);
                let capture_resize_ms =
                    average_ms(capture_performance.resize_nanos, capture_examined_frames);
                let capture_gpu_ms = average_ms(
                    capture_performance.gpu_convert_nanos,
                    capture_examined_frames,
                );
                self.screen_pipeline_summary = format!(
                    "5 s: captura {capture_fps:.1} FPS únicos ({backend}; {} callbacks, {} imagens iguais, {} descartados); encoder {encoder_fps:.1} FPS (novos {}, repetidos {}, worker atrasado {}, sequências puladas {}); envio {send_fps:.1} FPS; recepção {receive_fps:.1} quadros/s ({receive_packet_rate:.0} pacotes/s), decoder {decode_fps:.1} FPS, prévia publicada {publish_fps:.1} FPS. Captura: leitura {capture_readback_ms:.1} ms, prévia {capture_resize_ms:.1} ms, GPU {capture_gpu_ms:.1} ms.",
                    capture_performance.received_frames,
                    capture_performance.unchanged_frames,
                    capture_performance.skipped_frames,
                    performance.new_capture_frames,
                    performance.repeated_capture_frames,
                    performance.encoder_worker_late_frames,
                    performance.skipped_capture_sequences,
                );
                tracing::info!(
                    video_timing_scope = "wall_clock_not_gpu_execution",
                    video_pipeline_interval = %performance.video_performance.label(),
                    video_pipeline_total = %metrics.video_performance.label(),
                    capture_pipeline_interval = %capture_performance.pipeline.label(),
                    screen_share_session = metrics.session_id,
                    role = match &self.screen_share_role {
                        ScreenShareRole::Sending { .. } => "sender",
                        ScreenShareRole::Receiving { .. } => "receiver",
                        ScreenShareRole::Requesting { .. } => "requesting",
                        ScreenShareRole::Idle => "idle",
                    },
                    track_ssrc = ?metrics.track_ssrc,
                    outbound_video_ssrc = ?metrics.outbound_video_ssrc,
                    inbound_video_ssrc = ?metrics.inbound_video_ssrc,
                    nack_sent_video_ssrc = ?metrics.inbound_video_ssrc,
                    nack_received_video_ssrc = ?metrics.outbound_video_ssrc,
                    assembled_idr_with_parameters_total = metrics.assembled_idr_with_parameters,
                    assembled_idr_without_parameters_total = metrics.assembled_idr_without_parameters,
                    assembled_delta_units_total = metrics.assembled_delta_units,
                    assembled_no_frame_units_total = metrics.assembled_no_frame_units,
                    decoder_queue_accepted_total = metrics.decoder_queue_accepted,
                    capture_width = metrics.capture_width,
                    capture_height = metrics.capture_height,
                    encoder_width = metrics.encoder_width,
                    encoder_height = metrics.encoder_height,
                    decoder_width = metrics.decoder_width,
                    decoder_height = metrics.decoder_height,
                    selected_ice_pair = %metrics.selected_ice_pair,
                    rtc_outbound = %metrics.rtc_outbound_summary,
                    rtc_inbound = %metrics.rtc_inbound_summary,
                    p2p_connected = metrics.p2p_connected,
                    local_ice = metrics.local_ice_candidates,
                    remote_ice = metrics.remote_ice_candidates,
                    local_relay = metrics.local_relay_candidates,
                    remote_relay = metrics.remote_relay_candidates,
                    route = ?metrics.route,
                    encoder_backend = %metrics.encoder_backend,
                    encoder_fallback = metrics.encoder_fallback_reason.as_deref().unwrap_or(""),
                    decoder_backend = %metrics.decoder_backend,
                    decoder_preference = %metrics.decoder_preference,
                    decoder_fallback = metrics.decoder_fallback_reason.as_deref().unwrap_or(""),
                    encoded = metrics.encoded_frames,
                    sent = metrics.sent_frames,
                    received_delta_frames = metrics.received_delta_frames,
                    decoded_delta_frames = metrics.decoded_delta_frames,
                    received_packets_total = metrics.received_packets,
                    received_packets_interval = performance.received_packets,
                    sequence_gap_packets_interval = performance.observed_sequence_gaps,
                    reordered_packets_recovered_interval = performance.recovered_reordered_packets,
                    unmatched_out_of_order_interval = performance.unmatched_out_of_order_packets,
                    duplicate_packets_interval = performance.duplicate_packets,
                    confirmed_missing_packets_interval = performance.confirmed_missing_packets,
                    late_after_confirmed_packets_interval = performance.late_after_confirmed_packets,
                    sequence_gap_resyncs_interval = performance.sequence_gap_resyncs,
                    rtc_rtp_loss_delta = performance.inbound_rtp_lost_delta,
                    assembled_access_units_interval = performance.assembled_access_units,
                    assembly_errors_interval = performance.assembly_errors,
                    decoded = metrics.decoded_frames,
                    decode_errors = metrics.decode_errors,
                    decode_errors_interval = performance.decode_errors,
                    decoded_interval = performance.decoded_frames,
                    pli_sent_interval = performance.pli_requests_sent,
                    pli_received_interval = performance.pli_requests_received,
                    h264_pipeline_interval = %crate::screen_sharing::h264_pipeline_interval_label(&performance),
                    rtp_recovery_stats = %crate::screen_sharing::rtp_recovery_interval_label(metrics, &performance),
                    pli_queue_overflow = metrics.pli_queue_overflow,
                    rtc_rtp_out_packets = metrics.outbound_rtp_packets,
                    rtc_rtp_out_bytes = metrics.outbound_rtp_bytes,
                    rtc_rtp_in_packets = metrics.inbound_rtp_packets,
                    rtc_rtp_in_bytes = metrics.inbound_rtp_bytes,
                    rtc_rtp_in_lost = metrics.inbound_rtp_lost,
                    rtc_rtp_in_jitter_ms = metrics.inbound_rtp_jitter_ms,
                    rtp_reorder_window_ms = metrics.rtp_reorder_window_ms,
                    rtp_reorder_samples = metrics.rtp_reorder_samples,
                    decoder_input_frames = metrics.decoder_input_frames,
                    decoder_input_interval = performance.decoder_input_frames,
                    decoder_no_output_frames = metrics.decoder_no_output_frames,
                    decoder_no_output_interval = performance.decoder_no_output_frames,
                    decoder_queue_drops = metrics.decoder_queue_drops,
                    decoder_queue_drops_interval = performance.decoder_queue_drops,
                    published_frames = metrics.published_frames,
                    published_interval = performance.published_frames,
                    ui_texture_updates = metrics.ui_texture_updates,
                    ui_texture_updates_interval = performance.ui_texture_updates,
                    h264 = %metrics.h264_diagnostics,
                    last_decode_error = metrics.last_decode_error.as_deref().unwrap_or(""),
                    interval_seconds,
                    capture_fps = capture_performance.processed_frames as f64 / interval_seconds,
                    capture_callbacks = capture_performance.received_frames,
                    capture_processed = capture_performance.processed_frames,
                    capture_unchanged = capture_performance.unchanged_frames,
                    capture_skipped = capture_performance.skipped_frames,
                    capture_readback_avg_ms = average_ms(capture_performance.readback_nanos, capture_examined_frames),
                    capture_resize_avg_ms = average_ms(capture_performance.resize_nanos, capture_examined_frames),
                    capture_gpu_convert_avg_ms = average_ms(capture_performance.gpu_convert_nanos, capture_examined_frames),
                    encoder_new_capture_frames = performance.new_capture_frames,
                    encoder_repeated_capture_frames = performance.repeated_capture_frames,
                    encoder_worker_late_frames = performance.encoder_worker_late_frames,
                    skipped_capture_sequences = performance.skipped_capture_sequences,
                    encoder_input_frames = performance.encoder_input_frames,
                    encode_fps = performance.encoded_frames as f64 / interval_seconds,
                    encoded_frames = performance.encoded_frames,
                    encoded_idr_frames = performance.encoded_idr_frames,
                    encoded_delta_frames = performance.encoded_delta_frames,
                    encode_avg_ms = average_ms(performance.encode_nanos, performance.encode_samples),
                    send_fps = performance.sent_frames as f64 / interval_seconds,
                    send_frames = performance.sent_frames,
                    dropped_before_initial_idr = performance.dropped_before_initial_idr,
                    sent_idr_frames = performance.sent_idr_frames,
                    sent_delta_frames = performance.sent_delta_frames,
                    send_queue_wait_avg_ms = average_ms(performance.queue_wait_nanos, performance.queue_wait_samples),
                    write_sample_avg_ms = average_ms(performance.write_sample_nanos, performance.write_sample_samples),
                    write_sample_bytes = performance.write_sample_bytes,
                    write_sample_failures = performance.write_sample_failures,
                    rtc_rtp_out_packets_delta = performance.outbound_rtp_packets,
                    rtc_rtp_out_bytes_delta = performance.outbound_rtp_bytes,
                    rtc_rtp_in_packets_delta = performance.inbound_rtp_packets,
                    rtc_rtp_in_bytes_delta = performance.inbound_rtp_bytes,
                    "Resumo periódico da mídia de compartilhamento"
                );
                self.last_screen_metrics_log_at = Some(logged_at);
            }
        }
        let events = self
            .screen_share_session
            .as_ref()
            .map(|session| std::iter::from_fn(|| session.try_recv()).collect::<Vec<_>>())
            .unwrap_or_default();
        let screen_share_session = self.screen_share_metrics.session_id;
        let mut stop_session = false;
        for event in events {
            match event {
                ScreenShareEvent::Signal { kind, payload } => {
                    tracing::debug!(screen_share_session, signal_kind = ?kind, "Sinal de negociação de tela gerado; conteúdo omitido");
                    let result = self.send_screen_share_signal(kind, payload);
                    if let Err(error) = result {
                        tracing::error!(screen_share_session, error = %error, "Falha ao encaminhar sinal de tela pela sinalização");
                        self.screen_share_status = Some(error);
                        stop_session = true;
                    }
                }
                ScreenShareEvent::State(status) => {
                    tracing::info!(screen_share_session, state = %status, "Estado WebRTC de compartilhamento alterado");
                    self.screen_share_status = Some(status)
                }
                ScreenShareEvent::AudioState(status) => {
                    tracing::info!(screen_share_session, stage = "audio_pipeline_state", state = %status, "Estado agregado de áudio da sessão");
                    self.audio_status = Some(status);
                }
                ScreenShareEvent::Error(error) => {
                    tracing::error!(screen_share_session, error = %error, "Erro na sessão WebRTC de compartilhamento");
                    self.screen_share_status = Some(error);
                    stop_session = true;
                }
                ScreenShareEvent::AudioError(error) => {
                    tracing::error!(screen_share_session, stage = "audio_pipeline", error = %error, "Falha de áudio isolada; vídeo continua ativo");
                    self.screen_share_status = Some(error);
                }
                ScreenShareEvent::ConnectionClosed => {
                    tracing::warn!(
                        screen_share_session,
                        "Conexão P2P de compartilhamento encerrada"
                    );
                    self.screen_share_status =
                        Some("A conexão P2P de tela foi encerrada ou perdida.".to_owned());
                    stop_session = true;
                }
            }
        }
        if stop_session {
            let status = self.screen_share_status.clone();
            self.stop_screen_share(true);
            self.screen_share_status = status;
            return;
        }

        let receiving_stream_id = match &self.screen_share_role {
            ScreenShareRole::Receiving { request_id } => Some(request_id.clone()),
            _ => None,
        };
        if let Some(stream_id) = receiving_stream_id
            && let Some(session) = self.screen_share_session.as_ref()
            && let Some(decision) = session.observe_quality_feedback()
        {
            let peer_id = self
                .participants
                .iter()
                .find(|participant| participant.id != self.participant_id)
                .map(|participant| participant.id.clone());
            if let Some(peer_id) = peer_id {
                match decision {
                    QualityDecision::Changed {
                        feedback,
                        overload_evidence,
                    } => {
                        tracing::info!(
                            screen_share_session = session.metrics().session_id,
                            profile = ?feedback.profile,
                            reason = ?feedback.reason,
                            overload_evidence = ?overload_evidence,
                            "Solicitando ajuste adaptativo de qualidade"
                        );
                        self.send_quality_feedback(&peer_id, stream_id, feedback);
                    }
                    QualityDecision::StillOverloadedAtMinimum { overload_evidence } => {
                        self.log_minimum_profile_overload(&peer_id, &stream_id, overload_evidence);
                    }
                }
            }
        }

        let remote_frame = self
            .screen_share_session
            .as_ref()
            .and_then(ScreenShareSession::latest_remote_frame);
        if let Some(frame) = remote_frame
            && (self.remote_screen_sequence != frame.sequence
                || self
                    .remote_screen_texture
                    .as_ref()
                    .is_some_and(RemoteVideoTexture::needs_fallback))
        {
            let texture = self
                .remote_screen_texture
                .get_or_insert_with(|| self.remote_video_renderer.texture());
            let started = Instant::now();
            if let Some(session) = self.screen_share_session.as_ref() {
                match texture.update(context, Arc::clone(&frame), session.video_performance()) {
                    Ok(()) => {
                        session.record_ui_texture_update();
                        session.record_ui_texture_update_duration(started.elapsed());
                        if self.remote_screen_sequence == 0 {
                            self.screen_share_status =
                                Some("Primeiro quadro da tela recebido e decodificado.".to_owned());
                        }
                    }
                    Err(error) => {
                        tracing::error!(error = %error, "Falha ao preparar quadro remoto para apresentação");
                        self.screen_share_status =
                            Some("Não foi possível apresentar o quadro recebido.".to_owned());
                    }
                }
            }
            self.remote_screen_sequence = frame.sequence;
        }
    }

    pub(super) fn stop_screen_share(&mut self, announce: bool) {
        if self.group_share.local_sharing
            || !self.group_share.watched_shares.is_empty()
            || !self.group_share.inbound_sessions.is_empty()
            || !self.group_share.outbound_sessions.is_empty()
        {
            self.stop_group_media(announce);
        }
        let was_sending = matches!(&self.screen_share_role, ScreenShareRole::Sending { .. });
        let request_id = match &self.screen_share_role {
            ScreenShareRole::Requesting { request_id }
            | ScreenShareRole::Sending { request_id }
            | ScreenShareRole::Receiving { request_id } => Some(request_id.clone()),
            ScreenShareRole::Idle => None,
        };
        let was_active = request_id.is_some();
        if was_active {
            tracing::info!(
                announce,
                "Encerrando sessão de compartilhamento de tela; identificador omitido"
            );
        }
        if announce && let Some(request_id) = request_id {
            let _ = self.send_screen_share_signal(SignalKind::ScreenShareStopped, request_id);
        }
        if let Some(session) = self.screen_share_session.take() {
            let metrics = session.metrics();
            tracing::info!(
                video_pipeline_total = %metrics.video_performance.label(),
                screen_share_session = metrics.session_id,
                track_ssrc = ?metrics.track_ssrc,
                outbound_video_ssrc = ?metrics.outbound_video_ssrc,
                inbound_video_ssrc = ?metrics.inbound_video_ssrc,
                nack_sent_video_ssrc = ?metrics.inbound_video_ssrc,
                nack_received_video_ssrc = ?metrics.outbound_video_ssrc,
                nack_packets_received_observed_total = metrics.nack_packets_received_observed,
                nack_received_media_ssrc = ?metrics.nack_received_media_ssrc,
                nack_rtcp_packets_sent_total = %crate::screen_sharing::optional_count_label(metrics.nack_packets_sent),
                nack_rtcp_packets_received_total = %crate::screen_sharing::optional_count_label(metrics.nack_packets_received),
                retransmitted_packets_sent_total = %crate::screen_sharing::optional_count_label(metrics.retransmitted_packets_sent),
                retransmitted_bytes_sent_total = %crate::screen_sharing::optional_count_label(metrics.retransmitted_bytes_sent),
                retransmitted_packets_received_total = %crate::screen_sharing::optional_count_label(metrics.retransmitted_packets_received),
                retransmitted_bytes_received_total = %crate::screen_sharing::optional_count_label(metrics.retransmitted_bytes_received),
                role = match &self.screen_share_role {
                    ScreenShareRole::Sending { .. } => "sender",
                    ScreenShareRole::Receiving { .. } => "receiver",
                    ScreenShareRole::Requesting { .. } => "requesting",
                    ScreenShareRole::Idle => "idle",
                },
                phase = "final",
                termination_reason = "local_stop",
                video_track_seen = session.remote_video_track_seen(),
                encoded_frames = metrics.encoded_frames,
                sent_frames = metrics.sent_frames,
                received_rtp_packets = metrics.received_packets,
                inbound_rtp_packets = metrics.inbound_rtp_packets,
                decoded_frames = metrics.decoded_frames,
                published_frames = metrics.published_frames,
                decode_errors = metrics.decode_errors,
                rtp_reorder_window_ms = metrics.rtp_reorder_window_ms,
                rtp_reorder_samples = metrics.rtp_reorder_samples,
                selected_ice_pair = %metrics.selected_ice_pair,
                rtc_outbound = %metrics.rtc_outbound_summary,
                rtc_inbound = %metrics.rtc_inbound_summary,
                h264 = %metrics.h264_diagnostics,
                "Resumo final do compartilhamento antes de liberar a sessão"
            );
            session.stop();
        }
        self.last_screen_metrics_log_at = None;
        self.audio_status = None;
        self.screen_share_role = ScreenShareRole::Idle;
        self.remote_screen_texture = None;
        self.remote_screen_sequence = 0;
        if was_sending {
            self.clear_local_preview();
            self.capture_preview_enabled.store(false, Ordering::Relaxed);
        }
        if was_active {
            self.screen_share_status = Some("Compartilhamento de tela encerrado.".to_owned());
        }
    }

    pub(super) fn stun_server_for_room(&self) -> Option<String> {
        (self.room_mode == RoomMode::InternetTest).then(|| self.stun_server_url.trim().to_owned())
    }

    /// Sessão de compartilhamento de tela do usuário está ativa: pedido em
    /// andamento ou transmissão em curso (grupo ou peer direto).
    pub(super) fn screen_share_session_active(&self) -> bool {
        !matches!(&self.screen_share_role, ScreenShareRole::Idle) || self.group_share.local_sharing
    }

    /// Para a captura de tela ativa registrando a falha em vez de descartá-la
    /// silenciosamente; garante que nada fique rodando ao trocar de origem.
    pub(super) fn stop_running_screen_capture(&mut self) {
        if let Some(mut capture) = self.screen_capture.take() {
            self.active_share_source = None;
            if let Err(error) = capture.stop() {
                tracing::warn!(error = %error, "Falha ao parar a captura de tela que estava ativa");
            }
        }
    }
}

/// Decisão sobre a captura quando o usuário pede nova transmissão com outra
/// já em execução.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ShareCaptureRestart {
    /// Origem idêntica à captura em execução: o clique é ignorado em vez de
    /// reiniciar a captura, evitando fluxo duplo do compartilhamento.
    KeepRunning,
    /// Origem diferente, ou a origem da captura atual é desconhecida (caso do
    /// seletor nativo do Windows): pare a captura anterior explicitamente.
    ReplaceExisting,
}

pub(super) fn decide_share_capture_restart(
    active: Option<&ShareSourceSelection>,
    selected: &ShareSourceSelection,
) -> ShareCaptureRestart {
    if active.is_some_and(|active| active == selected) {
        ShareCaptureRestart::KeepRunning
    } else {
        ShareCaptureRestart::ReplaceExisting
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FAKE_MONITOR_DEVICE_ID: &str =
        r"\\?\DISPLAY#TESTE#0{00000000-0000-0000-0000-000000000000}";

    #[test]
    fn origem_igual_e_ativa_mantem_captura() {
        let source = ShareSourceSelection::Monitor(FAKE_MONITOR_DEVICE_ID.to_owned());
        assert_eq!(
            decide_share_capture_restart(Some(&source), &source),
            ShareCaptureRestart::KeepRunning
        );
    }

    #[test]
    fn origem_diferente_substitui_captura() {
        let active = ShareSourceSelection::Monitor(FAKE_MONITOR_DEVICE_ID.to_owned());
        let selected = ShareSourceSelection::Window(12345);
        assert_eq!(
            decide_share_capture_restart(Some(&active), &selected),
            ShareCaptureRestart::ReplaceExisting
        );
    }

    #[test]
    fn origem_atual_desconhecida_substitui_captura() {
        let selected = ShareSourceSelection::Monitor(FAKE_MONITOR_DEVICE_ID.to_owned());
        assert_eq!(
            decide_share_capture_restart(None, &selected),
            ShareCaptureRestart::ReplaceExisting
        );
    }
}
