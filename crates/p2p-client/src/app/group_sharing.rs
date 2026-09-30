use super::*;

fn group_share_bitrate(viewer_count: usize) -> u32 {
    (GROUP_SCREEN_MAX_AGGREGATE_BITRATE / viewer_count.max(1) as u32)
        .min(GROUP_SCREEN_MAX_PEER_BITRATE)
        .max(250_000)
}

fn group_screen_share_compatible(room_mode: RoomMode, participants: &[ParticipantInfo]) -> bool {
    room_mode == RoomMode::Local
        && (2..=8).contains(&participants.len())
        && participants
            .iter()
            .all(|participant| participant.supports_group_screen_share)
}

fn next_group_media_port(used_ports: &HashSet<u16>) -> Option<u16> {
    (9002..=9009).find(|port| !used_ports.contains(port))
}

fn group_outbound_rebalance_needed(
    current_viewers: &[String],
    current_bitrate_bps: Option<u32>,
    desired_viewers: &[String],
    desired_bitrate_bps: u32,
) -> bool {
    current_viewers != desired_viewers || current_bitrate_bps != Some(desired_bitrate_bps)
}

fn group_video_pipeline_stage(
    role: &str,
    p2p_connected: bool,
    remote_video_track_seen: bool,
    metrics: &ScreenShareMetrics,
) -> &'static str {
    if role == "sender" {
        if metrics.sent_frames == 0 {
            return "encoder_not_sending_frames";
        }
        if metrics.outbound_rtp_packets == 0 {
            return "frames_accepted_rtp_stats_pending_or_missing";
        }
        return "video_rtp_outbound";
    }
    if !p2p_connected {
        return "waiting_for_p2p";
    }
    if !remote_video_track_seen {
        return "no_remote_video_track";
    }
    if metrics.received_packets == 0 {
        return "video_track_without_rtp_packets";
    }
    if metrics.assembled_access_units == 0 {
        return "rtp_without_complete_h264_units";
    }
    if metrics.decoded_frames == 0 {
        return "h264_units_not_decoded";
    }
    if metrics.published_frames == 0 {
        return "decoded_frames_not_published";
    }
    if metrics.ui_texture_updates == 0 {
        return "published_frames_without_texture_update";
    }
    "group_texture_updated"
}

fn should_auto_focus_group_screen(current_focus: Option<&str>) -> bool {
    current_focus.is_none()
}

