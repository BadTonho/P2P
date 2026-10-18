use super::*;
use crate::screen_quality::{ScreenShareFrameRate, ScreenShareResolution};

fn adaptive_feedback_state(start: Instant) -> SessionQualityFeedback {
    SessionQualityFeedback {
        controller: AdaptiveQualityController::new(start),
        last_received: 0,
        last_published: 0,
        last_queue_drops: 0,
        last_recovery_gate_drops: 0,
    }
}

#[test]
fn adaptive_recovery_uses_complete_windows_when_ui_polls_faster_than_video() {
    let start = Instant::now();
    let mut state = adaptive_feedback_state(start);
    for step in 1..=3_u64 {
        state.observe(
            start + Duration::from_secs(step * 3),
            (step * 60, step * 47, step * 2, 0),
        );
    }
    assert_eq!(state.controller.profile(), AdaptiveVideoProfile::P720Fps15);
    let mut recoveries = Vec::new();
    // UI polls at 100 Hz, video arrives at 15 FPS, publication lags one poll.
    // Decisions must use the full three seconds, not the last 10 ms sample.
    for tick in 1..=3_600_u64 {
        let received = tick * 15 / 100;
        let published = (tick * 15).saturating_sub(1) / 100;
        if let Some(QualityDecision::Changed { feedback, .. }) = state.observe(
            start + Duration::from_secs(9) + Duration::from_millis(tick * 10),
            (180 + received, 141 + published, 6, 0),
        ) {
            assert_eq!(
                feedback.reason,
                adaptive_quality::QualityFeedbackReason::Recovered
            );
            recoveries.push(feedback.profile);
        }
    }
    assert_eq!(recoveries, vec![AdaptiveVideoProfile::P1080Fps60]);
}

#[test]
fn adaptive_recovery_preserves_drop_evidence_between_ui_polls() {
    let start = Instant::now();
    let mut state = adaptive_feedback_state(start);
    let mut decision = None;
    for tick in 1..=300_u64 {
        let drops = u64::from(tick >= 25) + u64::from(tick >= 180);
        decision = state.observe(
            start + Duration::from_millis(tick * 10),
            (tick / 5, tick * 47 / 300, drops, 0),
        );
    }
    assert!(matches!(
        decision,
        Some(QualityDecision::Changed {
            feedback: QualityFeedback {
                profile: AdaptiveVideoProfile::P1080Fps30,
                ..
            },
            overload_evidence: Some(QualityOverloadEvidence { queue_drops: 2, .. }),
        })
    ));
}

#[test]
fn adaptive_recovery_does_not_treat_an_empty_stream_as_healthy() {
    let start = Instant::now();
    let mut state = adaptive_feedback_state(start);
    let counters = (60, 47, 2, 0);
    state.observe(start + Duration::from_secs(3), counters);
    assert_eq!(state.controller.profile(), AdaptiveVideoProfile::P1080Fps30);
    for step in 2..=100_u64 {
        assert_eq!(
            state.observe(start + Duration::from_secs(step * 3), counters),
            None
        );
    }
    assert_eq!(state.controller.profile(), AdaptiveVideoProfile::P1080Fps30);
}

#[test]
fn ice_diagnostics_resolve_raw_and_prefixed_ids_without_guessing_another_candidate() {
    use rtc::statistics::stats::ice_candidate::RTCIceCandidateStats;
    use rtc::statistics::stats::{RTCStats, RTCStatsType};
    let candidate = |id: &str, local: bool| RTCIceCandidateStats {
        stats: RTCStats {
            timestamp: rtc::shared::time::SystemInstant::now(Instant::now()),
            typ: if local {
                RTCStatsType::LocalCandidate
            } else {
                RTCStatsType::RemoteCandidate
            },
            id: id.to_owned(),
        },
        transport_id: String::new(),
        address: Some("127.0.0.1".to_owned()),
        port: 9002,
        protocol: "udp".to_owned(),
        candidate_type: RTCIceCandidateType::Host,
        priority: 0,
        url: String::new(),
        relay_protocol: Default::default(),
        foundation: String::new(),
        related_address: String::new(),
        related_port: 0,
        username_fragment: String::new(),
        tcp_type: Default::default(),
    };
    let entry = RTCStatsReportEntry::LocalCandidate(candidate("RTCLocalIceCandidate_test", true));
    assert_eq!(
        candidate_stats_for_id(&entry, "test", true).unwrap().port,
        9002
    );
    assert!(candidate_stats_for_id(&entry, "RTCLocalIceCandidate_test", true).is_some());
    assert!(candidate_stats_for_id(&entry, "different", true).is_none());
    assert!(candidate_stats_for_id(&entry, "test", false).is_none());
    assert!(candidate_stats_for_id(&entry, "", true).is_none());
    let remote =
        RTCStatsReportEntry::RemoteCandidate(candidate("RTCRemoteIceCandidate_peer", false));
    assert!(candidate_stats_for_id(&remote, "peer", false).is_some());
}
use crate::audio_capture::AudioPlaybackSink;
use rtc::peer_connection::transport::RTCIceCandidateType;
use std::f32::consts::TAU;
use std::sync::atomic::AtomicU32;

#[derive(Default)]
struct SyntheticAudioProbe {
    source_callbacks: AtomicU64,
    source_frames: AtomicU64,
    source_audible_samples: AtomicU64,
    source_silent_samples: AtomicU64,
    playback_factory_starts: AtomicU64,
    decoded_frames: AtomicU64,
    decoded_audible_samples: AtomicU64,
    decoded_output_peak_max_bits: AtomicU32,
}

struct SyntheticAudioSource {
    probe: Arc<SyntheticAudioProbe>,
    sample_cursor: u64,
}

impl SyntheticAudioSource {
    fn new(probe: Arc<SyntheticAudioProbe>) -> Self {
        Self {
            probe,
            sample_cursor: 0,
        }
    }
}

impl AudioSampleSource for SyntheticAudioSource {
    fn input_format(&self) -> (u32, usize) {
        (OPUS_SAMPLE_RATE, OPUS_CHANNELS)
    }

    fn read_samples(&mut self, output: &mut [f32]) -> usize {
        let first_frame = self.sample_cursor / OPUS_CHANNELS as u64;
        let mut audible_samples = 0_u64;
        let mut silent_samples = 0_u64;
        for (index, sample) in output.iter_mut().enumerate() {
            let frame = first_frame + (index / OPUS_CHANNELS) as u64;
            // Alternate 250 ms tone and 250 ms silence, deterministically.
            let audible = (frame / 12_000).is_multiple_of(2);
            if audible {
                let phase = TAU * 440.0 * frame as f32 / OPUS_SAMPLE_RATE as f32;
                *sample = phase.sin() * 0.25;
                audible_samples += 1;
            } else {
                *sample = 0.0;
                silent_samples += 1;
            }
        }
        self.sample_cursor += output.len() as u64;
        self.probe.source_callbacks.fetch_add(1, Ordering::Relaxed);
        self.probe
            .source_frames
            .fetch_add((output.len() / OPUS_CHANNELS) as u64, Ordering::Relaxed);
        self.probe
            .source_audible_samples
            .fetch_add(audible_samples, Ordering::Relaxed);
        self.probe
            .source_silent_samples
            .fetch_add(silent_samples, Ordering::Relaxed);
        output.len()
    }

    fn callbacks(&self) -> u64 {
        self.probe.source_callbacks.load(Ordering::Relaxed)
    }

    fn input_frames(&self) -> u64 {
        self.probe.source_frames.load(Ordering::Relaxed)
    }

    fn non_silent_samples(&self) -> u64 {
        self.probe.source_audible_samples.load(Ordering::Relaxed)
    }

    fn captured_frames(&self) -> u64 {
        self.input_frames()
    }

    fn dropped_frames(&self) -> u64 {
        0
    }

    fn xruns(&self) -> u64 {
        0
    }

    fn device_changes(&self) -> u64 {
        0
    }

    fn realtime_denied(&self) -> u64 {
        0
    }

    fn take_error(&self) -> Option<(Option<cpal::ErrorKind>, String)> {
        None
    }
}

struct SyntheticAudioPlaybackFactory {
    probe: Arc<SyntheticAudioProbe>,
}

impl AudioPlaybackFactory for SyntheticAudioPlaybackFactory {
    fn start(
        &self,
        _session_id: u64,
        _ssrc: u32,
        volume: RemoteAudioVolume,
    ) -> Result<Box<dyn AudioPlaybackSink>, String> {
        self.probe
            .playback_factory_starts
            .fetch_add(1, Ordering::Relaxed);
        Ok(Box::new(SyntheticAudioPlayback {
            probe: Arc::clone(&self.probe),
            volume,
        }))
    }
}

struct SyntheticAudioPlayback {
    probe: Arc<SyntheticAudioProbe>,
    volume: RemoteAudioVolume,
}

impl AudioPlaybackSink for SyntheticAudioPlayback {
    fn push_decoded(&mut self, samples: &[f32], frames_per_channel: usize) {
        self.probe.decoded_frames.fetch_add(1, Ordering::Relaxed);
        let gain = self.volume.gain();
        let peak = samples
            .iter()
            .take(frames_per_channel * OPUS_CHANNELS)
            .map(|sample| (sample * gain).abs())
            .fold(0.0_f32, f32::max);
        self.probe
            .decoded_output_peak_max_bits
            .fetch_max(peak.to_bits(), Ordering::Relaxed);
        let audible = samples
            .iter()
            .take(frames_per_channel * OPUS_CHANNELS)
            .filter(|sample| (*sample * gain).abs() > 0.01)
            .count() as u64;
        self.probe
            .decoded_audible_samples
            .fetch_add(audible, Ordering::Relaxed);
    }

