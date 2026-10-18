use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use serde::Serialize;

pub(super) const FRAME_TRACE_CAPACITY: usize = 128;
pub(super) const VIDEO_PUBLISH_STALL: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub(super) struct FrameTraceKey {
    pub(super) ssrc: u32,
    pub(super) rtp_timestamp: u32,
}

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct SequenceObservation {
    pub(super) gaps_observed: u64,
    pub(super) reordered_recovered: u64,
    pub(super) missing_confirmed: u64,
    pub(super) late_after_confirmed: u64,
    pub(super) duplicates: u64,
}

#[derive(Clone, Debug, Default, Serialize)]
struct FrameTraceFrame {
    ssrc: u32,
    rtp_timestamp: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    sender_sample_id: Option<u64>,
    outbound_rtp_packets: u32,
    outbound_retransmissions: u32,
    outbound_first_sequence: Option<u16>,
    outbound_last_sequence: Option<u16>,
    outbound_marker_seen: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    outbound_sample_write: Option<&'static str>,
    inbound_rtp_packets: u32,
    inbound_first_sequence: Option<u16>,
    inbound_last_sequence: Option<u16>,
    inbound_marker_seen: bool,
    sequence_gaps_observed: u32,
    reordered_packets_recovered: u32,
    missing_packets_confirmed: u32,
    late_packets_after_confirmed: u32,
    duplicate_packets: u32,
    #[serde(skip_serializing_if = "str::is_empty")]
    sequence_observation_scope: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    assembly: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    assembly_error: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    decoder_queue: Option<&'static str>,
    decoder_input: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    decoder_call_result: Option<&'static str>,
    decoder_output_available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    decoder_output_attribution: Option<&'static str>,
    published_by_decode_call: bool,
    last_stage: &'static str,
}

impl FrameTraceFrame {
    fn new(key: FrameTraceKey) -> Self {
        Self {
            ssrc: key.ssrc,
            rtp_timestamp: key.rtp_timestamp,
            last_stage: "observed",
            ..Self::default()
        }
    }
}

#[derive(Default)]
pub(super) struct FrameTraceBuffer {
    frames: HashMap<FrameTraceKey, FrameTraceFrame>,
    order: VecDeque<FrameTraceKey>,
    sample_to_frame: HashMap<u64, FrameTraceKey>,
    sample_order: VecDeque<u64>,
    last_published_at: Option<Instant>,
    stall_report_emitted: bool,
    last_outbound_sample_write_at: Option<Instant>,
    outbound_stall_report_emitted: bool,
}

pub(super) struct StallTraceReport {
    pub(super) trigger: &'static str,
    pub(super) silence_millis: u64,
    pub(super) frames_json: String,
}

impl FrameTraceBuffer {
    fn frame_mut(&mut self, key: FrameTraceKey) -> &mut FrameTraceFrame {
        if !self.frames.contains_key(&key) {
            if self.order.len() == FRAME_TRACE_CAPACITY
                && let Some(expired) = self.order.pop_front()
            {
                self.frames.remove(&expired);
                self.sample_to_frame.retain(|_, mapped| *mapped != expired);
            }
            self.order.push_back(key);
            self.frames.insert(key, FrameTraceFrame::new(key));
        }
        self.frames.get_mut(&key).expect("frame was inserted")
    }

    pub(super) fn outbound_packet(
        &mut self,
        key: FrameTraceKey,
        sample_id: Option<u64>,
        sequence: u16,
        marker: bool,
        retransmission: bool,
    ) {
        if let Some(sample_id) = sample_id
            && !retransmission
            && !self.frames.contains_key(&key)
        {
            if !self.sample_to_frame.contains_key(&sample_id) {
                self.sample_to_frame.insert(sample_id, key);
                self.sample_order.push_back(sample_id);
                while self.sample_order.len() > FRAME_TRACE_CAPACITY {
                    if let Some(expired) = self.sample_order.pop_front() {
                        self.sample_to_frame.remove(&expired);
                    }
                }
            }
        }
        let frame = self.frame_mut(key);
        frame.outbound_rtp_packets = frame.outbound_rtp_packets.saturating_add(1);
        frame.outbound_retransmissions = frame
            .outbound_retransmissions
            .saturating_add(u32::from(retransmission));
        frame.outbound_first_sequence.get_or_insert(sequence);
        frame.outbound_last_sequence = Some(sequence);
        frame.outbound_marker_seen |= marker;
        if frame.sender_sample_id.is_none() {
            frame.sender_sample_id = sample_id;
        }
        frame.last_stage = "outbound_rtp_interceptor";
    }

