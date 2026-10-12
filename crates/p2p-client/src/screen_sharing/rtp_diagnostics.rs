use super::*;

const RECENT_PACKET_CAPACITY: usize = 4096;

#[derive(Default)]
pub(super) struct RtpWireDiagnostics {
    read_queue: VecDeque<TaggedPacket>,
    write_queue: VecDeque<TaggedPacket>,
    recent: HashSet<(u32, u16)>,
    order: VecDeque<(u32, u16)>,
    metrics: Option<Arc<SharedMetrics>>,
}

impl RtpWireDiagnostics {
    pub(super) fn new(metrics: Arc<SharedMetrics>) -> Self {
        Self {
            metrics: Some(metrics),
            ..Default::default()
        }
    }
    fn observe(&mut self, ssrc: u32, sequence: u16) -> bool {
        let identity = (ssrc, sequence);
        if !self.recent.insert(identity) {
            return true;
        }
        self.order.push_back(identity);
        if self.order.len() > RECENT_PACKET_CAPACITY {
            self.recent.remove(&self.order.pop_front().unwrap());
        }
        false
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
            let repeated = self.observe(packet.header.ssrc, packet.header.sequence_number);
            if repeated && let Some(metrics) = &self.metrics {
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
        // Retransmissions are necessary for recovery. Observe, never suppress them.
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
        assert!(!diagnostics.observe(1, 7));
        assert!(diagnostics.observe(1, 7));
        assert!(!diagnostics.observe(2, 7));
        for sequence in 8..(RECENT_PACKET_CAPACITY as u16 + 9) {
            assert!(!diagnostics.observe(1, sequence));
        }
        assert_eq!(diagnostics.recent.len(), RECENT_PACKET_CAPACITY);
        assert_eq!(diagnostics.order.len(), RECENT_PACKET_CAPACITY);
        assert!(!diagnostics.observe(1, 7));
    }
}