    fn output_underflow_frames(&self) -> u64 {
        0
    }

    fn dropped_frames(&self) -> u64 {
        0
    }

    fn callbacks(&self) -> u64 {
        self.probe.decoded_frames.load(Ordering::Relaxed)
    }

    fn non_silent_samples(&self) -> u64 {
        self.probe.decoded_audible_samples.load(Ordering::Relaxed)
    }

    fn take_error(&self) -> Option<String> {
        None
    }
}

#[test]
fn audio_activity_diagnostics_distinguish_silence_from_missing_capture_or_playback() {
    let capture_silent = capture_audio_state(120, 0, 0, 0);
    assert!(capture_silent.contains("silenciosa"));
    assert!(capture_silent.contains("silêncio não é erro"));
    assert!(capture_audio_state(0, 0, 0, 0).contains("não recebeu callbacks"));
    assert!(capture_audio_state(120, 240, 50, 50).contains("50 quadros Opus"));

    let remote_silent = playback_audio_state(250, 250, 0, 300, 0, 100);
    assert!(remote_silent.contains("pode ser silêncio"));
    assert!(playback_audio_state(0, 0, 0, 0, 0, 100).contains("nenhum pacote RTP"));
    assert!(playback_audio_state(250, 250, 1500, 0, 0, 100).contains("não executou callbacks"));
    assert!(
        playback_audio_state(250, 250, 1500, 300, 6000, 100).contains("saída do Windows ativas")
    );
    assert!(playback_audio_state(250, 250, 1500, 300, 0, 0).contains("volume em 0%"));
    assert!(playback_audio_state(0, 0, 0, 300, 0, 0).contains("nenhum pacote RTP"));
    assert!(playback_audio_state(250, 250, 1500, 300, 0, 100).contains("nenhuma amostra audível"));
}

fn pump_signals(from: &ScreenShareSession, to: &ScreenShareSession, label: &str) {
    for event in std::iter::from_fn(|| from.try_recv()) {
        match event {
            ScreenShareEvent::Signal { kind, payload } => {
                to.handle_signal(kind, payload)
                    .unwrap_or_else(|error| panic!("{label} signal failed: {error}"));
            }
            ScreenShareEvent::Error(error) => panic!("{label} session failed: {error}"),
            ScreenShareEvent::AudioError(error) => {
                panic!("{label} audio session failed: {error}")
            }
            ScreenShareEvent::ConnectionClosed => {
                panic!("{label} connection closed before receiving a frame")
            }
            ScreenShareEvent::State(_) => {}
            ScreenShareEvent::AudioState(_) => {}
        }
    }
}

fn test_frame(sequence: u64, value: u8) -> PreviewFrame {
    PreviewFrame {
        sequence,
        width: 320,
        height: 240,
        rgba: vec![value; 320 * 240 * 4],
        #[cfg(windows)]
        gpu_nv12: None,
        #[cfg(windows)]
        cpu_nv12: None,
    }
}

fn diagnostic_frame(
    sequence: u64,
    quality: ScreenShareQuality,
    surface: Arc<mf_video::GpuNv12Surface>,
) -> PreviewFrame {
    let (width, height) = quality.max_dimensions();
    PreviewFrame {
        sequence,
        width,
        height,
        // This path intentionally supplies a D3D11 NV12 surface and no RGBA input.
        rgba: Vec::new(),
        #[cfg(windows)]
        gpu_nv12: Some(surface),
        #[cfg(windows)]
        cpu_nv12: None,
    }
}

fn pump_diagnostic_signals(
    from: &ScreenShareSession,
    to: &ScreenShareSession,
    label: &str,
    errors: &mut Vec<String>,
) {
    for event in std::iter::from_fn(|| from.try_recv()) {
        match event {
            ScreenShareEvent::Signal { kind, payload } => {
                if let Err(error) = to.handle_signal(kind, payload) {
                    errors.push(format!("{label} signal: {error}"));
                }
            }
            ScreenShareEvent::Error(error) => errors.push(format!("{label}: {error}")),
            ScreenShareEvent::AudioError(error) => {
                errors.push(format!("{label} audio: {error}"));
            }
            ScreenShareEvent::ConnectionClosed => {
                errors.push(format!("{label}: connection closed"));
            }
            ScreenShareEvent::State(_) | ScreenShareEvent::AudioState(_) => {}
        }
    }
}

fn diagnostic_peer_json(
    role: &str,
    metrics: ScreenShareMetrics,
    performance: ScreenSharePerformanceSnapshot,
    duration_seconds: f64,
) -> serde_json::Value {
    let fps = |frames: u64| {
        if duration_seconds > 0.0 {
            frames as f64 / duration_seconds
        } else {
            0.0
        }
    };
    serde_json::json!({
        "role": role,
        "connected": metrics.p2p_connected,
        "route": match metrics.route {
            Some(MediaRoute::Direct) => "direct_loopback",
            Some(MediaRoute::Turn) => "relay",
            None => "unknown",
        },
        "backend": if role == "sender" {
            metrics.encoder_backend
        } else {
            metrics.decoder_backend
        },
        "decoder_preference": metrics.decoder_preference,
        "fps": {
            "encoded": fps(performance.encoded_frames),
            "sent": fps(performance.sent_frames),
            "decoded": fps(performance.decoded_frames),
            "published": fps(performance.published_frames),
        },
        "frames": {
            "new_capture": performance.new_capture_frames,
            "repeated_capture": performance.repeated_capture_frames,
            "encoder_worker_late": performance.encoder_worker_late_frames,
            "skipped_capture_sequences": performance.skipped_capture_sequences,
            "encoder_input": performance.encoder_input_frames,
            "encoded": performance.encoded_frames,
            "sent": performance.sent_frames,
            "sent_idr": performance.sent_idr_frames,
            "sent_delta": performance.sent_delta_frames,
            "assembled_access_units": performance.assembled_access_units,
            "decoder_input": performance.decoder_input_frames,
            "decoder_no_output": performance.decoder_no_output_frames,
            "decoded": performance.decoded_frames,
            "published": performance.published_frames,
            "ui_texture_updates": performance.ui_texture_updates,
        },
        "rtp": {
            "packets": if role == "sender" {
                performance.outbound_rtp_packets
            } else {
                performance.inbound_rtp_packets
            },
            "bytes": if role == "sender" {
                performance.outbound_rtp_bytes
            } else {
                performance.inbound_rtp_bytes
            },
            "observed_sequence_gaps": performance.observed_sequence_gaps,
            "recovered_reordered_packets": performance.recovered_reordered_packets,
            "unmatched_out_of_order_packets": performance.unmatched_out_of_order_packets,
            "duplicates": performance.duplicate_packets,
            "confirmed_missing_packets": performance.confirmed_missing_packets,
            "late_after_confirmed_packets": performance.late_after_confirmed_packets,
            "sequence_gap_resyncs": performance.sequence_gap_resyncs,
            "nack_sent": performance.nack_packets_sent,
            "nack_received": performance.nack_packets_received,
            "nack_received_observed": performance.nack_packets_received_observed,
            "pli_sent": performance.pli_requests_sent,
            "pli_received": performance.pli_requests_received,
            "retransmitted_packets_sent": performance.retransmitted_packets_sent,
            "retransmitted_packets_received": performance.retransmitted_packets_received,
        },
        "h264_decoder": {
            "assembly_errors": performance.assembly_errors,
            "decode_errors": performance.decode_errors,
            "decoder_queue_drops": performance.decoder_queue_drops,
            "decoder_queue_accepted": performance.decoder_queue_accepted,
            "dropped_waiting_for_idr": performance.dropped_recovery_gate_waiting_for_idr,
        },
        "timings": performance.video_performance.diagnostic_json(),
    })
}

