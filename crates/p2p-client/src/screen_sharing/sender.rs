use super::*;

#[derive(Default)]
struct LatestFramePacer {
    last_sequence: Option<u64>,
    last_encoded_at: Option<Instant>,
    next_frame_due_at: Option<Instant>,
}

impl LatestFramePacer {
    /// Returns `Some(true)` for a new captured frame and `Some(false)` for a
    /// one-second repeat of the latest image. `None` means keep waiting.
    fn should_encode(&self, sequence: u64, now: Instant) -> Option<bool> {
        let Some(last_encoded_at) = self.last_encoded_at else {
            return Some(true);
        };
        let elapsed = now.saturating_duration_since(last_encoded_at);
        if self.last_sequence != Some(sequence) {
            let next_due = self.next_frame_due_at.unwrap_or(last_encoded_at);
            if now
                .checked_add(ENCODER_PACING_JITTER_TOLERANCE)
                .unwrap_or(now)
                < next_due
            {
                return None;
            }
            Some(true)
        } else if elapsed >= STATIC_FRAME_REPEAT_INTERVAL {
            Some(false)
        } else {
            None
        }
    }

    fn record_encoded(&mut self, sequence: u64, now: Instant) {
        let is_repeat = self.last_sequence == Some(sequence);
        self.last_sequence = Some(sequence);
        self.last_encoded_at = Some(now);
        if is_repeat {
            self.next_frame_due_at = now.checked_add(FRAME_DURATION);
            return;
        }
        let mut next_due = self
            .next_frame_due_at
            .unwrap_or(now)
            .checked_add(FRAME_DURATION)
            .unwrap_or(now);
        while next_due <= now {
            let Some(advanced) = next_due.checked_add(FRAME_DURATION) else {
                break;
            };
            next_due = advanced;
        }
        self.next_frame_due_at = Some(next_due);
    }

    fn wait_duration(&self, sequence: Option<u64>, now: Instant) -> Duration {
        let Some(last_encoded_at) = self.last_encoded_at else {
            return Duration::ZERO;
        };
        if sequence.is_some() && sequence == self.last_sequence {
            return last_encoded_at
                .checked_add(STATIC_FRAME_REPEAT_INTERVAL)
                .map(|deadline| deadline.saturating_duration_since(now))
                .unwrap_or_default();
        }
        let next_due = self
            .next_frame_due_at
            .unwrap_or_else(|| last_encoded_at + FRAME_DURATION);
        let tolerance_due = next_due
            .checked_sub(ENCODER_PACING_JITTER_TOLERANCE)
            .unwrap_or(next_due);
        tolerance_due.saturating_duration_since(now)
    }
}

#[derive(Default)]
struct H264ForwardingGate {
    sps: Option<Vec<u8>>,
    pps: Option<Vec<u8>>,
    initial_idr_forwarded: bool,
}

enum H264ForwardDecision {
    Forward {
        access_unit: Vec<u8>,
        kind: H264FrameKind,
    },
    DropBeforeInitialIdr,
    DropInitialIdrWithoutParameterSets,
    DropWithoutPicture,
}

impl H264ForwardingGate {
    fn reset(&mut self) {
        self.sps = None;
        self.pps = None;
        self.initial_idr_forwarded = false;
    }

    fn waiting_for_initial_idr(&self) -> bool {
        !self.initial_idr_forwarded
    }

