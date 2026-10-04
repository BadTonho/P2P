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
    tracker.observe(u16::MAX - 1, start, INITIAL_RTP_REORDER_DELAY);
    assert_eq!(
        tracker.observe(u16::MAX, start, INITIAL_RTP_REORDER_DELAY),
        Default::default()
    );
    assert_eq!(
        tracker.observe(0, start, INITIAL_RTP_REORDER_DELAY),
        Default::default()
    );
    assert_eq!(
        tracker.observe(1, start, INITIAL_RTP_REORDER_DELAY),
        Default::default()
    );
}

#[test]
fn adaptive_reorder_window_uses_recent_p95_and_stays_within_limits() {
    let mut window = AdaptiveRtpReorderWindow::default();
    assert_eq!(window.delay(), Duration::from_millis(40));
    assert_eq!(window.sample_count(), 0);

    for delay_ms in [5, 10, 15] {
        window.observe(Duration::from_millis(delay_ms));
        assert_eq!(window.delay(), Duration::from_millis(40));
    }
    window.observe(Duration::from_millis(20));
    assert_eq!(window.delay(), Duration::from_millis(40));

    for _ in 0..4 {
        window.observe(Duration::from_millis(70));
    }
    assert_eq!(window.delay(), Duration::from_millis(80));
    assert_eq!(window.sample_count(), 8);

    for _ in 0..4 {
        window.observe(Duration::from_millis(500));
    }
    assert_eq!(window.delay(), MAX_RTP_REORDER_DELAY);

    for _ in 0..RTP_REORDER_DELAY_SAMPLE_CAPACITY {
        window.observe(Duration::from_millis(5));
    }
    assert_eq!(window.sample_count(), RTP_REORDER_DELAY_SAMPLE_CAPACITY);
    assert_eq!(window.delay(), MIN_RTP_REORDER_DELAY);
}

#[test]
fn rtp_reorder_buffer_waits_until_the_adaptive_window_expires() {
    let start = Instant::now();
    let mut buffer = RtpReorderBuffer::default();
    let adaptive_delay = Duration::from_millis(80);
    buffer.push(rtp_packet(10, 100, false, &[0x41, 1]), start);
    buffer.push(
        rtp_packet(12, 100, true, &[0x41, 3]),
        start + Duration::from_millis(1),
    );

    assert_eq!(
        buffer
            .expire(start + Duration::from_millis(80), adaptive_delay)
            .confirmed_missing_packets,
        0
    );
    assert_eq!(
        buffer
            .expire(start + Duration::from_millis(81), adaptive_delay)
            .confirmed_missing_packets,
        1
    );
}

#[test]
fn rtp_sequence_tracker_separates_reordering_duplicates_and_confirmed_loss() {
    let start = Instant::now();
    let mut tracker = RtpSequenceTracker::default();

    assert_eq!(
        tracker.observe(10, start, INITIAL_RTP_REORDER_DELAY),
        Default::default()
    );
    assert_eq!(
        tracker.observe(11, start, INITIAL_RTP_REORDER_DELAY),
        Default::default()
    );
    assert_eq!(
        tracker.observe(11, start, INITIAL_RTP_REORDER_DELAY),
        super::RtpSequenceUpdate {
            duplicate_packets: 1,
            ..Default::default()
        }
    );

    assert_eq!(
        tracker.observe(13, start, INITIAL_RTP_REORDER_DELAY),
        super::RtpSequenceUpdate {
            observed_gap_packets: 1,
            ..Default::default()
        }
    );
    assert_eq!(
        tracker.observe(
            12,
            start + Duration::from_millis(20),
            INITIAL_RTP_REORDER_DELAY
        ),
        super::RtpSequenceUpdate {
            recovered_reordered_packets: 1,
            reorder_delay_sample: Some(Duration::from_millis(20)),
            ..Default::default()
        }
    );
    assert_eq!(
        tracker.expire(start + Duration::from_millis(41), INITIAL_RTP_REORDER_DELAY),
        Default::default(),
        "o pacote reordenado preencheu a lacuna dentro dos 40 ms"
    );

    let mut lost_tracker = RtpSequenceTracker::default();
    lost_tracker.observe(20, start, INITIAL_RTP_REORDER_DELAY);
    lost_tracker.observe(22, start, INITIAL_RTP_REORDER_DELAY);
    assert_eq!(
        lost_tracker.expire(start + Duration::from_millis(40), INITIAL_RTP_REORDER_DELAY),
        super::RtpSequenceUpdate {
            confirmed_missing_packets: 1,
            ..Default::default()
        }
    );
    assert_eq!(
        lost_tracker.observe(
            21,
            start + Duration::from_millis(41),
            INITIAL_RTP_REORDER_DELAY
        ),
        super::RtpSequenceUpdate {
            late_after_confirmed_packets: 1,
            reorder_delay_sample: Some(Duration::from_millis(41)),
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
            .expire(
                start + INITIAL_RTP_REORDER_DELAY - Duration::from_nanos(1),
                INITIAL_RTP_REORDER_DELAY
            )
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
            .expire(start + Duration::from_millis(60), INITIAL_RTP_REORDER_DELAY)
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

    let lost = buffer.expire(
        start + INITIAL_RTP_REORDER_DELAY + Duration::from_millis(1),
        INITIAL_RTP_REORDER_DELAY,
    );
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
    let adaptive_delay = Duration::from_millis(80);
    buffer.push(rtp_packet(9, 99, true, &[0x41, 1]), start);
    let mut assembler = H264AccessUnitAssembler::default();

    let end = buffer.push(
        rtp_packet(11, 100, true, &[28, 0x45, 0x99, 0x22]),
        start + Duration::from_millis(1),
    );
    assert!(end.ordered_packets.is_empty());
    let start_fragment = buffer.push(
        rtp_packet(10, 100, false, &[28, 0x85, 0x88, 0x84]),
        start + Duration::from_millis(70),
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
    let ready = assembler.take_ready(start + Duration::from_millis(120), adaptive_delay);
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

    let loss = buffer.expire(
        now + INITIAL_RTP_REORDER_DELAY + Duration::from_millis(1),
        INITIAL_RTP_REORDER_DELAY,
    );
    assert_eq!(loss.confirmed_missing_packets, 1);
    assembler.discard_incomplete_after_loss(loss.resume_timestamp);
    for packet in loss.ordered_packets {
        assembler.push(packet);
    }
    assert!(
        assembler
            .take_ready(now + Duration::from_millis(100), INITIAL_RTP_REORDER_DELAY)
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
            .take_ready(now + Duration::from_millis(100), INITIAL_RTP_REORDER_DELAY)
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
