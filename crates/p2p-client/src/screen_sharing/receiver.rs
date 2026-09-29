use super::*;

#[derive(Default)]
struct RtpSequenceTracker {
    highest_sequence: Option<u16>,
    recent_seen: HashSet<u16>,
    recent_order: VecDeque<u16>,
    pending_missing: HashMap<u16, Instant>,
    confirmed_missing: HashSet<u16>,
    confirmed_order: VecDeque<u16>,
}

const MAX_BUFFERED_RTP_PACKETS: usize = 512;

#[derive(Default)]
struct RtpReorderBuffer {
    next_sequence: Option<u16>,
    pending: HashMap<u16, BufferedRtpPacket>,
    gap_since: Option<Instant>,
}

struct BufferedRtpPacket {
    received_at: Instant,
    packet: RtpPacket,
}

#[derive(Default)]
struct RtpReorderUpdate {
    ordered_packets: Vec<RtpPacket>,
    confirmed_missing_packets: u64,
    resume_timestamp: Option<u32>,
    overflowed: bool,
}

impl RtpReorderUpdate {
    fn append(&mut self, mut other: Self) {
        self.ordered_packets.append(&mut other.ordered_packets);
        self.confirmed_missing_packets = self
            .confirmed_missing_packets
            .saturating_add(other.confirmed_missing_packets);
        if self.resume_timestamp.is_none() {
            self.resume_timestamp = other.resume_timestamp;
        }
        self.overflowed |= other.overflowed;
    }
}

impl RtpReorderBuffer {
    fn push(&mut self, packet: RtpPacket, now: Instant) -> RtpReorderUpdate {
        let sequence = packet.header.sequence_number;
        let mut update = RtpReorderUpdate::default();
        let next_sequence = *self.next_sequence.get_or_insert(sequence);
        let distance = sequence.wrapping_sub(next_sequence);

        // A packet behind the released sequence was either duplicated or arrived too late.
        // RtpSequenceTracker owns the diagnostic classification; never feed it to H.264 again.
        if distance >= 0x8000 {
            return update;
        }
        if self.pending.contains_key(&sequence) {
            return update;
        }
        if distance != 0 && self.pending.len() >= MAX_BUFFERED_RTP_PACKETS {
            update.overflowed = true;
            self.refresh_gap_since();
            return update;
        }

        self.pending.insert(
            sequence,
            BufferedRtpPacket {
                received_at: now,
                packet,
            },
        );
        self.drain_contiguous(&mut update.ordered_packets);
        self.refresh_gap_since();
        update
    }

    fn expire(&mut self, now: Instant) -> RtpReorderUpdate {
        let mut update = RtpReorderUpdate::default();
        let gap_expired = self
            .gap_since
            .is_some_and(|since| now.saturating_duration_since(since) >= RTP_REORDER_DELAY);
        if !gap_expired {
            return update;
        }

        let Some(expected) = self.next_sequence else {
            self.gap_since = None;
            return update;
        };
        let Some(next_available) = self
            .pending
            .keys()
            .copied()
            .min_by_key(|sequence| sequence.wrapping_sub(expected))
        else {
            self.gap_since = None;
            return update;
        };
        let skipped = next_available.wrapping_sub(expected);
        if skipped == 0 || skipped >= 0x8000 {
            self.refresh_gap_since();
            return update;
        }

        update.confirmed_missing_packets = u64::from(skipped);
        update.resume_timestamp = self
            .pending
            .get(&next_available)
            .map(|buffered| buffered.packet.header.timestamp);
        self.next_sequence = Some(next_available);
        self.drain_contiguous(&mut update.ordered_packets);
        self.refresh_gap_since();
        update
    }

    fn drain_contiguous(&mut self, ordered: &mut Vec<RtpPacket>) {
        while let Some(sequence) = self.next_sequence {
            let Some(buffered) = self.pending.remove(&sequence) else {
                break;
            };
            ordered.push(buffered.packet);
            self.next_sequence = Some(sequence.wrapping_add(1));
        }
    }