fn log_group_session_diagnostics(
    session: &ScreenShareSession,
    role: &'static str,
    interval_seconds: Option<f64>,
    phase: &'static str,
    termination_reason: Option<&'static str>,
) {
    let metrics = session.metrics();
    let performance = session.take_performance_snapshot();
    let route = match metrics.route {
        Some(crate::screen_sharing::MediaRoute::Direct) => "direct",
        Some(crate::screen_sharing::MediaRoute::Turn) => "turn",
        None => "unknown",
    };
    let track_ssrc = metrics
        .track_ssrc
        .map(|ssrc| ssrc.to_string())
        .unwrap_or_else(|| "unknown".to_owned());
    let selected_ice_pair = if metrics.selected_ice_pair.is_empty() {
        "unknown"
    } else {
        metrics.selected_ice_pair.as_str()
    };

    tracing::info!(
        screen_share_session = metrics.session_id,
        track_ssrc = %track_ssrc,
        role,
        phase,
        termination_reason = termination_reason.unwrap_or(""),
        interval_seconds = ?interval_seconds,
        p2p_connected = metrics.p2p_connected,
        video_track_seen = session.remote_video_track_seen(),
        video_pipeline_stage = group_video_pipeline_stage(
            role,
            metrics.p2p_connected,
            session.remote_video_track_seen(),
            &metrics,
        ),
        route,
        selected_ice_pair = %selected_ice_pair,
        video_ssrc = ?metrics.track_ssrc,
        outbound_video_ssrc = ?metrics.outbound_video_ssrc,
        inbound_video_ssrc = ?metrics.inbound_video_ssrc,
        encoder_backend = %metrics.encoder_backend,
        encoder_fallback = metrics.encoder_fallback_reason.as_deref().unwrap_or(""),
        decoder_backend = %metrics.decoder_backend,
        decoder_preference = %metrics.decoder_preference,
        decoder_fallback = metrics.decoder_fallback_reason.as_deref().unwrap_or(""),
        encoder_input_total = metrics.encoder_input_frames,
        encoded_total = metrics.encoded_frames,
        sent_total = metrics.sent_frames,
        received_packets_total = metrics.received_packets,
        assembled_delta_total = metrics.received_delta_frames,
        decoded_total = metrics.decoded_frames,
        published_total = metrics.published_frames,
        rtp_out_total = %metrics.rtc_outbound_summary,
        rtp_in_total = %metrics.rtc_inbound_summary,
        encoder_inputs_interval = performance.encoder_input_frames,
        capture_new_interval = performance.new_capture_frames,
        capture_repeated_interval = performance.repeated_capture_frames,
        encoded_interval = performance.encoded_frames,
        encoded_idr_interval = performance.encoded_idr_frames,
        encoded_p_interval = performance.encoded_delta_frames,
        sent_interval = performance.sent_frames,
        sent_idr_interval = performance.sent_idr_frames,
        sent_p_interval = performance.sent_delta_frames,
        rtp_out_packets_interval = performance.outbound_rtp_packets,
        rtp_out_bytes_interval = performance.outbound_rtp_bytes,
        rtp_in_packets_interval = performance.inbound_rtp_packets,
        rtp_in_bytes_interval = performance.inbound_rtp_bytes,
        rtp_in_lost_interval = performance.inbound_rtp_lost_delta,
        rtp_in_jitter_ms = metrics.inbound_rtp_jitter_ms,
        packets_accepted_interval = performance.received_packets,
        sequence_gaps_observed_interval = performance.observed_sequence_gaps,
        reordered_packets_recovered_interval = performance.recovered_reordered_packets,
        out_of_order_packets_interval = performance.unmatched_out_of_order_packets,
        duplicate_packets_interval = performance.duplicate_packets,
        packets_lost_confirmed_interval = performance.confirmed_missing_packets,
        late_packets_interval = performance.late_after_confirmed_packets,
        sequence_resyncs_interval = performance.sequence_gap_resyncs,
        access_units_assembled_interval = performance.assembled_access_units,
        assembly_errors_interval = performance.assembly_errors,
        decoder_inputs_interval = performance.decoder_input_frames,
        decoder_no_output_interval = performance.decoder_no_output_frames,
        decoder_queue_drops_interval = performance.decoder_queue_drops,
        decoded_interval = performance.decoded_frames,
        decode_errors_interval = performance.decode_errors,
        published_interval = performance.published_frames,
        texture_updates_interval = performance.ui_texture_updates,
        pli_sent_interval = performance.pli_requests_sent,
        pli_received_interval = performance.pli_requests_received,
        pli_sent_total = metrics.pli_requests_sent,
        pli_received_total = metrics.pli_requests_received,
        h264 = %metrics.h264_diagnostics,
        last_decode_error = metrics.last_decode_error.as_deref().unwrap_or(""),
        "Diagnóstico individual da sessão de compartilhamento em grupo"
    );
}

pub(super) fn log_final_group_session_diagnostics(
    session: &ScreenShareSession,
    role: &'static str,
    termination_reason: &'static str,
) {
    log_group_session_diagnostics(session, role, None, "final", Some(termination_reason));
}

impl ClientUi {
    pub(super) fn group_sharing_compatible(&self) -> bool {
        group_screen_share_compatible(self.room_mode, &self.participants)
    }

    pub(super) fn allocate_group_media_port(&self) -> Result<u16, String> {
        let used = self
            .group_outbound_ports
            .values()
            .chain(self.group_inbound_ports.values())
            .copied()
            .collect::<HashSet<_>>();
        next_group_media_port(&used)
            .ok_or_else(|| "Todas as portas de mídia UDP 9002–9009 estão ocupadas.".to_owned())
    }