    pub(super) fn outbound_sample_write(&mut self, sample_id: u64, accepted: bool, now: Instant) {
        if let Some(key) = self.sample_to_frame.get(&sample_id).copied()
            && let Some(frame) = self.frames.get_mut(&key)
        {
            frame.outbound_sample_write = Some(if accepted { "accepted" } else { "failed" });
            frame.last_stage = "outbound_sample_write";
        }
        if accepted {
            self.last_outbound_sample_write_at = Some(now);
            self.outbound_stall_report_emitted = false;
        }
    }

    pub(super) fn inbound_packet(
        &mut self,
        key: FrameTraceKey,
        sequence: u16,
        marker: bool,
        observation: SequenceObservation,
    ) {
        let frame = self.frame_mut(key);
        frame.inbound_rtp_packets = frame.inbound_rtp_packets.saturating_add(1);
        frame.inbound_first_sequence.get_or_insert(sequence);
        frame.inbound_last_sequence = Some(sequence);
        frame.inbound_marker_seen |= marker;
        frame.sequence_gaps_observed = frame
            .sequence_gaps_observed
            .saturating_add(observation.gaps_observed.min(u64::from(u32::MAX)) as u32);
        frame.reordered_packets_recovered = frame
            .reordered_packets_recovered
            .saturating_add(observation.reordered_recovered.min(u64::from(u32::MAX)) as u32);
        frame.missing_packets_confirmed = frame
            .missing_packets_confirmed
            .saturating_add(observation.missing_confirmed.min(u64::from(u32::MAX)) as u32);
        frame.late_packets_after_confirmed = frame
            .late_packets_after_confirmed
            .saturating_add(observation.late_after_confirmed.min(u64::from(u32::MAX)) as u32);
        frame.duplicate_packets = frame
            .duplicate_packets
            .saturating_add(observation.duplicates.min(u64::from(u32::MAX)) as u32);
        frame.sequence_observation_scope = "counts observed while handling this packet; a gap can refer to an earlier RTP timestamp";
        frame.last_stage = "inbound_track_event";
    }

    pub(super) fn assembly(&mut self, key: FrameTraceKey, result: Result<(), String>) {
        let frame = self.frame_mut(key);
        match result {
            Ok(()) => {
                frame.assembly = Some("complete");
                frame.last_stage = "h264_assembled";
            }
            Err(error) => {
                frame.assembly = Some("discarded");
                frame.assembly_error = Some(assembly_error_category(&error));
                frame.last_stage = "h264_assembly_discard";
            }
        }
    }

    pub(super) fn decoder_queue(&mut self, key: FrameTraceKey, status: &'static str) {
        let frame = self.frame_mut(key);
        frame.decoder_queue = Some(status);
        frame.last_stage = "decoder_queue";
    }

    pub(super) fn decoder_input(&mut self, key: FrameTraceKey) {
        let frame = self.frame_mut(key);
        frame.decoder_input = true;
        frame.last_stage = "decoder_input";
    }

    pub(super) fn decoder_result(
        &mut self,
        key: FrameTraceKey,
        result: &'static str,
        output_attribution: Option<&'static str>,
        output_available: bool,
        published: bool,
        now: Instant,
    ) {
        let frame = self.frame_mut(key);
        frame.decoder_call_result = Some(result);
        frame.decoder_output_attribution = output_attribution;
        frame.decoder_output_available |= output_available;
        frame.published_by_decode_call |= published;
        frame.last_stage = if published {
            "image_published_by_decode_call"
        } else if result == "error" {
            "decoder_error"
        } else {
            "decoder_no_output"
        };
        if published {
            self.note_publication(now);
        }
    }

    pub(super) fn unattributed_publication(&mut self, now: Instant) {
        self.note_publication(now);
    }

