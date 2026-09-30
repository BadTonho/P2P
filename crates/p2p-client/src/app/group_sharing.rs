use super::*;

const PENDING_GROUP_ICE_TTL: Duration = Duration::from_secs(15);
const CLOSED_GROUP_GENERATION_TTL: Duration = Duration::from_secs(60);
const MAX_PENDING_GROUP_ICE_PER_SESSION: usize = 64;
const MAX_PENDING_GROUP_ICE_TOTAL: usize = 128;
const MAX_PENDING_GROUP_ICE_SESSIONS: usize = 32;
const MAX_PENDING_GROUP_ICE_PAYLOAD_BYTES: usize = 4_096;

fn group_share_bitrate(viewer_count: usize) -> u32 {
    (GROUP_SCREEN_MAX_AGGREGATE_BITRATE / viewer_count.max(1) as u32)
        .min(GROUP_SCREEN_MAX_PEER_BITRATE)
        .max(250_000)
}

fn group_screen_share_compatible(room_mode: RoomMode, participants: &[ParticipantInfo]) -> bool {
    room_mode == RoomMode::Local
        && (2..=8).contains(&participants.len())
        && participants.iter().all(|participant| {
            participant.supports_group_screen_share && participant.supports_group_session_ids
        })
}

fn group_generation_id(participant_id: &str, session_id: u64) -> String {
    format!("{GROUP_SIGNAL_ID_PREFIX}:{participant_id}:{session_id}")
}

fn is_group_generation_for_peer(peer_id: &str, generation: &str) -> bool {
    let prefix = format!("{GROUP_SIGNAL_ID_PREFIX}:{peer_id}:");
    generation
        .strip_prefix(&prefix)
        .is_some_and(|sequence| !sequence.is_empty() && sequence.parse::<u64>().is_ok())
}