    fn prepare(&mut self, access_unit: &[u8]) -> H264ForwardDecision {
        let nals = annex_b_nals(access_unit);
        if nals.is_empty() {
            return H264ForwardDecision::DropWithoutPicture;
        }

        for nal in &nals {
            match nal.first().map(|header| header & 0x1f) {
                Some(7) => self.sps = Some(nal.to_vec()),
                Some(8) => self.pps = Some(nal.to_vec()),
                _ => {}
            }
        }

        let nal_types = nals
            .iter()
            .filter_map(|nal| nal.first().map(|header| header & 0x1f))
            .collect::<Vec<_>>();
        let Some(kind) = classify_h264_access_unit(&nal_types) else {
            return H264ForwardDecision::DropWithoutPicture;
        };

        if self.waiting_for_initial_idr() && kind != H264FrameKind::Idr {
            return H264ForwardDecision::DropBeforeInitialIdr;
        }

        let Some(sps) = self.sps.as_deref() else {
            return if kind == H264FrameKind::Idr {
                H264ForwardDecision::DropInitialIdrWithoutParameterSets
            } else {
                H264ForwardDecision::DropBeforeInitialIdr
            };
        };
        let Some(pps) = self.pps.as_deref() else {
            return if kind == H264FrameKind::Idr {
                H264ForwardDecision::DropInitialIdrWithoutParameterSets
            } else {
                H264ForwardDecision::DropBeforeInitialIdr
            };
        };

        let includes_sps = nal_types.contains(&7);
        let includes_pps = nal_types.contains(&8);
        let output = if kind == H264FrameKind::Idr && (!includes_sps || !includes_pps) {
            access_unit_with_parameter_sets(&nals, sps, pps)
        } else {
            access_unit.to_vec()
        };

        if kind == H264FrameKind::Idr {
            self.initial_idr_forwarded = true;
        }
        H264ForwardDecision::Forward {
            access_unit: output,
            kind,
        }
    }
}

fn access_unit_with_parameter_sets(nals: &[&[u8]], sps: &[u8], pps: &[u8]) -> Vec<u8> {
    fn append_nal(output: &mut Vec<u8>, nal: &[u8]) {
        output.extend_from_slice(&[0, 0, 0, 1]);
        output.extend_from_slice(nal);
    }

    let mut output = Vec::new();
    let mut insertion_complete = false;
    for nal in nals {
        let nal_type = nal.first().map(|header| header & 0x1f);
        if !insertion_complete && nal_type != Some(9) {
            append_nal(&mut output, sps);
            append_nal(&mut output, pps);
            insertion_complete = true;
        }
        if nal_type != Some(7) && nal_type != Some(8) {
            append_nal(&mut output, nal);
        }
    }
    if !insertion_complete {
        append_nal(&mut output, sps);
        append_nal(&mut output, pps);
    }
    output
}