fn run_loopback_diagnostic_scenario(
    quality: ScreenShareQuality,
    loss_percent: u8,
) -> serde_json::Value {
    const MEASURE_FOR: Duration = Duration::from_secs(10);
    const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

    #[cfg(windows)]
    let surface = match mf_video::DiagnosticGpuSource::new(
        quality.max_dimensions().0,
        quality.max_dimensions().1,
        quality.frames_per_second(),
    )
    .and_then(|mut source| source.make_surface(1))
    {
        Ok(surface) => Arc::new(surface),
        Err(error) => {
            return serde_json::json!({
                "scenario": {
                    "resolution": quality.resolution.label(),
                    "frame_rate_fps": quality.frames_per_second(),
                    "rtp_loss_percent": loss_percent,
                    "status": "gpu_unavailable",
                    "gpu_error": error,
                    "hardware_gpu_required": true,
                }
            });
        }
    };
    #[cfg(not(windows))]
    let surface: Arc<mf_video::GpuNv12Surface> = {
        return serde_json::json!({
            "scenario": {
                "resolution": quality.resolution.label(),
                "frame_rate_fps": quality.frames_per_second(),
                "rtp_loss_percent": loss_percent,
                "status": "gpu_diagnostic_windows_only",
                "hardware_gpu_required": true,
            }
        });
    };

    let context = egui::Context::default();
    let sender = ScreenShareSession::new_loopback(context.clone()).unwrap();
    let receiver = ScreenShareSession::new_loopback_with_decoder_preference(
        context,
        VideoDecoderPreference::PreferDxva,
    )
    .unwrap();
    sender.set_outbound_rtp_loss_percent_for_test(loss_percent);

    let source = LatestFrame::default();
    source.publish(diagnostic_frame(1, quality, Arc::clone(&surface)));
    sender
        .start_sending_with_quality_and_options_and_exclusion(
            source.clone(),
            quality,
            quality.individual_bitrate_bps(),
            false,
            AudioExclusionConfig::default(),
        )
        .unwrap();

    let mut errors = Vec::new();
    let connect_deadline = Instant::now() + CONNECT_TIMEOUT;
    let mut connected_at = None;
    while Instant::now() < connect_deadline {
        pump_diagnostic_signals(&sender, &receiver, "sender", &mut errors);
        pump_diagnostic_signals(&receiver, &sender, "receiver", &mut errors);
        if sender.metrics().p2p_connected && receiver.metrics().p2p_connected {
            connected_at = Some(Instant::now());
            break;
        }
        thread::sleep(Duration::from_millis(5));
    }

    let (duration, source_stats) = if let Some(started_at) = connected_at {
        // Clear connection and startup measurements so the report covers only the fixed window.
        sender.take_performance_snapshot();
        receiver.take_performance_snapshot();
        let deadline = started_at + MEASURE_FOR;
        let source_worker = source.clone();
        let source_surface = Arc::clone(&surface);
        let producer = thread::spawn(move || {
            let mut sequence = 1_u64;
            let mut published_frames = 0_u64;
            let mut build_total_nanos = 0_u64;
            let mut build_max_nanos = 0_u64;
            let mut next_frame = Instant::now();
            while Instant::now() < deadline {
                let now = Instant::now();
                if now < next_frame {
                    thread::sleep(next_frame.saturating_duration_since(now));
                    continue;
                }
                sequence = sequence.saturating_add(1);
                let build_started_at = Instant::now();
                source_worker.publish(diagnostic_frame(
                    sequence,
                    quality,
                    Arc::clone(&source_surface),
                ));
                let build_nanos = build_started_at
                    .elapsed()
                    .as_nanos()
                    .min(u128::from(u64::MAX)) as u64;
                published_frames = published_frames.saturating_add(1);
                build_total_nanos = build_total_nanos.saturating_add(build_nanos);
                build_max_nanos = build_max_nanos.max(build_nanos);
                next_frame += quality.frame_duration();
                let now = Instant::now();
                while next_frame <= now {
                    next_frame += quality.frame_duration();
                }
            }
            (published_frames, build_total_nanos, build_max_nanos)
        });
        while Instant::now() < deadline {
            pump_diagnostic_signals(&sender, &receiver, "sender", &mut errors);
            pump_diagnostic_signals(&receiver, &sender, "receiver", &mut errors);
            thread::sleep(Duration::from_millis(5));
        }
        let source_stats = producer.join().unwrap();
        (started_at.elapsed().min(MEASURE_FOR), source_stats)
    } else {
        errors.push("loopback connection timed out".to_owned());
        (Duration::ZERO, (0, 0, 0))
    };

    pump_diagnostic_signals(&sender, &receiver, "sender", &mut errors);
    pump_diagnostic_signals(&receiver, &sender, "receiver", &mut errors);
    let sender_performance = sender.take_performance_snapshot();
    let receiver_performance = receiver.take_performance_snapshot();
    let sender_metrics = sender.metrics();
    let receiver_metrics = receiver.metrics();
    let encoder_gpu_surface_frames = sender_performance
        .video_performance
        .path_count(crate::video_performance::FramePath::EncoderGpuSurface);
    let decoder_dxva_frames = receiver_performance
        .video_performance
        .path_count(crate::video_performance::FramePath::DecoderDxva);
    let hardware_gpu_valid = encoder_gpu_surface_frames > 0
        && decoder_dxva_frames > 0
        && sender_metrics.encoder_backend.contains("GPU")
        && receiver_metrics.decoder_backend.contains("DXVA");
    let packet_count = sender
        .metrics
        .test_outbound_rtp_seen
        .load(Ordering::Relaxed);
    let dropped_packet_count = sender
        .metrics
        .test_dropped_outbound_rtp
        .load(Ordering::Relaxed);
    let duration_seconds = duration.as_secs_f64();
    let (source_frames, source_build_total_nanos, source_build_max_nanos) = source_stats;
    let status = if connected_at.is_some() {
        "connected"
    } else {
        "connection_timeout"
    };
    let result = serde_json::json!({
        "scenario": {
            "resolution": quality.resolution.label(),
            "frame_rate_fps": quality.frames_per_second(),
            "individual_bitrate_bps": quality.individual_bitrate_bps(),
            "rtp_loss_percent": loss_percent,
            "status": if connected_at.is_none() {
                status
            } else if hardware_gpu_valid {
                "ok_gpu"
            } else {
                "gpu_fallback_or_unverified"
            },
            "connection_status": status,
            "hardware_gpu_required": true,
            "hardware_gpu_valid": hardware_gpu_valid,
            "gpu_paths": {
                "encoder_gpu_surface_frames": encoder_gpu_surface_frames,
                "decoder_dxva_frames": decoder_dxva_frames,
            },
            "gpu_fallback_reason": {
                "encoder": sender_metrics.encoder_fallback_reason,
                "decoder": receiver_metrics.decoder_fallback_reason,
            },
            "measurement_seconds": duration_seconds,
            "injected_rtp_packets_seen": packet_count,
            "injected_rtp_packets_dropped": dropped_packet_count,
        },
        "synthetic_source": {
            "frames_published": source_frames,
            "fps": if duration_seconds > 0.0 { source_frames as f64 / duration_seconds } else { 0.0 },
            "frame_build_total_ms": source_build_total_nanos as f64 / 1_000_000.0,
            "frame_build_mean_ms": if source_frames > 0 { source_build_total_nanos as f64 / source_frames as f64 / 1_000_000.0 } else { 0.0 },
            "frame_build_max_ms": source_build_max_nanos as f64 / 1_000_000.0,
        },
        "sender": diagnostic_peer_json("sender", sender_metrics, sender_performance, duration_seconds),
        "receiver": diagnostic_peer_json("receiver", receiver_metrics, receiver_performance, duration_seconds),
        "errors": errors,
    });
    receiver.stop();
    sender.stop();
    result
}

#[test]
fn regression_dropping_screen_session_releases_its_worker_socket() {
    let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let address = socket.local_addr().unwrap();
    let (commands, mut command_rx) = mpsc::unbounded_channel();
    let (_, events) = std_mpsc::channel();
    let worker = thread::spawn(move || {
        assert!(matches!(command_rx.blocking_recv(), Some(Command::Stop)));
        thread::sleep(Duration::from_millis(50));
        drop(socket);
    });
    let session = ScreenShareSession {
        commands,
        events,
        remote_frame: Arc::new(Mutex::new(None)),
        remote_track: Arc::new(Mutex::new(None)),
        metrics: Arc::new(SharedMetrics::default()),
        remote_audio_volume: RemoteAudioVolume::default(),
        encoder_bitrate_bps: Arc::new(AtomicU32::new(0)),
        encoder_profile: Arc::new(AtomicU32::new(0)),
        adaptive_quality: Mutex::new(SessionQualityFeedback {
            controller: AdaptiveQualityController::new(Instant::now()),
            last_received: 0,
            last_published: 0,
            last_queue_drops: 0,
            last_recovery_gate_drops: 0,
        }),
        worker: Some(worker),
    };
    drop(session);
    assert!(
        std::net::UdpSocket::bind(address).is_ok(),
        "a replacement session must be able to reuse UDP immediately after Drop"
    );
}

#[test]
fn occupied_media_port_reports_the_actual_port_without_blaming_firewall() {
    let error = InterceptorError::from(std::io::Error::from(std::io::ErrorKind::AddrInUse));
    let message = media_connection_error("127.0.0.1:9003", &error);
    assert!(message.contains("UDP 9003"));
    assert!(message.contains("em uso"));
    assert!(!message.contains("9002"));
    assert!(!message.contains("firewall"));
}

#[test]
fn loopback_screen_session_releases_bound_udp_after_an_offer() {
    let reservation = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let address = reservation.local_addr().unwrap();
    drop(reservation);
    for _ in 0..3 {
        let session = ScreenShareSession::with_udp_address_and_audio_factory(
            egui::Context::default(),
            address.to_string(),
            None,
            None,
            VideoDecoderPreference::Cpu,
            Arc::new(SyntheticAudioPlaybackFactory {
                probe: Arc::new(SyntheticAudioProbe::default()),
            }),
        )
        .unwrap();
        let source = LatestFrame::default();
        source.publish(test_frame(1, 64));
        session.start_sending(source).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match session.try_recv() {
                Some(ScreenShareEvent::Signal {
                    kind: SignalKind::Offer,
                    ..
                }) => break,
                Some(ScreenShareEvent::Error(error)) => panic!("offer failed: {error}"),
                _ => {}
            }
            assert!(
                Instant::now() < deadline,
                "offer must confirm that the peer was built"
            );
            thread::sleep(Duration::from_millis(5));
        }
        drop(session);
        assert!(
            std::net::UdpSocket::bind(address).is_ok(),
            "the previous peer must release UDP before its replacement starts"
        );
    }
}

