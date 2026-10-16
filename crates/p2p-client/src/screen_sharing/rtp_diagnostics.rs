use super::*;

const RECENT_PACKET_CAPACITY: usize = 4096;
const RETRANSMIT_COOLDOWN: Duration = Duration::from_millis(25);
const MAX_RETRANSMITS_PER_PACKET: u8 = 3;

#[derive(Clone, Copy, Debug)]
struct RecentPacketState {
    repeats_sent: u8,
    last_retransmitted_at: Option<Instant>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OutboundRtpDecision {
    Initial,
    Retransmit,
    SuppressDuplicateRetransmit,
}

#[derive(Default)]
pub(super) struct RtpWireDiagnostics {
    read_queue: VecDeque<TaggedPacket>,
    write_queue: VecDeque<TaggedPacket>,
    recent: HashMap<(u32, u16), RecentPacketState>,
    order: VecDeque<(u32, u16)>,
    metrics: Option<Arc<SharedMetrics>>,
    suppressed_repeats: u64,
}

impl RtpWireDiagnostics {
    pub(super) fn new(metrics: Arc<SharedMetrics>) -> Self {
        Self {
            metrics: Some(metrics),
            ..Default::default()
        }
    }

    fn observe_at(&mut self, ssrc: u32, sequence: u16, now: Instant) -> OutboundRtpDecision {
        let identity = (ssrc, sequence);
        if let Some(state) = self.recent.get_mut(&identity) {
            if state.repeats_sent >= MAX_RETRANSMITS_PER_PACKET {
                return OutboundRtpDecision::SuppressDuplicateRetransmit;
            }
            if state
                .last_retransmitted_at
                .is_some_and(|last| now.saturating_duration_since(last) < RETRANSMIT_COOLDOWN)
            {
                return OutboundRtpDecision::SuppressDuplicateRetransmit;
            }
            state.repeats_sent = state.repeats_sent.saturating_add(1);
            state.last_retransmitted_at = Some(now);
            return OutboundRtpDecision::Retransmit;
        }
        self.recent.insert(
            identity,
            RecentPacketState {
                repeats_sent: 0,
                last_retransmitted_at: None,
            },
        );
        self.order.push_back(identity);
        if self.order.len() > RECENT_PACKET_CAPACITY {
            self.recent.remove(&self.order.pop_front().unwrap());
        }
        OutboundRtpDecision::Initial
    }
}

impl sansio::Protocol<TaggedPacket, TaggedPacket, ()> for RtpWireDiagnostics {
    type Rout = TaggedPacket;
    type Wout = TaggedPacket;
    type Eout = ();
    type Error = InterceptorError;
    type Time = Instant;