pub(super) fn encode_latest_frames(
    source: LatestFrame,
    samples: mpsc::Sender<EncodedFrame>,
    stop: Arc<AtomicBool>,
    metrics: Arc<SharedMetrics>,
    force_keyframe: Arc<AtomicBool>,
    bitrate_bps: u32,
) -> Result<(), String> {
    let mut encoder: Option<ActiveH264Encoder> = None;
    let mut hardware_warmup_frames = 0u32;
    let mut forwarding_gate = H264ForwardingGate::default();
    let mut pacer = LatestFramePacer::default();
    let mut observed_generation = source.generation();

    while !stop.load(Ordering::Relaxed) {
        // Do not consume the encoder's first IDR/SPS/PPS while ICE/DTLS is still
        // negotiating. RTP packets written before the peer is connected can be
        // discarded; starting with a P-frame then leaves the receiver without
        // the parameter sets needed to decode the stream.
        if !metrics.p2p_connected.load(Ordering::Relaxed) {
            let (generation, _, _) =
                source.wait_for_change(observed_generation, Duration::from_millis(250), &stop);
            observed_generation = generation;
            continue;
        }

        let frame = source.latest();
        let Some(frame) = frame else {
            let (generation, _, _) =
                source.wait_for_change(observed_generation, Duration::from_millis(250), &stop);
            observed_generation = generation;
            continue;
        };
        let now = Instant::now();
        let Some(is_new_capture_frame) = pacer.should_encode(frame.sequence, now) else {
            let wait = pacer.wait_duration(Some(frame.sequence), now);
            let (generation, _, _) = source.wait_for_change(
                observed_generation,
                wait.max(Duration::from_millis(1)),
                &stop,
            );
            observed_generation = generation;
            continue;
        };
        validate_encoder_frame(&frame)?;
        metrics.record_video_dimensions(frame.width, frame.height);
        let skipped_sequences = if is_new_capture_frame {
            pacer.last_sequence.map_or(0, |previous| {
                frame.sequence.wrapping_sub(previous).saturating_sub(1)
            })
        } else {
            0
        };
        let worker_was_late = pacer.last_encoded_at.is_some_and(|previous| {
            now.saturating_duration_since(previous) > FRAME_DURATION + Duration::from_millis(3)
        });
        metrics.record_capture_frame_for_encoder(!is_new_capture_frame);
        metrics.record_encoder_worker_delay(worker_was_late, skipped_sequences);
        if encoder.is_none() {
            #[cfg(windows)]
            {
                let hardware_result = if let Some(surface) = frame.gpu_nv12.as_ref() {
                    match mf_video::HardwareEncoder::new_gpu_with_bitrate(
                        frame.width,
                        frame.height,
                        surface.device(),
                        bitrate_bps,
                    ) {
                        Ok(hardware) => Ok((hardware, true, None)),
                        Err(gpu_error) => mf_video::HardwareEncoder::new_with_bitrate(frame.width, frame.height, bitrate_bps)
                            .map(|hardware| {
                                (
                                    hardware,
                                    false,
                                    Some(format!("Entrada por superfície D3D11 indisponível: {gpu_error}")),
                                )
                            })
                            .map_err(|cpu_error| {
                                format!(
                                    "Superfície D3D11: {gpu_error}; entrada do Media Foundation pela CPU: {cpu_error}"
                                )
                            }),
                    }
                } else {
                    mf_video::HardwareEncoder::new_with_bitrate(
                        frame.width,
                        frame.height,
                        bitrate_bps,
                    )
                    .map(|hardware| (hardware, false, None))
                };
                match hardware_result {
                    Ok((hardware, gpu_input, fallback_reason)) => {
                        let name = hardware.name().to_owned();
                        tracing::info!(codec = %name, width = frame.width, height = frame.height, "Codificador H.264 de hardware ativado");
                        metrics
                            .set_encoder_backend(format!("GPU — Media Foundation / {name}"), None);
                        encoder = Some(ActiveH264Encoder::MediaFoundation(hardware));
                        let active_backend = if gpu_input {
                            format!("GPU / D3D11 surface -> Media Foundation / {name}")
                        } else {
                            format!("GPU / Media Foundation / {name} (entrada em CPU)")
                        };
                        metrics.set_encoder_backend(active_backend, fallback_reason);
                    }
                    Err(error) => {
                        let reason =
                            format!("Media Foundation H.264 de hardware indisponível: {error}");
                        tracing::warn!(fallback_reason = %reason, "Usando codificador H.264 OpenH264 na CPU");
                        metrics.set_encoder_backend("CPU — OpenH264".to_owned(), Some(reason));
                        encoder = Some(ActiveH264Encoder::OpenH264(Box::new(openh264_encoder(
                            bitrate_bps,
                        )?)));
                    }
                }
            }
            #[cfg(not(windows))]
            {
                let reason = "Media Foundation está disponível apenas no Windows.".to_owned();
                metrics.set_encoder_backend("CPU — OpenH264".to_owned(), Some(reason));
                encoder = Some(ActiveH264Encoder::OpenH264(Box::new(openh264_encoder(
                    bitrate_bps,
                )?)));
            }
        }

        if force_keyframe.swap(false, Ordering::Relaxed) {
            match encoder.as_mut() {
                Some(ActiveH264Encoder::OpenH264(cpu)) => {
                    cpu.force_intra_frame();
                    tracing::debug!("OpenH264 recebeu solicitação para gerar um IDR");
                }
                #[cfg(windows)]
                Some(ActiveH264Encoder::MediaFoundation(hardware)) => {
                    match hardware.force_keyframe() {
                        Ok(true) => tracing::debug!("Media Foundation aceitou solicitação de IDR"),
                        Ok(false) => tracing::warn!(
                            "O codificador de hardware não oferece controle de quadro-chave; aguardando IDR periódico"
                        ),
                        Err(error) => tracing::warn!(
                            error = %error,
                            "O codificador de hardware recusou pedido de IDR; aguardando IDR periódico"
                        ),
                    }
                }
                None => {}
            }
        }

        let active = encoder
            .as_mut()
            .expect("codificador inicializado antes de codificar");
        pacer.record_encoded(frame.sequence, Instant::now());
        let hardware_failure = match active {
            ActiveH264Encoder::OpenH264(cpu) => {
                let encode_started_at = Instant::now();
                let encoded_result = encode_frame(cpu, &frame);
                metrics.record_encode_duration(encode_started_at.elapsed());
                let encoded = encoded_result?;
                forward_encoded_access_unit(&encoded, &samples, &metrics, &mut forwarding_gate)?;
                None
            }
            #[cfg(windows)]
            ActiveH264Encoder::MediaFoundation(hardware) => {
                if forwarding_gate.waiting_for_initial_idr() {
                    hardware_warmup_frames = hardware_warmup_frames.saturating_add(1);
                }
                let encode_started_at = Instant::now();
                let encode_result = hardware
                    .encode_gpu_or_nv12_or_rgba(
                        frame.gpu_nv12.as_deref(),
                        frame.cpu_nv12.as_deref(),
                        &frame.rgba,
                    )
                    .map(|(bytes, used_gpu, fallback_reason)| {
                        if !used_gpu && fallback_reason.is_some() {
                            metrics.set_encoder_backend(
                                "GPU / Media Foundation (entrada em CPU)".to_owned(),
                                fallback_reason,
                            );
                        }
                        bytes
                    });
                metrics.record_encode_duration(encode_started_at.elapsed());
                match encode_result {
                    Ok(encoded) => {
                        let was_waiting_for_idr = forwarding_gate.waiting_for_initial_idr();
                        forward_encoded_access_unit(
                            &encoded,
                            &samples,
                            &metrics,
                            &mut forwarding_gate,
                        )?;
                        if was_waiting_for_idr && !forwarding_gate.waiting_for_initial_idr() {
                            hardware_warmup_frames = 0;
                        }
                        if forwarding_gate.waiting_for_initial_idr() && hardware_warmup_frames > 90
                        {
                            Some("O codificador de hardware não enviou um IDR inicial com SPS/PPS após 90 quadros de entrada; usando OpenH264.".to_owned())
                        } else {
                            None
                        }
                    }
                    Err(error) => Some(error),
                }
            }
        };

        if let Some(reason) = hardware_failure {
            tracing::warn!(fallback_reason = %reason, "Falha no codificador H.264 de hardware; mudando para OpenH264 na CPU");
            metrics.set_encoder_backend("CPU — OpenH264".to_owned(), Some(reason));
            forwarding_gate.reset();
            hardware_warmup_frames = 0;
            let mut cpu = openh264_encoder(bitrate_bps)?;
            metrics.record_encoder_input_frame();
            let encode_started_at = Instant::now();
            let encoded_result = encode_frame(&mut cpu, &frame);
            metrics.record_encode_duration(encode_started_at.elapsed());
            let encoded = encoded_result?;
            forward_encoded_access_unit(&encoded, &samples, &metrics, &mut forwarding_gate)?;
            encoder = Some(ActiveH264Encoder::OpenH264(Box::new(cpu)));
        }
    }
    Ok(())
}