#[test]
fn synthetic_pcm_tone_and_silence_roundtrip_through_opus_without_audio_devices() {
    let probe = Arc::new(SyntheticAudioProbe::default());
    let mut source = SyntheticAudioSource::new(Arc::clone(&probe));
    let playback_factory = SyntheticAudioPlaybackFactory {
        probe: Arc::clone(&probe),
    };
    let mut playback = playback_factory
        .start(1, 1, RemoteAudioVolume::default())
        .unwrap();
    let mut encoder = opus::Encoder::new(
        OPUS_SAMPLE_RATE,
        opus::Channels::Stereo,
        opus::Application::Audio,
    )
    .unwrap();
    let mut decoder = opus::Decoder::new(OPUS_SAMPLE_RATE, opus::Channels::Stereo).unwrap();
    let mut pcm = vec![0.0; OPUS_FRAME_SAMPLES_PER_CHANNEL * OPUS_CHANNELS];
    let mut encoded = vec![0_u8; 4_000];
    let mut decoded = vec![0.0; 5_760 * OPUS_CHANNELS];

    for _ in 0..50 {
        let samples_read = source.read_samples(&mut pcm);
        assert_eq!(samples_read, pcm.len());
        let encoded_len = encoder.encode_float(&pcm, &mut encoded).unwrap();
        assert!(encoded_len > 0);
        let decoded_frames = decoder
            .decode_float(&encoded[..encoded_len], &mut decoded, false)
            .unwrap();
        if decoded_frames > 0 {
            playback.push_decoded(&decoded, decoded_frames);
        }
        assert!(playback.take_error().is_none());
    }

    assert!(source.callbacks() >= 50);
    assert!(source.non_silent_samples() > 0);
    assert!(probe.source_silent_samples.load(Ordering::Relaxed) > 0);
    assert!(playback.callbacks() >= 45);
    assert!(playback.non_silent_samples() > 0);
}

#[test]
fn remote_audio_volume_is_independent_between_sessions_and_inherited_by_new_sessions() {
    let make_session = || {
        ScreenShareSession::with_udp_address_and_audio_volume(
            egui::Context::default(),
            "127.0.0.1:0".to_owned(),
            None,
            None,
            VideoDecoderPreference::Cpu,
            Arc::new(SyntheticAudioPlaybackFactory {
                probe: Arc::new(SyntheticAudioProbe::default()),
            }),
            73,
        )
        .unwrap()
    };
    let first = make_session();
    let second = make_session();

    first.set_remote_audio_volume_percent(20);

    assert_eq!(first.remote_audio_volume_percent(), 20);
    assert_eq!(second.remote_audio_volume_percent(), 73);

    let future = make_session();
    assert_eq!(future.remote_audio_volume_percent(), 73);

    first.stop();
    second.stop();
    future.stop();
}

#[test]
fn loopback_group_audio_does_not_block_video_tracks_in_either_direction() {
    let context = egui::Context::default();
    let a_sender = ScreenShareSession::new_loopback(context.clone()).unwrap();
    let b_receive_audio_probe = Arc::new(SyntheticAudioProbe::default());
    let b_receiver = ScreenShareSession::with_udp_address_and_audio_factory(
        context.clone(),
        "127.0.0.1:0".to_owned(),
        None,
        None,
        VideoDecoderPreference::Cpu,
        Arc::new(SyntheticAudioPlaybackFactory {
            probe: Arc::clone(&b_receive_audio_probe),
        }),
    )
    .unwrap();
    let b_sender = ScreenShareSession::new_loopback(context.clone()).unwrap();
    let a_receive_audio_probe = Arc::new(SyntheticAudioProbe::default());
    let a_receiver = ScreenShareSession::with_udp_address_and_audio_factory(
        context,
        "127.0.0.1:0".to_owned(),
        None,
        None,
        VideoDecoderPreference::Cpu,
        Arc::new(SyntheticAudioPlaybackFactory {
            probe: Arc::clone(&a_receive_audio_probe),
        }),
    )
    .unwrap();
    a_receiver.set_remote_audio_volume_percent(50);
    b_receiver.set_remote_audio_volume_percent(0);
    let a_source = LatestFrame::default();
    let b_source = LatestFrame::default();
    let a_audio_source_probe = Arc::new(SyntheticAudioProbe::default());
    let b_audio_source_probe = Arc::new(SyntheticAudioProbe::default());
    a_source.publish(test_frame(1, 64));
    b_source.publish(test_frame(1, 192));
    a_sender
        .start_sending_with_test_audio(
            a_source.clone(),
            Box::new(SyntheticAudioSource::new(Arc::clone(&a_audio_source_probe))),
        )
        .unwrap();
    b_sender
        .start_sending_with_test_audio(
            b_source.clone(),
            Box::new(SyntheticAudioSource::new(Arc::clone(&b_audio_source_probe))),
        )
        .unwrap();

    let deadline = Instant::now() + Duration::from_secs(30);
    let mut sequence = 1_u64;
    let mut next_frame = Instant::now() + FRAME_DURATION;
    let mut a_received = false;
    let mut b_received = false;
    let mut b_volume_changed_after_frames = None;
    while Instant::now() < deadline {
        pump_signals(&a_sender, &b_receiver, "A to B offer");
        pump_signals(&b_receiver, &a_sender, "B to A answer");
        pump_signals(&b_sender, &a_receiver, "B to A offer");
        pump_signals(&a_receiver, &b_sender, "A to B answer");

        a_received |= a_receiver.latest_remote_frame().is_some();
        b_received |= b_receiver.latest_remote_frame().is_some();
        let b_audio_frames = b_receive_audio_probe.decoded_frames.load(Ordering::Relaxed);
        if b_volume_changed_after_frames.is_none() && b_audio_frames > 0 {
            assert_eq!(
                f32::from_bits(
                    b_receive_audio_probe
                        .decoded_output_peak_max_bits
                        .load(Ordering::Relaxed)
                ),
                0.0,
                "0% deve silenciar a sessão antes da alteração ao vivo"
            );
            b_receiver.set_remote_audio_volume_percent(100);
            b_volume_changed_after_frames = Some(b_audio_frames);
        }
        if a_received && b_received {
            let a_send = a_sender.metrics();
            let b_send = b_sender.metrics();
            let a_receive = a_receiver.metrics();
            let b_receive = b_receiver.metrics();
            if a_send.sent_frames > 0
                && b_send.sent_frames > 0
                && a_receive.decoded_frames > 0
                && b_receive.decoded_frames > 0
                && a_receive.published_frames > 0
                && b_receive.published_frames > 0
                && a_receive_audio_probe.decoded_frames.load(Ordering::Relaxed) > 0
                && b_volume_changed_after_frames
                    .is_some_and(|baseline| b_audio_frames >= baseline.saturating_add(5))
                && f32::from_bits(
                    a_receive_audio_probe
                        .decoded_output_peak_max_bits
                        .load(Ordering::Relaxed),
                ) > 0.01
                && f32::from_bits(
                    b_receive_audio_probe
                        .decoded_output_peak_max_bits
                        .load(Ordering::Relaxed),
                ) > 0.01
            {
                break;
            }
        }

        if Instant::now() >= next_frame {
            sequence = sequence.saturating_add(1);
            a_source.publish(test_frame(sequence, (sequence % 255) as u8));
            b_source.publish(test_frame(sequence, (255 - sequence % 255) as u8));
            next_frame += FRAME_DURATION;
        }
        thread::sleep(Duration::from_millis(5));
    }

    let a_send = a_sender.metrics();
    let b_send = b_sender.metrics();
    let a_receive = a_receiver.metrics();
    let b_receive = b_receiver.metrics();
    let a_volume_percent = a_receiver.remote_audio_volume_percent();
    let b_volume_percent = b_receiver.remote_audio_volume_percent();
    a_receiver.stop();
    b_receiver.stop();
    a_sender.stop();
    b_sender.stop();

    assert!(a_received, "A deve receber vídeo de B: {a_receive:?}");
    assert!(b_received, "B deve receber vídeo de A: {b_receive:?}");
    assert!(a_send.sent_frames > 0 && b_receive.received_packets > 0);
    assert!(b_send.sent_frames > 0 && a_receive.received_packets > 0);
    assert!(a_receive.decoded_frames > 0 && b_receive.decoded_frames > 0);
    assert!(a_receive.published_frames > 0 && b_receive.published_frames > 0);
    assert!(
        a_audio_source_probe
            .source_callbacks
            .load(Ordering::Relaxed)
            > 0
    );
    assert!(
        b_audio_source_probe
            .source_callbacks
            .load(Ordering::Relaxed)
            > 0
    );
    assert!(
        a_receive_audio_probe
            .playback_factory_starts
            .load(Ordering::Relaxed)
            > 0
    );
    assert!(
        b_receive_audio_probe
            .playback_factory_starts
            .load(Ordering::Relaxed)
            > 0
    );
    assert!(a_receive_audio_probe.decoded_frames.load(Ordering::Relaxed) > 0);
    assert!(b_receive_audio_probe.decoded_frames.load(Ordering::Relaxed) > 0);
    assert_eq!(a_volume_percent, 50);
    assert_eq!(b_volume_percent, 100);
    assert!(
        a_receive_audio_probe
            .decoded_audible_samples
            .load(Ordering::Relaxed)
            > 0
    );
    assert!(
        b_receive_audio_probe
            .decoded_audible_samples
            .load(Ordering::Relaxed)
            > 0
    );
    assert!(
        f32::from_bits(
            a_receive_audio_probe
                .decoded_output_peak_max_bits
                .load(Ordering::Relaxed)
        ) > 0.01
    );
    assert!(
        f32::from_bits(
            b_receive_audio_probe
                .decoded_output_peak_max_bits
                .load(Ordering::Relaxed)
        ) > 0.01
    );
    assert_eq!(a_send.track_ssrc, b_receive.track_ssrc);
    assert_eq!(b_send.track_ssrc, a_receive.track_ssrc);
}