    fn refresh_gap_since(&mut self) {
        let Some(expected) = self.next_sequence else {
            self.gap_since = None;
            return;
        };
        self.gap_since = self
            .pending
            .iter()
            .filter(|(sequence, _)| {
                let distance = sequence.wrapping_sub(expected);
                distance > 0 && distance < 0x8000
            })
            .min_by_key(|(sequence, _)| sequence.wrapping_sub(expected))
            .map(|(_, packet)| packet.received_at);
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct RtpSequenceUpdate {
    pub(super) observed_gap_packets: u64,
    pub(super) recovered_reordered_packets: u64,
    pub(super) unmatched_out_of_order_packets: u64,
    pub(super) duplicate_packets: u64,
    pub(super) confirmed_missing_packets: u64,
    pub(super) late_after_confirmed_packets: u64,
}

impl RtpSequenceTracker {
    fn observe(&mut self, sequence: u16, now: Instant) -> RtpSequenceUpdate {
        let mut update = self.expire(now);
        if self.recent_seen.contains(&sequence) {
            update.duplicate_packets += 1;
            return update;
        }

        if self.confirmed_missing.remove(&sequence) {
            update.late_after_confirmed_packets += 1;
        }

        match self.highest_sequence {
            None => self.highest_sequence = Some(sequence),
            Some(highest) => {
                let distance = sequence.wrapping_sub(highest);
                if distance == 0 {
                    update.duplicate_packets += 1;
                    return update;
                }
                if distance < 0x8000 {
                    if distance > 1 {
                        update.observed_gap_packets += u64::from(distance - 1);
                        for offset in 1..distance {
                            let missing = highest.wrapping_add(offset);
                            self.pending_missing.entry(missing).or_insert(now);
                        }
                    }
                    self.highest_sequence = Some(sequence);
                } else if self.pending_missing.remove(&sequence).is_some() {
                    update.recovered_reordered_packets += 1;
                } else if update.late_after_confirmed_packets == 0 {
                    update.unmatched_out_of_order_packets += 1;
                }
            }
        }

        self.remember(sequence);
        update
    }

    fn expire(&mut self, now: Instant) -> RtpSequenceUpdate {
        let expired = self
            .pending_missing
            .iter()
            .filter_map(|(sequence, since)| {
                (now.saturating_duration_since(*since) >= RTP_REORDER_DELAY).then_some(*sequence)
            })
            .collect::<Vec<_>>();
        for sequence in &expired {
            self.pending_missing.remove(sequence);
            self.confirmed_missing.insert(*sequence);
            self.confirmed_order.push_back(*sequence);
        }
        while self.confirmed_order.len() > RTP_SEQUENCE_HISTORY {
            if let Some(oldest) = self.confirmed_order.pop_front() {
                self.confirmed_missing.remove(&oldest);
            }
        }
        RtpSequenceUpdate {
            confirmed_missing_packets: expired.len() as u64,
            ..RtpSequenceUpdate::default()
        }
    }

    fn remember(&mut self, sequence: u16) {
        if self.recent_seen.insert(sequence) {
            self.recent_order.push_back(sequence);
        }
        while self.recent_order.len() > RTP_SEQUENCE_HISTORY {
            if let Some(oldest) = self.recent_order.pop_front() {
                self.recent_seen.remove(&oldest);
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) enum PliReason {
    SequenceGap,
    AssemblyError,
    DecodeError,
    DecoderQueueFull,
    #[cfg(test)]
    Explicit,
}

#[derive(Default)]
struct H264AccessUnitAssembler {
    frames: HashMap<u32, PendingRtpFrame>,
    discard_timestamp: Option<u32>,
}

struct PendingRtpFrame {
    packets: Vec<RtpPacket>,
    first_received: Instant,
    marker_seen: bool,
}

struct QueuedAccessUnit {
    generation: u64,
    bytes: Vec<u8>,
}

#[derive(Default)]
pub(super) struct KeyframeRequestLimiter {
    last_request: Option<Instant>,
}

impl KeyframeRequestLimiter {
    fn allow(&mut self, now: Instant) -> bool {
        if self
            .last_request
            .is_some_and(|last| now.saturating_duration_since(last) < KEYFRAME_REQUEST_MIN_INTERVAL)
        {
            return false;
        }
        self.last_request = Some(now);
        true
    }
}

pub(super) async fn send_picture_loss_indication(
    track: &dyn TrackRemote,
    metrics: &SharedMetrics,
    limiter: &mut KeyframeRequestLimiter,
    reason: PliReason,
) {
    if !limiter.allow(Instant::now()) {
        return;
    }
    let Some(media_ssrc) = track.ssrcs().await.first().copied() else {
        tracing::warn!(
            screen_share_session = metrics.session_id,
            ?reason,
            "Nao foi possivel enviar PLI: a faixa remota nao tem SSRC"
        );
        return;
    };
    let pli = PictureLossIndication {
        sender_ssrc: 0,
        media_ssrc,
    };
    match track.write_rtcp(vec![Box::new(pli)]).await {
        Ok(()) => {
            metrics.record_pli_sent(reason);
            tracing::debug!(
                screen_share_session = metrics.session_id,
                track_ssrc = media_ssrc,
                ?reason,
                "Pedido RTCP PLI enviado para solicitar um quadro-chave"
            );
        }
        Err(error) => tracing::warn!(
            screen_share_session = metrics.session_id,
            track_ssrc = media_ssrc,
            ?reason,
            error = %error,
            "Falha ao enviar pedido RTCP PLI"
        ),
    }
}

pub(super) fn begin_stream_resync(
    generation: &AtomicU64,
    metrics: &SharedMetrics,
    keyframe_request_tx: &mpsc::Sender<PliReason>,
    reason: PliReason,
) -> u64 {
    let generation = generation.fetch_add(1, Ordering::Relaxed) + 1;
    metrics.record_keyframe_resync();
    let _ = keyframe_request_tx.try_send(reason);
    generation
}

impl H264AccessUnitAssembler {
    fn push(&mut self, packet: RtpPacket) -> Option<String> {
        let timestamp = packet.header.timestamp;
        if let Some(discard_timestamp) = self.discard_timestamp {
            if discard_timestamp == timestamp {
                if packet.header.marker {
                    self.discard_timestamp = None;
                }
                return None;
            }
            self.discard_timestamp = None;
        }
        let now = Instant::now();
        let mut evicted = None;
        if !self.frames.contains_key(&timestamp) && self.frames.len() >= MAX_PENDING_RTP_FRAMES {
            if let Some(oldest_timestamp) = self
                .frames
                .iter()
                .min_by_key(|(_, frame)| frame.first_received)
                .map(|(timestamp, _)| *timestamp)
            {
                self.frames.remove(&oldest_timestamp);
                evicted = Some("limite de quadros RTP pendentes excedido".to_owned());
            }
        }

        let frame = self
            .frames
            .entry(timestamp)
            .or_insert_with(|| PendingRtpFrame {
                packets: Vec::new(),
                first_received: now,
                marker_seen: false,
            });
        frame.marker_seen |= packet.header.marker;
        frame.packets.push(packet);
        evicted
    }

    fn discard_incomplete_after_loss(&mut self, resume_timestamp: Option<u32>) {
        self.frames.clear();
        self.discard_timestamp = resume_timestamp;
    }

    fn take_ready(&mut self, now: Instant) -> Vec<Result<Vec<u8>, String>> {
        let mut ready_timestamps = self
            .frames
            .iter()
            .filter_map(|(timestamp, frame)| {
                let age = now.saturating_duration_since(frame.first_received);
                (age >= RTP_REORDER_DELAY && frame.marker_seen || age >= MAX_RTP_FRAME_AGE)
                    .then_some(*timestamp)
            })
            .collect::<Vec<_>>();
        ready_timestamps.sort_by(|left, right| {
            if left == right {
                std::cmp::Ordering::Equal
            } else if (left.wrapping_sub(*right) as i32) < 0 {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Greater
            }
        });

        ready_timestamps
            .into_iter()
            .filter_map(|timestamp| self.frames.remove(&timestamp))
            .map(|frame| {
                if !frame.marker_seen {
                    return Err("quadro RTP expirou sem marcador de fim".to_owned());
                }
                assemble_h264_access_unit(frame.packets)
            })
            .collect()
    }
}

fn assemble_h264_access_unit(mut packets: Vec<RtpPacket>) -> Result<Vec<u8>, String> {
    if packets.is_empty() {
        return Err("quadro RTP sem pacotes".to_owned());
    }

    let first_sequence = packets
        .iter()
        .map(|packet| packet.header.sequence_number)
        .reduce(|current, candidate| {
            let distance = current.wrapping_sub(candidate);
            if distance > 0 && distance < 0x8000 {
                candidate
            } else {
                current
            }
        })
        .unwrap_or_default();
    packets.sort_by_key(|packet| packet.header.sequence_number.wrapping_sub(first_sequence));
    packets.dedup_by_key(|packet| packet.header.sequence_number);

    if !packets.last().is_some_and(|packet| packet.header.marker) {
        return Err("marcador RTP não está no último pacote do quadro".to_owned());
    }
    if packets.first().is_some_and(|packet| {
        let Some(header) = packet.payload.first() else {
            return true;
        };
        if header & 0x1f == 28 {
            packet
                .payload
                .get(1)
                .is_none_or(|fu_header| fu_header & 0x80 == 0)
        } else {
            false
        }
    }) {
        return Err("o quadro começa no meio de um fragmento FU-A".to_owned());
    }

    for pair in packets.windows(2) {
        if pair[1].header.sequence_number != pair[0].header.sequence_number.wrapping_add(1) {
            return Err("há pacote(s) RTP ausente(s) dentro do quadro".to_owned());
        }
    }

    let mut depacketizer = H264Packet::default();
    let mut output = Vec::new();
    let mut active_fragment: Option<u8> = None;
    for packet in packets {
        let payload = &packet.payload;
        let Some(header) = payload.first() else {
            return Err("pacote RTP H.264 vazio".to_owned());
        };
        let packet_type = header & 0x1f;
        if packet_type == 28 {
            let Some(fu_header) = payload.get(1) else {
                return Err("cabeçalho FU-A incompleto".to_owned());
            };
            let nal_type = fu_header & 0x1f;
            let starts_nal = fu_header & 0x80 != 0;
            let ends_nal = fu_header & 0x40 != 0;
            if starts_nal {
                if active_fragment.is_some() {
                    return Err("um fragmento FU-A começou antes do anterior terminar".to_owned());
                }
                active_fragment = Some(nal_type);
            } else if active_fragment != Some(nal_type) {
                return Err("fragmento FU-A sem início correspondente".to_owned());
            }
            if ends_nal {
                active_fragment = None;
            }
        } else if active_fragment.is_some() {
            return Err("NAL H.264 interrompeu um fragmento FU-A".to_owned());
        }

        if packet_type == 24 {
            validate_stap_a(payload)?;
        }

        let depacketized = depacketizer
            .depacketize(payload)
            .map_err(|error| format!("depacketização H.264 falhou: {error}"))?;
        output.extend_from_slice(&depacketized);
    }
    if active_fragment.is_some() {
        return Err("quadro terminou antes do fim do fragmento FU-A".to_owned());
    }
    if output.is_empty() {
        return Err("quadro RTP não produziu dados H.264".to_owned());
    }
    Ok(output)
}

fn validate_stap_a(payload: &[u8]) -> Result<(), String> {
    let mut offset = 1;
    while offset < payload.len() {
        if offset + 2 > payload.len() {
            return Err("cabeçalho STAP-A incompleto".to_owned());
        }
        let nal_len = u16::from_be_bytes([payload[offset], payload[offset + 1]]) as usize;
        offset += 2;
        if nal_len == 0 || offset + nal_len > payload.len() {
            return Err("tamanho de NAL inválido dentro do STAP-A".to_owned());
        }
        offset += nal_len;
    }
    Ok(())
}

enum ActiveH264Decoder {
    OpenH264(Decoder),
    #[cfg(windows)]
    MediaFoundation(mf_video::HardwareDecoder),
}

struct DecodeFailure {
    detail: String,
    hardware_decoder: bool,
}

fn decode_h264_access_unit(
    access_unit: &[u8],
    decoder: &mut ActiveH264Decoder,
    context: &egui::Context,
    remote_frame: &RemoteFrameStore,
    sequence: &AtomicU64,
    metrics: &SharedMetrics,
) -> Option<DecodeFailure> {
    let nal_types = annex_b_nal_types(access_unit);
    let sample_diagnostics = describe_nal_types(&nal_types, access_unit.len());
    let decoded = match decoder {
        ActiveH264Decoder::OpenH264(decoder) => decoder
            .decode(access_unit)
            .map(|frame| {
                frame.map(|yuv| {
                    let (width, height) = yuv.dimensions();
                    let mut rgba = vec![0; yuv.rgba8_len()];
                    yuv.write_rgba8(&mut rgba);
                    (width as u32, height as u32, rgba)
                })
            })
            .map_err(|error| {
                let detail = if error.native_code() & 0x10 != 0 {
                    format!(
                        "{error} (dsNoParamSets: SPS/PPS ausentes ou incompatíveis; quadro: {sample_diagnostics})"
                    )
                } else {
                    format!("{error}; quadro: {sample_diagnostics}")
                };
                (detail, Some(error.native_code()))
            }),
        #[cfg(windows)]
        ActiveH264Decoder::MediaFoundation(decoder) => decoder
            .decode(access_unit)
            .map(|frame| frame.map(|frame| (frame.width, frame.height, frame.rgba)))
            .map_err(|error| (format!("{error}; quadro: {sample_diagnostics}"), None)),
    };

    match decoded {
        Ok(Some((width, height, rgba))) => {
            if width == 0 || height == 0 {
                return None;
            }
            metrics.decoded_frames.fetch_add(1, Ordering::Relaxed);
            metrics
                .interval_decoded_frames
                .fetch_add(1, Ordering::Relaxed);
            let frame_kind = classify_h264_access_unit(&nal_types);
            match frame_kind {
                Some(H264FrameKind::Delta) => {
                    metrics.decoded_delta_frames.fetch_add(1, Ordering::Relaxed);
                }
                Some(H264FrameKind::Idr) => {
                    metrics.decoded_idr_frames.fetch_add(1, Ordering::Relaxed);
                    metrics.record_idr_decoded();
                }
                None => {}
            }
            let next_sequence = sequence.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
            *remote_frame
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) =
                Some(Arc::new(PreviewFrame {
                    sequence: next_sequence,
                    width,
                    height,
                    rgba,
                    #[cfg(windows)]
                    gpu_nv12: None,
                    #[cfg(windows)]
                    cpu_nv12: None,
                }));
            metrics.published_frames.fetch_add(1, Ordering::Relaxed);
            metrics
                .interval_published_frames
                .fetch_add(1, Ordering::Relaxed);
            if frame_kind == Some(H264FrameKind::Idr) {
                metrics.record_idr_published();
            }
            context.request_repaint();
            None
        }
        Ok(None) => {
            metrics.record_decoder_no_output();
            None
        }
        Err((detail, native_code)) => {
            #[cfg(windows)]
            let hardware_decoder = matches!(&*decoder, ActiveH264Decoder::MediaFoundation(_));
            #[cfg(not(windows))]
            let hardware_decoder = false;
            let count = metrics.decode_errors.fetch_add(1, Ordering::Relaxed) + 1;
            metrics
                .interval_decode_errors
                .fetch_add(1, Ordering::Relaxed);
            *metrics
                .last_decode_error
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(detail.clone());
            if should_log_aggregate_error(count) {
                tracing::warn!(
                    screen_share_session = metrics.session_id,
                    decode_errors = count,
                    native_code = native_code.unwrap_or_default(),
                    frame = %sample_diagnostics,
                    error = %detail,
                    backend = %metrics.decoder_backend.lock().unwrap_or_else(std::sync::PoisonError::into_inner),
                    "Decodificador de vídeo rejeitou quadro; resumos repetidos registrados em contagens dobradas"
                );
            }
            context.request_repaint();
            Some(DecodeFailure {
                detail,
                hardware_decoder,
            })
        }
    }
}

fn is_hardware_device_failure(error: &str) -> bool {
    let error = error.to_ascii_lowercase();
    [
        "dxva",
        "d3d11",
        "dispositivo de vídeo",
        "superfície d3d",
        "mf_e_transform_stream_change",
        "0x887a",
        "0xc00d36b5",
    ]
    .iter()
    .any(|needle| error.contains(needle))
}

fn update_cached_idr(
    access_unit: &[u8],
    cached_sps: &mut Option<Vec<u8>>,
    cached_pps: &mut Option<Vec<u8>>,
) -> Option<Vec<u8>> {
    let nals = annex_b_nals(access_unit);
    for nal in &nals {
        match nal.first().map(|header| header & 0x1f) {
            Some(7) => *cached_sps = Some(nal.to_vec()),
            Some(8) => *cached_pps = Some(nal.to_vec()),
            _ => {}
        }
    }
    let (Some(sps), Some(pps)) = (cached_sps.as_deref(), cached_pps.as_deref()) else {
        return None;
    };
    if !nals
        .iter()
        .any(|nal| nal.first().is_some_and(|header| header & 0x1f == 5))
    {
        return None;
    }

    let mut cached = Vec::with_capacity(access_unit.len() + sps.len() + pps.len() + 8);
    cached.extend_from_slice(&[0, 0, 0, 1]);
    cached.extend_from_slice(sps);
    cached.extend_from_slice(&[0, 0, 0, 1]);
    cached.extend_from_slice(pps);
    for nal in nals {
        let nal_type = nal.first().map(|header| header & 0x1f);
        if nal_type != Some(7) && nal_type != Some(8) {
            cached.extend_from_slice(&[0, 0, 0, 1]);
            cached.extend_from_slice(nal);
        }
    }
    Some(cached)
}

fn dxva_watchdog_expired(last_input_at: Option<Instant>, now: Instant) -> bool {
    last_input_at
        .is_some_and(|last| now.saturating_duration_since(last) >= DXVA_FIRST_OUTPUT_WATCHDOG)
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct DxvaThroughputSnapshot {
    inputs: usize,
    outputs: usize,
    output_ratio: f64,
}

fn should_use_adaptive_decoder_fallback(preference: VideoDecoderPreference) -> bool {
    preference == VideoDecoderPreference::Automatic
}

fn should_try_hardware_decoder(
    preference: VideoDecoderPreference,
    hardware_disabled: bool,
    decoder_missing: bool,
    cpu_waiting_for_sps: bool,
    access_unit_has_sps_idr: bool,
) -> bool {
    preference != VideoDecoderPreference::Cpu
        && !hardware_disabled
        && (decoder_missing || (cpu_waiting_for_sps && access_unit_has_sps_idr))
}

#[derive(Default)]
struct DxvaThroughputMonitor {
    first_input_at: Option<Instant>,
    measurement_started_at: Option<Instant>,
    samples: VecDeque<(Instant, bool)>,
}

impl DxvaThroughputMonitor {
    fn observe(&mut self, now: Instant, produced_output: bool) -> Option<DxvaThroughputSnapshot> {
        let first_input_at = *self.first_input_at.get_or_insert(now);
        if now.saturating_duration_since(first_input_at) < DXVA_ADAPTIVE_WARMUP {
            return None;
        }

        let measurement_started_at = *self.measurement_started_at.get_or_insert(now);
        self.samples.push_back((now, produced_output));
        while self
            .samples
            .front()
            .is_some_and(|(at, _)| now.saturating_duration_since(*at) > DXVA_ADAPTIVE_WINDOW)
        {
            self.samples.pop_front();
        }

        if now.saturating_duration_since(measurement_started_at) < DXVA_ADAPTIVE_WINDOW
            || self.samples.len() < DXVA_ADAPTIVE_MIN_INPUTS
        {
            return None;
        }

        let inputs = self.samples.len();
        let outputs = self.samples.iter().filter(|(_, output)| *output).count();
        let output_ratio = outputs as f64 / inputs as f64;
        (output_ratio < DXVA_ADAPTIVE_MIN_OUTPUT_RATIO).then_some(DxvaThroughputSnapshot {
            inputs,
            outputs,
            output_ratio,
        })
    }
}

#[derive(Default)]
struct CachedH264Gop {
    idr: Option<Vec<u8>>,
    following: VecDeque<Vec<u8>>,
    byte_len: usize,
    complete: bool,
}

impl CachedH264Gop {
    fn start_at_idr(&mut self, idr: Vec<u8>) {
        self.byte_len = idr.len();
        self.idr = Some(idr);
        self.following.clear();
        self.complete = self.byte_len <= MAX_CACHED_GOP_BYTES;
    }

    fn push_delta(&mut self, access_unit: &[u8]) {
        if !self.complete
            || classify_h264_access_unit(&annex_b_nal_types(access_unit))
                != Some(H264FrameKind::Delta)
        {
            return;
        }

        self.byte_len = self.byte_len.saturating_add(access_unit.len());
        if self.following.len() >= MAX_CACHED_GOP_ACCESS_UNITS
            || self.byte_len > MAX_CACHED_GOP_BYTES
        {
            self.complete = false;
            self.following.clear();
            return;
        }
        self.following.push_back(access_unit.to_vec());
    }

    fn invalidate_delta_chain(&mut self) {
        self.complete = false;
        self.following.clear();
    }

    fn replay_chain(&self) -> Option<Vec<&[u8]>> {
        if !self.complete {
            return None;
        }
        let idr = self.idr.as_deref()?;
        let mut chain = Vec::with_capacity(1 + self.following.len());
        chain.push(idr);
        chain.extend(self.following.iter().map(Vec::as_slice));
        Some(chain)
    }
}

fn should_decode_access_unit(waiting_for_idr: &mut bool, access_unit: &[u8]) -> bool {
    if !*waiting_for_idr {
        return true;
    }
    let nals = annex_b_nal_types(access_unit);
    if nals.contains(&7) && nals.contains(&8) && nals.contains(&5) {
        *waiting_for_idr = false;
        true
    } else {
        false
    }
}

impl PeerEvents {
    pub(super) async fn receive_remote_track(&self, track: Arc<dyn TrackRemote>) {
        if let Some(ssrc) = track.ssrcs().await.first().copied() {
            self.metrics.set_track_ssrc(ssrc);
            tracing::info!(
                screen_share_session = self.metrics.session_id,
                track_ssrc = ssrc,
                "Faixa RTP de vídeo remota identificada"
            );
        }
        tracing::info!(
            screen_share_session = self.metrics.session_id,
            "Faixa de vídeo remota recebida; iniciando depacketizador e worker de codec"
        );
        let events = self.events.clone();
        let context = self.context.clone();
        let remote_frame = Arc::clone(&self.remote_frame);
        let sequence = Arc::clone(&self.remote_frame_sequence);
        let metrics = Arc::clone(&self.metrics);
        let decoder_preference = self.decoder_preference;
        *self
            .remote_track
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::clone(&track));
        tokio::spawn(async move {
            let (decoder_tx, decoder_rx) = std_mpsc::sync_channel::<QueuedAccessUnit>(8);
            let decoder_generation = Arc::new(AtomicU64::new(0));
            let worker_generation = Arc::clone(&decoder_generation);
            let (keyframe_request_tx, mut keyframe_request_rx) = mpsc::channel::<PliReason>(1);
            let worker_keyframe_request_tx = keyframe_request_tx.clone();
            let worker_events = events.clone();
            let worker_context = context.clone();
            let worker_remote_frame = Arc::clone(&remote_frame);
            let worker_sequence = Arc::clone(&sequence);
            let worker_metrics = Arc::clone(&metrics);
            let worker_decoder_preference = decoder_preference;
            let worker = thread::Builder::new()
                .name("p2p-h264-decoder".to_owned())
                .spawn(move || {
                    let mut decoder: Option<ActiveH264Decoder> = None;
                    let mut using_cpu_pending_sps = false;
                    let mut cpu_waiting_for_idr = true;
                    let mut hardware_frames_without_output = 0u32;
                    let mut generation_seen = 0_u64;
                    let mut cached_sps = None;
                    let mut cached_pps = None;
                    let mut cached_idr = None;
                    let mut cached_gop = CachedH264Gop::default();
                    let mut dxva_throughput = DxvaThroughputMonitor::default();
                    let mut last_hardware_input_at: Option<Instant> = None;
                    let mut hardware_decoder_disabled =
                        worker_decoder_preference == VideoDecoderPreference::Cpu;
                    loop {
                        let watchdog_wait = if matches!(
                            decoder.as_ref(),
                            Some(ActiveH264Decoder::MediaFoundation(_))
                        ) && hardware_frames_without_output > 0
                        {
                            last_hardware_input_at
                                .map(|last| DXVA_FIRST_OUTPUT_WATCHDOG.saturating_sub(last.elapsed()))
                                .unwrap_or(DXVA_FIRST_OUTPUT_WATCHDOG)
                        } else {
                            Duration::from_secs(60)
                        };
                        let queued = match decoder_rx.recv_timeout(watchdog_wait) {
                            Ok(queued) => queued,
                            Err(std_mpsc::RecvTimeoutError::Disconnected) => break,
                            Err(std_mpsc::RecvTimeoutError::Timeout) => {
                                if !matches!(
                                    decoder.as_ref(),
                                    Some(ActiveH264Decoder::MediaFoundation(_))
                                ) || hardware_frames_without_output == 0
                                    || !dxva_watchdog_expired(last_hardware_input_at, Instant::now())
                                {
                                    continue;
                                }

                                let reason = format!(
                                    "DXVA recebeu quadro(s), mas não publicou imagem em {} ms; usando OpenH264 na CPU.",
                                    DXVA_FIRST_OUTPUT_WATCHDOG.as_millis()
                                );
                                tracing::warn!(fallback_reason = %reason, cached_idr = cached_idr.is_some(), "Watchdog do primeiro quadro DXVA acionado");
                                match Decoder::new() {
                                    Ok(cpu) => {
                                        hardware_decoder_disabled = true;
                                        worker_metrics.set_decoder_backend(
                                            "CPU — OpenH264".to_owned(),
                                            Some(reason),
                                        );
                                        decoder = Some(ActiveH264Decoder::OpenH264(cpu));
                                        using_cpu_pending_sps = false;
                                        hardware_frames_without_output = 0;
                                        last_hardware_input_at = None;
                                        cpu_waiting_for_idr = true;

                                        if let Some(idr) = cached_idr.as_deref() {
                                            let before = worker_metrics.decoded_frames.load(Ordering::Relaxed);
                                            if should_decode_access_unit(&mut cpu_waiting_for_idr, idr) {
                                                worker_metrics.record_decoder_input();
                                                if let Some(active_decoder) = decoder.as_mut() {
                                                    let failure = decode_h264_access_unit(
                                                        idr,
                                                        active_decoder,
                                                        &worker_context,
                                                        &worker_remote_frame,
                                                        &worker_sequence,
                                                        &worker_metrics,
                                                    );
                                                    if let Some(failure) = failure {
                                                        tracing::warn!(error = %failure.detail, "OpenH264 não conseguiu publicar o IDR em cache; solicitando outro quadro-chave");
                                                    }
                                                }
                                            }
                                            if worker_metrics.decoded_frames.load(Ordering::Relaxed) == before {
                                                let generation = begin_stream_resync(
                                                    &worker_generation,
                                                    &worker_metrics,
                                                    &worker_keyframe_request_tx,
                                                    PliReason::DecodeError,
                                                );
                                                generation_seen = generation;
                                                cpu_waiting_for_idr = true;
                                            }
                                        } else {
                                            let generation = begin_stream_resync(
                                                &worker_generation,
                                                &worker_metrics,
                                                &worker_keyframe_request_tx,
                                                PliReason::DecodeError,
                                            );
                                            generation_seen = generation;
                                            cpu_waiting_for_idr = true;
                                        }
                                    }
                                    Err(error) => {
                                        let _ = worker_events.send(ScreenShareEvent::Error(format!(
                                            "DXVA não publicou imagem e OpenH264 não pôde iniciar: {error}"
                                        )));
                                        return;
                                    }
                                }
                                continue;
                            }
                        };
                        let current_generation = worker_generation.load(Ordering::Relaxed);
                        if queued.generation != current_generation {
                            worker_metrics.record_drop_waiting_for_idr();
                            continue;
                        }
                        if queued.generation != generation_seen {
                            generation_seen = queued.generation;
                            decoder = None;
                            using_cpu_pending_sps = false;
                            cpu_waiting_for_idr = true;
                            hardware_frames_without_output = 0;
                            last_hardware_input_at = None;
                            dxva_throughput = DxvaThroughputMonitor::default();
                            cached_gop.invalidate_delta_chain();
                        }
                        let access_unit = queued.bytes;
                        if let Some(idr) = update_cached_idr(
                            &access_unit,
                            &mut cached_sps,
                            &mut cached_pps,
                        ) {
                            cached_idr = Some(idr.clone());
                            cached_gop.start_at_idr(idr);
                        } else {
                            cached_gop.push_delta(&access_unit);
                        }
                        let has_sps_and_idr = {
                            let nals = annex_b_nal_types(&access_unit);
                            nals.contains(&7) && nals.contains(&8) && nals.contains(&5)
                        };

                        #[cfg(windows)]
                        let should_try_hardware = should_try_hardware_decoder(
                            worker_decoder_preference,
                            hardware_decoder_disabled,
                            decoder.is_none(),
                            using_cpu_pending_sps,
                            has_sps_and_idr,
                        );
                        #[cfg(not(windows))]
                        let should_try_hardware = false;

                        #[cfg(windows)]
                        if decoder.is_none()
                            && (worker_decoder_preference == VideoDecoderPreference::Cpu
                                || hardware_decoder_disabled)
                        {
                            match Decoder::new() {
                                Ok(cpu) => {
                                    let fallback_reason = if worker_decoder_preference
                                        == VideoDecoderPreference::Cpu
                                    {
                                        None
                                    } else {
                                        Some(
                                            worker_metrics
                                                .decoder_fallback_reason
                                                .lock()
                                                .unwrap_or_else(
                                                    std::sync::PoisonError::into_inner,
                                                )
                                                .clone()
                                                .unwrap_or_else(|| {
                                                    "DXVA foi desativado após uma falha anterior.".to_owned()
                                                }),
                                        )
                                    };
                                    worker_metrics.set_decoder_backend(
                                        if worker_decoder_preference
                                            == VideoDecoderPreference::Cpu
                                        {
                                            "CPU — OpenH264 (selecionado)".to_owned()
                                        } else {
                                            "CPU — OpenH264 (fallback)".to_owned()
                                        },
                                        fallback_reason,
                                    );
                                    tracing::info!(
                                        screen_share_session = worker_metrics.session_id,
                                        decoder_preference = worker_decoder_preference.label(),
                                        "Decoder OpenH264 selecionado pelo usuário"
                                    );
                                    decoder = Some(ActiveH264Decoder::OpenH264(cpu));
                                    using_cpu_pending_sps = false;
                                    cpu_waiting_for_idr = true;
                                }
                                Err(error) => {
                                    let _ = worker_events.send(ScreenShareEvent::Error(format!(
                                        "Não foi possível iniciar o decoder OpenH264 selecionado: {error}"
                                    )));
                                    return;
                                }
                            }
                        }

                        if should_try_hardware {
                            #[cfg(windows)]
                            {
                                if let Some((width, height)) = mf_video::sps_dimensions(&access_unit) {
                                    hardware_decoder_disabled = true;
                                    match mf_video::HardwareDecoder::new(width, height) {
                                        Ok(hardware) => {
                                            hardware_decoder_disabled = false;
                                            let name = hardware.name().to_owned();
                                            worker_metrics.set_decoder_backend(
                                                format!("GPU — DXVA / {name}"),
                                                None,
                                            );
                                            tracing::info!(
                                                codec = %name,
                                                width,
                                                height,
                                                "Decodificador H.264 DXVA ativado"
                                            );
                                            decoder = Some(ActiveH264Decoder::MediaFoundation(hardware));
                                            using_cpu_pending_sps = false;
                                        }
                                        Err(error) => {
                                            let reason = format!("DXVA indisponível: {error}");
                                            tracing::warn!(fallback_reason = %reason, "Usando decodificador H.264 OpenH264 na CPU");
                                            match Decoder::new() {
                                                Ok(cpu) => {
                                                    worker_metrics.set_decoder_backend(
                                                        "CPU — OpenH264".to_owned(),
                                                        Some(reason),
                                                    );
                                                    decoder = Some(ActiveH264Decoder::OpenH264(cpu));
                                                    using_cpu_pending_sps = false;
                                                    cpu_waiting_for_idr = true;
                                                }
                                                Err(cpu_error) => {
                                                    tracing::error!(error = %cpu_error, "Falha ao iniciar decodificador H.264 de CPU");
                                                    let _ = worker_events.send(ScreenShareEvent::Error(format!(
                                                        "Não foi possível iniciar decodificador H.264 de hardware nem OpenH264: {error}; {cpu_error}"
                                                    )));
                                                    return;
                                                }
                                            }
                                        }
                                    }
                                } else if decoder.is_none() {
                                    let reason = "Aguardando SPS/PPS para iniciar DXVA; usando CPU neste quadro".to_owned();
                                    match Decoder::new() {
                                        Ok(cpu) => {
                                            worker_metrics.set_decoder_backend(
                                                "CPU — OpenH264 (aguardando SPS/PPS)".to_owned(),
                                                Some(reason),
                                            );
                                            decoder = Some(ActiveH264Decoder::OpenH264(cpu));
                                            using_cpu_pending_sps = true;
                                            cpu_waiting_for_idr = true;
                                        }
                                        Err(error) => {
                                            let _ = worker_events.send(ScreenShareEvent::Error(format!(
                                                "Não foi possível iniciar o decodificador H.264: {error}"
                                            )));
                                            return;
                                        }
                                    }
                                }
                            }
                        }

                        #[cfg(not(windows))]
                        if decoder.is_none() {
                            match Decoder::new() {
                                Ok(cpu) => {
                                    worker_metrics.set_decoder_backend(
                                        "CPU — OpenH264".to_owned(),
                                        Some("Aceleração Media Foundation está disponível apenas no Windows.".to_owned()),
                                    );
                                    decoder = Some(ActiveH264Decoder::OpenH264(cpu));
                                    cpu_waiting_for_idr = true;
                                }
                                Err(error) => {
                                    let _ = worker_events.send(ScreenShareEvent::Error(format!(
                                        "Não foi possível iniciar o decodificador H.264: {error}"
                                    )));
                                    return;
                                }
                            }
                        }

                        if !should_decode_access_unit(&mut cpu_waiting_for_idr, &access_unit) {
                            worker_metrics.record_drop_waiting_for_idr();
                            continue;
                        }

                        if let Some(active_decoder) = decoder.as_mut() {
                            let decoding_on_hardware = matches!(
                                active_decoder,
                                ActiveH264Decoder::MediaFoundation(_)
                            );
                            worker_metrics.record_decoder_input();
                            let decoded_before =
                                worker_metrics.decoded_frames.load(Ordering::Relaxed);
                            let hardware_error = decode_h264_access_unit(
                                &access_unit,
                                active_decoder,
                                &worker_context,
                                &worker_remote_frame,
                                &worker_sequence,
                                &worker_metrics,
                            );
                            let decode_failed = hardware_error.is_some();
                            let mut decoder_entered_resync = false;
                            if hardware_error
                                .as_ref()
                                .is_some_and(|failure| !failure.hardware_decoder)
                            {
                                // Keep the CPU decoder alive; an IDR with parameter sets can
                                // restore its references after the incomplete/damaged frame.
                                decoder_entered_resync = true;
                            }
                            #[cfg(windows)]
                            if hardware_error.as_ref().is_some_and(|failure| {
                                failure.hardware_decoder
                                    && is_hardware_device_failure(&failure.detail)
                            }) {
                                hardware_decoder_disabled = true;
                                let reason = hardware_error
                                    .as_ref()
                                    .map(|failure| failure.detail.clone())
                                    .unwrap_or_default();
                                tracing::warn!(fallback_reason = %reason, "Falha do dispositivo DXVA; mudando para OpenH264 na CPU");
                                match Decoder::new() {
                                    Ok(cpu) => {
                                        worker_metrics.set_decoder_backend(
                                            "CPU — OpenH264".to_owned(),
                                            Some(reason),
                                        );
                                        decoder = Some(ActiveH264Decoder::OpenH264(cpu));
                                        using_cpu_pending_sps = false;
                                        cpu_waiting_for_idr = true;
                                        decoder_entered_resync = true;
                                    }
                                    Err(error) => {
                                        let _ = worker_events.send(ScreenShareEvent::Error(format!(
                                            "DXVA falhou e OpenH264 não pôde iniciar: {error}"
                                        )));
                                        return;
                                    }
                                }
                            }

                            if matches!(decoder.as_ref(), Some(ActiveH264Decoder::MediaFoundation(_)))
                                && worker_metrics.decoded_frames.load(Ordering::Relaxed) == decoded_before
                            {
                                last_hardware_input_at = Some(Instant::now());
                                hardware_frames_without_output = hardware_frames_without_output.saturating_add(1);
                                if hardware_frames_without_output >= 15 {
                                    hardware_decoder_disabled = true;
                                    let reason = "DXVA recebeu 15 quadros H.264 sem produzir imagem; usando OpenH264 na CPU.".to_owned();
                                    tracing::warn!(fallback_reason = %reason, "Decodificador DXVA sem saída; mudando para OpenH264 na CPU");
                                    match Decoder::new() {
                                        Ok(cpu) => {
                                            worker_metrics.set_decoder_backend(
                                                "CPU — OpenH264".to_owned(),
                                                Some(reason),
                                            );
                                            decoder = Some(ActiveH264Decoder::OpenH264(cpu));
                                            using_cpu_pending_sps = false;
                                            hardware_frames_without_output = 0;
                                            last_hardware_input_at = None;
                                            cpu_waiting_for_idr = true;
                                            decoder_entered_resync = true;
                                        }
                                        Err(error) => {
                                            let _ = worker_events.send(ScreenShareEvent::Error(format!(
                                                "DXVA não produziu imagem e OpenH264 não pôde iniciar: {error}"
                                            )));
                                            return;
                                        }
                                    }
                                }
                            } else {
                                if decoding_on_hardware {
                                    hardware_frames_without_output = 0;
                                    last_hardware_input_at = None;
                                }
                            }

                            if decode_failed || decoder_entered_resync {
                                let generation = begin_stream_resync(
                                    &worker_generation,
                                    &worker_metrics,
                                    &worker_keyframe_request_tx,
                                    PliReason::DecodeError,
                                );
                                generation_seen = generation;
                                cpu_waiting_for_idr = true;
                                if !decoder_entered_resync {
                                    decoder = None;
                                }
                            }

                            if should_use_adaptive_decoder_fallback(worker_decoder_preference)
                                && hardware_error.is_none()
                                && matches!(
                                    decoder.as_ref(),
                                    Some(ActiveH264Decoder::MediaFoundation(_))
                                )
                            {
                                let produced_output = worker_metrics
                                    .decoded_frames
                                    .load(Ordering::Relaxed)
                                    > decoded_before;
                                if let Some(window) =
                                    dxva_throughput.observe(Instant::now(), produced_output)
                                {
                                    let reason = format!(
                                        "DXVA publicou {}/{} quadros na janela de 3 s ({:.0}%); abaixo de 80%. Mudando para OpenH264 na CPU.",
                                        window.outputs,
                                        window.inputs,
                                        window.output_ratio * 100.0
                                    );
                                    tracing::warn!(
                                        screen_share_session = worker_metrics.session_id,
                                        decoder_inputs = window.inputs,
                                        decoder_outputs = window.outputs,
                                        output_ratio = window.output_ratio,
                                        fallback_reason = %reason,
                                        cached_gop_complete = cached_gop.complete,
                                        "Fallback adaptativo de DXVA para OpenH264"
                                    );
                                    hardware_decoder_disabled = true;
                                    match Decoder::new() {
                                        Ok(cpu) => {
                                            worker_metrics.set_decoder_backend(
                                                "CPU — OpenH264 (fallback adaptativo)".to_owned(),
                                                Some(reason),
                                            );
                                            decoder = Some(ActiveH264Decoder::OpenH264(cpu));
                                            using_cpu_pending_sps = false;
                                            hardware_frames_without_output = 0;
                                            last_hardware_input_at = None;
                                            cpu_waiting_for_idr = true;

                                            let complete_chain = cached_gop.replay_chain();
                                            let can_continue_from_cache = complete_chain.is_some();
                                            let mut replay = complete_chain.unwrap_or_default();
                                            if replay.is_empty() {
                                                if let Some(idr) = cached_idr.as_deref() {
                                                    replay.push(idr);
                                                }
                                            }

                                            let decoded_before_replay = worker_metrics
                                                .decoded_frames
                                                .load(Ordering::Relaxed);
                                            let mut replay_failed = false;
                                            let mut replayed_access_units = 0_u64;
                                            for cached_access_unit in replay {
                                                if !should_decode_access_unit(
                                                    &mut cpu_waiting_for_idr,
                                                    cached_access_unit,
                                                ) {
                                                    replay_failed = true;
                                                    break;
                                                }
                                                worker_metrics.record_decoder_input();
                                                replayed_access_units += 1;
                                                let Some(active_decoder) = decoder.as_mut() else {
                                                    replay_failed = true;
                                                    break;
                                                };
                                                if let Some(failure) = decode_h264_access_unit(
                                                    cached_access_unit,
                                                    active_decoder,
                                                    &worker_context,
                                                    &worker_remote_frame,
                                                    &worker_sequence,
                                                    &worker_metrics,
                                                ) {
                                                    tracing::warn!(
                                                        error = %failure.detail,
                                                        "OpenH264 falhou ao reconstruir a referência H.264 em cache"
                                                    );
                                                    replay_failed = true;
                                                    break;
                                                }
                                            }

                                            let replay_published = worker_metrics
                                                .decoded_frames
                                                .load(Ordering::Relaxed)
                                                > decoded_before_replay;
                                            if !can_continue_from_cache
                                                || replay_failed
                                                || !replay_published
                                            {
                                                let generation = begin_stream_resync(
                                                    &worker_generation,
                                                    &worker_metrics,
                                                    &worker_keyframe_request_tx,
                                                    PliReason::DecodeError,
                                                );
                                                generation_seen = generation;
                                                cpu_waiting_for_idr = true;
                                                tracing::info!(
                                                    screen_share_session = worker_metrics.session_id,
                                                    cached_idr_replayed = replay_published,
                                                    cached_gop_complete = can_continue_from_cache,
                                                    "Solicitado IDR novo para completar fallback do decoder"
                                                );
                                            } else {
                                                tracing::info!(
                                                    screen_share_session = worker_metrics.session_id,
                                                    replayed_access_units,
                                                    "OpenH264 retomou a cadeia H.264 a partir do GOP em cache"
                                                );
                                            }
                                        }
                                        Err(error) => {
                                            let _ = worker_events.send(ScreenShareEvent::Error(format!(
                                                "DXVA ficou abaixo da taxa recebida e OpenH264 não pôde iniciar: {error}"
                                            )));
                                            return;
                                        }
                                    }
                                }
                            }
                        }
                    }
                });
            if let Err(error) = worker {
                tracing::error!(error = %error, "Falha ao iniciar worker do decodificador H.264");
                let _ = events.send(ScreenShareEvent::Error(format!(
                    "Não foi possível iniciar o worker do decodificador H.264: {error}"
                )));
                return;
            }
            let mut assembler = H264AccessUnitAssembler::default();
            let mut sequence_tracker = RtpSequenceTracker::default();
            let mut reorder_buffer = RtpReorderBuffer::default();
            let mut keyframe_request_limiter = KeyframeRequestLimiter::default();
            let mut awaiting_sequence_recovery = false;
            let mut reorder_overflow_count = 0_u64;
            let mut flush_pending = tokio::time::interval(Duration::from_millis(10));
            flush_pending.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

            loop {
                let mut reorder_update = RtpReorderUpdate::default();
                let mut confirmed_sequence_loss = false;
                tokio::select! {
                    _ = flush_pending.tick() => {
                        let now = Instant::now();
                        let expired = sequence_tracker.expire(now);
                        if expired.confirmed_missing_packets > 0 {
                            metrics.record_sequence_update(expired);
                            confirmed_sequence_loss = true;
                        }
                        reorder_update = reorder_buffer.expire(now);
                        confirmed_sequence_loss |= reorder_update.confirmed_missing_packets > 0;
                    }
                    request = keyframe_request_rx.recv() => {
                        if let Some(reason) = request {
                            send_picture_loss_indication(
                                track.as_ref(),
                                &metrics,
                                &mut keyframe_request_limiter,
                                reason,
                            ).await;
                        }
                    }
                    event = track.poll() => {
                        let Some(event) = event else { break };
                        match event {
                            TrackRemoteEvent::OnRtpPacket(packet) => {
                                metrics.received_packets.fetch_add(1, Ordering::Relaxed);
                                metrics
                                    .interval_received_packets
                                    .fetch_add(1, Ordering::Relaxed);
                                let now = Instant::now();
                                let sequence_update = sequence_tracker.observe(
                                    packet.header.sequence_number,
                                    now,
                                );
                                metrics.record_sequence_update(sequence_update);
                                metrics.record_received_packet(&packet);
                                if sequence_update.confirmed_missing_packets > 0 {
                                    reorder_update.append(reorder_buffer.expire(now));
                                    confirmed_sequence_loss = true;
                                }
                                if sequence_update.duplicate_packets == 0
                                    && sequence_update.late_after_confirmed_packets == 0
                                {
                                    reorder_update.append(reorder_buffer.push(packet, now));
                                }
                            }
                            TrackRemoteEvent::OnEnded | TrackRemoteEvent::OnEnding => break,
                            _ => {}
                        }
                    }
                }

                if confirmed_sequence_loss || reorder_update.confirmed_missing_packets > 0 {
                    metrics.record_sequence_gap_resync();
                    assembler.discard_incomplete_after_loss(reorder_update.resume_timestamp);
                    if !awaiting_sequence_recovery {
                        begin_stream_resync(
                            &decoder_generation,
                            &metrics,
                            &keyframe_request_tx,
                            PliReason::SequenceGap,
                        );
                        awaiting_sequence_recovery = true;
                    }
                    context.request_repaint();
                }
                if reorder_update.overflowed {
                    reorder_overflow_count = reorder_overflow_count.saturating_add(1);
                    if should_log_aggregate_error(reorder_overflow_count) {
                        tracing::warn!(
                            screen_share_session = metrics.session_id,
                            track_ssrc = metrics.track_ssrc.load(Ordering::Relaxed),
                            overflow_count = reorder_overflow_count,
                            capacity_packets = MAX_BUFFERED_RTP_PACKETS,
                            "Buffer de reordenaÃ§Ã£o RTP cheio; pacote futuro descartado"
                        );
                    }
                }
                for packet in reorder_update.ordered_packets {
                    if let Some(error) = assembler.push(packet) {
                        metrics.record_assembly_error(error);
                        if !awaiting_sequence_recovery {
                            begin_stream_resync(
                                &decoder_generation,
                                &metrics,
                                &keyframe_request_tx,
                                PliReason::AssemblyError,
                            );
                            awaiting_sequence_recovery = true;
                        }
                        context.request_repaint();
                    }
                }

                for result in assembler.take_ready(Instant::now()) {
                    match result {
                        Ok(access_unit) => {
                            metrics.record_assembled_access_unit(&access_unit);
                            let nal_types = annex_b_nal_types(&access_unit);
                            let is_valid_recovery_idr = nal_types.contains(&5)
                                && nal_types.contains(&7)
                                && nal_types.contains(&8);
                            // After a confirmed gap, P frames still depend on the damaged
                            // reference chain. Keep them out of the decoder until a complete
                            // IDR with parameter sets is accepted by its bounded queue.
                            if awaiting_sequence_recovery && !is_valid_recovery_idr {
                                metrics.record_drop_waiting_for_idr();
                                continue;
                            }
                            match decoder_tx.try_send(QueuedAccessUnit {
                                generation: decoder_generation.load(Ordering::Relaxed),
                                bytes: access_unit,
                            }) {
                                Ok(()) => {
                                    if is_valid_recovery_idr {
                                        awaiting_sequence_recovery = false;
                                    }
                                }
                                Err(std_mpsc::TrySendError::Full(_)) => {
                                    metrics.record_decoder_queue_drop();
                                    if !awaiting_sequence_recovery {
                                        begin_stream_resync(
                                            &decoder_generation,
                                            &metrics,
                                            &keyframe_request_tx,
                                            PliReason::DecoderQueueFull,
                                        );
                                        awaiting_sequence_recovery = true;
                                    }
                                    context.request_repaint();
                                }
                                Err(std_mpsc::TrySendError::Disconnected(_)) => {
                                    let _ = events.send(ScreenShareEvent::Error(
                                        "O worker do decodificador H.264 foi encerrado.".to_owned(),
                                    ));
                                    return;
                                }
                            }
                        }
                        Err(error) => {
                            metrics.record_assembly_error(error);
                            if !awaiting_sequence_recovery {
                                begin_stream_resync(
                                    &decoder_generation,
                                    &metrics,
                                    &keyframe_request_tx,
                                    PliReason::AssemblyError,
                                );
                                awaiting_sequence_recovery = true;
                            }
                            context.request_repaint();
                        }
                    }
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn annex_b_access_unit(nals: &[&[u8]]) -> Vec<u8> {
        let mut access_unit = Vec::new();
        for nal in nals {
            access_unit.extend_from_slice(&[0, 0, 0, 1]);
            access_unit.extend_from_slice(nal);
        }
        access_unit
    }

    fn rtp_packet(sequence: u16, timestamp: u32, marker: bool, payload: &[u8]) -> RtpPacket {
        RtpPacket {
            header: rtc::rtp::header::Header {
                sequence_number: sequence,
                timestamp,
                marker,
                ..Default::default()
            },
            payload: Bytes::copy_from_slice(payload),
        }
    }

    #[test]
    fn adaptive_dxva_fallback_waits_for_warmup_and_full_window() {
        let start = Instant::now();
        let mut monitor = DxvaThroughputMonitor::default();
        assert!(monitor.observe(start, false).is_none());
        assert!(
            monitor
                .observe(
                    start + DXVA_ADAPTIVE_WARMUP - Duration::from_nanos(1),
                    false
                )
                .is_none()
        );
        assert!(
            monitor
                .observe(start + DXVA_ADAPTIVE_WARMUP, false)
                .is_none()
        );
        assert!(
            monitor
                .observe(
                    start + DXVA_ADAPTIVE_WARMUP + DXVA_ADAPTIVE_WINDOW - Duration::from_nanos(1),
                    false,
                )
                .is_none()
        );
    }

    #[test]
    fn decoder_recovery_gate_drops_delta_frames_until_sps_pps_idr() {
        let sps = [0x67, 0x64, 0x00, 0x1f];
        let pps = [0x68, 0x00];
        let idr = [0x65, 0x88];
        let delta = [0x41, 0x99];
        let mut waiting_for_idr = true;

        assert!(!should_decode_access_unit(
            &mut waiting_for_idr,
            &annex_b_access_unit(&[&delta])
        ));
        assert!(!should_decode_access_unit(
            &mut waiting_for_idr,
            &annex_b_access_unit(&[&sps, &pps])
        ));
        assert!(should_decode_access_unit(
            &mut waiting_for_idr,
            &annex_b_access_unit(&[&sps, &pps, &idr])
        ));
        assert!(!waiting_for_idr);
        assert!(should_decode_access_unit(
            &mut waiting_for_idr,
            &annex_b_access_unit(&[&delta])
        ));
    }

    #[test]
    fn keyframe_requests_are_rate_limited() {
        let mut limiter = KeyframeRequestLimiter::default();
        let now = Instant::now();
        assert!(limiter.allow(now));
        assert!(!limiter.allow(now + Duration::from_millis(500)));
        assert!(limiter.allow(now + Duration::from_millis(750)));
    }

    #[test]
    fn decoder_preferences_choose_hardware_cpu_and_adaptive_policy() {
        assert!(should_try_hardware_decoder(
            VideoDecoderPreference::Automatic,
            false,
            true,
            false,
            false
        ));
        assert!(should_try_hardware_decoder(
            VideoDecoderPreference::PreferDxva,
            false,
            true,
            false,
            false
        ));
        assert!(!should_try_hardware_decoder(
            VideoDecoderPreference::Cpu,
            false,
            true,
            false,
            false
        ));
        assert!(!should_try_hardware_decoder(
            VideoDecoderPreference::Automatic,
            true,
            true,
            false,
            false
        ));
        assert!(should_use_adaptive_decoder_fallback(
            VideoDecoderPreference::Automatic
        ));
        assert!(!should_use_adaptive_decoder_fallback(
            VideoDecoderPreference::PreferDxva
        ));
        assert!(!should_use_adaptive_decoder_fallback(
            VideoDecoderPreference::Cpu
        ));
    }

    #[test]
    fn cached_gop_preserves_the_idr_and_delta_frame_chain_for_cpu_fallback() {
        let sps = [0x67, 0x64, 0x00, 0x1f];
        let pps = [0x68, 0x00];
        let idr = [0x65, 0x88, 0x84];
        let delta_one = [0x41, 0x9a];
        let delta_two = [0x41, 0x9b];
        let mut gop = CachedH264Gop::default();
        gop.start_at_idr(annex_b_access_unit(&[&sps, &pps, &idr]));
        gop.push_delta(&annex_b_access_unit(&[&delta_one]));
        gop.push_delta(&annex_b_access_unit(&[&delta_two]));

        let chain = gop
            .replay_chain()
            .expect("GOP completo deve ser reutilizável");
        assert_eq!(annex_b_nal_types(chain[0]), [7, 8, 5]);
        assert_eq!(annex_b_nal_types(chain[1]), [1]);
        assert_eq!(annex_b_nal_types(chain[2]), [1]);

        gop.invalidate_delta_chain();
        assert!(gop.replay_chain().is_none());
    }

    #[test]
    fn cached_idr_keeps_parameter_sets_and_watchdog_expires_after_750ms() {
        let sps = [0x67, 0x64, 0x00, 0x1f];
        let pps = [0x68, 0x00];
        let idr = [0x65, 0x88, 0x84];
        let mut cached_sps = None;
        let mut cached_pps = None;
        assert!(
            update_cached_idr(
                &annex_b_access_unit(&[&sps, &pps]),
                &mut cached_sps,
                &mut cached_pps,
            )
            .is_none()
        );
        let cached = update_cached_idr(
            &annex_b_access_unit(&[&idr]),
            &mut cached_sps,
            &mut cached_pps,
        )
        .unwrap();
        assert_eq!(annex_b_nal_types(&cached), [7, 8, 5]);

        let received_at = Instant::now();
        assert!(!dxva_watchdog_expired(
            Some(received_at),
            received_at + DXVA_FIRST_OUTPUT_WATCHDOG - Duration::from_nanos(1)
        ));
        assert!(dxva_watchdog_expired(
            Some(received_at),
            received_at + DXVA_FIRST_OUTPUT_WATCHDOG
        ));
    }

    #[test]
    fn adaptive_dxva_fallback_requires_45_samples_and_less_than_80_percent_output() {
        let start = Instant::now();
        let mut too_few = DxvaThroughputMonitor::default();
        assert!(too_few.observe(start, true).is_none());
        for index in 1..=20 {
            let elapsed =
                DXVA_ADAPTIVE_WARMUP + Duration::from_millis(150 * index) + Duration::from_nanos(1);
            assert!(too_few.observe(start + elapsed, false).is_none());
        }

        let mut slow = DxvaThroughputMonitor::default();
        let mut decision = None;
        for index in 0..=180 {
            let at = start + Duration::from_nanos(33_333_333 * index);
            decision = slow.observe(at, index % 2 == 0).or(decision);
        }
        let decision = decision.expect("DXVA abaixo do limite deve acionar fallback");
        assert!(decision.inputs >= DXVA_ADAPTIVE_MIN_INPUTS);
        assert!(decision.output_ratio < DXVA_ADAPTIVE_MIN_OUTPUT_RATIO);
        assert!(decision.outputs < decision.inputs);

        let mut healthy = DxvaThroughputMonitor::default();
        for index in 0..=180 {
            let at = start + Duration::from_nanos(33_333_333 * index);
            assert!(healthy.observe(at, true).is_none());
        }
    }

    #[test]
    fn h264_reassembly_restores_packet_order_and_rejects_incomplete_frames() {
        let reordered = assemble_h264_access_unit(vec![
            rtp_packet(11, 100, true, &[0x41, 0x99, 0x05, 0x01]),
            rtp_packet(10, 100, false, &[0x65, 0x88, 0x84, 0x21]),
        ])
        .unwrap();
        assert_eq!(annex_b_nal_types(&reordered), vec![5, 1]);

        assert!(
            assemble_h264_access_unit(vec![
                rtp_packet(10, 101, false, &[0x65, 0x88, 0x84, 0x21]),
                rtp_packet(12, 101, true, &[0x41, 0x99, 0x05, 0x01]),
            ])
            .is_err()
        );
        assert!(
            assemble_h264_access_unit(vec![rtp_packet(10, 102, false, &[0x65, 0x88, 0x84, 0x21],)])
                .is_err()
        );
        assert!(
            assemble_h264_access_unit(vec![rtp_packet(
                10,
                103,
                true,
                &[28, 0x85, 0x88, 0x99, 0x22],
            )])
            .is_err()
        );
        assert!(
            assemble_h264_access_unit(vec![rtp_packet(
                10,
                104,
                true,
                &[28, 0x45, 0x88, 0x99, 0x22],
            )])
            .is_err()
        );
    }

    #[test]
    fn rtp_sequence_tracker_handles_u16_wrap_without_false_gap() {
        let start = Instant::now();
        let mut tracker = RtpSequenceTracker::default();
        tracker.observe(u16::MAX - 1, start);
        assert_eq!(tracker.observe(u16::MAX, start), Default::default());
        assert_eq!(tracker.observe(0, start), Default::default());
        assert_eq!(tracker.observe(1, start), Default::default());
    }

    #[test]
    fn rtp_sequence_tracker_separates_reordering_duplicates_and_confirmed_loss() {
        let start = Instant::now();
        let mut tracker = RtpSequenceTracker::default();

        assert_eq!(tracker.observe(10, start), Default::default());
        assert_eq!(tracker.observe(11, start), Default::default());
        assert_eq!(
            tracker.observe(11, start),
            super::RtpSequenceUpdate {
                duplicate_packets: 1,
                ..Default::default()
            }
        );

        assert_eq!(
            tracker.observe(13, start),
            super::RtpSequenceUpdate {
                observed_gap_packets: 1,
                ..Default::default()
            }
        );
        assert_eq!(
            tracker.observe(12, start + Duration::from_millis(20)),
            super::RtpSequenceUpdate {
                recovered_reordered_packets: 1,
                ..Default::default()
            }
        );
        assert_eq!(
            tracker.expire(start + Duration::from_millis(41)),
            Default::default(),
            "o pacote reordenado preencheu a lacuna dentro dos 40 ms"
        );

        let mut lost_tracker = RtpSequenceTracker::default();
        lost_tracker.observe(20, start);
        lost_tracker.observe(22, start);
        assert_eq!(
            lost_tracker.expire(start + Duration::from_millis(40)),
            super::RtpSequenceUpdate {
                confirmed_missing_packets: 1,
                ..Default::default()
            }
        );
        assert_eq!(
            lost_tracker.observe(21, start + Duration::from_millis(41)),
            super::RtpSequenceUpdate {
                late_after_confirmed_packets: 1,
                ..Default::default()
            }
        );
    }

    #[test]
    fn rtp_reorder_buffer_restores_packets_arriving_within_the_40ms_window() {
        let start = Instant::now();
        let mut buffer = RtpReorderBuffer::default();

        let first = buffer.push(rtp_packet(10, 100, false, &[0x41, 1]), start);
        assert_eq!(
            first
                .ordered_packets
                .iter()
                .map(|packet| packet.header.sequence_number)
                .collect::<Vec<_>>(),
            [10]
        );
        assert!(
            buffer
                .push(
                    rtp_packet(12, 100, true, &[0x41, 3]),
                    start + Duration::from_millis(1)
                )
                .ordered_packets
                .is_empty()
        );
        assert_eq!(
            buffer
                .expire(start + RTP_REORDER_DELAY - Duration::from_nanos(1))
                .confirmed_missing_packets,
            0
        );

        let reordered = buffer.push(
            rtp_packet(11, 100, false, &[0x41, 2]),
            start + Duration::from_millis(20),
        );
        assert_eq!(
            reordered
                .ordered_packets
                .iter()
                .map(|packet| packet.header.sequence_number)
                .collect::<Vec<_>>(),
            [11, 12]
        );
        assert_eq!(
            buffer
                .expire(start + Duration::from_millis(60))
                .confirmed_missing_packets,
            0
        );
    }

    #[test]
    fn rtp_reorder_buffer_confirms_loss_discards_late_packet_and_handles_wrap() {
        let start = Instant::now();
        let mut buffer = RtpReorderBuffer::default();
        buffer.push(rtp_packet(10, 100, false, &[0x41, 1]), start);
        buffer.push(
            rtp_packet(12, 200, true, &[0x41, 2]),
            start + Duration::from_millis(1),
        );

        let lost = buffer.expire(start + RTP_REORDER_DELAY + Duration::from_millis(1));
        assert_eq!(lost.confirmed_missing_packets, 1);
        assert_eq!(lost.resume_timestamp, Some(200));
        assert_eq!(
            lost.ordered_packets
                .iter()
                .map(|packet| packet.header.sequence_number)
                .collect::<Vec<_>>(),
            [12]
        );
        assert!(
            buffer
                .push(
                    rtp_packet(11, 100, true, &[0x41, 3]),
                    start + Duration::from_millis(50)
                )
                .ordered_packets
                .is_empty()
        );

        let mut wrapping = RtpReorderBuffer::default();
        wrapping.push(rtp_packet(u16::MAX - 1, 300, false, &[0x41, 1]), start);
        let wrapped_future = wrapping.push(rtp_packet(0, 300, true, &[0x41, 3]), start);
        assert!(wrapped_future.ordered_packets.is_empty());
        let wrapped = wrapping.push(rtp_packet(u16::MAX, 300, false, &[0x41, 2]), start);
        assert_eq!(
            wrapped
                .ordered_packets
                .iter()
                .map(|packet| packet.header.sequence_number)
                .collect::<Vec<_>>(),
            [u16::MAX, 0]
        );
    }

    #[test]
    fn rtp_reorder_buffer_is_bounded_and_reorders_fu_a_before_assembly() {
        let start = Instant::now();
        let mut buffer = RtpReorderBuffer::default();
        buffer.push(rtp_packet(9, 99, true, &[0x41, 1]), start);
        let mut assembler = H264AccessUnitAssembler::default();

        let end = buffer.push(
            rtp_packet(11, 100, true, &[28, 0x45, 0x99, 0x22]),
            start + Duration::from_millis(1),
        );
        assert!(end.ordered_packets.is_empty());
        let start_fragment = buffer.push(
            rtp_packet(10, 100, false, &[28, 0x85, 0x88, 0x84]),
            start + Duration::from_millis(2),
        );
        assert_eq!(
            start_fragment
                .ordered_packets
                .iter()
                .map(|packet| packet.header.sequence_number)
                .collect::<Vec<_>>(),
            [10, 11]
        );
        for packet in start_fragment.ordered_packets {
            assert!(assembler.push(packet).is_none());
        }
        let ready = assembler.take_ready(start + Duration::from_millis(50));
        let frame = ready
            .into_iter()
            .find_map(Result::ok)
            .expect("FU-A reordenado deve formar um access unit válido");
        assert_eq!(annex_b_nal_types(&frame), [5]);

        let mut bounded = RtpReorderBuffer::default();
        bounded.push(rtp_packet(100, 400, false, &[0x41, 1]), start);
        for sequence in 102..(102 + MAX_BUFFERED_RTP_PACKETS as u16) {
            let update = bounded.push(rtp_packet(sequence, 400, false, &[0x41, 2]), start);
            assert!(!update.overflowed);
        }
        assert_eq!(bounded.pending.len(), MAX_BUFFERED_RTP_PACKETS);
        assert!(
            bounded
                .push(
                    rtp_packet(
                        102 + MAX_BUFFERED_RTP_PACKETS as u16,
                        400,
                        false,
                        &[0x41, 3]
                    ),
                    start
                )
                .overflowed
        );
    }

    #[test]
    fn confirmed_loss_discards_damaged_fu_a_timestamp_and_accepts_next_idr() {
        let now = Instant::now();
        let mut buffer = RtpReorderBuffer::default();
        let mut assembler = H264AccessUnitAssembler::default();
        buffer.push(rtp_packet(9, 99, true, &[0x41, 1]), now);
        let first_fragment = buffer.push(rtp_packet(10, 100, false, &[28, 0x85, 0x88, 0x84]), now);
        for packet in first_fragment.ordered_packets {
            assembler.push(packet);
        }
        buffer.push(
            rtp_packet(12, 100, true, &[28, 0x45, 0x99, 0x22]),
            now + Duration::from_millis(1),
        );

        let loss = buffer.expire(now + RTP_REORDER_DELAY + Duration::from_millis(1));
        assert_eq!(loss.confirmed_missing_packets, 1);
        assembler.discard_incomplete_after_loss(loss.resume_timestamp);
        for packet in loss.ordered_packets {
            assembler.push(packet);
        }
        assert!(
            assembler
                .take_ready(now + Duration::from_millis(100))
                .is_empty()
        );

        let recovered = buffer.push(
            rtp_packet(13, 101, true, &[0x65, 0x88, 0x84, 0x21]),
            now + Duration::from_millis(50),
        );
        for packet in recovered.ordered_packets {
            assembler.push(packet);
        }
        assert!(
            assembler
                .take_ready(now + Duration::from_millis(100))
                .iter()
                .any(|unit| unit
                    .as_ref()
                    .is_ok_and(|bytes| annex_b_nal_types(bytes).contains(&5)))
        );
    }

    #[test]
    fn cpu_fallback_can_publish_the_cached_initial_idr_without_new_network_frames() {
        let mut encoder = super::openh264_encoder(4_000_000).unwrap();
        let source = PreviewFrame {
            sequence: 1,
            width: 320,
            height: 240,
            rgba: (0..320 * 240 * 4)
                .map(|index| ((index * 17) % 251) as u8)
                .collect(),
            #[cfg(windows)]
            gpu_nv12: None,
            #[cfg(windows)]
            cpu_nv12: None,
        };
        let encoded = encode_frame(&mut encoder, &source).unwrap();
        let mut cached_sps = None;
        let mut cached_pps = None;
        let cached_idr = update_cached_idr(&encoded, &mut cached_sps, &mut cached_pps)
            .expect("primeiro quadro OpenH264 deve conter SPS/PPS/IDR");
        let received_at = Instant::now();
        assert!(dxva_watchdog_expired(
            Some(received_at),
            received_at + DXVA_FIRST_OUTPUT_WATCHDOG
        ));

        let mut waiting_for_idr = true;
        assert!(should_decode_access_unit(&mut waiting_for_idr, &cached_idr));
        let mut decoder = ActiveH264Decoder::OpenH264(Decoder::new().unwrap());
        let remote_frame = Arc::new(Mutex::new(None));
        let sequence = std::sync::atomic::AtomicU64::new(0);
        let metrics = super::SharedMetrics::default();
        let failure = super::decode_h264_access_unit(
            &cached_idr,
            &mut decoder,
            &egui::Context::default(),
            &remote_frame,
            &sequence,
            &metrics,
        );

        assert!(
            failure.is_none(),
            "fallback de CPU deve decodificar o IDR em cache"
        );
        let published = remote_frame
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        assert!(published.is_some(), "o IDR em cache deve chegar à prévia");
        assert_eq!(
            metrics
                .decoded_frames
                .load(std::sync::atomic::Ordering::Relaxed),
            1
        );
        assert_eq!(
            metrics
                .published_frames
                .load(std::sync::atomic::Ordering::Relaxed),
            1
        );
    }
}