    fn note_publication(&mut self, now: Instant) {
        self.last_published_at = Some(now);
        self.stall_report_emitted = false;
    }

    pub(super) fn stall_report(
        &mut self,
        now: Instant,
        connected_for: Duration,
        initial_timeout_due: bool,
    ) -> Option<StallTraceReport> {
        if self.stall_report_emitted {
            return None;
        }
        let (trigger, silence) = match self.last_published_at {
            Some(last) => {
                let elapsed = now.saturating_duration_since(last);
                (
                    "publish_stalled",
                    (elapsed >= VIDEO_PUBLISH_STALL).then_some(elapsed)?,
                )
            }
            None => (
                "first_publish_timeout",
                initial_timeout_due.then_some(connected_for)?,
            ),
        };
        self.stall_report_emitted = true;
        let frames = self
            .order
            .iter()
            .filter_map(|key| self.frames.get(key).cloned())
            .collect::<Vec<_>>();
        Some(StallTraceReport {
            trigger,
            silence_millis: silence.as_millis().min(u128::from(u64::MAX)) as u64,
            frames_json: serde_json::to_string(&frames).unwrap_or_else(|_| "[]".to_owned()),
        })
    }

    pub(super) fn outbound_stall_report(&mut self, now: Instant) -> Option<StallTraceReport> {
        if self.outbound_stall_report_emitted {
            return None;
        }
        let last_write = self.last_outbound_sample_write_at?;
        let silence = now.saturating_duration_since(last_write);
        if silence < VIDEO_PUBLISH_STALL {
            return None;
        }
        self.outbound_stall_report_emitted = true;
        let frames = self
            .order
            .iter()
            .filter_map(|key| self.frames.get(key).cloned())
            .collect::<Vec<_>>();
        Some(StallTraceReport {
            trigger: "outbound_sample_stalled",
            silence_millis: silence.as_millis().min(u128::from(u64::MAX)) as u64,
            frames_json: serde_json::to_string(&frames).unwrap_or_else(|_| "[]".to_owned()),
        })
    }
}