#[test]
fn replacing_one_group_video_session_keeps_the_other_direction_active() {
    let context = egui::Context::default();
    let a_sender = ScreenShareSession::new_loopback(context.clone()).unwrap();
    let b_receiver = ScreenShareSession::new_loopback_with_decoder_preference(
        context.clone(),
        VideoDecoderPreference::Cpu,
    )
    .unwrap();
    let b_sender = ScreenShareSession::new_loopback(context.clone()).unwrap();
    let a_receiver = ScreenShareSession::new_loopback_with_decoder_preference(
        context.clone(),
        VideoDecoderPreference::Cpu,
    )
    .unwrap();
    let a_source = LatestFrame::default();
    let b_source = LatestFrame::default();
    a_source.publish(test_frame(1, 72));
    b_source.publish(test_frame(1, 184));
    a_sender.start_sending(a_source.clone()).unwrap();
    b_sender.start_sending(b_source.clone()).unwrap();

    let initial_deadline = Instant::now() + Duration::from_secs(20);
    let mut sequence = 1_u64;
    let mut next_frame = Instant::now() + FRAME_DURATION;
    while Instant::now() < initial_deadline {
        pump_signals(&a_sender, &b_receiver, "A to B initial offer");
        pump_signals(&b_receiver, &a_sender, "B to A initial answer");
        pump_signals(&b_sender, &a_receiver, "B to A initial offer");
        pump_signals(&a_receiver, &b_sender, "A to B initial answer");
        if b_receiver.metrics().decoded_frames > 0 && a_receiver.metrics().decoded_frames > 0 {
            break;
        }
        if Instant::now() >= next_frame {
            sequence = sequence.saturating_add(1);
            a_source.publish(test_frame(sequence, (sequence % 255) as u8));
            b_source.publish(test_frame(sequence, (255 - sequence % 255) as u8));
            next_frame += FRAME_DURATION;
        }
        thread::sleep(Duration::from_millis(5));
    }
    assert!(b_receiver.metrics().decoded_frames > 0);
    let a_to_b_before = b_receiver.metrics().decoded_frames;
    let b_to_a_before = a_receiver.metrics().decoded_frames;
    assert!(a_to_b_before > 0);

    // Recreate only A -> B, as the group sender does when its stream is replaced.
    a_sender.stop();
    b_receiver.stop();
    let replacement_sender = ScreenShareSession::new_loopback(context.clone()).unwrap();
    let replacement_receiver = ScreenShareSession::new_loopback_with_decoder_preference(
        context,
        VideoDecoderPreference::Cpu,
    )
    .unwrap();
    replacement_sender.start_sending(a_source.clone()).unwrap();

    let replacement_deadline = Instant::now() + Duration::from_secs(20);
    let mut replacement_next_frame = Instant::now() + FRAME_DURATION;
    while Instant::now() < replacement_deadline {
        pump_signals(
            &replacement_sender,
            &replacement_receiver,
            "A to B replacement offer",
        );
        pump_signals(
            &replacement_receiver,
            &replacement_sender,
            "B to A replacement answer",
        );
        pump_signals(&b_sender, &a_receiver, "B to A preserved offer");
        pump_signals(&a_receiver, &b_sender, "A to B preserved answer");

        let replaced_direction_decoded = replacement_receiver.metrics().decoded_frames > 0;
        let preserved_direction_decoded = a_receiver.metrics().decoded_frames > b_to_a_before;
        if replaced_direction_decoded && preserved_direction_decoded {
            break;
        }
        if Instant::now() >= replacement_next_frame {
            sequence = sequence.saturating_add(1);
            a_source.publish(test_frame(sequence, (sequence % 255) as u8));
            b_source.publish(test_frame(sequence, (255 - sequence % 255) as u8));
            replacement_next_frame += FRAME_DURATION;
        }
        thread::sleep(Duration::from_millis(5));
    }

    let replacement_metrics = replacement_sender.metrics();
    let replacement_received = replacement_receiver.metrics();
    let preserved_sender_metrics = b_sender.metrics();
    let preserved_receiver_metrics = a_receiver.metrics();
    replacement_receiver.stop();
    replacement_sender.stop();
    a_receiver.stop();
    b_sender.stop();

    assert!(replacement_metrics.sent_frames > 0);
    assert!(replacement_received.decoded_frames > 0);
    assert!(replacement_received.published_frames > 0);
    assert_eq!(
        replacement_metrics.track_ssrc,
        replacement_received.track_ssrc
    );
    assert!(preserved_sender_metrics.sent_frames > 0);
    assert!(preserved_receiver_metrics.decoded_frames > b_to_a_before);
}

#[test]
fn loopback_webrtc_requests_pli_and_recovers_with_another_decodable_idr() {
    let context = egui::Context::default();
    let sender = ScreenShareSession::new_loopback(context.clone()).unwrap();
    let receiver = ScreenShareSession::new_loopback_with_decoder_preference(
        context,
        VideoDecoderPreference::Cpu,
    )
    .unwrap();
    let source = LatestFrame::default();
    source.publish(PreviewFrame {
        sequence: 1,
        width: 320,
        height: 240,
        rgba: vec![128; 320 * 240 * 4],
        #[cfg(windows)]
        gpu_nv12: None,
        #[cfg(windows)]
        cpu_nv12: None,
    });
    sender.start_sending(source.clone()).unwrap();

    let deadline = Instant::now() + Duration::from_secs(30);
    let mut sequence = 1;
    let mut next_frame = Instant::now() + FRAME_DURATION;
    let mut received_frame = None;
    let mut pli_requested = false;
    let mut idr_frames_before_pli = 0;
    while Instant::now() < deadline {
        for event in std::iter::from_fn(|| sender.try_recv()) {
            match event {
                ScreenShareEvent::Signal { kind, payload } => {
                    receiver.handle_signal(kind, payload).unwrap();
                }
                ScreenShareEvent::Error(error) => panic!("sender session failed: {error}"),
                ScreenShareEvent::AudioError(error) => {
                    panic!("sender audio session failed: {error}")
                }
                ScreenShareEvent::ConnectionClosed => {
                    panic!("sender connection closed before receiving a frame")
                }
                ScreenShareEvent::State(_) => {}
                ScreenShareEvent::AudioState(_) => {}
            }
        }
        for event in std::iter::from_fn(|| receiver.try_recv()) {
            match event {
                ScreenShareEvent::Signal { kind, payload } => {
                    sender.handle_signal(kind, payload).unwrap();
                }
                ScreenShareEvent::Error(error) => panic!("receiver session failed: {error}"),
                ScreenShareEvent::AudioError(error) => {
                    panic!("receiver audio session failed: {error}")
                }
                ScreenShareEvent::ConnectionClosed => {
                    panic!("receiver connection closed before receiving a frame")
                }
                ScreenShareEvent::State(_) => {}
                ScreenShareEvent::AudioState(_) => {}
            }
        }

        if let Some(frame) = receiver.latest_remote_frame() {
            received_frame = Some((frame.width, frame.height));
        }
        let sender_metrics = sender.metrics();
        let receiver_metrics = receiver.metrics();
        if !pli_requested
            && sender_metrics.sent_idr_frames >= 1
            && receiver_metrics.decoded_delta_frames >= 1
        {
            idr_frames_before_pli = sender_metrics.sent_idr_frames;
            receiver.request_keyframe_for_test().unwrap();
            pli_requested = true;
        }
        if pli_requested
            && sender_metrics.pli_requests_received >= 1
            && sender_metrics.sent_idr_frames > idr_frames_before_pli
            && receiver_metrics.decoded_idr_frames >= 2
            && receiver_metrics.decoded_delta_frames >= 1
        {
            break;
        }
        if Instant::now() >= next_frame {
            sequence += 1;
            source.publish(PreviewFrame {
                sequence,
                width: 320,
                height: 240,
                rgba: vec![(sequence % 255) as u8; 320 * 240 * 4],
                #[cfg(windows)]
                gpu_nv12: None,
                #[cfg(windows)]
                cpu_nv12: None,
            });
            next_frame += FRAME_DURATION;
        }
        thread::sleep(Duration::from_millis(5));
    }

    let sender_metrics = sender.metrics();
    let receiver_metrics = receiver.metrics();
    receiver.stop();
    sender.stop();
    assert_eq!(
        received_frame,
        Some((320, 240)),
        "sender metrics: {:?}; receiver metrics: {:?}",
        sender_metrics,
        receiver_metrics
    );
    assert!(
        sender_metrics.sent_idr_frames >= 1,
        "loopback deve enviar o IDR inicial; sender metrics: {:?}",
        sender_metrics
    );
    assert!(
        pli_requested && sender_metrics.pli_requests_received >= 1,
        "loopback deve entregar o PLI ao emissor; sender metrics: {:?}",
        sender_metrics
    );
    assert!(
        sender_metrics.sent_idr_frames > idr_frames_before_pli,
        "o emissor deve enviar outro IDR depois do PLI; sender metrics: {:?}",
        sender_metrics
    );
    assert!(
        receiver_metrics.decoded_idr_frames >= 2,
        "o receptor deve decodificar o IDR de recuperação; receiver metrics: {:?}",
        receiver_metrics
    );
    assert!(
        receiver_metrics.pli_requests_sent >= 1,
        "a recuperação deve registrar PLI, ressincronização e tempo até o IDR: {:?}",
        receiver_metrics
    );
    assert!(
        sender_metrics.sent_delta_frames >= 1,
        "loopback deve enviar quadros P; sender metrics: {:?}",
        sender_metrics
    );
    assert!(
        receiver_metrics.decoded_delta_frames >= 1,
        "loopback deve decodificar pelo menos um quadro P; receiver metrics: {:?}",
        receiver_metrics
    );
    assert!(
        receiver_metrics.decoded_frames >= 2,
        "loopback deve decodificar o IDR e pelo menos um quadro P; receiver metrics: {:?}",
        receiver_metrics
    );
    assert!(
        receiver_metrics.decode_errors <= receiver_metrics.keyframe_resyncs,
        "H.264 errors must trigger resync; receiver metrics: {:?}",
        receiver_metrics
    );
}