enum ActiveH264Encoder {
    OpenH264(Box<Encoder>),
    #[cfg(windows)]
    MediaFoundation(mf_video::HardwareEncoder),
}

pub(super) fn openh264_encoder(bitrate_bps: u32) -> Result<Encoder, String> {
    let encoder_config = EncoderConfig::new()
        .bitrate(BitRate::from_bps(bitrate_bps))
        .max_frame_rate(FrameRate::from_hz(30.0))
        .usage_type(UsageType::ScreenContentRealTime)
        .adaptive_quantization(false)
        .background_detection(false)
        .intra_frame_period(IntraFramePeriod::from_num_frames(30));
    Encoder::with_api_config(OpenH264API::from_source(), encoder_config)
        .map_err(|error| format!("Não foi possível iniciar o codificador H.264 OpenH264: {error}"))
}

pub(super) struct EncodedFrame {
    pub(super) bytes: Vec<u8>,
    pub(super) kind: H264FrameKind,
}

fn forward_encoded_access_unit(
    encoded: &[u8],
    samples: &mpsc::Sender<EncodedFrame>,
    metrics: &SharedMetrics,
    forwarding_gate: &mut H264ForwardingGate,
) -> Result<(), String> {
    if encoded.is_empty() {
        return Ok(());
    }

    metrics.record_encoded_access_unit(encoded);
    match forwarding_gate.prepare(encoded) {
        H264ForwardDecision::Forward { access_unit, kind } => {
            send_encoded_frame(&access_unit, kind, samples, metrics)
        }
        H264ForwardDecision::DropBeforeInitialIdr => {
            metrics.record_drop_before_initial_idr();
            Ok(())
        }
        H264ForwardDecision::DropInitialIdrWithoutParameterSets
        | H264ForwardDecision::DropWithoutPicture => Ok(()),
    }
}