fn assembly_error_category(error: &str) -> &'static str {
    if error.contains("marcador") {
        "marker_missing_or_misplaced"
    } else if error.contains("FU-A") || error.contains("fragmento") {
        "fu_a_incomplete_or_malformed"
    } else if error.contains("pacote(s) RTP ausente(s)") {
        "rtp_sequence_hole"
    } else if error.contains("limite de quadros RTP") {
        "pending_frame_limit"
    } else if error.contains("depacketiza") {
        "h264_depacketization_failed"
    } else if error.contains("vazio") || error.contains("sem pacotes") {
        "empty_rtp_or_h264_frame"
    } else {
        "h264_assembly_failed"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(timestamp: u32) -> FrameTraceKey {
        FrameTraceKey {
            ssrc: 42,
            rtp_timestamp: timestamp,
        }
    }

    #[test]
    fn trace_correlates_outbound_and_inbound_events_by_ssrc_and_timestamp() {
        let mut trace = FrameTraceBuffer::default();
        trace.outbound_packet(key(7), Some(3), 100, true, false);
        trace.outbound_sample_write(3, true, Instant::now());
        trace.inbound_packet(key(7), 100, true, SequenceObservation::default());
        trace.assembly(key(7), Ok(()));
        trace.decoder_queue(key(7), "accepted");
        trace.decoder_input(key(7));

        let frame = trace.frames.get(&key(7)).unwrap();
        assert_eq!(frame.outbound_rtp_packets, 1);
        assert_eq!(frame.inbound_rtp_packets, 1);
        assert_eq!(frame.outbound_sample_write, Some("accepted"));
        assert_eq!(frame.last_stage, "decoder_input");
    }

    #[test]
    fn trace_keeps_only_the_newest_128_frames() {
        let mut trace = FrameTraceBuffer::default();
        for timestamp in 0..=(FRAME_TRACE_CAPACITY as u32) {
            trace.inbound_packet(
                key(timestamp),
                timestamp as u16,
                true,
                SequenceObservation::default(),
            );
        }

        assert_eq!(trace.order.len(), FRAME_TRACE_CAPACITY);
        assert!(!trace.frames.contains_key(&key(0)));
        assert!(trace.frames.contains_key(&key(FRAME_TRACE_CAPACITY as u32)));
    }

    #[test]
    fn stall_snapshot_is_emitted_once_and_rearmed_by_publication() {
        let start = Instant::now();
        let mut trace = FrameTraceBuffer::default();
        assert!(
            trace
                .stall_report(
                    start + Duration::from_secs(4),
                    Duration::from_secs(4),
                    false
                )
                .is_none()
        );
        let initial = trace
            .stall_report(start + Duration::from_secs(5), Duration::from_secs(5), true)
            .expect("first publish timeout should produce a trace");
        assert_eq!(initial.trigger, "first_publish_timeout");
        assert!(
            trace
                .stall_report(start + Duration::from_secs(9), Duration::from_secs(9), true)
                .is_none()
        );

        trace.decoder_result(
            key(1),
            "output",
            Some("same_decode_call"),
            true,
            true,
            start + Duration::from_secs(10),
        );
        assert!(
            trace
                .stall_report(
                    start + Duration::from_millis(11_999),
                    Duration::from_secs(12),
                    true
                )
                .is_none()
        );
        let stalled = trace
            .stall_report(
                start + Duration::from_secs(12),
                Duration::from_secs(12),
                true,
            )
            .expect("two seconds without a publication should produce a trace");
        assert_eq!(stalled.trigger, "publish_stalled");
        assert_eq!(stalled.silence_millis, 2_000);
        assert!(
            trace
                .stall_report(
                    start + Duration::from_secs(14),
                    Duration::from_secs(14),
                    true
                )
                .is_none()
        );
    }

    #[test]
    fn trace_keeps_reorder_categories_separate() {
        let mut trace = FrameTraceBuffer::default();
        trace.inbound_packet(
            key(9),
            9,
            false,
            SequenceObservation {
                gaps_observed: 2,
                reordered_recovered: 1,
                missing_confirmed: 1,
                late_after_confirmed: 1,
                duplicates: 1,
            },
        );
        let frame = trace.frames.get(&key(9)).unwrap();
        assert_eq!(frame.sequence_gaps_observed, 2);
        assert_eq!(frame.reordered_packets_recovered, 1);
        assert_eq!(frame.missing_packets_confirmed, 1);
        assert_eq!(frame.late_packets_after_confirmed, 1);
        assert_eq!(frame.duplicate_packets, 1);
    }

    #[test]
    fn incomplete_frame_error_remains_distinct_in_trace() {
        let mut trace = FrameTraceBuffer::default();
        trace.assembly(
            key(10),
            Err("quadro RTP expirou sem marcador de fim".to_owned()),
        );
        let frame = trace.frames.get(&key(10)).unwrap();
        assert_eq!(frame.assembly, Some("discarded"));
        assert_eq!(frame.assembly_error, Some("marker_missing_or_misplaced"));
        assert!(!frame.decoder_input);
    }

    #[test]
    fn outbound_stall_snapshot_is_emitted_once_and_rearmed_after_a_write() {
        let start = Instant::now();
        let mut trace = FrameTraceBuffer::default();
        trace.outbound_packet(key(1), Some(1), 10, true, false);
        trace.outbound_sample_write(1, true, start);
        assert!(
            trace
                .outbound_stall_report(start + VIDEO_PUBLISH_STALL - Duration::from_nanos(1))
                .is_none()
        );
        let stalled = trace
            .outbound_stall_report(start + VIDEO_PUBLISH_STALL)
            .expect("two seconds without an accepted sample should be reported");
        assert_eq!(stalled.trigger, "outbound_sample_stalled");
        assert!(
            trace
                .outbound_stall_report(start + Duration::from_secs(5))
                .is_none()
        );

        trace.outbound_packet(key(2), Some(2), 11, true, false);
        trace.outbound_sample_write(2, true, start + Duration::from_secs(5));
        assert!(
            trace
                .outbound_stall_report(start + Duration::from_secs(6))
                .is_none()
        );
        assert!(
            trace
                .outbound_stall_report(start + Duration::from_secs(7))
                .is_some()
        );
    }
}