    pub(super) fn set_group_share(&mut self, enabled: bool) {
        if enabled && !self.group_sharing_compatible() {
            self.screen_share_status = Some(if self.participants.len() > 2 {
                "O compartilhamento em grupo exige que todos atualizem para uma versão compatível."
                    .to_owned()
            } else {
                "O compartilhamento em grupo não está disponível nesta sala.".to_owned()
            });
            return;
        }
        let kind = if enabled {
            SignalKind::ScreenShareAvailable
        } else {
            SignalKind::ScreenShareUnavailable
        };
        match self.send_screen_share_signal(kind, String::new()) {
            Ok(()) => {
                self.group_local_sharing = enabled;
                if !enabled {
                    for session in self.group_outbound_sessions.values() {
                        log_final_group_session_diagnostics(session, "sender", "share_stopped");
                    }
                    self.group_outbound_sessions.clear();
                    self.group_outbound_ports.clear();
                    self.group_outbound_target_bitrate_bps = None;
                }
                self.screen_share_status = Some(if enabled {
                    "Sua tela está disponível. Cada participante escolhe se quer assistir."
                        .to_owned()
                } else {
                    "Você parou de compartilhar sua tela.".to_owned()
                });
                tracing::info!(enabled, "Estado de compartilhamento em grupo alterado");
            }
            Err(error) => self.screen_share_status = Some(error),
        }
    }

    pub(super) fn toggle_group_watch(&mut self, peer_id: &str) {
        if self.group_watched_shares.remove(peer_id) {
            self.group_auto_focus_pending.remove(peer_id);
            if let Err(error) = self.send_screen_share_signal_to(
                peer_id,
                SignalKind::ScreenShareUnwatch,
                String::new(),
            ) {
                self.screen_share_status = Some(error);
            }
            if let Some(session) = self.group_inbound_sessions.remove(peer_id) {
                log_final_group_session_diagnostics(&session, "receiver", "watch_stopped");
            }
            self.group_inbound_ports.remove(peer_id);
            self.group_remote_textures.remove(peer_id);
            self.group_remote_sequences.remove(peer_id);
            self.group_peer_status.remove(peer_id);
        } else if self.group_available_shares.contains(peer_id) {
            match self.send_screen_share_signal_to(
                peer_id,
                SignalKind::ScreenShareWatch,
                String::new(),
            ) {
                Ok(()) => {
                    self.group_watched_shares.insert(peer_id.to_owned());
                    self.group_auto_focus_pending.insert(peer_id.to_owned());
                    self.group_peer_status.insert(
                        peer_id.to_owned(),
                        "Pedido para assistir enviado…".to_owned(),
                    );
                }
                Err(error) => self.screen_share_status = Some(error),
            }
        }
    }

    pub(super) fn start_group_outbound(
        &mut self,
        viewer_id: String,
        context: &egui::Context,
        bitrate: u32,
    ) {
        let Some(source) = self
            .screen_capture
            .as_ref()
            .map(ScreenCapture::frame_source)
        else {
            self.screen_share_status = Some("Selecione uma tela antes de compartilhar.".to_owned());
            return;
        };
        let (address, port) = match (self.selected_media_ipv4(), self.allocate_group_media_port()) {
            (Ok(address), Ok(port)) => (address, port),
            (Err(error), _) | (_, Err(error)) => {
                self.screen_share_status = Some(error);
                return;
            }
        };
        match ScreenShareSession::new_with_port(
            context.clone(),
            address,
            port,
            None,
            None,
            self.video_decoder_preference,
        ) {
            Ok(session) => {
                if let Err(error) =
                    session.start_sending_with_options(source, bitrate, self.include_system_audio)
                {
                    self.screen_share_status = Some(error);
                } else {
                    self.group_outbound_ports.insert(viewer_id.clone(), port);
                    self.group_outbound_sessions.insert(viewer_id, session);
                }
            }
            Err(error) => self.screen_share_status = Some(error),
        }
    }