fn send_encoded_frame(
    encoded: &[u8],
    kind: H264FrameKind,
    samples: &mpsc::Sender<EncodedFrame>,
    metrics: &SharedMetrics,
) -> Result<(), String> {
    if encoded.is_empty() {
        return Ok(());
    }
    let sample = EncodedFrame {
        bytes: encoded.to_vec(),
        kind,
    };
    let queue_wait_started_at = Instant::now();
    let send_result = samples.blocking_send(sample);
    metrics.interval_queue_wait_nanos.fetch_add(
        queue_wait_started_at.elapsed().as_nanos() as u64,
        Ordering::Relaxed,
    );
    metrics
        .interval_queue_wait_samples
        .fetch_add(1, Ordering::Relaxed);
    send_result.map_err(|_| "O envio de vídeo foi encerrado.".to_owned())
}

fn validate_encoder_frame(frame: &PreviewFrame) -> Result<(), String> {
    if frame.width < 2
        || frame.height < 2
        || frame.width > 1280
        || frame.height > 720
        || !frame.width.is_multiple_of(2)
        || !frame.height.is_multiple_of(2)
    {
        return Err(
            "O quadro excede o limite 1280×720 ou não tem dimensões H.264 válidas.".to_owned(),
        );
    }
    let expected_len = (frame.width as usize)
        .checked_mul(frame.height as usize)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| "O tamanho do quadro da tela é inválido.".to_owned())?;
    let rgba_is_valid = frame.rgba.len() == expected_len;
    #[cfg(windows)]
    let cpu_encoder_input_available = frame.cpu_nv12.is_some();
    #[cfg(not(windows))]
    let cpu_encoder_input_available = false;
    if !rgba_is_valid && !cpu_encoder_input_available {
        return Err("O quadro da tela não contém uma entrada válida para o encoder.".to_owned());
    }
    Ok(())
}

pub(super) fn encode_frame(encoder: &mut Encoder, frame: &PreviewFrame) -> Result<Vec<u8>, String> {
    validate_encoder_frame(frame)?;

    let expected_len = frame.width as usize * frame.height as usize * 4;
    let rgba_bytes: Cow<'_, [u8]> = if frame.rgba.len() == expected_len {
        Cow::Borrowed(&frame.rgba)
    } else {
        #[cfg(windows)]
        {
            let on_demand_nv12;
            let input = if let Some(cpu_nv12) = frame.cpu_nv12.as_deref() {
                cpu_nv12
            } else if let Some(gpu_nv12) = frame.gpu_nv12.as_deref() {
                let (bytes, stride) = gpu_nv12.readback_nv12()?;
                on_demand_nv12 = crate::screen_capture::CpuNv12Frame {
                    bytes: std::sync::Arc::new(bytes),
                    stride,
                };
                &on_demand_nv12
            } else {
                return Err("O quadro sem prévia não contém entrada NV12 para OpenH264.".to_owned());
            };
            Cow::Owned(mf_video::cpu_nv12_to_rgba(
                input,
                frame.width,
                frame.height,
            )?)
        }
        #[cfg(not(windows))]
        {
            return Err("O quadro da tela tem um tamanho de imagem inválido.".to_owned());
        }
    };
    let rgba = RgbaSliceU8::new(
        rgba_bytes.as_ref(),
        (frame.width as usize, frame.height as usize),
    );
    let yuv = YUVBuffer::from_rgba8_source(rgba);
    encoder
        .encode(&yuv)
        .map(|encoded| encoded.to_vec())
        .map_err(|error| format!("Falha ao codificar um quadro da tela em H.264: {error}"))
}