#[test]
fn loopback_controlled_rtp_loss_correlates_nack_and_retransmits_the_missing_packet() {
    let context = egui::Context::default();
    let sender = ScreenShareSession::new_loopback(context.clone()).unwrap();
    let receiver = ScreenShareSession::new_loopback_with_decoder_preference(
        context,
        VideoDecoderPreference::Cpu,
    )
    .unwrap();
    let source = LatestFrame::default();
    source.publish(test_frame(1, 64));
    sender.start_sending(source.clone()).unwrap();

    let deadline = Instant::now() + Duration::from_secs(30);
    let mut sequence = 1_u64;
    let mut next_frame = Instant::now() + FRAME_DURATION;
    let mut loss_requested = false;
    let mut nack_stats_correlated = false;
    while Instant::now() < deadline {
        pump_signals(&sender, &receiver, "loss-test offer");
        pump_signals(&receiver, &sender, "loss-test answer");

        if !loss_requested && receiver.metrics().decoded_frames >= 2 {
            sender.drop_next_outbound_rtp_for_test();
            loss_requested = true;
        }
        let sender_metrics = sender.metrics();
        let receiver_metrics = receiver.metrics();
        let injected = sender
            .metrics
            .test_dropped_outbound_rtp
            .load(Ordering::Relaxed)
            > 0;
        // RTP/RTCP recovery and ICE statistics arrive independently. Wait for both
        // snapshots before asserting the route; a NACK alone does not make them ready.
        let route_stats_ready = sender_metrics.route == Some(MediaRoute::Direct)
            && receiver_metrics.route == Some(MediaRoute::Direct)
            && sender_metrics.selected_ice_pair.contains("127.0.0.1")
            && !sender_metrics.selected_ice_pair.contains("indisponível");
        if injected
            && sender
                .metrics
                .test_retransmitted_outbound_rtp
                .load(Ordering::Relaxed)
                > 0
            && receiver_metrics
                .nack_packets_sent
                .is_some_and(|count| count > 0)
            && sender_metrics.nack_packets_received_observed > 0
            && sender_metrics.original_rtp_repeats_sent > 0
            && receiver_metrics.nack_packets_sent
                == Some(sender_metrics.nack_packets_received_observed)
            && route_stats_ready
        {
            nack_stats_correlated = true;
            break;
        }

        if Instant::now() >= next_frame {
            sequence = sequence.saturating_add(1);
            source.publish(test_frame(sequence, (sequence % 251) as u8));
            next_frame += FRAME_DURATION;
        }
        thread::sleep(Duration::from_millis(5));
    }

    let sender_metrics = sender.metrics();
    let receiver_metrics = receiver.metrics();
    let injected_packets = sender
        .metrics
        .test_dropped_outbound_rtp
        .load(Ordering::Relaxed);
    let retransmitted_packets = sender
        .metrics
        .test_retransmitted_outbound_rtp
        .load(Ordering::Relaxed);
    receiver.stop();
    sender.stop();

    assert!(
        loss_requested,
        "the test must wait for decoded video before loss injection"
    );
    assert_eq!(
        injected_packets, 1,
        "exactly one video RTP packet is dropped"
    );
    assert!(
        retransmitted_packets > 0,
        "NACK must retransmit the packet dropped after the send cache"
    );
    assert!(
        nack_stats_correlated,
        "the receiver's NACK count should match packets observed in the sender's inbound RTCP interceptor; sender={sender_metrics:?}, receiver={receiver_metrics:?}"
    );
    assert!(sender_metrics.nack_packets_received.is_some());
    assert_eq!(sender_metrics.route, Some(MediaRoute::Direct));
    assert_eq!(receiver_metrics.route, Some(MediaRoute::Direct));
    assert!(sender_metrics.selected_ice_pair.contains("127.0.0.1"));
    assert!(!sender_metrics.selected_ice_pair.contains("indisponível"));
    assert!(
        sender_metrics.original_rtp_repeats_sent > 0,
        "original RTP retransmission must be measured separately from RTX"
    );
    // The RTC exposes retransmission counters in this build, so zero is a known result rather
    // than missing data. This diagnostic test measures NACK delivery; it does not change or
    // assume the negotiated retransmission behavior.
    assert!(sender_metrics.retransmitted_packets_sent.is_some());
    assert!(receiver_metrics.retransmitted_packets_received.is_some());
}

#[test]
fn deterministic_rtp_loss_injector_matches_configured_percentages() {
    for loss_percent in [0, 1, 4, 8, 100, u8::MAX] {
        let expected = u64::from(loss_percent.min(100));
        let dropped = (1..=100)
            .filter(|packet_index| deterministic_rtp_loss_at(*packet_index, loss_percent))
            .count() as u64;
        assert_eq!(dropped, expected, "loss rate {loss_percent}%");
    }
    assert!(!deterministic_rtp_loss_at(0, 100));
}

#[test]
#[ignore = "diagnóstico manual: executa 16 janelas WebRTC de 10 segundos, exige GPU e grava JSON em target/diagnostics"]
fn screen_share_loopback_diagnostic() {
    let created_at_unix_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let quality_profiles = [
        ScreenShareQuality::default(),
        ScreenShareQuality {
            resolution: ScreenShareResolution::P720,
            frame_rate: ScreenShareFrameRate::Fps60,
        },
        ScreenShareQuality {
            resolution: ScreenShareResolution::P1080,
            frame_rate: ScreenShareFrameRate::Fps30,
        },
        ScreenShareQuality {
            resolution: ScreenShareResolution::P1080,
            frame_rate: ScreenShareFrameRate::Fps60,
        },
    ];
    let scenarios = quality_profiles
        .into_iter()
        .flat_map(|quality| {
            [0_u8, 1, 4, 8]
                .into_iter()
                .map(move |loss_percent| run_loopback_diagnostic_scenario(quality, loss_percent))
        })
        .collect::<Vec<_>>();
    let report = serde_json::json!({
        "schema_version": 1,
        "created_at_unix_ms": created_at_unix_ms,
        "build_profile": if cfg!(debug_assertions) { "debug" } else { "release" },
        "source": "synthetic_d3d11_nv12_gpu_surface",
        "quality_profiles": ["720p30", "720p60", "1080p30", "1080p60"],
        "rtp_loss_profiles_percent": [0, 1, 4, 8],
        "measurement_seconds_per_scenario": 10,
        "scenarios": scenarios,
        "limitations": [
            "sender and receiver run on the same machine",
            "desktop capture WGC/DXGI is not measured",
            "application UI texture upload and drawing are not measured",
            "timings are wall-clock call durations and not exclusive GPU execution time",
            "synthetic input reuses a GPU NV12 surface; desktop capture and changing source pixels are not measured",
        ],
    });
    let report_bytes = serde_json::to_vec_pretty(&report).unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&report_bytes).unwrap();
    assert_eq!(parsed["scenarios"].as_array().unwrap().len(), 16);

    let report_directory =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/diagnostics");
    std::fs::create_dir_all(&report_directory).unwrap();
    let report_path = report_directory.join(format!(
        "screen-share-loopback-gpu-{}-{}.json",
        created_at_unix_ms,
        std::process::id()
    ));
    std::fs::write(&report_path, report_bytes).unwrap();
    println!("Relatório de diagnóstico: {}", report_path.display());
}

