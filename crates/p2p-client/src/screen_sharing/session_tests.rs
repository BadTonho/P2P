use super::*;
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

    let remote_silent = playback_audio_state(250, 250, 0, 300, 0);
    assert!(remote_silent.contains("pode ser silêncio"));
    assert!(playback_audio_state(0, 0, 0, 0, 0).contains("nenhum pacote RTP"));
    assert!(playback_audio_state(250, 250, 1500, 0, 0).contains("não executou callbacks"));
    assert!(playback_audio_state(250, 250, 1500, 300, 6000).contains("saída do Windows ativas"));
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
fn loopback_controlled_rtp_loss_correlates_nack_packet_stats() {
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
        if injected
            && receiver_metrics
                .nack_packets_sent
                .is_some_and(|count| count > 0)
            && sender_metrics.nack_packets_received_observed > 0
            && receiver_metrics.nack_packets_sent
                == Some(sender_metrics.nack_packets_received_observed)
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
        nack_stats_correlated,
        "the receiver's NACK count should match packets observed in the sender's inbound RTCP interceptor; sender={sender_metrics:?}, receiver={receiver_metrics:?}"
    );
    assert!(sender_metrics.nack_packets_received.is_some());
    // The RTC exposes retransmission counters in this build, so zero is a known result rather
    // than missing data. This diagnostic test measures NACK delivery; it does not change or
    // assume the negotiated retransmission behavior.
    assert!(sender_metrics.retransmitted_packets_sent.is_some());
    assert!(receiver_metrics.retransmitted_packets_received.is_some());
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