pub(super) fn unique_ssrc() -> u32 {
    let time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u32;
    time ^ std::process::id().rotate_left(13)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_encoder_fallback_builds_rgba_from_nv12_without_local_preview() {
        let width = 320u32;
        let height = 240u32;
        let y_plane_len = width as usize * height as usize;
        let mut nv12 = vec![16; y_plane_len];
        nv12.resize(y_plane_len + y_plane_len / 2, 128);
        let frame = PreviewFrame {
            sequence: 1,
            width,
            height,
            rgba: Vec::new(),
            gpu_nv12: None,
            cpu_nv12: Some(Arc::new(crate::screen_capture::CpuNv12Frame {
                bytes: Arc::new(nv12),
                stride: width as usize,
            })),
        };

        let mut encoder = Encoder::new().unwrap();
        let encoded = encode_frame(&mut encoder, &frame).unwrap();
        assert!(!encoded.is_empty());
        let mut decoder = Decoder::new().unwrap();
        let decoded = decoder.decode(&encoded).unwrap().unwrap();
        assert_eq!(decoded.dimensions(), (width as usize, height as usize));
    }

    #[test]
    fn rgba_frame_can_be_encoded_and_decoded_as_h264() {
        let mut encoder = Encoder::new().unwrap();
        let frame = PreviewFrame {
            sequence: 1,
            width: 320,
            height: 240,
            rgba: vec![96; 320 * 240 * 4],
            #[cfg(windows)]
            gpu_nv12: None,
            #[cfg(windows)]
            cpu_nv12: None,
        };
        let encoded = encode_frame(&mut encoder, &frame).unwrap();
        assert!(!encoded.is_empty());

        let mut decoder = Decoder::new().unwrap();
        let decoded = decoder.decode(&encoded).unwrap();
        assert!(decoded.is_some());
        let (width, height) = decoded.unwrap().dimensions();
        assert_eq!((width, height), (320, 240));
    }

    #[cfg(windows)]
    #[test]
    fn available_media_foundation_h264_hardware_emits_baseline_parameter_sets() {
        let mut encoder = match mf_video::HardwareEncoder::new(320, 240) {
            Ok(encoder) => encoder,
            Err(reason) => {
                eprintln!("Sem codificador H.264 de hardware compatível neste PC: {reason}");
                return;
            }
        };
        eprintln!("Codificador de teste: {}", encoder.name());
        let mut forwarding_gate = H264ForwardingGate::default();
        let mut initial_idr = None;
        let mut forwarded_delta = false;
        for sequence in 0..90u8 {
            let mut rgba = vec![0u8; 320 * 240 * 4];
            for (index, pixel) in rgba.chunks_exact_mut(4).enumerate() {
                pixel[0] = (index as u8).wrapping_add(sequence.wrapping_mul(11));
                pixel[1] = (index / 320) as u8;
                pixel[2] = (index % 320) as u8;
                pixel[3] = 255;
            }
            let encoded = encoder.encode_rgba(&rgba).unwrap();
            if let H264ForwardDecision::Forward { access_unit, kind } =
                forwarding_gate.prepare(&encoded)
            {
                match kind {
                    H264FrameKind::Idr => {
                        initial_idr.get_or_insert(access_unit);
                    }
                    H264FrameKind::Delta => forwarded_delta = true,
                };
            }
        }
        assert!(
            forwarded_delta,
            "codificador de hardware precisa emitir quadros P depois do IDR inicial"
        );
        let access_unit = initial_idr.expect("codificador de hardware precisa emitir SPS/PPS/IDR");
        assert_eq!(
            mf_video::sps_dimensions(&access_unit),
            Some((320, 240)),
            "SPS H.264 de hardware precisa anunciar o tamanho do quadro"
        );
        let nals = annex_b_nal_types(&access_unit);
        assert!(nals.contains(&7));
        let mut cursor = 0;
        let mut sps_profile = None;
        while cursor + 3 < access_unit.len() {
            let mut prefix = None;
            for start in cursor..access_unit.len().saturating_sub(2) {
                if access_unit[start..].starts_with(&[0, 0, 0, 1]) {
                    prefix = Some((start, 4));
                    break;
                }
                if access_unit[start..].starts_with(&[0, 0, 1]) {
                    prefix = Some((start, 3));
                    break;
                }
            }
            let Some((_, prefix_len)) = prefix else { break };
            let nal_start = prefix.unwrap().0 + prefix_len;
            let mut next = None;
            for start in nal_start..access_unit.len().saturating_sub(2) {
                if access_unit[start..].starts_with(&[0, 0, 0, 1]) {
                    next = Some(start);
                    break;
                }
                if access_unit[start..].starts_with(&[0, 0, 1]) {
                    next = Some(start);
                    break;
                }
            }
            let end = next.unwrap_or(access_unit.len());
            if nal_start + 1 < end && access_unit[nal_start] & 0x1f == 7 {
                sps_profile = Some(access_unit[nal_start + 1]);
                break;
            }
            cursor = end;
        }
        assert_eq!(sps_profile, Some(66), "SPS deve usar perfil Baseline");

        let mut decoder = match mf_video::HardwareDecoder::new(320, 240) {
            Ok(decoder) => decoder,
            Err(reason) => {
                eprintln!("Sem decodificador DXVA compatível neste PC: {reason}");
                return;
            }
        };
        eprintln!("Decodificador de teste: {}", decoder.name());
        let mut decoded = None;
        let mut hardware_error = None;
        for _ in 0..8 {
            match decoder.decode(&access_unit) {
                Ok(Some(frame)) => {
                    decoded = Some(frame);
                    break;
                }
                Ok(None) => {}
                Err(error) => {
                    hardware_error = Some(error);
                    break;
                }
            }
        }
        let Some(decoded) = decoded else {
            let reason = hardware_error.unwrap_or_else(|| {
                "o decodificador DXVA não produziu quadro no teste controlado".to_owned()
            });
            eprintln!("DXVA sem saída; conferindo fallback OpenH264: {reason}");
            let mut cpu = Decoder::new().unwrap();
            let decoded = cpu
                .decode(&access_unit)
                .unwrap()
                .expect("OpenH264 deve decodificar o mesmo quadro H.264");
            assert_eq!(decoded.dimensions(), (320, 240));
            return;
        };
        assert_eq!((decoded.width, decoded.height), (320, 240));
        assert_eq!(decoded.rgba.len(), 320 * 240 * 4);
    }

    #[test]
    fn encoder_rejects_frames_above_720p() {
        let mut encoder = Encoder::new().unwrap();
        let frame = PreviewFrame {
            sequence: 1,
            width: 1282,
            height: 720,
            rgba: Vec::new(),
            #[cfg(windows)]
            gpu_nv12: None,
            #[cfg(windows)]
            cpu_nv12: None,
        };
        assert!(encode_frame(&mut encoder, &frame).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn frame_pacer_sends_new_frames_at_up_to_30_fps_and_repeats_static_at_1_fps() {
        let start = Instant::now();
        let mut pacer = LatestFramePacer::default();
        assert_eq!(pacer.should_encode(1, start), Some(true));
        pacer.record_encoded(1, start);
        assert_eq!(
            pacer.should_encode(
                2,
                start + FRAME_DURATION - ENCODER_PACING_JITTER_TOLERANCE - Duration::from_nanos(1),
            ),
            None,
            "não pode exceder 30 FPS"
        );
        assert_eq!(
            pacer.should_encode(2, start + FRAME_DURATION - ENCODER_PACING_JITTER_TOLERANCE,),
            Some(true),
            "o quadro novo deve ser enviado assim que o intervalo permitir"
        );
        pacer.record_encoded(2, start + FRAME_DURATION);
        assert_eq!(
            pacer.should_encode(2, start + FRAME_DURATION + Duration::from_millis(999)),
            None
        );
        assert_eq!(
            pacer.should_encode(2, start + FRAME_DURATION + Duration::from_secs(1)),
            Some(false),
            "imagem estática deve ser repetida a cada segundo"
        );
    }

    #[test]
    fn frame_pacer_accepts_30_fps_coalesces_60_fps_and_keeps_15_fps() {
        let start = Instant::now();
        let mut at_30 = LatestFramePacer::default();
        at_30.record_encoded(0, start);
        let mut accepted_30 = 0;
        for index in 1..=30 {
            let now = start + FRAME_DURATION * index;
            if at_30.should_encode(index as u64, now).is_some() {
                accepted_30 += 1;
                at_30.record_encoded(index as u64, now);
            }
        }
        assert_eq!(accepted_30, 30);

        let mut at_60 = LatestFramePacer::default();
        assert_eq!(at_60.should_encode(0, start), Some(true));
        at_60.record_encoded(0, start);
        let half_frame = Duration::from_nanos(FRAME_DURATION.as_nanos() as u64 / 2 + 1);
        let mut accepted_60 = 0;
        for index in 1..=60 {
            let now = start + half_frame * index;
            if at_60.should_encode(index as u64, now).is_some() {
                accepted_60 += 1;
                at_60.record_encoded(index as u64, now);
            }
        }
        assert!(
            (29..=30).contains(&accepted_60),
            "aceitos em 60 callbacks: {accepted_60}"
        );

        let mut at_15 = LatestFramePacer::default();
        at_15.record_encoded(0, start);
        for index in 1..=15 {
            let now = start + FRAME_DURATION * (index * 2);
            assert_eq!(at_15.should_encode(index as u64, now), Some(true));
            at_15.record_encoded(index as u64, now);
        }

        let mut jittered = LatestFramePacer::default();
        jittered.record_encoded(0, start);
        let mut elapsed = Duration::ZERO;
        let mut accepted_jittered = 0;
        for index in 1..=90u64 {
            elapsed += if index % 2 == 0 {
                Duration::from_micros(33_800)
            } else {
                Duration::from_micros(32_800)
            };
            let now = start + elapsed;
            if jittered.should_encode(index, now).is_some() {
                accepted_jittered += 1;
                jittered.record_encoded(index, now);
            }
        }
        assert!(
            accepted_jittered >= 89,
            "pequena variação não deve descartar quadros alternados: {accepted_jittered}/90"
        );
    }

    #[test]
    fn h264_forwarding_gate_waits_for_idr_then_forwards_delta_frames() {
        let sps = [0x67, 0x64, 0x00, 0x1f];
        let pps = [0x68, 0x00];
        let idr = [0x65, 0x88];
        let delta = [0x41, 0x9a];
        let mut gate = H264ForwardingGate::default();

        assert_eq!(classify_h264_access_unit(&[7, 8]), None);
        assert_eq!(
            classify_h264_access_unit(&[7, 8, 5]),
            Some(H264FrameKind::Idr)
        );
        assert_eq!(classify_h264_access_unit(&[1]), Some(H264FrameKind::Delta));

        assert!(matches!(
            gate.prepare(&annex_b_access_unit(&[&idr])),
            H264ForwardDecision::DropInitialIdrWithoutParameterSets
        ));
        assert!(matches!(
            gate.prepare(&annex_b_access_unit(&[&delta])),
            H264ForwardDecision::DropBeforeInitialIdr
        ));
        assert!(matches!(
            gate.prepare(&annex_b_access_unit(&[&sps, &pps])),
            H264ForwardDecision::DropWithoutPicture
        ));

        let first_idr = gate.prepare(&annex_b_access_unit(&[&idr]));
        match first_idr {
            H264ForwardDecision::Forward { access_unit, kind } => {
                assert_eq!(kind, H264FrameKind::Idr);
                assert_eq!(annex_b_nal_types(&access_unit), vec![7, 8, 5]);
            }
            _ => panic!("o IDR inicial deve ser encaminhado com SPS e PPS"),
        }

        assert!(matches!(
            gate.prepare(&annex_b_access_unit(&[&delta])),
            H264ForwardDecision::Forward {
                kind: H264FrameKind::Delta,
                ..
            }
        ));

        let next_idr = gate.prepare(&annex_b_access_unit(&[&idr]));
        match next_idr {
            H264ForwardDecision::Forward { access_unit, kind } => {
                assert_eq!(kind, H264FrameKind::Idr);
                assert_eq!(annex_b_nal_types(&access_unit), vec![7, 8, 5]);
            }
            _ => panic!("IDR posterior deve reenviar os parâmetros guardados"),
        }

        assert!(matches!(
            gate.prepare(&annex_b_access_unit(&[&sps, &pps])),
            H264ForwardDecision::DropWithoutPicture
        ));
    }
}