fn group_signal_generation_is_well_formed(generation: &str) -> bool {
    generation.len() <= 128
        && generation.split_once(':').is_some_and(|(prefix, rest)| {
            prefix == GROUP_SIGNAL_ID_PREFIX
                && rest
                    .rsplit_once(':')
                    .is_some_and(|(_, id)| !id.is_empty() && id.parse::<u64>().is_ok())
        })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GroupSignalRoute {
    IncomingOffer,
    Outbound,
    Inbound,
    QueueEarlyIce,
    Ignore,
}

fn route_group_signal(
    peer_id: &str,
    generation: &str,
    kind: SignalKind,
    watched: bool,
    outbound_generation: Option<&str>,
    inbound_generation: Option<&str>,
) -> GroupSignalRoute {
    match kind {
        SignalKind::Offer if watched && is_group_generation_for_peer(peer_id, generation) => {
            GroupSignalRoute::IncomingOffer
        }
        SignalKind::Answer if outbound_generation == Some(generation) => GroupSignalRoute::Outbound,
        SignalKind::IceCandidate if outbound_generation == Some(generation) => {
            GroupSignalRoute::Outbound
        }
        SignalKind::IceCandidate if inbound_generation == Some(generation) => {
            GroupSignalRoute::Inbound
        }
        SignalKind::IceCandidate
            if watched && is_group_generation_for_peer(peer_id, generation) =>
        {
            GroupSignalRoute::QueueEarlyIce
        }
        _ => GroupSignalRoute::Ignore,
    }
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
    capture_fps: Option<f64>,
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
    let rate_window = interval_seconds.unwrap_or(5.0).max(0.001);
    let encoder_input_fps = performance.encoder_input_frames as f64 / rate_window;
    let encode_fps = performance.encoded_frames as f64 / rate_window;
    let send_fps = performance.sent_frames as f64 / rate_window;
    let receive_fps = performance.assembled_access_units as f64 / rate_window;
    let decode_fps = performance.decoded_frames as f64 / rate_window;
    let publish_fps = performance.published_frames as f64 / rate_window;

    tracing::info!(
        screen_share_session = metrics.session_id,
        track_ssrc = %track_ssrc,
        role,
        phase,
        termination_reason = termination_reason.unwrap_or(""),
        interval_seconds = ?interval_seconds,
        p2p_connected = metrics.p2p_connected,
        capture_width = metrics.capture_width,
        capture_height = metrics.capture_height,
        capture_fps = ?capture_fps,
        encoder_width = metrics.encoder_width,
        encoder_height = metrics.encoder_height,
        decoder_width = metrics.decoder_width,
        decoder_height = metrics.decoder_height,
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
        rtp_reorder_window_ms = metrics.rtp_reorder_window_ms,
        rtp_reorder_samples = metrics.rtp_reorder_samples,
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
        nack_requests_sent_interval = performance.nack_requests_sent,
        nack_requests_received_interval = performance.nack_requests_received,
        nack_requests_sent_total = metrics.nack_requests_sent,
        nack_requests_received_total = metrics.nack_requests_received,
        encoder_input_fps,
        encode_fps,
        send_fps,
        receive_fps,
        decode_fps,
        publish_fps,
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
    log_group_session_diagnostics(session, role, None, None, "final", Some(termination_reason));
}

impl ClientUi {
    pub(super) fn group_sharing_compatible(&self) -> bool {
        group_screen_share_compatible(self.room_mode, &self.participants)
    }

    pub(super) fn group_sharing_upgrade_required(&self) -> bool {
        self.room_mode == RoomMode::Local
            && (2..=8).contains(&self.participants.len())
            && self.participants.iter().any(|participant| {
                !participant.supports_group_screen_share || !participant.supports_group_session_ids
            })
    }

    fn prune_group_signal_state(&mut self) {
        let now = Instant::now();
        self.closed_group_generations.retain(|_, closed_at| {
            now.saturating_duration_since(*closed_at) < CLOSED_GROUP_GENERATION_TTL
        });
        self.pending_group_ice.retain(|_, candidates| {
            candidates.retain(|candidate| {
                now.saturating_duration_since(candidate.queued_at) < PENDING_GROUP_ICE_TTL
            });
            !candidates.is_empty()
        });
    }

    pub(super) fn mark_group_generation_closed(&mut self, peer_id: &str, generation: String) {
        self.pending_group_ice
            .remove(&(peer_id.to_owned(), generation.clone()));
        self.closed_group_generations
            .insert((peer_id.to_owned(), generation), Instant::now());
        if self.closed_group_generations.len() > 128 {
            if let Some(oldest) = self
                .closed_group_generations
                .iter()
                .min_by_key(|(_, closed_at)| **closed_at)
                .map(|(key, _)| key.clone())
            {
                self.closed_group_generations.remove(&oldest);
            }
        }
    }

    fn queue_early_group_ice(&mut self, peer_id: &str, generation: &str, payload: String) {
        self.prune_group_signal_state();
        let key = (peer_id.to_owned(), generation.to_owned());
        if payload.len() > MAX_PENDING_GROUP_ICE_PAYLOAD_BYTES {
            tracing::warn!(
                candidate_bytes = payload.len(),
                "Candidato ICE antecipado excedeu o limite de tamanho e foi descartado"
            );
            return;
        }
        if self.closed_group_generations.contains_key(&key) {
            tracing::debug!(
                signal_kind = "ice_candidate",
                "ICE tardio descartado para geração já encerrada"
            );
            return;
        }
        if !is_group_generation_for_peer(peer_id, generation) {
            tracing::debug!(
                signal_kind = "ice_candidate",
                "ICE sem sessão ativa descartado; geração não pertence ao remetente"
            );
            return;
        }
        if !self.pending_group_ice.contains_key(&key)
            && self.pending_group_ice.len() >= MAX_PENDING_GROUP_ICE_SESSIONS
        {
            if let Some(oldest) = self
                .pending_group_ice
                .iter()
                .min_by_key(|(_, candidates)| candidates.front().map(|item| item.queued_at))
                .map(|(key, _)| key.clone())
            {
                self.pending_group_ice.remove(&oldest);
            }
        }
        let total = self
            .pending_group_ice
            .values()
            .map(VecDeque::len)
            .sum::<usize>();
        if total >= MAX_PENDING_GROUP_ICE_TOTAL {
            if let Some(oldest) = self
                .pending_group_ice
                .iter()
                .min_by_key(|(_, candidates)| candidates.front().map(|item| item.queued_at))
                .map(|(key, _)| key.clone())
            {
                if let Some(candidates) = self.pending_group_ice.get_mut(&oldest) {
                    candidates.pop_front();
                    if candidates.is_empty() {
                        self.pending_group_ice.remove(&oldest);
                    }
                }
            }
        }
        let candidates = self.pending_group_ice.entry(key).or_default();
        if candidates.len() >= MAX_PENDING_GROUP_ICE_PER_SESSION {
            candidates.pop_front();
        }
        candidates.push_back(PendingGroupIce {
            queued_at: Instant::now(),
            payload,
        });
        tracing::debug!(
            pending_candidates = candidates.len(),
            "Candidato ICE antecipado guardado temporariamente; payload omitido"
        );
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
            self.screen_share_status = Some(if self.group_sharing_upgrade_required() {
                "O compartilhamento em grupo com correlação segura exige a versão 1.1.3 em todos os participantes. Atualizem antes de compartilhar.".to_owned()
            } else if self.participants.len() > 2 {
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
                self.audio_status = None;
                if !enabled {
                    let generations = self
                        .group_outbound_generations
                        .iter()
                        .map(|(peer, generation)| (peer.clone(), generation.clone()))
                        .collect::<Vec<_>>();
                    for (peer_id, generation) in generations {
                        let _ = self.send_screen_share_signal_to_stream(
                            &peer_id,
                            generation.clone(),
                            SignalKind::ScreenShareStopped,
                            String::new(),
                        );
                        self.mark_group_generation_closed(&peer_id, generation);
                    }
                    for session in self.group_outbound_sessions.values() {
                        log_final_group_session_diagnostics(session, "sender", "share_stopped");
                    }
                    self.group_outbound_sessions.clear();
                    self.group_outbound_generations.clear();
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
            if let Some(generation) = self.group_inbound_generations.remove(peer_id) {
                self.mark_group_generation_closed(peer_id, generation);
            }
            self.group_inbound_ports.remove(peer_id);
            self.group_remote_textures.remove(peer_id);
            self.group_remote_sequences.remove(peer_id);
            self.group_peer_status.remove(peer_id);
            self.group_audio_status.remove(peer_id);
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
                let generation =
                    group_generation_id(&self.participant_id, session.metrics().session_id);
                if let Err(error) =
                    session.start_sending_with_options(source, bitrate, self.include_system_audio)
                {
                    self.screen_share_status = Some(error);
                } else {
                    self.group_outbound_ports.insert(viewer_id.clone(), port);
                    self.group_outbound_generations
                        .insert(viewer_id.clone(), generation.clone());
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
        let old_generations = self
            .group_outbound_generations
            .iter()
            .map(|(peer, generation)| (peer.clone(), generation.clone()))
            .collect::<Vec<_>>();
        for (viewer, generation) in old_generations {
            let _ = self.send_screen_share_signal_to_stream(
                &viewer,
                generation.clone(),
                SignalKind::ScreenShareStopped,
                String::new(),
            );
            self.mark_group_generation_closed(&viewer, generation);
        }
        for session in self.group_outbound_sessions.values() {
            log_final_group_session_diagnostics(session, "sender", "bitrate_rebalance");
        }
        self.group_outbound_sessions.clear();
        self.group_outbound_generations.clear();
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
        for (peer_id, generation) in std::mem::take(&mut self.group_outbound_generations) {
            if announce {
                let _ = self.send_screen_share_signal_to_stream(
                    &peer_id,
                    generation.clone(),
                    SignalKind::ScreenShareStopped,
                    String::new(),
                );
            }
            self.mark_group_generation_closed(&peer_id, generation);
        }
        for session in self.group_inbound_sessions.values() {
            log_final_group_session_diagnostics(session, "receiver", "room_media_stopped");
        }
        for (peer_id, generation) in std::mem::take(&mut self.group_inbound_generations) {
            self.mark_group_generation_closed(&peer_id, generation);
        }
        self.group_outbound_sessions.clear();
        self.group_inbound_sessions.clear();
        self.group_outbound_ports.clear();
        self.group_outbound_target_bitrate_bps = None;
        self.group_inbound_ports.clear();
        self.pending_group_ice.clear();
        self.group_remote_textures.clear();
        self.group_remote_sequences.clear();
        self.group_auto_focus_pending.clear();
        self.group_peer_status.clear();
        self.group_audio_status.clear();
        self.group_local_sharing = false;
        self.audio_status = None;
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
        self.prune_group_signal_state();
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
        if !group_signal_generation_is_well_formed(&stream_id) {
            tracing::warn!(
                signal_kind = ?kind,
                "Sinal de grupo com identificador de sessão inválido; payload omitido"
            );
            return;
        }
        let signal_route = route_group_signal(
            &peer_id,
            &stream_id,
            kind,
            self.group_watched_shares.contains(&peer_id),
            self.group_outbound_generations
                .get(&peer_id)
                .map(String::as_str),
            self.group_inbound_generations
                .get(&peer_id)
                .map(String::as_str),
        );
        if kind == SignalKind::Offer && signal_route != GroupSignalRoute::IncomingOffer {
            tracing::warn!(
                signal_kind = ?kind,
                "Oferta de grupo sem pedido de exibição ou com geração inválida; payload omitido"
            );
            return;
        }
        let generation_key = (peer_id.clone(), stream_id.clone());
        if self.closed_group_generations.contains_key(&generation_key) {
            tracing::debug!(
                signal_kind = ?kind,
                "Sinal atrasado descartado para sessão de grupo encerrada"
            );
            return;
        }

        if kind == SignalKind::Offer {
            if self.group_inbound_generations.get(&peer_id) == Some(&stream_id) {
                tracing::debug!("Oferta duplicada ignorada para sessão de grupo já ativa");
                return;
            }
            if let Some(old_generation) = self.group_inbound_generations.remove(&peer_id) {
                if let Some(old_session) = self.group_inbound_sessions.remove(&peer_id) {
                    log_final_group_session_diagnostics(
                        &old_session,
                        "receiver",
                        "replaced_by_new_generation",
                    );
                }
                self.mark_group_generation_closed(&peer_id, old_generation);
                self.group_inbound_ports.remove(&peer_id);
                self.group_remote_textures.remove(&peer_id);
                self.group_remote_sequences.remove(&peer_id);
            }
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
                    self.group_inbound_generations
                        .insert(peer_id.clone(), stream_id.clone());
                    self.group_peer_status
                        .insert(peer_id.clone(), "Negociando a tela recebida…".to_owned());
                }
                Err(error) => {
                    self.screen_share_status = Some(error);
                    return;
                }
            }
        }
        if kind == SignalKind::Offer {
            if let Some(session) = self.group_inbound_sessions.get(&peer_id) {
                if let Err(error) = session.handle_signal(kind, payload) {
                    self.group_peer_status
                        .insert(peer_id.clone(), error.clone());
                    self.screen_share_status = Some(error);
                    return;
                }
                if let Some(mut candidates) = self.pending_group_ice.remove(&generation_key) {
                    let mut applied = 0usize;
                    while let Some(candidate) = candidates.pop_front() {
                        if candidate.queued_at.elapsed() < PENDING_GROUP_ICE_TTL
                            && session
                                .handle_signal(SignalKind::IceCandidate, candidate.payload)
                                .is_ok()
                        {
                            applied += 1;
                        }
                    }
                    tracing::debug!(
                        applied_candidates = applied,
                        "Candidatos ICE antecipados associados à oferta; payloads omitidos"
                    );
                }
            }
            return;
        }

        let session = match signal_route {
            GroupSignalRoute::Outbound => self.group_outbound_sessions.get(&peer_id),
            GroupSignalRoute::Inbound => self.group_inbound_sessions.get(&peer_id),
            _ => None,
        };
        if let Some(session) = session {
            if let Err(error) = session.handle_signal(kind, payload) {
                self.group_peer_status.insert(peer_id, error.clone());
                self.screen_share_status = Some(error);
            }
        } else if signal_route == GroupSignalRoute::QueueEarlyIce {
            self.queue_early_group_ice(&peer_id, &stream_id, payload);
        } else {
            tracing::debug!(
                signal_kind = ?kind,
                "Ignorando sinal WebRTC sem geração ativa correspondente; payload omitido"
            );
        }
    }

    pub(super) fn refresh_group_screen_shares(&mut self, context: &egui::Context) {
        self.prune_group_signal_state();
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
                        ScreenShareEvent::AudioState(state) => {
                            self.group_audio_status.insert(peer_id.clone(), state);
                        }
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
                        ScreenShareEvent::AudioState(state) => {
                            self.group_audio_status.insert(peer_id.clone(), state);
                        }
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
                let generation = if is_inbound {
                    self.group_inbound_generations.get(&peer_id)
                } else {
                    self.group_outbound_generations.get(&peer_id)
                };
                let result = generation
                    .ok_or_else(|| {
                        "A sessão de grupo não tem identificador ativo para encaminhar o sinal."
                            .to_owned()
                    })
                    .and_then(|generation| {
                        self.send_screen_share_signal_to_stream(
                            &peer_id,
                            generation.clone(),
                            kind,
                            payload,
                        )
                    });
                if let Err(error) = result {
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
                if let Some(generation) = self.group_inbound_generations.remove(&peer_id) {
                    self.mark_group_generation_closed(&peer_id, generation);
                }
                self.group_inbound_ports.remove(&peer_id);
                self.group_remote_textures.remove(&peer_id);
                self.group_remote_sequences.remove(&peer_id);
                self.group_watched_shares.remove(&peer_id);
                self.group_auto_focus_pending.remove(&peer_id);
                self.group_audio_status.remove(&peer_id);
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
                let generation = self.group_outbound_generations.remove(&peer_id);
                self.group_outbound_ports.remove(&peer_id);
                if let Some(generation) = generation {
                    let _ = self.send_screen_share_signal_to_stream(
                        &peer_id,
                        generation.clone(),
                        SignalKind::ScreenShareStopped,
                        String::new(),
                    );
                    self.mark_group_generation_closed(&peer_id, generation);
                }
                self.group_audio_status.remove(&peer_id);
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
            let capture_performance = self
                .screen_capture
                .as_ref()
                .map(ScreenCapture::take_performance_snapshot);
            let capture_fps = capture_performance.map(|snapshot| {
                snapshot.processed_frames as f64 / interval_seconds.unwrap_or(5.0).max(0.001)
            });
            for session in self.group_outbound_sessions.values() {
                log_group_session_diagnostics(
                    session,
                    "sender",
                    interval_seconds,
                    capture_fps,
                    "periodic",
                    None,
                );
            }
            for session in self.group_inbound_sessions.values() {
                log_group_session_diagnostics(
                    session,
                    "receiver",
                    interval_seconds,
                    None,
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
                local_capture_fps = ?capture_fps,
                capture_received = capture_performance.map(|snapshot| snapshot.received_frames),
                capture_processed = capture_performance.map(|snapshot| snapshot.processed_frames),
                capture_unchanged = capture_performance.map(|snapshot| snapshot.unchanged_frames),
                capture_skipped = capture_performance.map(|snapshot| snapshot.skipped_frames),
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
            supports_group_session_ids: supports_group_screen_share,
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
        let mut peer_without_generation_ids = eight[..2].to_vec();
        peer_without_generation_ids[1].supports_group_session_ids = false;
        assert!(!group_screen_share_compatible(
            RoomMode::Local,
            &peer_without_generation_ids
        ));
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

    #[test]
    fn group_generation_ids_are_bound_to_the_offering_participant_and_session() {
        let generation = group_generation_id("peer-a", 42);
        let replacement = group_generation_id("peer-a", 43);
        assert!(group_signal_generation_is_well_formed(&generation));
        assert!(is_group_generation_for_peer("peer-a", &generation));
        assert!(!is_group_generation_for_peer("peer-b", &generation));
        assert_eq!(
            route_group_signal("peer-a", &generation, SignalKind::Offer, true, None, None,),
            GroupSignalRoute::IncomingOffer
        );
        assert_eq!(
            route_group_signal(
                "peer-a",
                &generation,
                SignalKind::Answer,
                true,
                Some(&replacement),
                None,
            ),
            GroupSignalRoute::Ignore
        );
        assert_eq!(
            route_group_signal(
                "peer-a",
                &generation,
                SignalKind::IceCandidate,
                true,
                None,
                None,
            ),
            GroupSignalRoute::QueueEarlyIce
        );
        assert_eq!(
            route_group_signal(
                "peer-a",
                &generation,
                SignalKind::IceCandidate,
                true,
                Some(&replacement),
                Some(&replacement),
            ),
            GroupSignalRoute::QueueEarlyIce
        );
        assert_eq!(
            route_group_signal(
                "peer-a",
                &replacement,
                SignalKind::IceCandidate,
                true,
                Some(&replacement),
                None,
            ),
            GroupSignalRoute::Outbound
        );
        assert!(!group_signal_generation_is_well_formed("peer-a"));
        assert!(!group_signal_generation_is_well_formed(
            "p2p-group-session-v1:peer-a:not-a-number"
        ));
        assert!(!group_signal_generation_is_well_formed(&format!(
            "{}{}",
            generation,
            "x".repeat(128)
        )));
    }

    #[test]
    fn early_group_ice_is_bounded_expires_and_does_not_resurrect_closed_sessions() {
        let mut app = ClientUi::default();
        let generation = group_generation_id("peer-a", 99);
        for index in 0..(MAX_PENDING_GROUP_ICE_PER_SESSION + 5) {
            app.queue_early_group_ice("peer-a", &generation, format!("candidate-{index}"));
        }
        let key = ("peer-a".to_owned(), generation.clone());
        assert_eq!(
            app.pending_group_ice.get(&key).map(VecDeque::len),
            Some(MAX_PENDING_GROUP_ICE_PER_SESSION)
        );

        if let Some(candidates) = app.pending_group_ice.get_mut(&key) {
            for candidate in candidates {
                candidate.queued_at =
                    Instant::now() - PENDING_GROUP_ICE_TTL - Duration::from_millis(1);
            }
        }
        app.prune_group_signal_state();
        assert!(!app.pending_group_ice.contains_key(&key));

        app.mark_group_generation_closed("peer-a", generation.clone());
        app.queue_early_group_ice("peer-a", &generation, "late-candidate".to_owned());
        assert!(!app.pending_group_ice.contains_key(&key));
    }

    #[test]
    fn concurrent_group_sessions_route_ice_independently_and_ignore_closed_generation() {
        let generation_a = group_generation_id("peer-a", 42);
        let generation_b = group_generation_id("peer-b", 77);

        assert_eq!(
            route_group_signal("peer-a", &generation_a, SignalKind::Offer, true, None, None,),
            GroupSignalRoute::IncomingOffer
        );
        assert_eq!(
            route_group_signal("peer-b", &generation_b, SignalKind::Offer, true, None, None,),
            GroupSignalRoute::IncomingOffer
        );
        assert_eq!(
            route_group_signal(
                "peer-a",
                &generation_a,
                SignalKind::Answer,
                true,
                Some(&generation_a),
                None,
            ),
            GroupSignalRoute::Outbound
        );
        assert_eq!(
            route_group_signal(
                "peer-a",
                &generation_b,
                SignalKind::Answer,
                true,
                Some(&generation_a),
                None,
            ),
            GroupSignalRoute::Ignore
        );
        assert_eq!(
            route_group_signal(
                "peer-a",
                &generation_a,
                SignalKind::IceCandidate,
                true,
                None,
                None,
            ),
            GroupSignalRoute::QueueEarlyIce
        );
        assert_eq!(
            route_group_signal(
                "peer-b",
                &generation_b,
                SignalKind::IceCandidate,
                true,
                None,
                None,
            ),
            GroupSignalRoute::QueueEarlyIce
        );

        let mut app = ClientUi::default();
        app.queue_early_group_ice("peer-a", &generation_a, "candidate-a".to_owned());
        app.queue_early_group_ice("peer-b", &generation_b, "candidate-b".to_owned());
        let key_a = ("peer-a".to_owned(), generation_a.clone());
        let key_b = ("peer-b".to_owned(), generation_b.clone());
        assert_eq!(
            app.pending_group_ice[&key_a]
                .front()
                .map(|candidate| candidate.payload.as_str()),
            Some("candidate-a")
        );
        assert_eq!(
            app.pending_group_ice[&key_b]
                .front()
                .map(|candidate| candidate.payload.as_str()),
            Some("candidate-b")
        );

        app.mark_group_generation_closed("peer-a", generation_a.clone());
        app.queue_early_group_ice("peer-a", &generation_a, "stale-candidate".to_owned());
        assert!(!app.pending_group_ice.contains_key(&key_a));
        assert_eq!(
            app.pending_group_ice[&key_b]
                .front()
                .map(|candidate| candidate.payload.as_str()),
            Some("candidate-b"),
            "closing one generation must not disturb another session's ICE"
        );
        assert_eq!(
            route_group_signal(
                "peer-a",
                &generation_a,
                SignalKind::Answer,
                true,
                Some(&generation_b),
                None,
            ),
            GroupSignalRoute::Ignore
        );
    }
}