    pub(super) fn rebalance_group_outbound(
        &mut self,
        context: &egui::Context,
        additional_viewer: Option<String>,
    ) {
        let mut viewers = self
            .group_outbound_sessions
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        if let Some(viewer) = additional_viewer {
            if !viewers.contains(&viewer) {
                viewers.push(viewer);
            }
        }
        viewers.sort();
        if viewers.is_empty() {
            self.group_outbound_target_bitrate_bps = None;
            return;
        }
        let mut current_viewers = self
            .group_outbound_sessions
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        current_viewers.sort();
        let bitrate = group_share_bitrate(viewers.len());
        if !group_outbound_rebalance_needed(
            &current_viewers,
            self.group_outbound_target_bitrate_bps,
            &viewers,
            bitrate,
        ) {
            tracing::debug!(
                viewers = viewers.len(),
                target_bitrate_bps = bitrate,
                "Ignorando reequilíbrio de compartilhamento sem mudança de espectadores ou taxa"
            );
            return;
        }
        for viewer in self.group_outbound_sessions.keys() {
            let _ = self.send_screen_share_signal_to_stream(
                viewer,
                self.participant_id.clone(),
                SignalKind::ScreenShareStopped,
                String::new(),
            );
        }
        for session in self.group_outbound_sessions.values() {
            log_final_group_session_diagnostics(session, "sender", "bitrate_rebalance");
        }
        self.group_outbound_sessions.clear();
        self.group_outbound_ports.clear();
        self.group_outbound_target_bitrate_bps = Some(bitrate);
        for viewer in viewers {
            self.start_group_outbound(viewer, context, bitrate);
        }
        tracing::info!(
            viewers = self.group_outbound_sessions.len(),
            target_bitrate_bps = bitrate,
            "Encoder de grupo reequilibrado dentro do limite de banda"
        );
    }

    pub(super) fn stop_group_media(&mut self, announce: bool) {
        if announce && self.group_local_sharing {
            let _ =
                self.send_screen_share_signal(SignalKind::ScreenShareUnavailable, String::new());
        }
        if announce {
            let watched = self.group_watched_shares.drain().collect::<Vec<_>>();
            for peer_id in watched {
                let _ = self.send_screen_share_signal_to(
                    &peer_id,
                    SignalKind::ScreenShareUnwatch,
                    String::new(),
                );
            }
        } else {
            self.group_watched_shares.clear();
        }
        self.group_available_shares.clear();
        for session in self.group_outbound_sessions.values() {
            log_final_group_session_diagnostics(session, "sender", "room_media_stopped");
        }
        for session in self.group_inbound_sessions.values() {
            log_final_group_session_diagnostics(session, "receiver", "room_media_stopped");
        }
        self.group_outbound_sessions.clear();
        self.group_inbound_sessions.clear();
        self.group_outbound_ports.clear();
        self.group_outbound_target_bitrate_bps = None;
        self.group_inbound_ports.clear();
        self.group_remote_textures.clear();
        self.group_remote_sequences.clear();
        self.group_auto_focus_pending.clear();
        self.group_peer_status.clear();
        self.group_local_sharing = false;
        self.focused_group_screen = None;
    }

    pub(super) fn handle_group_peer_signal(
        &mut self,
        from_participant_id: Option<String>,
        stream_id: Option<String>,
        kind: SignalKind,
        payload: String,
        context: &egui::Context,
    ) {
        let Some(peer_id) = from_participant_id else {
            self.screen_share_status = Some(
                "O servidor não identificou o participante que enviou a negociação WebRTC."
                    .to_owned(),
            );
            return;
        };
        if !self.group_sharing_compatible() || peer_id == self.participant_id {
            return;
        }
        let Some(stream_id) = stream_id else {
            self.group_peer_status.insert(
                peer_id,
                "A negociação WebRTC veio sem identificação da transmissão.".to_owned(),
            );
            return;
        };
        if kind == SignalKind::Offer && stream_id != peer_id {
            tracing::warn!(signal_kind = ?kind, "Oferta de grupo não corresponde à identidade do transmissor");
            return;
        }
        if kind == SignalKind::Offer && !self.group_inbound_sessions.contains_key(&peer_id) {
            let (address, port) =
                match (self.selected_media_ipv4(), self.allocate_group_media_port()) {
                    (Ok(address), Ok(port)) => (address, port),
                    (Err(error), _) | (_, Err(error)) => {
                        self.screen_share_status = Some(error);
                        return;
                    }
                };
            match ScreenShareSession::new_with_port(
                context.clone(),
                address,
                port,
                None,
                None,
                self.video_decoder_preference,
            ) {
                Ok(session) => {
                    self.group_inbound_ports.insert(peer_id.clone(), port);
                    self.group_inbound_sessions.insert(peer_id.clone(), session);
                    self.group_peer_status
                        .insert(peer_id.clone(), "Negociando a tela recebida…".to_owned());
                }
                Err(error) => {
                    self.screen_share_status = Some(error);
                    return;
                }
            }
        }
        let session = match kind {
            SignalKind::Answer if stream_id == self.participant_id => {
                self.group_outbound_sessions.get(&peer_id)
            }
            SignalKind::Offer => self.group_inbound_sessions.get(&peer_id),
            SignalKind::IceCandidate if stream_id == self.participant_id => {
                self.group_outbound_sessions.get(&peer_id)
            }
            SignalKind::IceCandidate if stream_id == peer_id => {
                self.group_inbound_sessions.get(&peer_id)
            }
            _ => None,
        };
        if let Some(session) = session {
            if let Err(error) = session.handle_signal(kind, payload) {
                self.group_peer_status.insert(peer_id, error.clone());
                self.screen_share_status = Some(error);
            }
        } else {
            tracing::debug!(signal_kind = ?kind, "Ignorando sinal WebRTC sem sessão de grupo correspondente");
        }
    }