    fn handle_read(&mut self, message: TaggedPacket) -> Result<(), Self::Error> {
        self.read_queue.push_back(message);
        Ok(())
    }
    fn poll_read(&mut self) -> Option<Self::Rout> {
        self.read_queue.pop_front()
    }
    fn handle_write(&mut self, message: TaggedPacket) -> Result<(), Self::Error> {
        if let InterceptorPacket::Rtp(packet) = &message.message.packet {
            match self.observe_at(
                packet.header.ssrc,
                packet.header.sequence_number,
                message.now,
            ) {
                OutboundRtpDecision::Initial => {}
                OutboundRtpDecision::Retransmit => {
                    if let Some(metrics) = &self.metrics {
                        let count = metrics
                            .original_rtp_repeats_sent
                            .fetch_add(1, Ordering::Relaxed)
                            + 1;
                        if count.is_power_of_two() {
                            tracing::info!(
                                screen_share_session = metrics.session_id,
                                track_ssrc = packet.header.ssrc,
                                sequence = packet.header.sequence_number,
                                repeats_total = count,
                                "RTP original reenviado na saída; contador separado de RTX"
                            );
                        }
                    }
                }
                OutboundRtpDecision::SuppressDuplicateRetransmit => {
                    self.suppressed_repeats = self.suppressed_repeats.saturating_add(1);
                    if self.suppressed_repeats.is_power_of_two() {
                        tracing::debug!(
                            screen_share_session =
                                self.metrics.as_ref().map_or(0, |m| m.session_id),
                            track_ssrc = packet.header.ssrc,
                            sequence = packet.header.sequence_number,
                            suppressed_total = self.suppressed_repeats,
                            "Retransmissão RTP redundante suprimida por cooldown/limite por pacote"
                        );
                    }
                    return Ok(());
                }
            }
        }
        self.write_queue.push_back(message);
        Ok(())
    }
    fn poll_write(&mut self) -> Option<Self::Wout> {
        self.write_queue.pop_front()
    }
}

impl Interceptor for RtpWireDiagnostics {
    fn bind_local_stream(&mut self, _info: &StreamInfo) {}
    fn unbind_local_stream(&mut self, _info: &StreamInfo) {}
    fn bind_remote_stream(&mut self, _info: &StreamInfo) {}
    fn unbind_remote_stream(&mut self, _info: &StreamInfo) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn original_repeats_are_bounded_and_separated_by_ssrc() {
        let mut diagnostics = RtpWireDiagnostics::default();
        let now = Instant::now();
        assert_eq!(
            diagnostics.observe_at(1, 7, now),
            OutboundRtpDecision::Initial
        );
        assert_eq!(
            diagnostics.observe_at(1, 7, now),
            OutboundRtpDecision::Retransmit
        );
        assert_eq!(
            diagnostics.observe_at(2, 7, now),
            OutboundRtpDecision::Initial
        );
        for sequence in 8..(RECENT_PACKET_CAPACITY as u16 + 9) {
            assert_eq!(
                diagnostics.observe_at(1, sequence, now),
                OutboundRtpDecision::Initial
            );
        }
        assert_eq!(diagnostics.recent.len(), RECENT_PACKET_CAPACITY);
        assert_eq!(diagnostics.order.len(), RECENT_PACKET_CAPACITY);
        assert_eq!(
            diagnostics.observe_at(1, 7, now),
            OutboundRtpDecision::Initial
        );
    }

    #[test]
    fn burst_retransmissions_of_same_sequence_are_rate_limited_and_capped() {
        let mut diagnostics = RtpWireDiagnostics::default();
        let start = Instant::now();

        // First transmission is Initial; first NACK retransmit is allowed immediately.
        assert_eq!(
            diagnostics.observe_at(42, 100, start),
            OutboundRtpDecision::Initial
        );
        assert_eq!(
            diagnostics.observe_at(42, 100, start + Duration::from_millis(2)),
            OutboundRtpDecision::Retransmit
        );

        // Redundant NACKs arriving within the 25ms cooldown window are suppressed.
        assert_eq!(
            diagnostics.observe_at(42, 100, start + Duration::from_millis(5)),
            OutboundRtpDecision::SuppressDuplicateRetransmit
        );
        assert_eq!(
            diagnostics.observe_at(42, 100, start + Duration::from_millis(26)),
            OutboundRtpDecision::SuppressDuplicateRetransmit
        );

        // After the cooldown expires, a second and third retransmit are allowed.
        let second_retry = start + Duration::from_millis(2) + RETRANSMIT_COOLDOWN;
        assert_eq!(
            diagnostics.observe_at(42, 100, second_retry),
            OutboundRtpDecision::Retransmit
        );
        let third_retry = second_retry + RETRANSMIT_COOLDOWN;
        assert_eq!(
            diagnostics.observe_at(42, 100, third_retry),
            OutboundRtpDecision::Retransmit
        );

        // Even after another cooldown, per-packet cap (3) prevents unbounded storms.
        let fourth_retry = third_retry + RETRANSMIT_COOLDOWN * 2;
        assert_eq!(
            diagnostics.observe_at(42, 100, fourth_retry),
            OutboundRtpDecision::SuppressDuplicateRetransmit
        );
    }
}
