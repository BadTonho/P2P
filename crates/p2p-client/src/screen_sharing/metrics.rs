use super::*;

#[derive(Clone, Debug, Default)]
pub struct ScreenShareMetrics {
    pub session_id: u64,
    pub track_ssrc: Option<u32>,
    pub outbound_video_ssrc: Option<u32>,
    pub inbound_video_ssrc: Option<u32>,
    pub capture_width: u32,
    pub capture_height: u32,
    pub encoder_width: u32,
    pub encoder_height: u32,
    pub decoder_width: u32,
    pub decoder_height: u32,
    pub p2p_connected: bool,
    pub route: Option<MediaRoute>,
    pub local_ice_candidates: u64,
    pub remote_ice_candidates: u64,
    pub local_srflx_candidates: u64,
    pub remote_srflx_candidates: u64,
    pub local_relay_candidates: u64,
    pub remote_relay_candidates: u64,
    pub encoder_input_frames: u64,
    pub encoded_frames: u64,
    pub sent_frames: u64,
    pub dropped_before_initial_idr: u64,
    pub sent_idr_frames: u64,
    pub sent_delta_frames: u64,
    pub received_packets: u64,
    pub assembled_access_units: u64,
    pub received_delta_frames: u64,
    pub decoded_frames: u64,
    pub decoded_delta_frames: u64,
    pub decoded_idr_frames: u64,
    pub decode_errors: u64,
    pub decoder_input_frames: u64,
    pub decoder_no_output_frames: u64,
    pub decoder_queue_drops: u64,
    pub published_frames: u64,
    pub ui_texture_updates: u64,
    pub pli_requests_sent: u64,
    pub pli_requests_received: u64,
    pub nack_packets_sent: Option<u64>,
    pub nack_packets_received: Option<u64>,
    pub nack_packets_received_observed: u64,
    pub nack_received_media_ssrc: Option<u32>,
    pub retransmitted_packets_sent: Option<u64>,
    pub retransmitted_bytes_sent: Option<u64>,
    pub retransmitted_packets_received: Option<u64>,
    pub retransmitted_bytes_received: Option<u64>,
    pub assembled_idr_with_parameters: u64,
    pub assembled_idr_without_parameters: u64,
    pub assembled_delta_units: u64,
    pub assembled_no_frame_units: u64,
    pub decoder_queue_accepted: u64,
    pub pli_queue_overflow: u64,
    pub keyframe_resyncs: u64,
    pub last_recovery_time_millis: Option<u64>,
    pub last_decode_error: Option<String>,
    pub h264_diagnostics: String,
    pub selected_ice_pair: String,
    pub rtc_outbound_summary: String,
    pub rtc_inbound_summary: String,
    pub outbound_rtp_packets: u64,
    pub outbound_rtp_bytes: u64,
    pub inbound_rtp_packets: u64,
    pub inbound_rtp_bytes: u64,
    pub inbound_rtp_lost: i64,
    pub inbound_rtp_jitter_ms: f64,
    pub rtp_reorder_window_ms: u64,
    pub rtp_reorder_samples: u64,
    pub encoder_backend: String,
    pub encoder_fallback_reason: Option<String>,
    pub decoder_backend: String,
    pub decoder_fallback_reason: Option<String>,
    pub decoder_preference: String,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ScreenSharePerformanceSnapshot {
    pub new_capture_frames: u64,
    pub repeated_capture_frames: u64,
    pub encoder_worker_late_frames: u64,
    pub skipped_capture_sequences: u64,
    pub encoder_input_frames: u64,
    pub encoded_frames: u64,
    pub encoded_idr_frames: u64,
    pub encoded_delta_frames: u64,
    pub sent_frames: u64,
    pub dropped_before_initial_idr: u64,
    pub sent_idr_frames: u64,
    pub sent_delta_frames: u64,
    pub encode_nanos: u64,
    pub encode_samples: u64,
    pub queue_wait_nanos: u64,
    pub queue_wait_samples: u64,
    pub write_sample_nanos: u64,
    pub write_sample_samples: u64,
    pub write_sample_bytes: u64,
    pub write_sample_failures: u64,
    pub outbound_rtp_packets: u64,
    pub outbound_rtp_bytes: u64,
    pub inbound_rtp_packets: u64,
    pub inbound_rtp_bytes: u64,
    pub inbound_rtp_lost_delta: i64,
    pub received_packets: u64,
    pub observed_sequence_gaps: u64,
    pub recovered_reordered_packets: u64,
    pub unmatched_out_of_order_packets: u64,
    pub duplicate_packets: u64,
    pub confirmed_missing_packets: u64,
    pub late_after_confirmed_packets: u64,
    pub sequence_gap_resyncs: u64,
    pub assembled_access_units: u64,
    pub assembly_errors: u64,
    pub decoder_input_frames: u64,
    pub decoder_no_output_frames: u64,
    pub decoder_queue_drops: u64,
    pub decode_errors: u64,
    pub decoded_frames: u64,
    pub published_frames: u64,
    pub ui_texture_updates: u64,
    pub pli_requests_sent: u64,
    pub pli_requests_received: u64,
    pub nack_packets_sent: Option<u64>,
    pub nack_packets_received: Option<u64>,
    pub nack_packets_received_observed: u64,
    pub retransmitted_packets_sent: Option<u64>,
    pub retransmitted_bytes_sent: Option<u64>,
    pub retransmitted_packets_received: Option<u64>,
    pub retransmitted_bytes_received: Option<u64>,
    pub assembled_idr_with_parameters: u64,
    pub assembled_idr_without_parameters: u64,
    pub assembled_delta_units: u64,
    pub assembled_no_frame_units: u64,
    pub dropped_recovery_gate_waiting_for_idr: u64,
    pub dropped_worker_waiting_for_idr: u64,
    pub dropped_stale_generation: u64,
    pub decoder_queue_accepted: u64,
    pub decoder_queue_full: u64,
    pub decoder_queue_disconnected: u64,
    pub decoder_inputs_live: u64,
    pub decoder_inputs_cached_replay: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaRoute {
    Direct,
    Turn,
}

pub(super) fn should_log_aggregate_error(count: u64) -> bool {
    count.is_power_of_two()
}

pub(crate) fn optional_count_label(value: Option<u64>) -> String {
    value.map_or_else(|| "desconhecido".to_owned(), |count| count.to_string())
}

pub(crate) fn h264_pipeline_interval_label(snapshot: &ScreenSharePerformanceSnapshot) -> String {
    format!(
        "classificadas IDR_com_SPS_PPS/IDR_sem_parametros/delta/sem_quadro={}/{}/{}/{}; destino bloqueio_apos_perda/worker_aguardando_IDR/geracao_antiga/fila_aceita/fila_cheia/fila_encerrada={}/{}/{}/{}/{}/{}; entradas_decoder ao_vivo/cache={}/{}",
        snapshot.assembled_idr_with_parameters,
        snapshot.assembled_idr_without_parameters,
        snapshot.assembled_delta_units,
        snapshot.assembled_no_frame_units,
        snapshot.dropped_recovery_gate_waiting_for_idr,
        snapshot.dropped_worker_waiting_for_idr,
        snapshot.dropped_stale_generation,
        snapshot.decoder_queue_accepted,
        snapshot.decoder_queue_full,
        snapshot.decoder_queue_disconnected,
        snapshot.decoder_inputs_live,
        snapshot.decoder_inputs_cached_replay,
    )
}

pub(crate) fn rtp_recovery_interval_label(
    metrics: &ScreenShareMetrics,
    performance: &ScreenSharePerformanceSnapshot,
) -> String {
    let sent_ssrc = metrics
        .inbound_video_ssrc
        .map_or_else(|| "desconhecido".to_owned(), |ssrc| ssrc.to_string());
    let received_ssrc = metrics
        .outbound_video_ssrc
        .map_or_else(|| "desconhecido".to_owned(), |ssrc| ssrc.to_string());
    format!(
        "NACK_RTCP_pacotes_enviados_intervalo/total={} / {} SSRC {}; recebidos={} / {} SSRC {}; NACK_RTCP_recebido_observado_no_interceptor_intervalo/total={}/{} SSRC {}; RTX_enviado_pacotes/bytes_intervalo={}/{}, total={}/{}; RTX_recebido_pacotes/bytes_intervalo={}/{}, total={}/{}",
        optional_count_label(performance.nack_packets_sent),
        optional_count_label(metrics.nack_packets_sent),
        sent_ssrc,
        optional_count_label(performance.nack_packets_received),
        optional_count_label(metrics.nack_packets_received),
        received_ssrc,
        performance.nack_packets_received_observed,
        metrics.nack_packets_received_observed,
        metrics
            .nack_received_media_ssrc
            .map_or_else(|| "desconhecido".to_owned(), |ssrc| ssrc.to_string()),
        optional_count_label(performance.retransmitted_packets_sent),
        optional_count_label(performance.retransmitted_bytes_sent),
        optional_count_label(metrics.retransmitted_packets_sent),
        optional_count_label(metrics.retransmitted_bytes_sent),
        optional_count_label(performance.retransmitted_packets_received),
        optional_count_label(performance.retransmitted_bytes_received),
        optional_count_label(metrics.retransmitted_packets_received),
        optional_count_label(metrics.retransmitted_bytes_received),
    )
}

impl SharedMetrics {
    pub(super) fn snapshot(&self) -> ScreenShareMetrics {
        let h264_flow = self
            .h264_flow
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let rtc_stats = self
            .rtc_stats
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        ScreenShareMetrics {
            session_id: self.session_id,
            track_ssrc: match self.track_ssrc.load(Ordering::Relaxed) {
                0 => None,
                ssrc => Some(ssrc as u32),
            },
            outbound_video_ssrc: (rtc_stats.outbound_ssrc != 0).then_some(rtc_stats.outbound_ssrc),
            inbound_video_ssrc: (rtc_stats.inbound_ssrc != 0).then_some(rtc_stats.inbound_ssrc),
            capture_width: self.capture_width.load(Ordering::Relaxed) as u32,
            capture_height: self.capture_height.load(Ordering::Relaxed) as u32,
            encoder_width: self.encoder_width.load(Ordering::Relaxed) as u32,
            encoder_height: self.encoder_height.load(Ordering::Relaxed) as u32,
            decoder_width: rtc_stats.inbound_frame_width,
            decoder_height: rtc_stats.inbound_frame_height,
            p2p_connected: self.p2p_connected.load(Ordering::Relaxed),
            route: match self.route.load(Ordering::Relaxed) {
                1 => Some(MediaRoute::Direct),
                2 => Some(MediaRoute::Turn),
                _ => None,
            },
            local_ice_candidates: self.local_ice_candidates.load(Ordering::Relaxed),
            remote_ice_candidates: self.remote_ice_candidates.load(Ordering::Relaxed),
            local_srflx_candidates: self.local_srflx_candidates.load(Ordering::Relaxed),
            remote_srflx_candidates: self.remote_srflx_candidates.load(Ordering::Relaxed),
            local_relay_candidates: self.local_relay_candidates.load(Ordering::Relaxed),
            remote_relay_candidates: self.remote_relay_candidates.load(Ordering::Relaxed),
            encoder_input_frames: self.encoder_input_frames.load(Ordering::Relaxed),
            encoded_frames: self.encoded_frames.load(Ordering::Relaxed),
            sent_frames: self.sent_frames.load(Ordering::Relaxed),
            dropped_before_initial_idr: self.dropped_before_initial_idr.load(Ordering::Relaxed),
            sent_idr_frames: self.sent_idr_frames.load(Ordering::Relaxed),
            sent_delta_frames: self.sent_delta_frames.load(Ordering::Relaxed),
            received_packets: self.received_packets.load(Ordering::Relaxed),
            assembled_access_units: h264_flow.assembled_access_units,
            received_delta_frames: h264_flow.assembled_delta_frames,
            decoded_frames: self.decoded_frames.load(Ordering::Relaxed),
            decoded_delta_frames: self.decoded_delta_frames.load(Ordering::Relaxed),
            decoded_idr_frames: self.decoded_idr_frames.load(Ordering::Relaxed),
            decode_errors: self.decode_errors.load(Ordering::Relaxed),
            decoder_input_frames: self.decoder_input_frames.load(Ordering::Relaxed),
            decoder_no_output_frames: self.decoder_no_output_frames.load(Ordering::Relaxed),
            decoder_queue_drops: self.decoder_queue_drops.load(Ordering::Relaxed),
            published_frames: self.published_frames.load(Ordering::Relaxed),
            ui_texture_updates: self.ui_texture_updates.load(Ordering::Relaxed),
            pli_requests_sent: h264_flow.pli_requests_sent,
            pli_requests_received: h264_flow.pli_requests_received,
            nack_packets_sent: rtc_stats.inbound_nack_count,
            nack_packets_received: rtc_stats.outbound_nack_count,
            nack_packets_received_observed: self
                .nack_packets_received_observed
                .load(Ordering::Relaxed),
            nack_received_media_ssrc: match self.nack_received_media_ssrc.load(Ordering::Relaxed) {
                0 => None,
                ssrc => Some(ssrc as u32),
            },
            retransmitted_packets_sent: rtc_stats.outbound_retransmitted_packets,
            retransmitted_bytes_sent: rtc_stats.outbound_retransmitted_bytes,
            retransmitted_packets_received: rtc_stats.inbound_retransmitted_packets,
            retransmitted_bytes_received: rtc_stats.inbound_retransmitted_bytes,
            assembled_idr_with_parameters: h264_flow.assembled_idr_with_parameters,
            assembled_idr_without_parameters: h264_flow.assembled_idr_without_parameters,
            assembled_delta_units: h264_flow.assembled_delta_frames,
            assembled_no_frame_units: h264_flow.assembled_no_frame_units,
            decoder_queue_accepted: h264_flow.decoder_queue_accepted,
            pli_queue_overflow: h264_flow.pli_queue_overflow,
            keyframe_resyncs: h264_flow.keyframe_resyncs,
            last_recovery_time_millis: h264_flow.last_recovery_time_millis,
            last_decode_error: self
                .last_decode_error
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone(),
            selected_ice_pair: self
                .selected_ice_pair
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone(),
            rtc_outbound_summary: self
                .rtc_outbound_summary
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone(),
            rtc_inbound_summary: self
                .rtc_inbound_summary
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone(),
            outbound_rtp_packets: rtc_stats.outbound_packets,
            outbound_rtp_bytes: rtc_stats.outbound_bytes,
            inbound_rtp_packets: rtc_stats.inbound_packets,
            inbound_rtp_bytes: rtc_stats.inbound_bytes,
            inbound_rtp_lost: rtc_stats.inbound_packets_lost,
            inbound_rtp_jitter_ms: rtc_stats.inbound_jitter_ms,
            rtp_reorder_window_ms: self
                .rtp_reorder_window_millis
                .load(Ordering::Relaxed)
                .max(INITIAL_RTP_REORDER_DELAY.as_millis() as u64),
            rtp_reorder_samples: self.rtp_reorder_delay_samples.load(Ordering::Relaxed),
            encoder_backend: self
                .encoder_backend
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone(),
            encoder_fallback_reason: self
                .encoder_fallback_reason
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone(),
            decoder_backend: self
                .decoder_backend
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone(),
            decoder_fallback_reason: self
                .decoder_fallback_reason
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone(),
            decoder_preference: self
                .decoder_preference
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone(),
            h264_diagnostics: format!(
                "codificador SPS/PPS/IDR/P {}/{}/{}/{}, última saída {}; recepção NAL SPS/PPS/IDR/P {}/{}/{}/{}, pacotes single/STAP-A/FU-A início/fim {}/{}/{}/{}; saltos observados {}, reordenação recuperada {}, fora de ordem {}, duplicatas {}, perdas confirmadas {}, tardios após confirmação {}, resyncs por salto {}; unidades completas {} (IDR {}, P {}), com SPS/PPS {}/{}, erros de montagem {} [marcador {}, lacuna {}, fragmentação {}, limite {}, outros {}]; unidades IDR com SPS+PPS/sem parâmetros/delta/sem quadro {}/{}/{}/{}, destino: descartadas no bloqueio após perda {}, descartadas pelo worker aguardando IDR {}, geração antiga {}, fila aceita {}, fila cheia {}, fila encerrada {}; entradas do decoder ao vivo/cache {}/{}; última unidade: {}{}; PLI enviado/recebido {}/{}, motivos salto/montagem/decodificação/fila/manual {}/{}/{}/{}/{}, resyncs {}, descartes até IDR {}, recuperação IDR (montado/decodificado/publicado) {:?}/{:?}/{:?} ms.",
                h264_flow.encoded_sps,
                h264_flow.encoded_pps,
                h264_flow.encoded_idr,
                h264_flow.encoded_delta_frames,
                h264_flow.last_encoded_nals,
                h264_flow.received_sps,
                h264_flow.received_pps,
                h264_flow.received_idr,
                h264_flow.received_delta_nals,
                h264_flow.received_single_nals,
                h264_flow.received_stap_a,
                h264_flow.received_fu_a_start,
                h264_flow.received_fu_a_end,
                h264_flow.sequence_gaps,
                h264_flow.recovered_reordered_packets,
                h264_flow.out_of_order_packets,
                h264_flow.duplicate_packets,
                h264_flow.confirmed_missing_packets,
                h264_flow.late_after_confirmed_packets,
                h264_flow.sequence_gap_resyncs,
                h264_flow.assembled_access_units,
                h264_flow.assembled_with_idr,
                h264_flow.assembled_delta_frames,
                h264_flow.assembled_with_sps,
                h264_flow.assembled_with_pps,
                h264_flow.assembly_errors,
                h264_flow.marker_timeouts,
                h264_flow.sequence_hole_errors,
                h264_flow.fragment_errors,
                h264_flow.pending_frame_evictions,
                h264_flow.other_assembly_errors,
                h264_flow.assembled_idr_with_parameters,
                h264_flow.assembled_idr_without_parameters,
                h264_flow.assembled_delta_frames,
                h264_flow.assembled_no_frame_units,
                h264_flow.dropped_recovery_gate_waiting_for_idr,
                h264_flow.dropped_worker_waiting_for_idr,
                h264_flow.dropped_stale_generation,
                h264_flow.decoder_queue_accepted,
                h264_flow.decoder_queue_drops,
                h264_flow.decoder_queue_disconnected,
                h264_flow.decoder_inputs_live,
                h264_flow.decoder_inputs_cached_replay,
                h264_flow.last_access_unit,
                h264_flow
                    .last_assembly_error
                    .as_ref()
                    .map(|error| format!("; último erro de montagem: {error}"))
                    .unwrap_or_default(),
                h264_flow.pli_requests_sent,
                h264_flow.pli_requests_received,
                h264_flow.pli_sequence_gap,
                h264_flow.pli_assembly_error,
                h264_flow.pli_decode_error,
                h264_flow.pli_queue_overflow,
                h264_flow.pli_explicit,
                h264_flow.keyframe_resyncs,
                h264_flow.dropped_while_waiting_for_idr,
                h264_flow.last_idr_assembled_recovery_ms,
                h264_flow.last_idr_decoded_recovery_ms,
                h264_flow.last_idr_published_recovery_ms,
            ),
        }
    }

    pub(super) fn set_encoder_backend(&self, backend: String, fallback: Option<String>) {
        *self
            .encoder_backend
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = backend;
        *self
            .encoder_fallback_reason
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = fallback;
    }

    pub(super) fn set_decoder_backend(&self, backend: String, fallback: Option<String>) {
        *self
            .decoder_backend
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = backend;
        *self
            .decoder_fallback_reason
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = fallback;
    }

    pub(super) fn set_track_ssrc(&self, ssrc: u32) {
        self.track_ssrc.store(u64::from(ssrc), Ordering::Relaxed);
        crate::logging::register_srtp_track_context(ssrc, self.session_id, None, None);
    }

    pub(super) fn update_transport_diagnostics(
        &self,
        snapshot: PeerStatsSnapshot,
        previous: &mut Option<PeerStatsSnapshot>,
    ) -> bool {
        let changed = {
            let mut key = self
                .selected_pair_key
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let changed = *key != snapshot.selected_pair_key;
            *key = snapshot.selected_pair_key.clone();
            changed
        };
        *self
            .selected_ice_pair
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            snapshot.selected_pair_summary.clone();
        let ssrc = self.track_ssrc.load(Ordering::Relaxed) as u32;
        if ssrc != 0 {
            let route = snapshot.route.map(|route| match route {
                MediaRoute::Direct => "Direto (P2P)",
                MediaRoute::Turn => "Retransmitido (TURN)",
            });
            let selected_pair = (!snapshot.selected_pair_key.is_empty()
                && !snapshot.selected_pair_summary.contains("desconhecido"))
            .then_some(snapshot.selected_pair_summary.as_str());
            crate::logging::register_srtp_track_context(
                ssrc,
                self.session_id,
                route,
                selected_pair,
            );
        }
        *self
            .rtc_outbound_summary
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = snapshot.outbound_summary.clone();
        *self
            .rtc_inbound_summary
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = snapshot.inbound_summary.clone();
        if let Some(previous) = previous.as_ref() {
            self.interval_outbound_rtp_packets.fetch_add(
                snapshot
                    .outbound_packets
                    .saturating_sub(previous.outbound_packets),
                Ordering::Relaxed,
            );
            self.interval_outbound_rtp_bytes.fetch_add(
                snapshot
                    .outbound_bytes
                    .saturating_sub(previous.outbound_bytes),
                Ordering::Relaxed,
            );
            self.interval_inbound_rtp_packets.fetch_add(
                snapshot
                    .inbound_packets
                    .saturating_sub(previous.inbound_packets),
                Ordering::Relaxed,
            );
            self.interval_inbound_rtp_bytes.fetch_add(
                snapshot
                    .inbound_bytes
                    .saturating_sub(previous.inbound_bytes),
                Ordering::Relaxed,
            );
            self.interval_inbound_rtp_lost.fetch_add(
                snapshot
                    .inbound_packets_lost
                    .saturating_sub(previous.inbound_packets_lost),
                Ordering::Relaxed,
            );
            let mut recovery = self
                .rtp_recovery_interval
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            recovery
                .nack_packets_sent
                .observe(snapshot.inbound_nack_count, previous.inbound_nack_count);
            recovery
                .nack_packets_received
                .observe(snapshot.outbound_nack_count, previous.outbound_nack_count);
            recovery.retransmitted_packets_sent.observe(
                snapshot.outbound_retransmitted_packets,
                previous.outbound_retransmitted_packets,
            );
            recovery.retransmitted_bytes_sent.observe(
                snapshot.outbound_retransmitted_bytes,
                previous.outbound_retransmitted_bytes,
            );
            recovery.retransmitted_packets_received.observe(
                snapshot.inbound_retransmitted_packets,
                previous.inbound_retransmitted_packets,
            );
            recovery.retransmitted_bytes_received.observe(
                snapshot.inbound_retransmitted_bytes,
                previous.inbound_retransmitted_bytes,
            );
        }
        *self
            .rtc_stats
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = snapshot.clone();
        *previous = Some(snapshot);
        changed
    }

    pub(super) fn record_encoded_access_unit(&self, data: &[u8]) {
        let nals = annex_b_nal_types(data);
        match classify_h264_access_unit(&nals) {
            Some(H264FrameKind::Idr) => {
                self.encoded_frames.fetch_add(1, Ordering::Relaxed);
                self.interval_encoded_frames.fetch_add(1, Ordering::Relaxed);
                self.interval_encoded_idr_frames
                    .fetch_add(1, Ordering::Relaxed);
            }
            Some(H264FrameKind::Delta) => {
                self.encoded_frames.fetch_add(1, Ordering::Relaxed);
                self.interval_encoded_frames.fetch_add(1, Ordering::Relaxed);
                self.interval_encoded_delta_frames
                    .fetch_add(1, Ordering::Relaxed);
            }
            None => {}
        }
        let mut flow = self
            .h264_flow
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for nal_type in &nals {
            match nal_type {
                7 => flow.encoded_sps += 1,
                8 => flow.encoded_pps += 1,
                5 => flow.encoded_idr += 1,
                _ => {}
            }
        }
        if classify_h264_access_unit(&nals) == Some(H264FrameKind::Delta) {
            flow.encoded_delta_frames += 1;
        }
        flow.last_encoded_nals = describe_nal_types(&nals, data.len());
    }

    pub(super) fn record_encoder_input_frame(&self) {
        self.encoder_input_frames.fetch_add(1, Ordering::Relaxed);
        self.interval_encoder_input_frames
            .fetch_add(1, Ordering::Relaxed);
    }

    pub(super) fn record_video_dimensions(&self, width: u32, height: u32) {
        self.capture_width
            .store(u64::from(width), Ordering::Relaxed);
        self.capture_height
            .store(u64::from(height), Ordering::Relaxed);
        self.encoder_width
            .store(u64::from(width), Ordering::Relaxed);
        self.encoder_height
            .store(u64::from(height), Ordering::Relaxed);
    }

    pub(super) fn record_capture_frame_for_encoder(&self, repeated: bool) {
        if repeated {
            self.interval_repeated_capture_frames
                .fetch_add(1, Ordering::Relaxed);
        } else {
            self.interval_new_capture_frames
                .fetch_add(1, Ordering::Relaxed);
        }
        self.record_encoder_input_frame();
    }

    pub(super) fn record_encoder_worker_delay(&self, late: bool, skipped_sequences: u64) {
        if late {
            self.interval_encoder_worker_late_frames
                .fetch_add(1, Ordering::Relaxed);
        }
        self.interval_skipped_capture_sequences
            .fetch_add(skipped_sequences, Ordering::Relaxed);
    }

    pub(super) fn record_drop_before_initial_idr(&self) {
        self.dropped_before_initial_idr
            .fetch_add(1, Ordering::Relaxed);
        self.interval_dropped_before_initial_idr
            .fetch_add(1, Ordering::Relaxed);
    }

    pub(super) fn record_sent_frame(&self, kind: H264FrameKind) {
        self.sent_frames.fetch_add(1, Ordering::Relaxed);
        self.interval_sent_frames.fetch_add(1, Ordering::Relaxed);
        match kind {
            H264FrameKind::Idr => {
                self.sent_idr_frames.fetch_add(1, Ordering::Relaxed);
                self.interval_sent_idr_frames
                    .fetch_add(1, Ordering::Relaxed);
            }
            H264FrameKind::Delta => {
                self.sent_delta_frames.fetch_add(1, Ordering::Relaxed);
                self.interval_sent_delta_frames
                    .fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    pub(super) fn record_received_packet(&self, packet: &rtc::rtp::packet::Packet) {
        let payload = &packet.payload;
        let packet_type = payload.first().map(|byte| byte & 0x1f);
        let nals = rtp_payload_nal_types(payload);
        let mut flow = self
            .h264_flow
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        match packet_type {
            Some(24) => flow.received_stap_a += 1,
            Some(28) if payload.len() >= 2 && payload[1] & 0x80 != 0 => {
                flow.received_fu_a_start += 1;
            }
            Some(28) if payload.len() >= 2 && payload[1] & 0x40 != 0 => {
                flow.received_fu_a_end += 1;
            }
            Some(1..=23) => flow.received_single_nals += 1,
            _ => {}
        }
        for nal_type in nals {
            match nal_type {
                7 => flow.received_sps += 1,
                8 => flow.received_pps += 1,
                5 => flow.received_idr += 1,
                1..=4 => flow.received_delta_nals += 1,
                _ => {}
            }
        }
    }

    pub(super) fn record_sequence_update(&self, update: RtpSequenceUpdate) {
        self.interval_observed_sequence_gaps
            .fetch_add(update.observed_gap_packets, Ordering::Relaxed);
        self.interval_recovered_reordered_packets
            .fetch_add(update.recovered_reordered_packets, Ordering::Relaxed);
        self.interval_unmatched_out_of_order_packets
            .fetch_add(update.unmatched_out_of_order_packets, Ordering::Relaxed);
        self.interval_duplicate_packets
            .fetch_add(update.duplicate_packets, Ordering::Relaxed);
        self.interval_confirmed_missing_packets
            .fetch_add(update.confirmed_missing_packets, Ordering::Relaxed);
        self.interval_late_after_confirmed_packets
            .fetch_add(update.late_after_confirmed_packets, Ordering::Relaxed);
        let mut flow = self
            .h264_flow
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        flow.sequence_gaps += update.observed_gap_packets;
        flow.recovered_reordered_packets += update.recovered_reordered_packets;
        flow.out_of_order_packets += update.unmatched_out_of_order_packets;
        flow.duplicate_packets += update.duplicate_packets;
        flow.confirmed_missing_packets += update.confirmed_missing_packets;
        flow.late_after_confirmed_packets += update.late_after_confirmed_packets;
    }

    pub(super) fn record_rtp_reorder_window(&self, delay: Duration, sample_count: usize) {
        self.rtp_reorder_window_millis.store(
            delay.as_millis().min(u128::from(u64::MAX)) as u64,
            Ordering::Relaxed,
        );
        self.rtp_reorder_delay_samples
            .store(sample_count as u64, Ordering::Relaxed);
    }

    pub(super) fn record_sequence_gap_resync(&self) {
        self.interval_sequence_gap_resyncs
            .fetch_add(1, Ordering::Relaxed);
        self.h264_flow
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .sequence_gap_resyncs += 1;
    }

    pub(super) fn record_assembled_access_unit(&self, data: &[u8]) -> String {
        self.interval_assembled_access_units
            .fetch_add(1, Ordering::Relaxed);
        let nals = annex_b_nal_types(data);
        let description = describe_nal_types(&nals, data.len());
        let mut flow = self
            .h264_flow
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        flow.assembled_access_units += 1;
        let contains_sps = nals.contains(&7);
        let contains_pps = nals.contains(&8);
        let contains_idr = nals.contains(&5);
        match classify_h264_access_unit_diagnostics(&nals) {
            H264AccessUnitClass::IdrWithParameters => {
                flow.assembled_idr_with_parameters += 1;
                flow.interval.assembled_idr_with_parameters += 1;
            }
            H264AccessUnitClass::IdrWithoutParameters => {
                flow.assembled_idr_without_parameters += 1;
                flow.interval.assembled_idr_without_parameters += 1;
            }
            H264AccessUnitClass::Delta => flow.interval.assembled_delta_units += 1,
            H264AccessUnitClass::NoFrame => {
                flow.assembled_no_frame_units += 1;
                flow.interval.assembled_no_frame_units += 1;
            }
        }
        if contains_sps {
            flow.assembled_with_sps += 1;
        }
        if contains_pps {
            flow.assembled_with_pps += 1;
        }
        if contains_idr {
            flow.assembled_with_idr += 1;
        }
        if contains_idr && let Some(started_at) = flow.resync_started_at {
            let elapsed = started_at.elapsed().as_millis();
            flow.last_idr_assembled_recovery_ms = Some(elapsed.min(u128::from(u64::MAX)) as u64);
        }
        if !contains_idr && nals.iter().any(|nal_type| (1..=4).contains(nal_type)) {
            flow.assembled_delta_frames += 1;
        }
        flow.last_access_unit = description.clone();
        description
    }

    pub(super) fn record_idr_decoded(&self) {
        let mut flow = self
            .h264_flow
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(started_at) = flow.resync_started_at {
            let elapsed = started_at.elapsed().as_millis();
            flow.last_idr_decoded_recovery_ms = Some(elapsed.min(u128::from(u64::MAX)) as u64);
        }
    }

    pub(super) fn record_idr_published(&self) {
        let mut flow = self
            .h264_flow
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(started_at) = flow.resync_started_at.take() {
            let elapsed = started_at.elapsed().as_millis();
            let elapsed = elapsed.min(u128::from(u64::MAX)) as u64;
            flow.last_idr_published_recovery_ms = Some(elapsed);
            flow.last_recovery_time_millis = Some(elapsed);
            flow.recovery_count += 1;
            flow.recovery_time_millis_total += u128::from(elapsed);
        }
    }

    pub(super) fn record_assembly_error(&self, error: String) {
        self.interval_assembly_errors
            .fetch_add(1, Ordering::Relaxed);
        let mut flow = self
            .h264_flow
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        flow.assembly_errors += 1;
        let lower = error.to_ascii_lowercase();
        if lower.contains("marcador") {
            flow.marker_timeouts += 1;
        } else if lower.contains("sequência")
            || lower.contains("lacuna")
            || lower.contains("ausente")
        {
            flow.sequence_hole_errors += 1;
        } else if lower.contains("fu-a") || lower.contains("fragmento") || lower.contains("stap-a")
        {
            flow.fragment_errors += 1;
        } else if lower.contains("limite") || lower.contains("pendentes") {
            flow.pending_frame_evictions += 1;
        } else {
            flow.other_assembly_errors += 1;
        }
        let count = flow.assembly_errors;
        flow.last_assembly_error = Some(error.clone());
        if should_log_aggregate_error(count) {
            tracing::warn!(
                screen_share_session = self.session_id,
                assembly_errors = count,
                reason = %error,
                "Erro de montagem H.264; resumos repetidos registrados em contagens dobradas"
            );
        }
    }

    pub(super) fn record_pli_sent(&self, reason: PliReason) {
        self.interval_pli_requests_sent
            .fetch_add(1, Ordering::Relaxed);
        let mut flow = self
            .h264_flow
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        flow.pli_requests_sent += 1;
        match reason {
            PliReason::SequenceGap => flow.pli_sequence_gap += 1,
            PliReason::AssemblyError => flow.pli_assembly_error += 1,
            PliReason::DecodeError => flow.pli_decode_error += 1,
            PliReason::DecoderQueueFull => flow.pli_queue_overflow += 1,
            #[cfg(test)]
            PliReason::Explicit => flow.pli_explicit += 1,
        }
    }

    pub(super) fn record_pli_received(&self) {
        self.interval_pli_requests_received
            .fetch_add(1, Ordering::Relaxed);
        self.h264_flow
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pli_requests_received += 1;
    }

    pub(super) fn record_inbound_nack_packet(&self, media_ssrc: u32) {
        self.nack_packets_received_observed
            .fetch_add(1, Ordering::Relaxed);
        self.interval_nack_packets_received_observed
            .fetch_add(1, Ordering::Relaxed);
        self.nack_received_media_ssrc
            .store(u64::from(media_ssrc), Ordering::Relaxed);
    }

    pub(super) fn record_keyframe_resync(&self) {
        let mut flow = self
            .h264_flow
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        flow.keyframe_resyncs += 1;
        if flow.resync_started_at.is_none() {
            flow.resync_started_at = Some(Instant::now());
        }
    }

    pub(super) fn record_drop_recovery_gate_waiting_for_idr(&self) {
        let mut flow = self
            .h264_flow
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        flow.dropped_while_waiting_for_idr += 1;
        flow.dropped_recovery_gate_waiting_for_idr += 1;
        flow.interval.dropped_recovery_gate_waiting_for_idr += 1;
    }

    pub(super) fn record_drop_worker_waiting_for_idr(&self) {
        let mut flow = self
            .h264_flow
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        flow.dropped_while_waiting_for_idr += 1;
        flow.dropped_worker_waiting_for_idr += 1;
        flow.interval.dropped_worker_waiting_for_idr += 1;
    }

    pub(super) fn record_drop_stale_generation(&self) {
        let mut flow = self
            .h264_flow
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        flow.dropped_stale_generation += 1;
        flow.interval.dropped_stale_generation += 1;
    }

    pub(super) fn record_decoder_queue_accepted(&self) {
        let mut flow = self
            .h264_flow
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        flow.decoder_queue_accepted += 1;
        flow.interval.decoder_queue_accepted += 1;
    }

    pub(super) fn record_decoder_queue_disconnected(&self) {
        let mut flow = self
            .h264_flow
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        flow.decoder_queue_disconnected += 1;
        flow.interval.decoder_queue_disconnected += 1;
    }

    pub(super) fn record_decoder_no_output(&self) {
        self.interval_decoder_no_output_frames
            .fetch_add(1, Ordering::Relaxed);
        self.decoder_no_output_frames
            .fetch_add(1, Ordering::Relaxed);
        self.h264_flow
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .decoder_no_output_frames += 1;
    }

    fn record_decoder_input(&self, cached_replay: bool) {
        self.decoder_input_frames.fetch_add(1, Ordering::Relaxed);
        self.interval_decoder_input_frames
            .fetch_add(1, Ordering::Relaxed);
        let mut flow = self
            .h264_flow
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if cached_replay {
            flow.decoder_inputs_cached_replay += 1;
            flow.interval.decoder_inputs_cached_replay += 1;
        } else {
            flow.decoder_inputs_live += 1;
            flow.interval.decoder_inputs_live += 1;
        }
    }

    pub(super) fn record_decoder_input_live(&self) {
        self.record_decoder_input(false);
    }

    pub(super) fn record_decoder_input_cached_replay(&self) {
        self.record_decoder_input(true);
    }

    pub(super) fn record_ui_texture_update(&self) {
        self.ui_texture_updates.fetch_add(1, Ordering::Relaxed);
        self.interval_ui_texture_updates
            .fetch_add(1, Ordering::Relaxed);
    }

    pub(super) fn record_decoder_queue_drop(&self) {
        self.decoder_queue_drops.fetch_add(1, Ordering::Relaxed);
        self.interval_decoder_queue_drops
            .fetch_add(1, Ordering::Relaxed);
        let mut flow = self
            .h264_flow
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        flow.decoder_queue_drops += 1;
        flow.interval.decoder_queue_full += 1;
    }

    pub(super) fn take_performance_snapshot(&self) -> ScreenSharePerformanceSnapshot {
        let h264_interval = {
            let mut flow = self
                .h264_flow
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            std::mem::take(&mut flow.interval)
        };
        let mut rtp_recovery_interval = self
            .rtp_recovery_interval
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        ScreenSharePerformanceSnapshot {
            new_capture_frames: self.interval_new_capture_frames.swap(0, Ordering::Relaxed),
            repeated_capture_frames: self
                .interval_repeated_capture_frames
                .swap(0, Ordering::Relaxed),
            encoder_worker_late_frames: self
                .interval_encoder_worker_late_frames
                .swap(0, Ordering::Relaxed),
            skipped_capture_sequences: self
                .interval_skipped_capture_sequences
                .swap(0, Ordering::Relaxed),
            encoder_input_frames: self
                .interval_encoder_input_frames
                .swap(0, Ordering::Relaxed),
            encoded_frames: self.interval_encoded_frames.swap(0, Ordering::Relaxed),
            encoded_idr_frames: self.interval_encoded_idr_frames.swap(0, Ordering::Relaxed),
            encoded_delta_frames: self
                .interval_encoded_delta_frames
                .swap(0, Ordering::Relaxed),
            sent_frames: self.interval_sent_frames.swap(0, Ordering::Relaxed),
            dropped_before_initial_idr: self
                .interval_dropped_before_initial_idr
                .swap(0, Ordering::Relaxed),
            sent_idr_frames: self.interval_sent_idr_frames.swap(0, Ordering::Relaxed),
            sent_delta_frames: self.interval_sent_delta_frames.swap(0, Ordering::Relaxed),
            encode_nanos: self.interval_encode_nanos.swap(0, Ordering::Relaxed),
            encode_samples: self.interval_encode_samples.swap(0, Ordering::Relaxed),
            queue_wait_nanos: self.interval_queue_wait_nanos.swap(0, Ordering::Relaxed),
            queue_wait_samples: self.interval_queue_wait_samples.swap(0, Ordering::Relaxed),
            write_sample_nanos: self.interval_write_sample_nanos.swap(0, Ordering::Relaxed),
            write_sample_samples: self
                .interval_write_sample_samples
                .swap(0, Ordering::Relaxed),
            write_sample_bytes: self.interval_write_sample_bytes.swap(0, Ordering::Relaxed),
            write_sample_failures: self
                .interval_write_sample_failures
                .swap(0, Ordering::Relaxed),
            outbound_rtp_packets: self
                .interval_outbound_rtp_packets
                .swap(0, Ordering::Relaxed),
            outbound_rtp_bytes: self.interval_outbound_rtp_bytes.swap(0, Ordering::Relaxed),
            inbound_rtp_packets: self.interval_inbound_rtp_packets.swap(0, Ordering::Relaxed),
            inbound_rtp_bytes: self.interval_inbound_rtp_bytes.swap(0, Ordering::Relaxed),
            inbound_rtp_lost_delta: self.interval_inbound_rtp_lost.swap(0, Ordering::Relaxed),
            received_packets: self.interval_received_packets.swap(0, Ordering::Relaxed),
            observed_sequence_gaps: self
                .interval_observed_sequence_gaps
                .swap(0, Ordering::Relaxed),
            recovered_reordered_packets: self
                .interval_recovered_reordered_packets
                .swap(0, Ordering::Relaxed),
            unmatched_out_of_order_packets: self
                .interval_unmatched_out_of_order_packets
                .swap(0, Ordering::Relaxed),
            duplicate_packets: self.interval_duplicate_packets.swap(0, Ordering::Relaxed),
            confirmed_missing_packets: self
                .interval_confirmed_missing_packets
                .swap(0, Ordering::Relaxed),
            late_after_confirmed_packets: self
                .interval_late_after_confirmed_packets
                .swap(0, Ordering::Relaxed),
            sequence_gap_resyncs: self
                .interval_sequence_gap_resyncs
                .swap(0, Ordering::Relaxed),
            assembled_access_units: self
                .interval_assembled_access_units
                .swap(0, Ordering::Relaxed),
            assembled_idr_with_parameters: h264_interval.assembled_idr_with_parameters,
            assembled_idr_without_parameters: h264_interval.assembled_idr_without_parameters,
            assembled_delta_units: h264_interval.assembled_delta_units,
            assembled_no_frame_units: h264_interval.assembled_no_frame_units,
            dropped_recovery_gate_waiting_for_idr: h264_interval
                .dropped_recovery_gate_waiting_for_idr,
            dropped_worker_waiting_for_idr: h264_interval.dropped_worker_waiting_for_idr,
            dropped_stale_generation: h264_interval.dropped_stale_generation,
            decoder_queue_accepted: h264_interval.decoder_queue_accepted,
            decoder_queue_full: h264_interval.decoder_queue_full,
            decoder_queue_disconnected: h264_interval.decoder_queue_disconnected,
            decoder_inputs_live: h264_interval.decoder_inputs_live,
            decoder_inputs_cached_replay: h264_interval.decoder_inputs_cached_replay,
            assembly_errors: self.interval_assembly_errors.swap(0, Ordering::Relaxed),
            decoder_input_frames: self
                .interval_decoder_input_frames
                .swap(0, Ordering::Relaxed),
            decoder_no_output_frames: self
                .interval_decoder_no_output_frames
                .swap(0, Ordering::Relaxed),
            decoder_queue_drops: self.interval_decoder_queue_drops.swap(0, Ordering::Relaxed),
            decode_errors: self.interval_decode_errors.swap(0, Ordering::Relaxed),
            decoded_frames: self.interval_decoded_frames.swap(0, Ordering::Relaxed),
            published_frames: self.interval_published_frames.swap(0, Ordering::Relaxed),
            ui_texture_updates: self.interval_ui_texture_updates.swap(0, Ordering::Relaxed),
            pli_requests_sent: self.interval_pli_requests_sent.swap(0, Ordering::Relaxed),
            pli_requests_received: self
                .interval_pli_requests_received
                .swap(0, Ordering::Relaxed),
            nack_packets_sent: rtp_recovery_interval.nack_packets_sent.take(),
            nack_packets_received: rtp_recovery_interval.nack_packets_received.take(),
            nack_packets_received_observed: self
                .interval_nack_packets_received_observed
                .swap(0, Ordering::Relaxed),
            retransmitted_packets_sent: rtp_recovery_interval.retransmitted_packets_sent.take(),
            retransmitted_bytes_sent: rtp_recovery_interval.retransmitted_bytes_sent.take(),
            retransmitted_packets_received: rtp_recovery_interval
                .retransmitted_packets_received
                .take(),
            retransmitted_bytes_received: rtp_recovery_interval.retransmitted_bytes_received.take(),
        }
    }

    pub(super) fn record_encode_duration(&self, elapsed: Duration) {
        self.interval_encode_nanos
            .fetch_add(elapsed.as_nanos() as u64, Ordering::Relaxed);
        self.interval_encode_samples.fetch_add(1, Ordering::Relaxed);
    }
}

impl Drop for SharedMetrics {
    fn drop(&mut self) {
        crate::logging::remove_srtp_contexts_for_session(self.session_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_metrics_distinguish_new_frames_from_static_repeats() {
        let metrics = super::SharedMetrics::default();
        metrics.record_capture_frame_for_encoder(false);
        metrics.record_capture_frame_for_encoder(true);
        let snapshot = metrics.take_performance_snapshot();
        assert_eq!(snapshot.new_capture_frames, 1);
        assert_eq!(snapshot.repeated_capture_frames, 1);
        assert_eq!(snapshot.encoder_input_frames, 2);
    }

    #[test]
    fn media_interval_snapshot_reports_each_video_pipeline_stage() {
        let metrics = super::SharedMetrics::default();
        metrics.record_rtp_reorder_window(Duration::from_millis(85), 12);
        metrics
            .interval_received_packets
            .store(12, std::sync::atomic::Ordering::Relaxed);
        metrics.record_sequence_update(super::RtpSequenceUpdate {
            observed_gap_packets: 2,
            recovered_reordered_packets: 1,
            unmatched_out_of_order_packets: 1,
            duplicate_packets: 1,
            confirmed_missing_packets: 1,
            late_after_confirmed_packets: 1,
            reorder_delay_sample: Some(Duration::from_millis(85)),
        });
        metrics.record_sequence_gap_resync();
        metrics.record_assembled_access_unit(&annex_b_access_unit(&[
            &[0x67, 1],
            &[0x68, 1],
            &[0x65, 1],
        ]));
        metrics.record_assembly_error("lacuna de sequência no quadro".to_owned());
        metrics.record_decoder_input_live();
        metrics.record_decoder_no_output();
        metrics.record_decoder_queue_drop();
        metrics.record_pli_sent(super::PliReason::SequenceGap);
        metrics.record_pli_received();
        metrics.record_ui_texture_update();

        let interval = metrics.take_performance_snapshot();
        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.rtp_reorder_window_ms, 85);
        assert_eq!(snapshot.rtp_reorder_samples, 12);
        assert_eq!(interval.received_packets, 12);
        assert_eq!(interval.observed_sequence_gaps, 2);
        assert_eq!(interval.recovered_reordered_packets, 1);
        assert_eq!(interval.unmatched_out_of_order_packets, 1);
        assert_eq!(interval.duplicate_packets, 1);
        assert_eq!(interval.confirmed_missing_packets, 1);
        assert_eq!(interval.late_after_confirmed_packets, 1);
        assert_eq!(interval.sequence_gap_resyncs, 1);
        assert_eq!(interval.assembled_access_units, 1);
        assert_eq!(interval.assembly_errors, 1);
        assert_eq!(interval.decoder_input_frames, 1);
        assert_eq!(interval.decoder_inputs_live, 1);
        assert_eq!(interval.decoder_inputs_cached_replay, 0);
        assert_eq!(interval.decoder_no_output_frames, 1);
        assert_eq!(interval.decoder_queue_drops, 1);
        assert_eq!(interval.pli_requests_sent, 1);
        assert_eq!(interval.pli_requests_received, 1);
        assert_eq!(interval.ui_texture_updates, 1);
        assert_eq!(metrics.take_performance_snapshot().received_packets, 0);
    }

    #[test]
    fn interval_snapshots_are_isolated_between_screen_share_sessions() {
        let mut sender = super::SharedMetrics::default();
        sender.session_id = 101;
        sender.interval_encoded_frames.store(7, Ordering::Relaxed);
        sender
            .interval_outbound_rtp_packets
            .store(31, Ordering::Relaxed);
        sender.record_inbound_nack_packet(0xaaaa_0001);

        let mut receiver = super::SharedMetrics::default();
        receiver.session_id = 202;
        receiver
            .interval_received_packets
            .store(43, Ordering::Relaxed);
        receiver
            .interval_assembled_access_units
            .store(5, Ordering::Relaxed);
        receiver.record_inbound_nack_packet(0xbbbb_0002);

        let sender_metrics = sender.snapshot();
        let receiver_metrics = receiver.snapshot();
        let sender_interval = sender.take_performance_snapshot();
        let receiver_interval = receiver.take_performance_snapshot();

        assert_eq!(sender_metrics.session_id, 101);
        assert_eq!(receiver_metrics.session_id, 202);
        assert_eq!(sender_interval.encoded_frames, 7);
        assert_eq!(sender_interval.outbound_rtp_packets, 31);
        assert_eq!(sender_interval.received_packets, 0);
        assert_eq!(sender_metrics.nack_packets_received_observed, 1);
        assert_eq!(sender_metrics.nack_received_media_ssrc, Some(0xaaaa_0001));
        assert_eq!(receiver_interval.received_packets, 43);
        assert_eq!(receiver_interval.assembled_access_units, 5);
        assert_eq!(receiver_metrics.nack_packets_received_observed, 1);
        assert_eq!(receiver_metrics.nack_received_media_ssrc, Some(0xbbbb_0002));
        assert_eq!(receiver_interval.encoded_frames, 0);

        assert_eq!(sender.take_performance_snapshot().encoded_frames, 0);
        assert_eq!(receiver.take_performance_snapshot().received_packets, 0);
    }

    #[test]
    fn access_unit_classification_and_terminal_destinations_are_counted_per_interval() {
        let metrics = SharedMetrics::default();
        metrics.record_assembled_access_unit(&annex_b_access_unit(&[
            &[0x67, 1],
            &[0x68, 1],
            &[0x65, 1],
        ]));
        metrics.record_assembled_access_unit(&annex_b_access_unit(&[&[0x67, 1], &[0x65, 1]]));
        metrics.record_assembled_access_unit(&annex_b_access_unit(&[&[0x41, 1]]));
        metrics.record_assembled_access_unit(&annex_b_access_unit(&[&[0x67, 1], &[0x68, 1]]));

        metrics.record_drop_recovery_gate_waiting_for_idr();
        metrics.record_drop_worker_waiting_for_idr();
        metrics.record_drop_stale_generation();
        metrics.record_decoder_queue_accepted();
        metrics.record_decoder_queue_drop();
        metrics.record_decoder_queue_disconnected();
        metrics.record_decoder_input_live();
        metrics.record_decoder_input_cached_replay();
        metrics.record_inbound_nack_packet(0x1122_3344);

        let interval = metrics.take_performance_snapshot();
        assert_eq!(interval.assembled_idr_with_parameters, 1);
        assert_eq!(interval.assembled_idr_without_parameters, 1);
        assert_eq!(interval.assembled_delta_units, 1);
        assert_eq!(interval.assembled_no_frame_units, 1);
        assert_eq!(interval.dropped_recovery_gate_waiting_for_idr, 1);
        assert_eq!(interval.dropped_worker_waiting_for_idr, 1);
        assert_eq!(interval.dropped_stale_generation, 1);
        assert_eq!(interval.decoder_queue_accepted, 1);
        assert_eq!(interval.decoder_queue_full, 1);
        assert_eq!(interval.decoder_queue_disconnected, 1);
        assert_eq!(interval.decoder_inputs_live, 1);
        assert_eq!(interval.decoder_inputs_cached_replay, 1);
        assert_eq!(interval.nack_packets_received_observed, 1);

        let next_interval = metrics.take_performance_snapshot();
        assert_eq!(next_interval.assembled_idr_with_parameters, 0);
        assert_eq!(next_interval.dropped_recovery_gate_waiting_for_idr, 0);
        assert_eq!(next_interval.decoder_queue_full, 0);
        let lifetime = metrics.snapshot();
        assert_eq!(lifetime.assembled_idr_with_parameters, 1);
        assert_eq!(lifetime.assembled_idr_without_parameters, 1);
        assert_eq!(lifetime.assembled_delta_units, 1);
        assert_eq!(lifetime.assembled_no_frame_units, 1);
        assert_eq!(lifetime.decoder_queue_accepted, 1);
        assert_eq!(lifetime.decoder_queue_drops, 1);
        assert_eq!(lifetime.nack_packets_received_observed, 1);
        assert_eq!(lifetime.nack_received_media_ssrc, Some(0x1122_3344));
    }

    #[test]
    fn optional_rtp_recovery_counters_are_unknown_until_two_stats_samples_exist() {
        let metrics = SharedMetrics::default();
        let mut previous = Some(PeerStatsSnapshot {
            inbound_nack_count: Some(4),
            outbound_nack_count: Some(7),
            outbound_retransmitted_packets: Some(2),
            outbound_retransmitted_bytes: Some(1200),
            inbound_retransmitted_packets: Some(1),
            inbound_retransmitted_bytes: Some(600),
            ..PeerStatsSnapshot::default()
        });
        let mut current = previous.clone().unwrap();
        current.inbound_nack_count = Some(6);
        current.outbound_nack_count = Some(10);
        current.outbound_retransmitted_packets = Some(5);
        current.outbound_retransmitted_bytes = Some(2400);
        current.inbound_retransmitted_packets = Some(2);
        current.inbound_retransmitted_bytes = Some(1100);
        metrics.update_transport_diagnostics(current, &mut previous);

        let interval = metrics.take_performance_snapshot();
        assert_eq!(interval.nack_packets_sent, Some(2));
        assert_eq!(interval.nack_packets_received, Some(3));
        assert_eq!(interval.retransmitted_packets_sent, Some(3));
        assert_eq!(interval.retransmitted_bytes_sent, Some(1200));
        assert_eq!(interval.retransmitted_packets_received, Some(1));
        assert_eq!(interval.retransmitted_bytes_received, Some(500));

        let total = metrics.snapshot();
        let interval_label = rtp_recovery_interval_label(&total, &interval);
        assert!(interval_label.contains("NACK_RTCP_pacotes_enviados_intervalo/total=2 / 6"));
        assert!(interval_label.contains("RTX_enviado_pacotes/bytes_intervalo=3/1200"));

        let unknown_metrics = SharedMetrics::default();
        assert_eq!(unknown_metrics.snapshot().nack_packets_sent, None);
        assert_eq!(unknown_metrics.snapshot().retransmitted_packets_sent, None);
        let unknown_total = unknown_metrics.snapshot();
        let unknown_interval = unknown_metrics.take_performance_snapshot();
        assert_eq!(unknown_interval.nack_packets_sent, None);
        let unknown_label = rtp_recovery_interval_label(&unknown_total, &unknown_interval);
        assert!(
            unknown_label
                .contains("NACK_RTCP_pacotes_enviados_intervalo/total=desconhecido / desconhecido")
        );

        let zero_metrics = SharedMetrics::default();
        let zero_sample = PeerStatsSnapshot {
            inbound_nack_count: Some(0),
            outbound_nack_count: Some(0),
            outbound_retransmitted_packets: Some(0),
            outbound_retransmitted_bytes: Some(0),
            inbound_retransmitted_packets: Some(0),
            inbound_retransmitted_bytes: Some(0),
            ..PeerStatsSnapshot::default()
        };
        let mut previous_zero = Some(zero_sample.clone());
        zero_metrics.update_transport_diagnostics(zero_sample, &mut previous_zero);
        let zero_total = zero_metrics.snapshot();
        let zero_interval = zero_metrics.take_performance_snapshot();
        assert_eq!(zero_interval.nack_packets_sent, Some(0));
        let zero_label = rtp_recovery_interval_label(&zero_total, &zero_interval);
        assert!(zero_label.contains("NACK_RTCP_pacotes_enviados_intervalo/total=0 / 0"));
    }

    #[test]
    fn repeated_media_errors_are_logged_at_doubling_counts() {
        assert!([1, 2, 4, 8, 16].into_iter().all(should_log_aggregate_error));
        assert!(
            [0, 3, 5, 25, 100]
                .into_iter()
                .all(|count| !should_log_aggregate_error(count))
        );
    }
}