    pub(super) fn refresh_group_screen_shares(&mut self, context: &egui::Context) {
        let mut peer_ids = self
            .group_inbound_sessions
            .keys()
            .chain(self.group_outbound_sessions.keys())
            .cloned()
            .collect::<HashSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        peer_ids.sort();
        let mut outbound_membership_changed = false;
        for peer_id in peer_ids {
            let mut signal_events = Vec::new();
            let mut failures = Vec::new();
            let mut states = Vec::new();
            let mut inbound_failed = false;
            let mut outbound_failed = false;
            let mut inbound_termination_reason = "unknown";
            let mut outbound_termination_reason = "unknown";
            if let Some(session) = self.group_inbound_sessions.get(&peer_id) {
                while let Some(event) = session.try_recv() {
                    match event {
                        ScreenShareEvent::Signal { kind, payload } => {
                            signal_events.push((true, kind, payload))
                        }
                        ScreenShareEvent::State(state) => states.push(state),
                        ScreenShareEvent::Error(error) => {
                            inbound_failed = true;
                            inbound_termination_reason = "session_error";
                            failures.push(error);
                        }
                        ScreenShareEvent::AudioError(error) => {
                            tracing::error!(peer = %peer_id, stage = "audio_pipeline", error = %error, "Falha de áudio na sessão de grupo; vídeo continua ativo");
                            states.push(error);
                        }
                        ScreenShareEvent::ConnectionClosed => {
                            inbound_failed = true;
                            inbound_termination_reason = "connection_closed";
                            failures.push("A conexão P2P foi encerrada.".to_owned());
                        }
                    }
                }
                if let Some(frame) = session.latest_remote_frame() {
                    let sequence = self
                        .group_remote_sequences
                        .get(&peer_id)
                        .copied()
                        .unwrap_or(0);
                    if sequence != frame.sequence {
                        let image = egui::ColorImage::from_rgba_unmultiplied(
                            [frame.width as usize, frame.height as usize],
                            &frame.rgba,
                        );
                        if let Some(texture) = self.group_remote_textures.get_mut(&peer_id) {
                            texture.set(image, egui::TextureOptions::LINEAR);
                        } else {
                            self.group_remote_textures.insert(
                                peer_id.clone(),
                                context.load_texture(
                                    format!("group-screen-{peer_id}"),
                                    image,
                                    egui::TextureOptions::LINEAR,
                                ),
                            );
                        }
                        session.record_ui_texture_update();
                        if self.group_auto_focus_pending.remove(&peer_id)
                            && should_auto_focus_group_screen(self.focused_group_screen.as_deref())
                        {
                            self.focused_group_screen = Some(peer_id.clone());
                        }
                        self.group_remote_sequences
                            .insert(peer_id.clone(), frame.sequence);
                    }
                }
            }
            if let Some(session) = self.group_outbound_sessions.get(&peer_id) {
                while let Some(event) = session.try_recv() {
                    match event {
                        ScreenShareEvent::Signal { kind, payload } => {
                            signal_events.push((false, kind, payload))
                        }
                        ScreenShareEvent::State(state) => states.push(state),
                        ScreenShareEvent::Error(error) => {
                            outbound_failed = true;
                            outbound_termination_reason = "session_error";
                            failures.push(error);
                        }
                        ScreenShareEvent::AudioError(error) => {
                            tracing::error!(peer = %peer_id, stage = "audio_pipeline", error = %error, "Falha de áudio na sessão de grupo; vídeo continua ativo");
                            states.push(error);
                        }
                        ScreenShareEvent::ConnectionClosed => {
                            outbound_failed = true;
                            outbound_termination_reason = "connection_closed";
                            failures.push("A conexão P2P foi encerrada.".to_owned());
                        }
                    }
                }
            }
            for (is_inbound, kind, payload) in signal_events {
                let stream_id = if is_inbound {
                    peer_id.clone()
                } else {
                    self.participant_id.clone()
                };
                if let Err(error) =
                    self.send_screen_share_signal_to_stream(&peer_id, stream_id, kind, payload)
                {
                    if is_inbound {
                        inbound_failed = true;
                        inbound_termination_reason = "signal_forward_failed";
                    } else {
                        outbound_failed = true;
                        outbound_termination_reason = "signal_forward_failed";
                    }
                    failures.push(error);
                }
            }
            if let Some(state) = states.last() {
                self.group_peer_status
                    .insert(peer_id.clone(), state.clone());
            }
            if let Some(error) = failures.last() {
                tracing::error!(error = %error, "Falha isolada em sessão de tela de participante");
                self.group_peer_status
                    .insert(peer_id.clone(), error.clone());
            }
            if inbound_failed {
                if let Some(session) = self.group_inbound_sessions.remove(&peer_id) {
                    log_final_group_session_diagnostics(
                        &session,
                        "receiver",
                        inbound_termination_reason,
                    );
                }
                self.group_inbound_ports.remove(&peer_id);
                self.group_remote_textures.remove(&peer_id);
                self.group_remote_sequences.remove(&peer_id);
                self.group_watched_shares.remove(&peer_id);
                self.group_auto_focus_pending.remove(&peer_id);
                let _ = self.send_screen_share_signal_to(
                    &peer_id,
                    SignalKind::ScreenShareUnwatch,
                    String::new(),
                );
            }
            if outbound_failed {
                if let Some(session) = self.group_outbound_sessions.remove(&peer_id) {
                    log_final_group_session_diagnostics(
                        &session,
                        "sender",
                        outbound_termination_reason,
                    );
                }
                self.group_outbound_ports.remove(&peer_id);
                let _ = self.send_screen_share_signal_to_stream(
                    &peer_id,
                    self.participant_id.clone(),
                    SignalKind::ScreenShareStopped,
                    String::new(),
                );
                outbound_membership_changed = true;
            }
        }
        if outbound_membership_changed {
            self.rebalance_group_outbound(context, None);
        }

        if self
            .last_group_metrics_log_at
            .is_none_or(|last| last.elapsed() >= Duration::from_secs(5))
        {
            let interval_seconds = self
                .last_group_metrics_log_at
                .map(|last| last.elapsed().as_secs_f64());
            for session in self.group_outbound_sessions.values() {
                log_group_session_diagnostics(
                    session,
                    "sender",
                    interval_seconds,
                    "periodic",
                    None,
                );
            }
            for session in self.group_inbound_sessions.values() {
                log_group_session_diagnostics(
                    session,
                    "receiver",
                    interval_seconds,
                    "periodic",
                    None,
                );
            }

            let outbound = self
                .group_outbound_sessions
                .values()
                .map(ScreenShareSession::metrics)
                .collect::<Vec<_>>();
            let inbound = self
                .group_inbound_sessions
                .values()
                .map(ScreenShareSession::metrics)
                .collect::<Vec<_>>();
            tracing::info!(
                outbound_peers = outbound.len(),
                inbound_peers = inbound.len(),
                encoded_frames = outbound.iter().map(|m| m.encoded_frames).sum::<u64>(),
                sent_frames = outbound.iter().map(|m| m.sent_frames).sum::<u64>(),
                received_packets = inbound.iter().map(|m| m.received_packets).sum::<u64>(),
                decoded_frames = inbound.iter().map(|m| m.decoded_frames).sum::<u64>(),
                decode_errors = inbound.iter().map(|m| m.decode_errors).sum::<u64>(),
                watching = self.group_watched_shares.len(),
                sharing = self.group_local_sharing,
                "Resumo agregado de compartilhamento em grupo"
            );
            self.last_group_metrics_log_at = Some(Instant::now());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn participant(id: usize, supports_group_screen_share: bool) -> ParticipantInfo {
        ParticipantInfo {
            id: format!("participant-{id}"),
            display_name: format!("Participante {id}"),
            order: id as u8,
            may_host: false,
            control_address: String::new(),
            avatar_jpeg_base64: None,
            supports_group_screen_share,
        }
    }

    #[test]
    fn group_sender_bitrate_stays_within_aggregate_limit() {
        for viewers in [1, 2, 7] {
            let per_peer = super::group_share_bitrate(viewers);
            assert!(per_peer <= 4_000_000);
            assert!(u64::from(per_peer) * viewers as u64 <= 8_000_000);
        }
        assert_eq!(super::group_share_bitrate(1), 4_000_000);
        assert_eq!(super::group_share_bitrate(2), 4_000_000);
        assert_eq!(super::group_share_bitrate(7), 1_142_857);
    }

    #[test]
    fn group_sharing_requires_two_to_eight_compatible_local_participants() {
        let eight = (1..=8).map(|id| participant(id, true)).collect::<Vec<_>>();
        assert!(group_screen_share_compatible(RoomMode::Local, &eight));

        let mut old_client = eight[..2].to_vec();
        old_client[1].supports_group_screen_share = false;
        assert!(!group_screen_share_compatible(RoomMode::Local, &old_client));
        assert!(!group_screen_share_compatible(
            RoomMode::InternetTest,
            &eight[..2]
        ));
        assert!(!group_screen_share_compatible(RoomMode::Local, &eight[..1]));
        assert!(!group_screen_share_compatible(
            RoomMode::Local,
            &(1..=9).map(|id| participant(id, true)).collect::<Vec<_>>()
        ));
    }

    #[test]
    fn group_media_ports_are_unique_and_limited_to_the_documented_range() {
        let mut used = HashSet::new();
        for expected in 9002..=9009 {
            let port = next_group_media_port(&used).unwrap();
            assert_eq!(port, expected);
            assert!(used.insert(port));
        }
        assert_eq!(next_group_media_port(&used), None);
    }

    #[test]
    fn duplicate_rebalance_does_not_restart_unchanged_viewers() {
        let current = vec!["viewer-a".to_owned(), "viewer-b".to_owned()];
        assert!(!group_outbound_rebalance_needed(
            &current,
            Some(group_share_bitrate(current.len())),
            &current,
            group_share_bitrate(current.len()),
        ));
        assert!(group_outbound_rebalance_needed(
            &current,
            Some(group_share_bitrate(current.len())),
            &["viewer-a".to_owned()],
            group_share_bitrate(1),
        ));
        assert!(group_outbound_rebalance_needed(
            &current,
            Some(1_000_000),
            &current,
            2_000_000,
        ));
    }

    #[test]
    fn group_video_stage_identifies_the_first_missing_stage() {
        let mut metrics = ScreenShareMetrics::default();
        assert_eq!(
            group_video_pipeline_stage("receiver", true, false, &metrics),
            "no_remote_video_track"
        );
        assert_eq!(
            group_video_pipeline_stage("receiver", true, true, &metrics),
            "video_track_without_rtp_packets"
        );
        metrics.received_packets = 10;
        assert_eq!(
            group_video_pipeline_stage("receiver", true, true, &metrics),
            "rtp_without_complete_h264_units"
        );
        metrics.assembled_access_units = 1;
        assert_eq!(
            group_video_pipeline_stage("receiver", true, true, &metrics),
            "h264_units_not_decoded"
        );
        metrics.decoded_frames = 1;
        metrics.published_frames = 1;
        assert_eq!(
            group_video_pipeline_stage("receiver", true, true, &metrics),
            "published_frames_without_texture_update"
        );
        metrics.ui_texture_updates = 1;
        assert_eq!(
            group_video_pipeline_stage("receiver", true, true, &metrics),
            "group_texture_updated"
        );
    }

    #[test]
    fn first_incoming_screen_auto_focuses_only_when_no_screen_is_focused() {
        assert!(should_auto_focus_group_screen(None));
        assert!(!should_auto_focus_group_screen(Some("another-screen")));
    }
}