#[cfg(windows)]
struct LiveCaptureStimulus {
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

#[cfg(windows)]
impl LiveCaptureStimulus {
    fn start() -> Result<Self, String> {
        struct StimulusApp {
            stop: Arc<AtomicBool>,
            ready: Option<std_mpsc::SyncSender<()>>,
            started_at: Instant,
        }

        impl eframe::App for StimulusApp {
            fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
                if let Some(ready) = self.ready.take() {
                    let _ = ready.send(());
                }
                let bounds = ui.max_rect();
                let painter = ui.painter();
                let elapsed = self.started_at.elapsed().as_secs_f32();
                let frame_index = (elapsed * 60.0) as usize;
                let background = (frame_index.wrapping_mul(13) % 128) as u8;
                painter.rect_filled(
                    bounds,
                    0.0,
                    egui::Color32::from_rgb(
                        8 + background / 4,
                        12 + background / 3,
                        32 + background / 2,
                    ),
                );
                let tile_width = bounds.width() / 8.0;
                let tile_height = bounds.height() / 4.0;
                for tile in 0..32_usize {
                    let column = tile % 8;
                    let row = tile / 8;
                    let value = ((tile * 37 + frame_index * 11) % 240) as u8;
                    painter.rect_filled(
                        egui::Rect::from_min_size(
                            egui::pos2(
                                bounds.left() + column as f32 * tile_width,
                                bounds.top() + row as f32 * tile_height,
                            ),
                            egui::vec2(tile_width - 2.0, tile_height - 2.0),
                        ),
                        0.0,
                        egui::Color32::from_rgb(value, value.wrapping_mul(3), 255 - value),
                    );
                }
                let band_width = (bounds.width() / 12.0).max(1.0);
                for index in 0..12 {
                    let phase = (elapsed * (0.7 + index as f32 * 0.09) + index as f32 * 0.43).sin();
                    let height = bounds.height() * (0.18 + 0.36 * (phase + 1.0) * 0.5);
                    let x = bounds.left() + band_width * index as f32;
                    let hue = ((index * 31 + elapsed as usize * 17) % 255) as u8;
                    painter.rect_filled(
                        egui::Rect::from_min_size(
                            egui::pos2(x, bounds.bottom() - height),
                            egui::vec2(band_width - 2.0, height),
                        ),
                        0.0,
                        egui::Color32::from_rgb(hue, 220_u8.saturating_sub(hue / 2), 96),
                    );
                }
                let x = bounds.left() + (elapsed * 170.0) % bounds.width().max(1.0);
                painter.circle_filled(
                    egui::pos2(x, bounds.center().y),
                    44.0,
                    egui::Color32::from_rgb(255, 240, 80),
                );
                ui.ctx().request_repaint_after(Duration::from_millis(16));
                if self.stop.load(Ordering::Relaxed) {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
        }

        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let (ready_tx, ready_rx) = std_mpsc::sync_channel(1);
        let (error_tx, error_rx) = std_mpsc::sync_channel(1);
        let window_thread = thread::Builder::new()
            .name("screen-share-live-diagnostic-stimulus".to_owned())
            .spawn(move || {
                let result = eframe::run_native(
                    "P2P Screen Share Diagnostic — moving GPU stimulus",
                    eframe::NativeOptions {
                        renderer: eframe::Renderer::Glow,
                        event_loop_builder: Some(Box::new(|builder| {
                            use winit::platform::windows::EventLoopBuilderExtWindows;
                            builder.with_any_thread(true);
                        })),
                        viewport: egui::ViewportBuilder::default()
                            .with_inner_size([960.0, 540.0])
                            .with_position([64.0, 64.0])
                            .with_resizable(false)
                            .with_always_on_top(),
                        ..Default::default()
                    },
                    Box::new(move |_| {
                        Ok(Box::new(StimulusApp {
                            stop: thread_stop,
                            ready: Some(ready_tx),
                            started_at: Instant::now(),
                        }))
                    }),
                );
                if let Err(error) = result {
                    let _ = error_tx.send(error.to_string());
                }
            })
            .map_err(|error| format!("could not start moving stimulus window: {error}"))?;
        match ready_rx.recv_timeout(Duration::from_secs(10)) {
            Ok(()) => Ok(Self {
                stop,
                thread: Some(window_thread),
            }),
            Err(_) => {
                let reason = error_rx
                    .try_recv()
                    .unwrap_or_else(|_| "stimulus window did not become ready".to_owned());
                stop.store(true, Ordering::Relaxed);
                drop(window_thread);
                Err(reason)
            }
        }
    }
}

#[cfg(windows)]
impl Drop for LiveCaptureStimulus {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let deadline = Instant::now() + Duration::from_secs(3);
            while !thread.is_finished() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(20));
            }
            if thread.is_finished() {
                let _ = thread.join();
            } else {
                // The viewport close is processed on the event loop. Don't hang test cleanup
                // indefinitely if a display driver wedges its window thread.
                let _ = thread;
            }
        }
    }
}

#[cfg(windows)]
fn run_live_capture_diagnostic_scenario(
    quality: ScreenShareQuality,
    monitor_device_id: &str,
) -> serde_json::Value {
    use crate::screen_capture::ScreenCapture;

    const MEASURE_FOR: Duration = Duration::from_secs(10);
    const SETUP_TIMEOUT: Duration = Duration::from_secs(15);
    let context = egui::Context::default();
    let mut capture = match ScreenCapture::start_monitor(
        monitor_device_id,
        context.clone(),
        Arc::new(AtomicBool::new(true)),
        quality,
    ) {
        Ok(capture) => capture,
        Err(error) => {
            return serde_json::json!({"quality": quality, "status": "capture_unavailable", "error": error});
        }
    };
    let source = capture.frame_source();
    let setup_deadline = Instant::now() + SETUP_TIMEOUT;
    while source.latest().is_none() && Instant::now() < setup_deadline {
        if let Some(Err(error)) = capture.poll_finished() {
            let _ = capture.stop();
            return serde_json::json!({"quality": quality, "status": "capture_failed", "error": error});
        }
        thread::sleep(Duration::from_millis(20));
    }
    let Some(first_frame) = source.latest() else {
        let error = capture
            .fallback_reason()
            .unwrap_or_else(|| "DXGI não publicou um quadro inicial".to_owned());
        let _ = capture.stop();
        return serde_json::json!({"quality": quality, "status": "capture_timeout", "error": error});
    };
    let input_gpu = first_frame.gpu_nv12.is_some();
    let input_cpu_nv12 = first_frame.cpu_nv12.is_some();
    drop(first_frame);
    capture.take_performance_snapshot();

    let sender = match ScreenShareSession::new_loopback(context.clone()) {
        Ok(sender) => sender,
        Err(error) => {
            let _ = capture.stop();
            return serde_json::json!({"quality": quality, "status": "sender_setup_failed", "error": error});
        }
    };
    let receiver = match ScreenShareSession::new_loopback_with_decoder_preference(
        context,
        VideoDecoderPreference::PreferDxva,
    ) {
        Ok(receiver) => receiver,
        Err(error) => {
            sender.stop();
            let _ = capture.stop();
            return serde_json::json!({"quality": quality, "status": "receiver_setup_failed", "error": error});
        }
    };
    if let Err(error) = sender.start_sending_with_quality_and_options_and_exclusion(
        source,
        quality,
        quality.individual_bitrate_bps(),
        false,
        AudioExclusionConfig::default(),
    ) {
        receiver.stop();
        sender.stop();
        let _ = capture.stop();
        return serde_json::json!({"quality": quality, "status": "sender_start_failed", "error": error});
    }

    let mut errors = Vec::new();
    let connect_deadline = Instant::now() + SETUP_TIMEOUT;
    let mut connected_at = None;
    while Instant::now() < connect_deadline {
        pump_diagnostic_signals(&sender, &receiver, "sender", &mut errors);
        pump_diagnostic_signals(&receiver, &sender, "receiver", &mut errors);
        if sender.metrics().p2p_connected && receiver.metrics().p2p_connected {
            connected_at = Some(Instant::now());
            break;
        }
        thread::sleep(Duration::from_millis(5));
    }
    let duration = if let Some(started) = connected_at {
        sender.take_performance_snapshot();
        receiver.take_performance_snapshot();
        let deadline = started + MEASURE_FOR;
        println!(
            "Perfil {}{}: janela animada de carga na GPU por 10 segundos.",
            quality.resolution.label(),
            quality.frames_per_second()
        );
        while Instant::now() < deadline {
            pump_diagnostic_signals(&sender, &receiver, "sender", &mut errors);
            pump_diagnostic_signals(&receiver, &sender, "receiver", &mut errors);
            if let Some(Err(error)) = capture.poll_finished() {
                errors.push(format!("capture: {error}"));
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
        started.elapsed().min(MEASURE_FOR)
    } else {
        errors.push("loopback connection timed out".to_owned());
        Duration::ZERO
    };

    pump_diagnostic_signals(&sender, &receiver, "sender", &mut errors);
    pump_diagnostic_signals(&receiver, &sender, "receiver", &mut errors);
    let sender_performance = sender.take_performance_snapshot();
    let receiver_performance = receiver.take_performance_snapshot();
    let capture_performance = capture.take_performance_snapshot();
    let sender_metrics = sender.metrics();
    let receiver_metrics = receiver.metrics();
    let capture_gpu = capture_performance
        .pipeline
        .path_count(crate::video_performance::FramePath::CaptureGpuNv12);
    let encoder_gpu = sender_performance
        .video_performance
        .path_count(crate::video_performance::FramePath::EncoderGpuSurface);
    let decoder_gpu = receiver_performance
        .video_performance
        .path_count(crate::video_performance::FramePath::DecoderDxva);
    let gpu_valid = input_gpu && capture_gpu > 0 && encoder_gpu > 0 && decoder_gpu > 0;
    let seconds = duration.as_secs_f64();
    let report = serde_json::json!({
        "scenario": {
            "quality": format!("{}{}", quality.resolution.label(), quality.frames_per_second()),
            "status": if connected_at.is_none() { "connection_timeout" } else if gpu_valid { "ok_live_capture_gpu" } else { "gpu_fallback_or_unverified" },
            "measurement_seconds": seconds,
            "capture_backend": capture.backend_name(),
            "capture_fallback_reason": capture.fallback_reason(),
            "input_gpu_surface": input_gpu,
            "input_cpu_nv12_copy": input_cpu_nv12,
            "encoder_backend": sender_metrics.encoder_backend,
            "encoder_fallback_reason": sender_metrics.encoder_fallback_reason,
            "decoder_backend": receiver_metrics.decoder_backend,
            "decoder_fallback_reason": receiver_metrics.decoder_fallback_reason,
            "gpu_paths": {"capture_nv12": capture_gpu, "encoder_surface": encoder_gpu, "decoder_dxva": decoder_gpu},
            "hardware_gpu_valid": gpu_valid,
        },
        "capture": {
            "received_frames": capture_performance.received_frames,
            "processed_frames": capture_performance.processed_frames,
            "unchanged_frames": capture_performance.unchanged_frames,
            "skipped_frames": capture_performance.skipped_frames,
            "gpu_convert_total_ms": capture_performance.gpu_convert_nanos as f64 / 1_000_000.0,
            "readback_total_ms": capture_performance.readback_nanos as f64 / 1_000_000.0,
            "resize_total_ms": capture_performance.resize_nanos as f64 / 1_000_000.0,
            "timings": capture_performance.pipeline.diagnostic_json(),
        },
        "sender": diagnostic_peer_json("sender", sender_metrics, sender_performance, seconds),
        "receiver": diagnostic_peer_json("receiver", receiver_metrics, receiver_performance, seconds),
        "errors": errors,
    });
    receiver.stop();
    sender.stop();
    let _ = capture.stop();
    report
}

#[test]
#[cfg(windows)]
#[ignore = "diagnóstico manual: usa captura real DXGI do monitor principal; abre uma janela animada e captura o monitor real"]
fn screen_share_live_capture_loopback_diagnostic() {
    use crate::screen_capture::ScreenCapture;

    let created_at_unix_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let (stimulus, stimulus_status) = match LiveCaptureStimulus::start() {
        Ok(stimulus) => (Some(stimulus), "visible_motion_window_started".to_owned()),
        Err(error) => (None, format!("stimulus_unavailable: {error}")),
    };
    let profiles = [
        ScreenShareQuality::default(),
        ScreenShareQuality {
            resolution: ScreenShareResolution::P720,
            frame_rate: ScreenShareFrameRate::Fps60,
        },
        ScreenShareQuality {
            resolution: ScreenShareResolution::P1080,
            frame_rate: ScreenShareFrameRate::Fps30,
        },
        ScreenShareQuality {
            resolution: ScreenShareResolution::P1080,
            frame_rate: ScreenShareFrameRate::Fps60,
        },
    ];
    let setup = match ScreenCapture::monitors() {
        Ok(monitors) => match monitors
            .iter()
            .find(|m| m.is_primary)
            .or_else(|| monitors.first())
        {
            Some(monitor) => serde_json::json!({
                "monitor_dimensions": [monitor.width, monitor.height],
                "scenarios": profiles.into_iter()
                    .map(|quality| run_live_capture_diagnostic_scenario(quality, &monitor.device_id))
                    .collect::<Vec<_>>(),
            }),
            None => {
                serde_json::json!({"setup_error": "nenhum monitor disponível", "scenarios": []})
            }
        },
        Err(error) => serde_json::json!({"setup_error": error, "scenarios": []}),
    };
    let report = serde_json::json!({
        "schema_version": 1,
        "created_at_unix_ms": created_at_unix_ms,
        "build_profile": if cfg!(debug_assertions) { "debug" } else { "release" },
        "source": "live_primary_monitor_dxgi_desktop_duplication",
        "transport": "two_local_webrtc_peers_over_loopback",
        "visual_stimulus": stimulus_status,
        "measurement_seconds_per_scenario": 10,
        "quality_profiles": ["720p30", "720p60", "1080p30", "1080p60"],
        "setup": setup,
        "privacy": "pixels are streamed only to the local loopback peer and are not stored in this report",
        "limitations": [
            "requires an unlocked interactive Windows desktop and DXGI access",
            "the animated window supplies visual motion during each measurement",
            "does not exercise signaling between separate PCs",
            "GPU utilization and physical display scanout are not measured",
        ],
    });
    let bytes = serde_json::to_vec_pretty(&report).unwrap();
    let _: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let directory =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/diagnostics");
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join(format!(
        "screen-share-live-capture-{}-{}.json",
        created_at_unix_ms,
        std::process::id()
    ));
    std::fs::write(&path, bytes).unwrap();
    println!("Live capture diagnostic: {}", path.display());
    drop(stimulus);
}

#[test]
fn rtp_jitter_ticks_are_converted_to_milliseconds() {
    assert!((rtp_jitter_ticks_to_ms(317.0, 90_000.0) - 3.522_222_2).abs() < 0.001);
    assert_eq!(rtp_jitter_ticks_to_ms(10.0, 0.0), 0.0);
}

#[test]
fn stun_uri_accepts_one_stun_server_and_rejects_turn_or_invalid_values() {
    assert_eq!(
        validate_stun_uri("stun:stun.l.google.com:19302").unwrap(),
        "stun:stun.l.google.com:19302"
    );
    assert!(validate_stun_uri("turn:relay.example:3478").is_err());
    assert!(validate_stun_uri("stun:one.example:3478,stun:two.example:3478").is_err());
    assert!(validate_stun_uri("not-a-stun-uri").is_err());
}

#[test]
fn selected_media_route_diagnostics_distinguish_direct_turn_and_missing_stats() {
    assert_eq!(
        media_route_from_candidate_types(
            Some(RTCIceCandidateType::Host),
            Some(RTCIceCandidateType::Host)
        ),
        Some(MediaRoute::Direct)
    );
    assert_eq!(
        media_route_from_candidate_types(
            Some(RTCIceCandidateType::Relay),
            Some(RTCIceCandidateType::Srflx)
        ),
        Some(MediaRoute::Turn)
    );
    assert_eq!(media_route_from_candidate_types(None, None), None);
    assert_eq!(media_route_label(Some(MediaRoute::Direct)), "Direto (P2P)");
    assert_eq!(
        media_route_label(Some(MediaRoute::Turn)),
        "Retransmitido (TURN)"
    );
    assert_eq!(media_route_label(None), "desconhecido");

    let unavailable = peer_stats_snapshot(&RTCStatsReport::default());
    assert_eq!(unavailable.route, None);
    assert!(unavailable.selected_pair_summary.contains("desconhecido"));
}

#[test]
fn buffered_system_audio_source_drains_burst_backlog_without_queue_drops() {
    struct BurstBufferedAudioSource {
        consumer: rtrb::Consumer<f32>,
        reads: Arc<AtomicU64>,
    }

    impl AudioSampleSource for BurstBufferedAudioSource {
        fn input_format(&self) -> (u32, usize) {
            (OPUS_SAMPLE_RATE, OPUS_CHANNELS)
        }

        fn read_samples(&mut self, output: &mut [f32]) -> usize {
            self.reads.fetch_add(1, Ordering::Relaxed);
            let mut count = 0;
            while count < output.len() {
                match self.consumer.pop() {
                    Ok(sample) => {
                        output[count] = sample;
                        count += 1;
                    }
                    Err(_) => break,
                }
            }
            count
        }

        fn available_samples(&self) -> Option<usize> {
            Some(self.consumer.slots())
        }

        fn callbacks(&self) -> u64 {
            1
        }
        fn input_frames(&self) -> u64 {
            0
        }
        fn non_silent_samples(&self) -> u64 {
            0
        }
        fn captured_frames(&self) -> u64 {
            0
        }
        fn dropped_frames(&self) -> u64 {
            0
        }
        fn xruns(&self) -> u64 {
            0
        }
        fn device_changes(&self) -> u64 {
            0
        }
        fn realtime_denied(&self) -> u64 {
            0
        }
        fn take_error(&self) -> Option<(Option<cpal::ErrorKind>, String)> {
            None
        }
    }

    let frame_len = OPUS_FRAME_SAMPLES_PER_CHANNEL * OPUS_CHANNELS;
    let burst_frames = 5_usize;
    let (mut producer, consumer) = rtrb::RingBuffer::<f32>::new(frame_len * 8);
    for i in 0..(frame_len * burst_frames) {
        let phase = TAU * 440.0 * (i / OPUS_CHANNELS) as f32 / OPUS_SAMPLE_RATE as f32;
        producer.push(phase.sin() * 0.25).unwrap();
    }

    let reads = Arc::new(AtomicU64::new(0));
    let mut source = BurstBufferedAudioSource {
        consumer,
        reads: Arc::clone(&reads),
    };
    assert_eq!(source.available_samples(), Some(frame_len * burst_frames));

    // Simulate the drain loop inside send_system_audio for a single 20ms tick.
    let mut pcm = vec![0.0_f32; frame_len];
    let mut filled = 0_usize;
    let mut drained_frames = 0_usize;
    while drained_frames < 8 {
        if drained_frames > 0
            && !source
                .available_samples()
                .is_some_and(|avail| filled + avail >= frame_len)
        {
            break;
        }
        filled += source.read_samples(&mut pcm[filled..]);
        if filled < frame_len {
            break;
        }
        drained_frames += 1;
        filled = 0;
    }

    assert_eq!(drained_frames, burst_frames);
    assert_eq!(source.available_samples(), Some(0));
    assert_eq!(reads.load(Ordering::Relaxed), burst_frames as u64);
}
