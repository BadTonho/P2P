use std::collections::{HashMap, HashSet, VecDeque};
use std::net::{IpAddr, Ipv4Addr};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use bytes::Bytes;
use eframe::egui;
use openh264::OpenH264API;
use openh264::decoder::Decoder;
use openh264::encoder::{BitRate, Encoder, EncoderConfig, FrameRate, IntraFramePeriod, UsageType};
use openh264::formats::{RgbaSliceU8, YUVBuffer, YUVSource};
use rtc::interceptor::{
    Attribute, Interceptor, Packet as InterceptorPacket, Registry, Slot, StreamInfo, TaggedPacket,
};
use rtc::media::Sample;
use rtc::media_stream::MediaStreamTrack;
use rtc::peer_connection::configuration::interceptor_registry::register_default_interceptors;
use rtc::peer_connection::configuration::media_engine::{MIME_TYPE_H264, MediaEngine};
use rtc::peer_connection::configuration::{RTCConfigurationBuilder, RTCIceServer};
use rtc::peer_connection::transport::RTCIceCandidateInit;
use rtc::rtcp::payload_feedbacks::picture_loss_indication::PictureLossIndication;
use rtc::rtp::codec::h264::H264Packet;
use rtc::rtp::packet::Packet as RtpPacket;
use rtc::rtp::packetizer::Depacketizer;
use rtc::rtp_transceiver::PayloadType;
use rtc::rtp_transceiver::rtp_sender::{
    RTCPFeedback, RTCRtpCodec, RTCRtpCodecParameters, RTCRtpCodingParameters,
    RTCRtpEncodingParameters, RtpCodecKind,
};
use rtc::sansio;
use rtc::shared::error::Error as InterceptorError;
use rtc::statistics::StatsSelector;
use rtc::statistics::report::{RTCStatsReport, RTCStatsReportEntry};
use signaling_protocol::SignalKind;
use tokio::sync::mpsc;
use tokio::task::JoinHandle as TokioJoinHandle;
use tokio::time::timeout;
use webrtc::media_stream::Track;
use webrtc::media_stream::track_local::static_sample::TrackLocalStaticSample;
use webrtc::media_stream::track_local::{TrackLocal, TrackLocalEvent};
use webrtc::media_stream::track_remote::{TrackRemote, TrackRemoteEvent};
use webrtc::peer_connection::{
    PeerConnection, PeerConnectionBuilder, PeerConnectionEventHandler, RTCPeerConnectionIceEvent,
    RTCPeerConnectionState,
};
use webrtc::runtime::TokioRuntime;

use crate::logging::safe_stun_endpoint;
use crate::mf_video;
use crate::screen_capture::{LatestFrame, PreviewFrame};
use crate::settings::VideoDecoderPreference;
use crate::turn_relay::TurnCredentials;

const VIDEO_PAYLOAD_TYPE: PayloadType = 102;
const VIDEO_CLOCK_RATE: u32 = 90_000;
const FRAME_DURATION: Duration = Duration::from_nanos(1_000_000_000 / 30);
const ENCODER_PACING_JITTER_TOLERANCE: Duration = Duration::from_micros(1_000);
const STATIC_FRAME_REPEAT_INTERVAL: Duration = Duration::from_secs(1);
const DXVA_FIRST_OUTPUT_WATCHDOG: Duration = Duration::from_millis(750);
const DXVA_ADAPTIVE_WARMUP: Duration = Duration::from_secs(2);
const DXVA_ADAPTIVE_WINDOW: Duration = Duration::from_secs(3);
const DXVA_ADAPTIVE_MIN_INPUTS: usize = 45;
const DXVA_ADAPTIVE_MIN_OUTPUT_RATIO: f64 = 0.80;
const MAX_CACHED_GOP_ACCESS_UNITS: usize = 240;
const MAX_CACHED_GOP_BYTES: usize = 16 * 1024 * 1024;
const RTP_REORDER_DELAY: Duration = Duration::from_millis(40);
const MAX_RTP_FRAME_AGE: Duration = Duration::from_millis(200);
const MAX_PENDING_RTP_FRAMES: usize = 8;
const MEDIA_UDP_PORT: u16 = 9002;
const PEER_CONNECTION_TIMEOUT: Duration = Duration::from_secs(20);
const INTERNET_PEER_CONNECTION_TIMEOUT: Duration = Duration::from_secs(30);
const FIRST_VIDEO_FRAME_TIMEOUT: Duration = Duration::from_secs(5);
const CONNECTION_CHECK_INTERVAL: Duration = Duration::from_millis(200);
const KEYFRAME_REQUEST_MIN_INTERVAL: Duration = Duration::from_millis(750);
const RTP_SEQUENCE_HISTORY: usize = 4096;

static NEXT_SCREEN_SHARE_SESSION_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Default)]
struct PliForwarder {
    read_queue: VecDeque<TaggedPacket>,
    write_queue: VecDeque<TaggedPacket>,
}

impl sansio::Protocol<TaggedPacket, TaggedPacket, ()> for PliForwarder {
    type Rout = TaggedPacket;
    type Wout = TaggedPacket;
    type Eout = ();
    type Error = InterceptorError;
    type Time = Instant;

    fn handle_read(&mut self, mut message: TaggedPacket) -> Result<(), Self::Error> {
        if let InterceptorPacket::Rtcp(packets) = &message.message.packet {
            let pli_packets = packets
                .iter()
                .filter(|packet| packet.as_any().is::<PictureLossIndication>())
                .cloned()
                .collect::<Vec<_>>();
            if pli_packets.is_empty() {
                return Ok(());
            }
            message.message.packet = InterceptorPacket::Rtcp(pli_packets);
            message.message.add(Attribute::DeliverToApplication);
        }
        self.read_queue.push_back(message);
        Ok(())
    }

    fn poll_read(&mut self) -> Option<Self::Rout> {
        self.read_queue.pop_front()
    }

    fn handle_write(&mut self, message: TaggedPacket) -> Result<(), Self::Error> {
        self.write_queue.push_back(message);
        Ok(())
    }

    fn poll_write(&mut self) -> Option<Self::Wout> {
        self.write_queue.pop_front()
    }
}

impl Interceptor for PliForwarder {
    fn bind_local_stream(&mut self, _info: &StreamInfo) {}
    fn unbind_local_stream(&mut self, _info: &StreamInfo) {}
    fn bind_remote_stream(&mut self, _info: &StreamInfo) {}
    fn unbind_remote_stream(&mut self, _info: &StreamInfo) {}
}

type RemoteFrameStore = Arc<Mutex<Option<Arc<PreviewFrame>>>>;
type RemoteTrackStore = Arc<Mutex<Option<Arc<dyn TrackRemote>>>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum H264FrameKind {
    Idr,
    Delta,
}

fn classify_h264_access_unit(nals: &[u8]) -> Option<H264FrameKind> {
    if nals.contains(&5) {
        Some(H264FrameKind::Idr)
    } else if nals.iter().any(|nal_type| (1..=4).contains(nal_type)) {
        Some(H264FrameKind::Delta)
    } else {
        None
    }
}

fn should_log_aggregate_error(count: u64) -> bool {
    count.is_power_of_two()
}

pub fn validate_stun_uri(input: &str) -> Result<String, String> {
    let uri = input.trim();
    if !uri.starts_with("stun:")
        || uri.len() <= "stun:".len()
        || uri.chars().any(char::is_whitespace)
        || uri.contains(',')
    {
        return Err(
            "Informe uma única URI STUN no formato stun:servidor:porta. TURN não é permitido nesta etapa."
                .to_owned(),
        );
    }
    let server = RTCIceServer {
        urls: vec![uri.to_owned()],
        ..Default::default()
    };
    server
        .urls()
        .map_err(|error| format!("A URI STUN não é válida: {error}"))?;
    Ok(uri.to_owned())
}

#[derive(Clone, Debug, Default)]
pub struct ScreenShareMetrics {
    pub session_id: u64,
    pub track_ssrc: Option<u32>,
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
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaRoute {
    Direct,
    Turn,
}

#[derive(Clone, Debug, Default)]
struct PeerStatsSnapshot {
    selected_pair_key: String,
    selected_pair_summary: String,
    route: Option<MediaRoute>,
    pair_packets_sent: u64,
    pair_packets_received: u64,
    pair_bytes_sent: u64,
    pair_bytes_received: u64,
    pair_packets_discarded_on_send: u32,
    pair_rtt_ms: f64,
    outbound_packets: u64,
    outbound_bytes: u64,
    outbound_frames_encoded: u32,
    outbound_frames_sent: u32,
    outbound_ssrc: u32,
    inbound_packets: u64,
    inbound_bytes: u64,
    inbound_packets_lost: i64,
    inbound_jitter_ms: f64,
    inbound_frames_received: u32,
    inbound_frames_decoded: u32,
    inbound_frames_rendered: u32,
    inbound_frames_dropped: u32,
    inbound_packets_discarded: u64,
    inbound_ssrc: u32,
    outbound_summary: String,
    inbound_summary: String,
}

#[derive(Default)]
struct SharedMetrics {
    session_id: u64,
    track_ssrc: AtomicU64,
    p2p_connected: AtomicBool,
    route: AtomicU64,
    local_ice_candidates: AtomicU64,
    remote_ice_candidates: AtomicU64,
    local_srflx_candidates: AtomicU64,
    remote_srflx_candidates: AtomicU64,
    local_relay_candidates: AtomicU64,
    remote_relay_candidates: AtomicU64,
    encoder_input_frames: AtomicU64,
    encoded_frames: AtomicU64,
    sent_frames: AtomicU64,
    dropped_before_initial_idr: AtomicU64,
    sent_idr_frames: AtomicU64,
    sent_delta_frames: AtomicU64,
    received_packets: AtomicU64,
    decoded_frames: AtomicU64,
    decoded_delta_frames: AtomicU64,
    decoded_idr_frames: AtomicU64,
    decode_errors: AtomicU64,
    decoder_no_output_frames: AtomicU64,
    decoder_queue_drops: AtomicU64,
    published_frames: AtomicU64,
    decoder_input_frames: AtomicU64,
    ui_texture_updates: AtomicU64,
    interval_received_packets: AtomicU64,
    interval_observed_sequence_gaps: AtomicU64,
    interval_recovered_reordered_packets: AtomicU64,
    interval_unmatched_out_of_order_packets: AtomicU64,
    interval_duplicate_packets: AtomicU64,
    interval_confirmed_missing_packets: AtomicU64,
    interval_late_after_confirmed_packets: AtomicU64,
    interval_sequence_gap_resyncs: AtomicU64,
    interval_assembled_access_units: AtomicU64,
    interval_assembly_errors: AtomicU64,
    interval_decoder_input_frames: AtomicU64,
    interval_decoder_no_output_frames: AtomicU64,
    interval_decoder_queue_drops: AtomicU64,
    interval_decode_errors: AtomicU64,
    interval_decoded_frames: AtomicU64,
    interval_published_frames: AtomicU64,
    interval_ui_texture_updates: AtomicU64,
    interval_pli_requests_sent: AtomicU64,
    interval_pli_requests_received: AtomicU64,
    interval_encoder_input_frames: AtomicU64,
    interval_new_capture_frames: AtomicU64,
    interval_repeated_capture_frames: AtomicU64,
    interval_encoder_worker_late_frames: AtomicU64,
    interval_skipped_capture_sequences: AtomicU64,
    interval_encoded_frames: AtomicU64,
    interval_encoded_idr_frames: AtomicU64,
    interval_encoded_delta_frames: AtomicU64,
    interval_sent_frames: AtomicU64,
    interval_dropped_before_initial_idr: AtomicU64,
    interval_sent_idr_frames: AtomicU64,
    interval_sent_delta_frames: AtomicU64,
    interval_encode_nanos: AtomicU64,
    interval_encode_samples: AtomicU64,
    interval_queue_wait_nanos: AtomicU64,
    interval_queue_wait_samples: AtomicU64,
    interval_write_sample_nanos: AtomicU64,
    interval_write_sample_samples: AtomicU64,
    interval_write_sample_bytes: AtomicU64,
    interval_write_sample_failures: AtomicU64,
    interval_outbound_rtp_packets: AtomicU64,
    interval_outbound_rtp_bytes: AtomicU64,
    interval_inbound_rtp_packets: AtomicU64,
    interval_inbound_rtp_bytes: AtomicU64,
    interval_inbound_rtp_lost: AtomicI64,
    last_decode_error: Mutex<Option<String>>,
    encoder_backend: Mutex<String>,
    encoder_fallback_reason: Mutex<Option<String>>,
    decoder_backend: Mutex<String>,
    decoder_fallback_reason: Mutex<Option<String>>,
    decoder_preference: Mutex<String>,
    connected_at: Mutex<Option<Instant>>,
    selected_ice_pair: Mutex<String>,
    selected_pair_key: Mutex<String>,
    rtc_outbound_summary: Mutex<String>,
    rtc_inbound_summary: Mutex<String>,
    rtc_stats: Mutex<PeerStatsSnapshot>,
    h264_flow: Mutex<H264FlowDiagnostics>,
}

#[derive(Default)]
struct H264FlowDiagnostics {
    encoded_sps: u64,
    encoded_pps: u64,
    encoded_idr: u64,
    encoded_delta_frames: u64,
    received_sps: u64,
    received_pps: u64,
    received_idr: u64,
    received_delta_nals: u64,
    received_single_nals: u64,
    received_stap_a: u64,
    received_fu_a_start: u64,
    received_fu_a_end: u64,
    sequence_gaps: u64,
    out_of_order_packets: u64,
    duplicate_packets: u64,
    recovered_reordered_packets: u64,
    confirmed_missing_packets: u64,
    late_after_confirmed_packets: u64,
    sequence_gap_resyncs: u64,
    assembled_access_units: u64,
    assembled_with_sps: u64,
    assembled_with_pps: u64,
    assembled_with_idr: u64,
    assembled_delta_frames: u64,
    assembly_errors: u64,
    marker_timeouts: u64,
    sequence_hole_errors: u64,
    fragment_errors: u64,
    pending_frame_evictions: u64,
    other_assembly_errors: u64,
    pli_requests_sent: u64,
    pli_requests_received: u64,
    pli_sequence_gap: u64,
    pli_assembly_error: u64,
    pli_decode_error: u64,
    pli_queue_overflow: u64,
    pli_explicit: u64,
    keyframe_resyncs: u64,
    dropped_while_waiting_for_idr: u64,
    decoder_no_output_frames: u64,
    decoder_queue_drops: u64,
    resync_started_at: Option<Instant>,
    last_idr_assembled_recovery_ms: Option<u64>,
    last_idr_decoded_recovery_ms: Option<u64>,
    last_idr_published_recovery_ms: Option<u64>,
    recovery_count: u64,
    recovery_time_millis_total: u128,
    last_recovery_time_millis: Option<u64>,
    last_encoded_nals: String,
    last_access_unit: String,
    last_assembly_error: Option<String>,
}

impl SharedMetrics {
    fn snapshot(&self) -> ScreenShareMetrics {
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
                "codificador SPS/PPS/IDR/P {}/{}/{}/{}, última saída {}; recepção NAL SPS/PPS/IDR/P {}/{}/{}/{}, pacotes single/STAP-A/FU-A início/fim {}/{}/{}/{}; saltos observados {}, reordenação recuperada {}, fora de ordem {}, duplicatas {}, perdas confirmadas {}, tardios após confirmação {}, resyncs por salto {}; unidades completas {} (IDR {}, P {}), com SPS/PPS {}/{}, erros de montagem {} [marcador {}, lacuna {}, fragmentação {}, limite {}, outros {}]; última unidade: {}{}; PLI enviado/recebido {}/{}, motivos salto/montagem/decodificação/fila/manual {}/{}/{}/{}/{}, resyncs {}, descartes até IDR {}, recuperação IDR (montado/decodificado/publicado) {:?}/{:?}/{:?} ms.",
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

    fn set_encoder_backend(&self, backend: String, fallback: Option<String>) {
        *self
            .encoder_backend
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = backend;
        *self
            .encoder_fallback_reason
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = fallback;
    }

    fn set_decoder_backend(&self, backend: String, fallback: Option<String>) {
        *self
            .decoder_backend
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = backend;
        *self
            .decoder_fallback_reason
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = fallback;
    }

    fn set_track_ssrc(&self, ssrc: u32) {
        self.track_ssrc.store(u64::from(ssrc), Ordering::Relaxed);
    }

    fn update_transport_diagnostics(
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
        }
        *self
            .rtc_stats
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = snapshot.clone();
        *previous = Some(snapshot);
        changed
    }

    fn record_encoded_access_unit(&self, data: &[u8]) {
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

    fn record_encoder_input_frame(&self) {
        self.encoder_input_frames.fetch_add(1, Ordering::Relaxed);
        self.interval_encoder_input_frames
            .fetch_add(1, Ordering::Relaxed);
    }

    fn record_capture_frame_for_encoder(&self, repeated: bool) {
        if repeated {
            self.interval_repeated_capture_frames
                .fetch_add(1, Ordering::Relaxed);
        } else {
            self.interval_new_capture_frames
                .fetch_add(1, Ordering::Relaxed);
        }
        self.record_encoder_input_frame();
    }

    fn record_encoder_worker_delay(&self, late: bool, skipped_sequences: u64) {
        if late {
            self.interval_encoder_worker_late_frames
                .fetch_add(1, Ordering::Relaxed);
        }
        self.interval_skipped_capture_sequences
            .fetch_add(skipped_sequences, Ordering::Relaxed);
    }

    fn record_drop_before_initial_idr(&self) {
        self.dropped_before_initial_idr
            .fetch_add(1, Ordering::Relaxed);
        self.interval_dropped_before_initial_idr
            .fetch_add(1, Ordering::Relaxed);
    }

    fn record_sent_frame(&self, kind: H264FrameKind) {
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

    fn record_received_packet(&self, packet: &rtc::rtp::packet::Packet) {
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

    fn record_sequence_update(&self, update: RtpSequenceUpdate) {
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

    fn record_sequence_gap_resync(&self) {
        self.interval_sequence_gap_resyncs
            .fetch_add(1, Ordering::Relaxed);
        self.h264_flow
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .sequence_gap_resyncs += 1;
    }

    fn record_assembled_access_unit(&self, data: &[u8]) -> String {
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
        if contains_sps {
            flow.assembled_with_sps += 1;
        }
        if contains_pps {
            flow.assembled_with_pps += 1;
        }
        if contains_idr {
            flow.assembled_with_idr += 1;
        }
        if contains_idr {
            if let Some(started_at) = flow.resync_started_at {
                let elapsed = started_at.elapsed().as_millis();
                flow.last_idr_assembled_recovery_ms =
                    Some(elapsed.min(u128::from(u64::MAX)) as u64);
            }
        }
        if !contains_idr && nals.iter().any(|nal_type| (1..=4).contains(nal_type)) {
            flow.assembled_delta_frames += 1;
        }
        flow.last_access_unit = description.clone();
        description
    }

    fn record_idr_decoded(&self) {
        let mut flow = self
            .h264_flow
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(started_at) = flow.resync_started_at {
            let elapsed = started_at.elapsed().as_millis();
            flow.last_idr_decoded_recovery_ms = Some(elapsed.min(u128::from(u64::MAX)) as u64);
        }
    }

    fn record_idr_published(&self) {
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

    fn record_assembly_error(&self, error: String) {
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

    fn record_pli_sent(&self, reason: PliReason) {
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

    fn record_pli_received(&self) {
        self.interval_pli_requests_received
            .fetch_add(1, Ordering::Relaxed);
        self.h264_flow
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pli_requests_received += 1;
    }

    fn record_keyframe_resync(&self) {
        let mut flow = self
            .h264_flow
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        flow.keyframe_resyncs += 1;
        if flow.resync_started_at.is_none() {
            flow.resync_started_at = Some(Instant::now());
        }
    }

    fn record_drop_waiting_for_idr(&self) {
        self.h264_flow
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .dropped_while_waiting_for_idr += 1;
    }

    fn record_decoder_no_output(&self) {
        self.interval_decoder_no_output_frames
            .fetch_add(1, Ordering::Relaxed);
        self.decoder_no_output_frames
            .fetch_add(1, Ordering::Relaxed);
        self.h264_flow
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .decoder_no_output_frames += 1;
    }

    fn record_decoder_input(&self) {
        self.decoder_input_frames.fetch_add(1, Ordering::Relaxed);
        self.interval_decoder_input_frames
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_ui_texture_update(&self) {
        self.ui_texture_updates.fetch_add(1, Ordering::Relaxed);
        self.interval_ui_texture_updates
            .fetch_add(1, Ordering::Relaxed);
    }

    fn record_decoder_queue_drop(&self) {
        self.decoder_queue_drops.fetch_add(1, Ordering::Relaxed);
        self.interval_decoder_queue_drops
            .fetch_add(1, Ordering::Relaxed);
        self.h264_flow
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .decoder_queue_drops += 1;
    }

    fn take_performance_snapshot(&self) -> ScreenSharePerformanceSnapshot {
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
        }
    }

    fn record_encode_duration(&self, elapsed: Duration) {
        self.interval_encode_nanos
            .fetch_add(elapsed.as_nanos() as u64, Ordering::Relaxed);
        self.interval_encode_samples.fetch_add(1, Ordering::Relaxed);
    }
}

#[derive(Default)]
struct RtpSequenceTracker {
    highest_sequence: Option<u16>,
    recent_seen: HashSet<u16>,
    recent_order: VecDeque<u16>,
    pending_missing: HashMap<u16, Instant>,
    confirmed_missing: HashSet<u16>,
    confirmed_order: VecDeque<u16>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct RtpSequenceUpdate {
    observed_gap_packets: u64,
    recovered_reordered_packets: u64,
    unmatched_out_of_order_packets: u64,
    duplicate_packets: u64,
    confirmed_missing_packets: u64,
    late_after_confirmed_packets: u64,
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
enum PliReason {
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
struct KeyframeRequestLimiter {
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

async fn send_picture_loss_indication(
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

fn begin_stream_resync(
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

fn annex_b_nals(data: &[u8]) -> Vec<&[u8]> {
    fn next_start_code(data: &[u8], from: usize) -> Option<(usize, usize)> {
        let mut index = from;
        while index + 3 <= data.len() {
            if index + 4 <= data.len() && data[index..index + 4] == [0, 0, 0, 1] {
                return Some((index, 4));
            }
            if data[index..index + 3] == [0, 0, 1] {
                return Some((index, 3));
            }
            index += 1;
        }
        None
    }

    let Some((start, start_code_len)) = next_start_code(data, 0) else {
        return Vec::new();
    };
    let mut nal_start = start + start_code_len;
    let mut nals = Vec::new();

    while nal_start < data.len() {
        if let Some((next_start, next_start_code_len)) = next_start_code(data, nal_start) {
            if next_start > nal_start {
                nals.push(&data[nal_start..next_start]);
            }
            nal_start = next_start + next_start_code_len;
        } else {
            nals.push(&data[nal_start..]);
            break;
        }
    }
    nals
}

fn annex_b_nal_types(data: &[u8]) -> Vec<u8> {
    annex_b_nals(data)
        .into_iter()
        .filter_map(|nal| nal.first().map(|header| header & 0x1f))
        .collect()
}

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

fn rtp_payload_nal_types(payload: &[u8]) -> Vec<u8> {
    let Some(header) = payload.first() else {
        return Vec::new();
    };
    match header & 0x1f {
        1..=23 => vec![header & 0x1f],
        24 => {
            let mut types = Vec::new();
            let mut offset = 1;
            while offset + 2 <= payload.len() {
                let nal_len = u16::from_be_bytes([payload[offset], payload[offset + 1]]) as usize;
                offset += 2;
                if nal_len == 0 || offset + nal_len > payload.len() {
                    return vec![24];
                }
                types.push(payload[offset] & 0x1f);
                offset += nal_len;
            }
            if offset != payload.len() || types.is_empty() {
                vec![24]
            } else {
                types
            }
        }
        28 if payload.len() >= 2 && payload[1] & 0x80 != 0 => {
            vec![payload[1] & 0x1f]
        }
        _ => Vec::new(),
    }
}

fn describe_nal_types(nal_types: &[u8], byte_len: usize) -> String {
    if nal_types.is_empty() {
        return format!("sem início Annex-B, {byte_len} bytes");
    }
    let names = nal_types
        .iter()
        .map(|nal_type| match nal_type {
            1 => "1(slice)".to_owned(),
            5 => "5(IDR)".to_owned(),
            6 => "6(SEI)".to_owned(),
            7 => "7(SPS)".to_owned(),
            8 => "8(PPS)".to_owned(),
            9 => "9(AUD)".to_owned(),
            other => other.to_string(),
        })
        .collect::<Vec<_>>()
        .join(",");
    format!("{byte_len} bytes, NAL [{names}]")
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

#[derive(Debug)]
pub enum ScreenShareEvent {
    Signal { kind: SignalKind, payload: String },
    State(String),
    Error(String),
    ConnectionClosed,
}

enum Command {
    StartSending(LatestFrame),
    Signal {
        kind: SignalKind,
        payload: String,
    },
    #[cfg(test)]
    RequestKeyFrameForTest,
    Stop,
}

pub struct ScreenShareSession {
    commands: mpsc::UnboundedSender<Command>,
    events: std_mpsc::Receiver<ScreenShareEvent>,
    remote_frame: RemoteFrameStore,
    #[allow(dead_code)] // Usado pelo teste loopback para provocar um PLI explícito.
    remote_track: RemoteTrackStore,
    metrics: Arc<SharedMetrics>,
    worker: Option<JoinHandle<()>>,
}

impl ScreenShareSession {
    pub fn new(
        context: egui::Context,
        bind_ipv4: Ipv4Addr,
        stun_server: Option<String>,
        turn_credentials: Option<TurnCredentials>,
        decoder_preference: VideoDecoderPreference,
    ) -> Result<Self, String> {
        if let Some(server) = stun_server.as_deref() {
            if let Err(error) = validate_stun_uri(server) {
                tracing::error!(reason = %error, "URI STUN recusada antes de iniciar WebRTC");
                return Err(error);
            }
        }
        Self::with_udp_address(
            context,
            format!("{bind_ipv4}:{MEDIA_UDP_PORT}"),
            stun_server,
            turn_credentials,
            decoder_preference,
        )
    }

    #[cfg(test)]
    fn new_loopback(context: egui::Context) -> Result<Self, String> {
        Self::with_udp_address(
            context,
            "127.0.0.1:0".to_owned(),
            None,
            None,
            VideoDecoderPreference::Automatic,
        )
    }

    fn with_udp_address(
        context: egui::Context,
        udp_address: String,
        stun_server: Option<String>,
        turn_credentials: Option<TurnCredentials>,
        decoder_preference: VideoDecoderPreference,
    ) -> Result<Self, String> {
        tracing::info!(
            udp_address = %udp_address,
            stun_endpoint = %stun_server.as_deref().map(safe_stun_endpoint).unwrap_or_else(|| "(não configurado)".to_owned()),
            decoder_preference = decoder_preference.label(),
            "Criando sessão WebRTC para compartilhamento de tela"
        );
        let (commands_tx, commands_rx) = mpsc::unbounded_channel();
        let (events_tx, events_rx) = std_mpsc::channel();
        let remote_frame = Arc::new(Mutex::new(None));
        let remote_track = Arc::new(Mutex::new(None));
        let mut initial_metrics = SharedMetrics::default();
        initial_metrics.session_id = NEXT_SCREEN_SHARE_SESSION_ID.fetch_add(1, Ordering::Relaxed);
        *initial_metrics
            .decoder_preference
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            decoder_preference.label().to_owned();
        *initial_metrics
            .selected_ice_pair
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            "Par ICE selecionado: aguardando conexão".to_owned();
        *initial_metrics
            .rtc_outbound_summary
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            "RTP de saída: aguardando faixa".to_owned();
        *initial_metrics
            .rtc_inbound_summary
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            "RTP de entrada: aguardando faixa".to_owned();
        let metrics = Arc::new(initial_metrics);
        tracing::info!(
            screen_share_session = metrics.session_id,
            udp_address = %udp_address,
            "Sessão de tela criada para diagnóstico"
        );
        let worker_remote_frame = Arc::clone(&remote_frame);
        let worker_remote_track = Arc::clone(&remote_track);
        let worker_metrics = Arc::clone(&metrics);
        let worker = thread::Builder::new()
            .name("p2p-screen-share".to_owned())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        let _ = events_tx.send(ScreenShareEvent::Error(format!(
                            "Não foi possível iniciar a rede WebRTC: {error}"
                        )));
                        return;
                    }
                };
                runtime.block_on(run_session(
                    commands_rx,
                    events_tx,
                    context,
                    worker_remote_frame,
                    worker_remote_track,
                    udp_address,
                    stun_server,
                    turn_credentials,
                    worker_metrics,
                    decoder_preference,
                ));
            })
            .map_err(|error| format!("Não foi possível iniciar a sessão de tela: {error}"))?;

        Ok(Self {
            commands: commands_tx,
            events: events_rx,
            remote_frame,
            remote_track,
            metrics,
            worker: Some(worker),
        })
    }

    pub fn start_sending(&self, source: LatestFrame) -> Result<(), String> {
        self.commands
            .send(Command::StartSending(source))
            .map_err(|_| "A sessão WebRTC foi encerrada.".to_owned())
    }

    #[cfg(test)]
    fn request_keyframe_for_test(&self) -> Result<(), String> {
        if self
            .remote_track
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_none()
        {
            return Err("A faixa remota ainda nao esta disponivel para o teste PLI.".to_owned());
        }
        self.commands
            .send(Command::RequestKeyFrameForTest)
            .map_err(|_| "A sessÃ£o WebRTC foi encerrada.".to_owned())
    }

    pub fn handle_signal(&self, kind: SignalKind, payload: String) -> Result<(), String> {
        self.commands
            .send(Command::Signal { kind, payload })
            .map_err(|_| "A sessão WebRTC foi encerrada.".to_owned())
    }

    pub fn try_recv(&self) -> Option<ScreenShareEvent> {
        self.events.try_recv().ok()
    }

    pub fn latest_remote_frame(&self) -> Option<Arc<PreviewFrame>> {
        self.remote_frame
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub fn metrics(&self) -> ScreenShareMetrics {
        self.metrics.snapshot()
    }

    pub fn record_ui_texture_update(&self) {
        self.metrics.record_ui_texture_update();
    }

    pub fn take_performance_snapshot(&self) -> ScreenSharePerformanceSnapshot {
        self.metrics.take_performance_snapshot()
    }

    pub fn stop(mut self) {
        let _ = self.commands.send(Command::Stop);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Drop for ScreenShareSession {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Stop);
    }
}

struct PeerSession {
    connection: Arc<dyn PeerConnection>,
    pending_ice: Vec<RTCIceCandidateInit>,
    remote_description_set: bool,
    started_at: Instant,
    expects_inbound_video: bool,
    no_video_notice_sent: bool,
    metrics: Arc<SharedMetrics>,
    encoder_stop: Option<Arc<AtomicBool>>,
    encoder_source: Option<LatestFrame>,
    encoder_task: Option<TokioJoinHandle<Result<(), String>>>,
    sample_writer_task: Option<TokioJoinHandle<()>>,
    rtcp_feedback_task: Option<TokioJoinHandle<()>>,
    #[allow(dead_code)] // Usado pelo teste loopback para emitir o PLI de diagnóstico.
    remote_track: RemoteTrackStore,
    #[allow(dead_code)] // Usado pelo teste loopback para limitar pedidos explícitos de PLI.
    keyframe_request_limiter: KeyframeRequestLimiter,
}

#[derive(Clone)]
struct PeerEvents {
    events: std_mpsc::Sender<ScreenShareEvent>,
    context: egui::Context,
    remote_frame: RemoteFrameStore,
    remote_track: RemoteTrackStore,
    remote_frame_sequence: Arc<AtomicU64>,
    metrics: Arc<SharedMetrics>,
    stun_server: Option<String>,
    turn_enabled: bool,
    decoder_preference: VideoDecoderPreference,
}

#[async_trait::async_trait]
impl PeerConnectionEventHandler for PeerEvents {
    async fn on_ice_candidate(&self, event: RTCPeerConnectionIceEvent) {
        if event.candidate.address.is_empty() {
            return;
        }
        self.metrics
            .local_ice_candidates
            .fetch_add(1, Ordering::Relaxed);
        tracing::debug!(
            candidate_type = ?event.candidate.typ,
            address = %event.candidate.address,
            port = event.candidate.port,
            "Candidato ICE local descoberto; SDP/candidato completo omitido"
        );
        if event.candidate.typ == rtc::peer_connection::transport::RTCIceCandidateType::Srflx {
            self.metrics
                .local_srflx_candidates
                .fetch_add(1, Ordering::Relaxed);
        }
        if event.candidate.typ == rtc::peer_connection::transport::RTCIceCandidateType::Relay {
            self.metrics
                .local_relay_candidates
                .fetch_add(1, Ordering::Relaxed);
        }
        match event.candidate.to_json() {
            Ok(candidate) => match serde_json::to_string(&candidate) {
                Ok(payload) => {
                    let _ = self.events.send(ScreenShareEvent::Signal {
                        kind: SignalKind::IceCandidate,
                        payload,
                    });
                }
                Err(error) => {
                    let _ = self.events.send(ScreenShareEvent::Error(format!(
                        "Não foi possível preparar o candidato ICE: {error}"
                    )));
                }
            },
            Err(error) => {
                let _ = self.events.send(ScreenShareEvent::Error(format!(
                    "Não foi possível converter o candidato ICE: {error}"
                )));
            }
        }
        self.context.request_repaint();
    }

    async fn on_ice_candidate_error(
        &self,
        event: rtc::peer_connection::event::RTCPeerConnectionIceErrorEvent,
    ) {
        let is_turn = event.url.starts_with("turn:");
        let is_auth_error = matches!(event.error_code, 401 | 438 | 702);
        tracing::warn!(
            screen_share_session = self.metrics.session_id,
            ice_error_code = event.error_code,
            turn_server = is_turn,
            turn_enabled = self.turn_enabled,
            "Falha ao reunir candidato ICE; URL e credenciais omitidas"
        );
        let message = if is_turn && is_auth_error {
            "O servidor TURN recusou as credenciais temporárias (erro de autenticação ICE). A sala pode ter expirado; crie outra sala e tente novamente.".to_owned()
        } else if is_turn {
            format!(
                "Não foi possível obter um endereço de retransmissão TURN (erro ICE {}). Confira UDP 3478 e UDP 50000–50100 no roteador e no firewall do anfitrião.",
                event.error_code
            )
        } else if self.turn_enabled {
            format!(
                "Falha ao reunir candidato ICE via STUN (erro {}). O app ainda tentará TURN se o anfitrião o habilitou.",
                event.error_code
            )
        } else {
            format!(
                "Falha ao reunir candidato ICE via STUN (erro {}).",
                event.error_code
            )
        };
        let _ = self.events.send(ScreenShareEvent::State(message));
        self.context.request_repaint();
    }

    async fn on_ice_connection_state_change(
        &self,
        state: webrtc::peer_connection::RTCIceConnectionState,
    ) {
        tracing::info!(screen_share_session = self.metrics.session_id, state = ?state, "Estado ICE mudou");
        let status = match state {
            webrtc::peer_connection::RTCIceConnectionState::New => {
                "ICE aguardando candidatos do outro computador.".to_owned()
            }
            webrtc::peer_connection::RTCIceConnectionState::Checking => {
                "ICE verificando caminhos UDP entre os computadores…".to_owned()
            }
            webrtc::peer_connection::RTCIceConnectionState::Connected
            | webrtc::peer_connection::RTCIceConnectionState::Completed => {
                "ICE encontrou um caminho UDP; finalizando a conexão WebRTC…".to_owned()
            }
            webrtc::peer_connection::RTCIceConnectionState::Disconnected => {
                self.metrics.p2p_connected.store(false, Ordering::Relaxed);
                self.metrics.route.store(0, Ordering::Relaxed);
                *self
                    .metrics
                    .connected_at
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
                "Conexão ICE interrompida; aguardando recuperação…".to_owned()
            }
            webrtc::peer_connection::RTCIceConnectionState::Failed => {
                self.metrics.p2p_connected.store(false, Ordering::Relaxed);
                self.metrics.route.store(0, Ordering::Relaxed);
                *self
                    .metrics
                    .connected_at
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
                let message = if let Some(stun_server) = &self.stun_server {
                    let safe_stun_server = safe_stun_endpoint(stun_server);
                    let local = self.metrics.local_srflx_candidates.load(Ordering::Relaxed);
                    let remote = self.metrics.remote_srflx_candidates.load(Ordering::Relaxed);
                    if self.turn_enabled {
                        let local_relay =
                            self.metrics.local_relay_candidates.load(Ordering::Relaxed);
                        let remote_relay =
                            self.metrics.remote_relay_candidates.load(Ordering::Relaxed);
                        format!(
                            "O ICE não conectou. Candidatos STUN: {local} locais e {remote} remotos; candidatos TURN: {local_relay} locais e {remote_relay} remotos. Confira {safe_stun_server}, UDP 3478 e UDP 50000–50100 no anfitrião; se não houve candidato TURN, recrie a sala para renovar as credenciais."
                        )
                    } else {
                        format!(
                            "O ICE não encontrou um caminho direto pela internet. Candidatos públicos via STUN: {local} locais e {remote} recebidos do amigo. Confira a URI {safe_stun_server}, o firewall e UDP {MEDIA_UDP_PORT}; TURN está desativado nesta sala."
                        )
                    }
                } else {
                    format!(
                        "O ICE não encontrou um caminho UDP. Confira se o firewall dos dois PCs permite o aplicativo ou UDP {MEDIA_UDP_PORT} na rede privada."
                    )
                };
                tracing::error!(
                    screen_share_session = self.metrics.session_id,
                    local_srflx_candidates =
                        self.metrics.local_srflx_candidates.load(Ordering::Relaxed),
                    remote_srflx_candidates =
                        self.metrics.remote_srflx_candidates.load(Ordering::Relaxed),
                    local_relay_candidates =
                        self.metrics.local_relay_candidates.load(Ordering::Relaxed),
                    remote_relay_candidates =
                        self.metrics.remote_relay_candidates.load(Ordering::Relaxed),
                    stun_configured = self.stun_server.is_some(),
                    turn_enabled = self.turn_enabled,
                    "ICE falhou em estabelecer caminho UDP"
                );
                let _ = self.events.send(ScreenShareEvent::Error(message));
                self.context.request_repaint();
                return;
            }
            webrtc::peer_connection::RTCIceConnectionState::Closed => {
                self.metrics.p2p_connected.store(false, Ordering::Relaxed);
                self.metrics.route.store(0, Ordering::Relaxed);
                *self
                    .metrics
                    .connected_at
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
                "Conexão ICE encerrada.".to_owned()
            }
            _ => format!("Estado ICE: {state:?}."),
        };
        let _ = self.events.send(ScreenShareEvent::State(status));
        self.context.request_repaint();
    }

    async fn on_connection_state_change(&self, state: RTCPeerConnectionState) {
        tracing::info!(screen_share_session = self.metrics.session_id, state = ?state, "Estado da conexão WebRTC mudou");
        match state {
            RTCPeerConnectionState::Connected => {
                self.metrics.p2p_connected.store(true, Ordering::Relaxed);
                let mut connected_at = self
                    .metrics
                    .connected_at
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                connected_at.get_or_insert_with(Instant::now);
                let _ = self.events.send(ScreenShareEvent::State(
                    "Conexão WebRTC P2P estabelecida; aguardando os quadros de vídeo.".to_owned(),
                ));
            }
            RTCPeerConnectionState::Failed => {}
            RTCPeerConnectionState::Disconnected | RTCPeerConnectionState::Closed => {
                self.metrics.p2p_connected.store(false, Ordering::Relaxed);
                self.metrics.route.store(0, Ordering::Relaxed);
                *self
                    .metrics
                    .connected_at
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
                let _ = self.events.send(ScreenShareEvent::ConnectionClosed);
            }
            _ => {
                let _ = self.events.send(ScreenShareEvent::State(
                    "Negociando conexão P2P…".to_owned(),
                ));
            }
        }
        self.context.request_repaint();
    }

    async fn on_track(&self, track: Arc<dyn TrackRemote>) {
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
            let mut keyframe_request_limiter = KeyframeRequestLimiter::default();
            let mut flush_pending = tokio::time::interval(Duration::from_millis(10));
            flush_pending.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

            loop {
                tokio::select! {
                    _ = flush_pending.tick() => {
                        let expired = sequence_tracker.expire(Instant::now());
                        if expired.confirmed_missing_packets > 0 {
                            metrics.record_sequence_update(expired);
                        }
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
                                let sequence_update = sequence_tracker.observe(
                                    packet.header.sequence_number,
                                    Instant::now(),
                                );
                                metrics.record_sequence_update(sequence_update);
                                metrics.record_received_packet(&packet);
                                if sequence_update.observed_gap_packets > 0 {
                                    metrics.record_sequence_gap_resync();
                                    assembler.frames.clear();
                                    begin_stream_resync(
                                        &decoder_generation,
                                        &metrics,
                                        &keyframe_request_tx,
                                        PliReason::SequenceGap,
                                    );
                                    context.request_repaint();
                                }
                                if let Some(error) = assembler.push(packet) {
                                    metrics.record_assembly_error(error);
                                    begin_stream_resync(
                                        &decoder_generation,
                                        &metrics,
                                        &keyframe_request_tx,
                                        PliReason::AssemblyError,
                                    );
                                    context.request_repaint();
                                }
                            }
                            TrackRemoteEvent::OnEnded | TrackRemoteEvent::OnEnding => break,
                            _ => {}
                        }
                    }
                }

                for result in assembler.take_ready(Instant::now()) {
                    match result {
                        Ok(access_unit) => {
                            metrics.record_assembled_access_unit(&access_unit);
                            match decoder_tx.try_send(QueuedAccessUnit {
                                generation: decoder_generation.load(Ordering::Relaxed),
                                bytes: access_unit,
                            }) {
                                Ok(()) => {}
                                Err(std_mpsc::TrySendError::Full(_)) => {
                                    metrics.record_decoder_queue_drop();
                                    begin_stream_resync(
                                        &decoder_generation,
                                        &metrics,
                                        &keyframe_request_tx,
                                        PliReason::DecoderQueueFull,
                                    );
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
                            begin_stream_resync(
                                &decoder_generation,
                                &metrics,
                                &keyframe_request_tx,
                                PliReason::AssemblyError,
                            );
                            context.request_repaint();
                        }
                    }
                }
            }
        });
    }
}

async fn run_session(
    mut commands: mpsc::UnboundedReceiver<Command>,
    events: std_mpsc::Sender<ScreenShareEvent>,
    context: egui::Context,
    remote_frame: RemoteFrameStore,
    remote_track: RemoteTrackStore,
    udp_address: String,
    stun_server: Option<String>,
    turn_credentials: Option<TurnCredentials>,
    metrics: Arc<SharedMetrics>,
    decoder_preference: VideoDecoderPreference,
) {
    let mut active_peer: Option<PeerSession> = None;
    let mut ice_before_peer = Vec::new();
    let remote_frame_sequence = Arc::new(AtomicU64::new(0));
    let mut connection_check = tokio::time::interval(CONNECTION_CHECK_INTERVAL);
    connection_check.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut last_route_check = Instant::now();
    let mut previous_peer_stats = None;

    loop {
        tokio::select! {
            _ = connection_check.tick() => {
                let Some(peer) = active_peer.as_mut() else {
                    continue;
                };
                if peer.metrics.p2p_connected.load(Ordering::Relaxed)
                    && last_route_check.elapsed() >= Duration::from_secs(1)
                {
                    last_route_check = Instant::now();
                    let report = peer
                        .connection
                        .get_stats(Instant::now(), StatsSelector::None)
                        .await;
                    let current_stats = peer_stats_snapshot(&report);
                    let route = current_stats.route;
                    let pair_changed = peer
                        .metrics
                        .update_transport_diagnostics(current_stats, &mut previous_peer_stats);
                    if let Some(route) = route {
                        let route_code = match route {
                            MediaRoute::Direct => 1,
                            MediaRoute::Turn => 2,
                        };
                        let previous = peer.metrics.route.swap(route_code, Ordering::Relaxed);
                        if previous != route_code {
                            tracing::info!(
                                screen_share_session = peer.metrics.session_id,
                                track_ssrc = peer.metrics.track_ssrc.load(Ordering::Relaxed),
                                ?route,
                                "Rota ICE de mídia selecionada"
                            );
                            let label = match route {
                                MediaRoute::Direct => "Conexão direta P2P selecionada.",
                                MediaRoute::Turn => "Conexão retransmitida pelo servidor TURN do anfitrião.",
                            };
                            let _ = events.send(ScreenShareEvent::State(label.to_owned()));
                        }
                    } else {
                        let previous = peer.metrics.route.swap(0, Ordering::Relaxed);
                        if previous != 0 {
                            tracing::info!(
                                screen_share_session = peer.metrics.session_id,
                                "Metadados da rota ICE indisponíveis; classificação atualizada para desconhecida"
                            );
                            let _ = events.send(ScreenShareEvent::State(
                                "Conexão P2P ativa; rota da mídia desconhecida nos metadados ICE.".to_owned(),
                            ));
                        }
                    }
                    if pair_changed {
                        let diagnostics = peer.metrics.snapshot();
                        tracing::info!(
                            screen_share_session = diagnostics.session_id,
                            track_ssrc = diagnostics.track_ssrc.unwrap_or_default(),
                            selected_ice_pair = %diagnostics.selected_ice_pair,
                            "Par ICE de mídia selecionado ou alterado"
                        );
                    }
                    context.request_repaint();
                }

                let connection_timeout = if stun_server.is_some() || turn_credentials.is_some() {
                    INTERNET_PEER_CONNECTION_TIMEOUT
                } else {
                    PEER_CONNECTION_TIMEOUT
                };
                if !peer.metrics.p2p_connected.load(Ordering::Relaxed)
                    && peer.started_at.elapsed() >= connection_timeout
                {
                    let message = if turn_credentials.is_some() {
                        let local_relay = peer.metrics.local_relay_candidates.load(Ordering::Relaxed);
                        let remote_relay = peer.metrics.remote_relay_candidates.load(Ordering::Relaxed);
                        let local_srflx = peer.metrics.local_srflx_candidates.load(Ordering::Relaxed);
                        let remote_srflx = peer.metrics.remote_srflx_candidates.load(Ordering::Relaxed);
                        format!(
                            "A conexão WebRTC não foi estabelecida em {connection_timeout:?}. O ICE reuniu {local_relay} candidatos TURN locais e {remote_relay} remotos; STUN: {local_srflx} locais e {remote_srflx} remotos. Confira o endereço público, UDP 3478, UDP 50000–50100 e as regras do firewall/roteador do anfitrião."
                        )
                    } else if let Some(stun_server) = &stun_server {
                        let local = peer.metrics.local_srflx_candidates.load(Ordering::Relaxed);
                        let remote = peer.metrics.remote_srflx_candidates.load(Ordering::Relaxed);
                        format!(
                            "A conexão WebRTC não foi estabelecida em {connection_timeout:?}. Candidatos públicos via STUN: {local} locais e {remote} recebidos do amigo. Confira a URI {stun_server}, as regras de NAT e o firewall/UDP {MEDIA_UDP_PORT} dos dois PCs. TURN está desativado nesta sala."
                        )
                    } else {
                        format!(
                            "A conexão P2P não foi estabelecida em {connection_timeout:?}. Confira se os dois PCs permitem UDP {MEDIA_UDP_PORT} no firewall do Windows (perfil de rede privada)."
                        )
                    };
                    let _ = events.send(ScreenShareEvent::Error(message));
                    if let Some(peer) = active_peer.take() {
                        close_peer(peer).await;
                    }
                    continue;
                }

                if peer.expects_inbound_video && !peer.no_video_notice_sent {
                    let connected_at = *peer.metrics.connected_at
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    if connected_at.is_some_and(|time| {
                        time.elapsed() >= FIRST_VIDEO_FRAME_TIMEOUT
                            && peer.metrics.decoded_frames.load(Ordering::Relaxed) == 0
                    }) {
                        let packets = peer.metrics.received_packets.load(Ordering::Relaxed);
                        let decode_errors = peer.metrics.decode_errors.load(Ordering::Relaxed);
                        let message = if packets == 0 {
                            format!(
                                "P2P conectado, mas nenhum pacote de vídeo chegou em 5 segundos. Confirme que o emissor está enviando e que UDP {MEDIA_UDP_PORT} está permitido no firewall dos dois PCs."
                            )
                        } else if decode_errors > 0 {
                            let detail = peer.metrics.last_decode_error
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                                .clone()
                                .unwrap_or_else(|| "erro de decodificação H.264".to_owned());
                            format!(
                                "P2P conectado e {packets} pacotes chegaram, mas não foi possível decodificar a tela ({decode_errors} erros): {detail}"
                            )
                        } else {
                            format!(
                                "P2P conectado e {packets} pacotes chegaram, mas nenhum quadro H.264 completo foi decodificado."
                            )
                        };
                        let _ = events.send(ScreenShareEvent::State(message));
                        peer.no_video_notice_sent = true;
                    }
                }
            }
            command = commands.recv() => {
                let Some(command) = command else { break };
        match command {
            Command::StartSending(source) => {
                if active_peer.is_some() {
                    let _ = events.send(ScreenShareEvent::Error(
                        "Já existe uma sessão de compartilhamento ativa.".to_owned(),
                    ));
                    continue;
                }
                match create_sender(
                    source,
                    &events,
                    context.clone(),
                    Arc::clone(&remote_frame),
                    Arc::clone(&remote_track),
                    Arc::clone(&remote_frame_sequence),
                    Arc::clone(&metrics),
                    &udp_address,
                    stun_server.as_deref(),
                    turn_credentials.as_ref(),
                    decoder_preference,
                )
                .await
                {
                    Ok(peer) => active_peer = Some(peer),
                    Err(error) => {
                        let _ = events.send(ScreenShareEvent::Error(error));
                    }
                }
            }
            Command::Signal { kind, payload } => match kind {
                SignalKind::Offer => {
                    if active_peer.is_some() {
                        let _ = events.send(ScreenShareEvent::Error(
                            "A sessão já está ocupada com outra negociação de tela.".to_owned(),
                        ));
                        continue;
                    }
                    match create_receiver(
                        payload,
                        &events,
                        context.clone(),
                        Arc::clone(&remote_frame),
                        Arc::clone(&remote_track),
                        Arc::clone(&remote_frame_sequence),
                        Arc::clone(&metrics),
                        &udp_address,
                        stun_server.as_deref(),
                        turn_credentials.as_ref(),
                        decoder_preference,
                    )
                    .await
                    {
                        Ok(mut peer) => {
                            for candidate in ice_before_peer.drain(..) {
                                peer.pending_ice.push(candidate);
                            }
                            apply_pending_ice(&mut peer, &events).await;
                            active_peer = Some(peer);
                        }
                        Err(error) => {
                            let _ = events.send(ScreenShareEvent::Error(error));
                        }
                    }
                }
                SignalKind::Answer => {
                    let Some(peer) = active_peer.as_mut() else {
                        let _ = events.send(ScreenShareEvent::Error(
                            "A resposta WebRTC chegou antes da oferta local.".to_owned(),
                        ));
                        continue;
                    };
                    let result = async {
                        let answer = serde_json::from_str(&payload)
                            .map_err(|error| format!("Resposta SDP inválida: {error}"))?;
                        peer.connection
                            .set_remote_description(answer)
                            .await
                            .map_err(|error| {
                                format!("Não foi possível aplicar a resposta SDP: {error}")
                            })?;
                        peer.remote_description_set = true;
                        Ok::<(), String>(())
                    }
                    .await;
                    if let Err(error) = result {
                        let _ = events.send(ScreenShareEvent::Error(error));
                    } else {
                        apply_pending_ice(peer, &events).await;
                    }
                }
                SignalKind::IceCandidate => {
                    match serde_json::from_str::<RTCIceCandidateInit>(&payload) {
                        Ok(candidate) => {
                            metrics
                                .remote_ice_candidates
                                .fetch_add(1, Ordering::Relaxed);
                            if candidate.candidate.contains(" typ srflx ") {
                                metrics
                                    .remote_srflx_candidates
                                    .fetch_add(1, Ordering::Relaxed);
                            }
                            if candidate.candidate.contains(" typ relay ") {
                                metrics
                                    .remote_relay_candidates
                                    .fetch_add(1, Ordering::Relaxed);
                            }
                            if let Some(peer) = active_peer.as_mut() {
                                if peer.remote_description_set {
                                    if let Err(error) =
                                        peer.connection.add_ice_candidate(candidate).await
                                    {
                                        let _ = events.send(ScreenShareEvent::Error(format!(
                                            "Não foi possível adicionar o candidato ICE: {error}"
                                        )));
                                    }
                                } else {
                                    peer.pending_ice.push(candidate);
                                }
                            } else {
                                ice_before_peer.push(candidate);
                            }
                        }
                        Err(error) => {
                            let _ = events.send(ScreenShareEvent::Error(format!(
                                "Candidato ICE inválido: {error}"
                            )));
                        }
                    }
                }
                _ => {}
            },
            #[cfg(test)]
            Command::RequestKeyFrameForTest => {
                if let Some(peer) = active_peer.as_mut() {
                    let track = peer
                        .remote_track
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .clone();
                    if let Some(track) = track {
                        send_picture_loss_indication(
                            track.as_ref(),
                            &peer.metrics,
                            &mut peer.keyframe_request_limiter,
                            PliReason::Explicit,
                        )
                        .await;
                    }
                }
            }
            Command::Stop => break,
        }
            }
        }
    }

    if let Some(peer) = active_peer {
        close_peer(peer).await;
    }
}

fn peer_stats_snapshot(report: &RTCStatsReport) -> PeerStatsSnapshot {
    let mut snapshot = PeerStatsSnapshot {
        selected_pair_summary: "Par ICE selecionado: desconhecido (metadados indisponíveis)"
            .to_owned(),
        outbound_summary: "RTP de saída: estatísticas ainda indisponíveis".to_owned(),
        inbound_summary: "RTP de entrada: estatísticas ainda indisponíveis".to_owned(),
        ..PeerStatsSnapshot::default()
    };

    let selected_pair_id = report.iter().find_map(|entry| match entry {
        RTCStatsReportEntry::Transport(transport)
            if !transport.selected_candidate_pair_id.is_empty() =>
        {
            Some(transport.selected_candidate_pair_id.as_str())
        }
        _ => None,
    });
    if let Some(selected_pair_id) = selected_pair_id {
        if let Some(RTCStatsReportEntry::IceCandidatePair(pair)) = report.get(selected_pair_id) {
            let local = match report.get(&pair.local_candidate_id) {
                Some(RTCStatsReportEntry::LocalCandidate(candidate)) => Some(candidate),
                _ => None,
            };
            let remote = match report.get(&pair.remote_candidate_id) {
                Some(RTCStatsReportEntry::RemoteCandidate(candidate)) => Some(candidate),
                _ => None,
            };
            let route = media_route_from_candidate_types(
                local.map(|candidate| candidate.candidate_type),
                remote.map(|candidate| candidate.candidate_type),
            );
            let local_addr = local
                .and_then(|candidate| candidate.address.as_deref())
                .unwrap_or("indisponível");
            let remote_addr = remote
                .and_then(|candidate| candidate.address.as_deref())
                .unwrap_or("indisponível");
            let local_adapter = adapter_name_for_ip(local_addr);
            snapshot.selected_pair_key = selected_pair_id.to_owned();
            snapshot.route = route;
            snapshot.pair_packets_sent = pair.packets_sent;
            snapshot.pair_packets_received = pair.packets_received;
            snapshot.pair_bytes_sent = pair.bytes_sent;
            snapshot.pair_bytes_received = pair.bytes_received;
            snapshot.pair_packets_discarded_on_send = pair.packets_discarded_on_send;
            snapshot.pair_rtt_ms = pair.current_round_trip_time * 1000.0;
            snapshot.selected_pair_summary = format!(
                "ICE {}: local {}/{}/{}:{} ({local_adapter}) -> remoto {}/{}/{}:{}; par {}/{} pacotes e {}/{} bytes; descartados no envio {}; RTT {:.1} ms",
                media_route_label(route),
                local
                    .map(|candidate| format!("{:?}", candidate.candidate_type))
                    .unwrap_or_else(|| "?".to_owned()),
                local
                    .map(|candidate| candidate.protocol.as_str())
                    .unwrap_or("?"),
                local_addr,
                local.map(|candidate| candidate.port).unwrap_or_default(),
                remote
                    .map(|candidate| format!("{:?}", candidate.candidate_type))
                    .unwrap_or_else(|| "?".to_owned()),
                remote
                    .map(|candidate| candidate.protocol.as_str())
                    .unwrap_or("?"),
                remote_addr,
                remote.map(|candidate| candidate.port).unwrap_or_default(),
                pair.packets_sent,
                pair.packets_received,
                pair.bytes_sent,
                pair.bytes_received,
                pair.packets_discarded_on_send,
                snapshot.pair_rtt_ms,
            );
        } else {
            snapshot.selected_pair_key = selected_pair_id.to_owned();
            snapshot.selected_pair_summary = format!(
                "Par ICE selecionado: desconhecido (ID {selected_pair_id}; metadados do par ainda indisponíveis)"
            );
        }
    }

    if let Some(outbound) = report.iter().find_map(|entry| match entry {
        RTCStatsReportEntry::OutboundRtp(stats)
            if stats.sent_rtp_stream_stats.rtp_stream_stats.kind == RtpCodecKind::Video =>
        {
            Some(stats)
        }
        _ => None,
    }) {
        snapshot.outbound_packets = outbound.sent_rtp_stream_stats.packets_sent;
        snapshot.outbound_bytes = outbound.sent_rtp_stream_stats.bytes_sent;
        snapshot.outbound_frames_encoded = outbound.frames_encoded;
        snapshot.outbound_frames_sent = outbound.frames_sent;
        snapshot.outbound_ssrc = outbound.sent_rtp_stream_stats.rtp_stream_stats.ssrc;
        snapshot.outbound_summary = format!(
            "RTP de saída: {} pacotes / {} bytes; frames codificados/enviados {}/{}; SSRC {}; encoder RTC {}",
            snapshot.outbound_packets,
            snapshot.outbound_bytes,
            snapshot.outbound_frames_encoded,
            snapshot.outbound_frames_sent,
            snapshot.outbound_ssrc,
            outbound.encoder_implementation,
        );
    }

    if let Some(inbound) = report.iter().find_map(|entry| match entry {
        RTCStatsReportEntry::InboundRtp(stats)
            if stats.received_rtp_stream_stats.rtp_stream_stats.kind == RtpCodecKind::Video =>
        {
            Some(stats)
        }
        _ => None,
    }) {
        let received = &inbound.received_rtp_stream_stats;
        snapshot.inbound_packets = received.packets_received;
        snapshot.inbound_bytes = inbound.bytes_received;
        snapshot.inbound_packets_lost = received.packets_lost;
        snapshot.inbound_jitter_ms = rtp_jitter_ticks_to_ms(received.jitter, 90_000.0);
        snapshot.inbound_frames_received = inbound.frames_received;
        snapshot.inbound_frames_decoded = inbound.frames_decoded;
        snapshot.inbound_frames_rendered = inbound.frames_rendered;
        snapshot.inbound_frames_dropped = inbound.frames_dropped;
        snapshot.inbound_packets_discarded = inbound.packets_discarded;
        snapshot.inbound_ssrc = received.rtp_stream_stats.ssrc;
        snapshot.inbound_summary = format!(
            "RTP de entrada: {} pacotes / {} bytes; perda reportada {}; jitter {:.1} ms; frames recebidos/decodificados/renderizados/descartados {}/{}/{}/{}; descartados no jitter buffer {}; SSRC {}; decoder RTC {}",
            snapshot.inbound_packets,
            snapshot.inbound_bytes,
            snapshot.inbound_packets_lost,
            snapshot.inbound_jitter_ms,
            snapshot.inbound_frames_received,
            snapshot.inbound_frames_decoded,
            snapshot.inbound_frames_rendered,
            snapshot.inbound_frames_dropped,
            snapshot.inbound_packets_discarded,
            snapshot.inbound_ssrc,
            inbound.decoder_implementation,
        );
    }

    snapshot
}

fn media_route_from_candidate_types(
    local: Option<rtc::peer_connection::transport::RTCIceCandidateType>,
    remote: Option<rtc::peer_connection::transport::RTCIceCandidateType>,
) -> Option<MediaRoute> {
    use rtc::peer_connection::transport::RTCIceCandidateType;

    let (Some(local), Some(remote)) = (local, remote) else {
        return None;
    };
    Some(
        if local == RTCIceCandidateType::Relay || remote == RTCIceCandidateType::Relay {
            MediaRoute::Turn
        } else {
            MediaRoute::Direct
        },
    )
}

fn media_route_label(route: Option<MediaRoute>) -> &'static str {
    match route {
        Some(MediaRoute::Direct) => "Direto (P2P)",
        Some(MediaRoute::Turn) => "Retransmitido (TURN)",
        None => "desconhecido",
    }
}

fn rtp_jitter_ticks_to_ms(jitter_ticks: f64, clock_rate_hz: f64) -> f64 {
    if clock_rate_hz.is_finite() && clock_rate_hz > 0.0 && jitter_ticks.is_finite() {
        jitter_ticks * 1000.0 / clock_rate_hz
    } else {
        0.0
    }
}

fn adapter_name_for_ip(address: &str) -> String {
    let Ok(ip) = address.parse::<IpAddr>() else {
        return "adaptador não identificado".to_owned();
    };
    #[cfg(windows)]
    {
        static ADAPTERS: OnceLock<HashMap<IpAddr, String>> = OnceLock::new();
        let adapters = ADAPTERS.get_or_init(|| {
            ipconfig::get_adapters()
                .unwrap_or_default()
                .into_iter()
                .flat_map(|adapter| {
                    let name = adapter.friendly_name().to_owned();
                    adapter
                        .ip_addresses()
                        .iter()
                        .copied()
                        .map(move |address| (address, name.clone()))
                        .collect::<Vec<_>>()
                })
                .collect()
        });
        return adapters
            .get(&ip)
            .cloned()
            .unwrap_or_else(|| "adaptador não identificado".to_owned());
    }
    #[cfg(not(windows))]
    {
        let _ = ip;
        "adaptador não identificado".to_owned()
    }
}

async fn create_peer(
    events: &std_mpsc::Sender<ScreenShareEvent>,
    context: egui::Context,
    remote_frame: RemoteFrameStore,
    remote_track: RemoteTrackStore,
    remote_frame_sequence: Arc<AtomicU64>,
    metrics: Arc<SharedMetrics>,
    udp_address: &str,
    stun_server: Option<&str>,
    turn_credentials: Option<&TurnCredentials>,
    decoder_preference: VideoDecoderPreference,
) -> Result<Arc<dyn PeerConnection>, String> {
    let video_codec = RTCRtpCodecParameters {
        rtp_codec: RTCRtpCodec {
            mime_type: MIME_TYPE_H264.to_owned(),
            clock_rate: VIDEO_CLOCK_RATE,
            channels: 0,
            sdp_fmtp_line: "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f"
                .to_owned(),
            rtcp_feedback: vec![RTCPFeedback {
                typ: "nack".to_owned(),
                parameter: "pli".to_owned(),
            }],
        },
        payload_type: VIDEO_PAYLOAD_TYPE,
        ..Default::default()
    };
    let mut media_engine = MediaEngine::default();
    media_engine
        .register_codec(video_codec, RtpCodecKind::Video)
        .map_err(|error| format!("Não foi possível registrar H.264 no WebRTC: {error}"))?;
    let interceptors = register_default_interceptors(Registry::new(), &mut media_engine)
        .map_err(|error| format!("Não foi possível preparar o WebRTC: {error}"))?;
    let interceptors = interceptors.with(Slot::Custom(14_000), PliForwarder::default());
    let handler = Arc::new(PeerEvents {
        events: events.clone(),
        context,
        remote_frame,
        remote_track,
        remote_frame_sequence,
        metrics,
        stun_server: stun_server.map(str::to_owned),
        turn_enabled: turn_credentials.is_some(),
        decoder_preference,
    });
    let mut configuration = RTCConfigurationBuilder::new();
    let mut ice_servers = Vec::new();
    if let Some(stun_server) = stun_server {
        let validated_stun = validate_stun_uri(stun_server)?;
        ice_servers.push(RTCIceServer {
            urls: vec![validated_stun],
            ..Default::default()
        });
    }
    if let Some(credentials) = turn_credentials {
        if !credentials.url.starts_with("turn:")
            || !credentials.url.contains("?transport=udp")
            || credentials.url.chars().any(char::is_whitespace)
            || credentials.url.contains('@')
            || credentials.username.is_empty()
            || credentials.credential.is_empty()
        {
            return Err("A configuração TURN recebida do anfitrião é inválida.".to_owned());
        }
        let turn_server = RTCIceServer {
            urls: vec![credentials.url.clone()],
            username: credentials.username.clone(),
            credential: credentials.credential.clone(),
            ..Default::default()
        };
        turn_server
            .urls()
            .map_err(|error| format!("A URL do servidor TURN é inválida: {error}"))?;
        ice_servers.push(turn_server);
    }
    if !ice_servers.is_empty() {
        configuration = configuration.with_ice_servers(ice_servers);
    }
    let connection = PeerConnectionBuilder::new()
        .with_configuration(configuration.build())
        .with_media_engine(media_engine)
        .with_interceptor_registry(interceptors)
        .with_handler(handler)
        .with_runtime(Arc::new(TokioRuntime))
        .with_udp_addrs(vec![udp_address.to_owned()])
        .build()
        .await
        .map_err(|error| {
            if udp_address.ends_with(&format!(":{MEDIA_UDP_PORT}")) {
                format!(
                    "Não foi possível abrir UDP {MEDIA_UDP_PORT} para o compartilhamento. Verifique se a porta está livre e permita o aplicativo ou UDP {MEDIA_UDP_PORT} no firewall do Windows: {error}"
                )
            } else {
                format!("Não foi possível criar a conexão WebRTC P2P: {error}")
            }
        })?;
    Ok(Arc::new(connection))
}

async fn create_sender(
    source: LatestFrame,
    events: &std_mpsc::Sender<ScreenShareEvent>,
    context: egui::Context,
    remote_frame: RemoteFrameStore,
    remote_track: RemoteTrackStore,
    remote_frame_sequence: Arc<AtomicU64>,
    metrics: Arc<SharedMetrics>,
    udp_address: &str,
    stun_server: Option<&str>,
    turn_credentials: Option<&TurnCredentials>,
    decoder_preference: VideoDecoderPreference,
) -> Result<PeerSession, String> {
    let connection = create_peer(
        events,
        context,
        remote_frame,
        Arc::clone(&remote_track),
        remote_frame_sequence,
        Arc::clone(&metrics),
        udp_address,
        stun_server,
        turn_credentials,
        decoder_preference,
    )
    .await?;
    let codec = RTCRtpCodec {
        mime_type: MIME_TYPE_H264.to_owned(),
        clock_rate: VIDEO_CLOCK_RATE,
        channels: 0,
        sdp_fmtp_line: "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f"
            .to_owned(),
        rtcp_feedback: vec![RTCPFeedback {
            typ: "nack".to_owned(),
            parameter: "pli".to_owned(),
        }],
    };
    let ssrc = unique_ssrc();
    let track = Arc::new(
        TrackLocalStaticSample::new(
            Instant::now(),
            MediaStreamTrack::new(
                "p2p-screen-stream".to_owned(),
                "p2p-screen-track".to_owned(),
                "Tela compartilhada".to_owned(),
                RtpCodecKind::Video,
                vec![RTCRtpEncodingParameters {
                    rtp_coding_parameters: RTCRtpCodingParameters {
                        ssrc: Some(ssrc),
                        ..Default::default()
                    },
                    codec,
                    ..Default::default()
                }],
            ),
        )
        .map_err(|error| format!("Não foi possível criar a trilha de vídeo: {error}"))?,
    );
    connection
        .add_track(Arc::clone(&track) as Arc<dyn TrackLocal>)
        .await
        .map_err(|error| format!("Não foi possível adicionar a tela à conexão P2P: {error}"))?;

    let offer = connection
        .create_offer(None)
        .await
        .map_err(|error| format!("Não foi possível criar a oferta WebRTC: {error}"))?;
    connection
        .set_local_description(offer)
        .await
        .map_err(|error| format!("Não foi possível iniciar a negociação WebRTC: {error}"))?;
    let local_description = connection
        .local_description()
        .await
        .ok_or_else(|| "O WebRTC não gerou a descrição local.".to_owned())?;
    let payload = serde_json::to_string(&local_description)
        .map_err(|error| format!("Não foi possível serializar a oferta WebRTC: {error}"))?;
    events
        .send(ScreenShareEvent::Signal {
            kind: SignalKind::Offer,
            payload,
        })
        .map_err(|_| "A interface encerrou a sessão de tela.".to_owned())?;

    // Preserve the order of encoded H.264 frames: P-frames depend on earlier frames.
    // The source capture already keeps only its newest raw frame, so a tiny bounded queue
    // limits latency without replacing encoded reference frames.
    let (sample_tx, sample_rx) = mpsc::channel::<EncodedFrame>(1);
    let encoder_stop = Arc::new(AtomicBool::new(false));
    let encoder_stop_worker = Arc::clone(&encoder_stop);
    let encoder_source = source.clone();
    let force_keyframe = Arc::new(AtomicBool::new(false));
    let encoder_force_keyframe = Arc::clone(&force_keyframe);
    let encoder_events = events.clone();
    let encoder_metrics = Arc::clone(&metrics);
    let encoder_task = tokio::task::spawn_blocking(move || {
        match encode_latest_frames(
            source,
            sample_tx,
            encoder_stop_worker,
            encoder_metrics,
            encoder_force_keyframe,
        ) {
            Ok(()) => Ok(()),
            Err(error) => {
                let _ = encoder_events.send(ScreenShareEvent::Error(error.clone()));
                Err(error)
            }
        }
    });
    let writer_track = Arc::clone(&track);
    let writer_events = events.clone();
    let writer_metrics = Arc::clone(&metrics);
    let sample_writer_task = tokio::spawn(async move {
        let Some(ssrc) = writer_track.ssrcs().await.first().copied() else {
            let _ = writer_events.send(ScreenShareEvent::Error(
                "A trilha H.264 não recebeu um identificador RTP.".to_owned(),
            ));
            return;
        };
        writer_metrics.set_track_ssrc(ssrc);
        tracing::info!(
            screen_share_session = writer_metrics.session_id,
            track_ssrc = ssrc,
            "Faixa RTP de vídeo local identificada"
        );
        let mut sample_rx = sample_rx;
        while let Some(encoded_frame) = sample_rx.recv().await {
            let sample_bytes = encoded_frame.bytes.len() as u64;
            let frame_kind = encoded_frame.kind;
            let sample = Sample {
                data: Bytes::from(encoded_frame.bytes),
                duration: FRAME_DURATION,
                ..Sample::new(Instant::now())
            };
            let write_started_at = Instant::now();
            let write_result = writer_track
                .sample_writer(ssrc, VIDEO_PAYLOAD_TYPE)
                .write_sample(&sample)
                .await;
            writer_metrics.interval_write_sample_nanos.fetch_add(
                write_started_at.elapsed().as_nanos() as u64,
                Ordering::Relaxed,
            );
            writer_metrics
                .interval_write_sample_samples
                .fetch_add(1, Ordering::Relaxed);
            if let Err(error) = write_result {
                writer_metrics
                    .interval_write_sample_failures
                    .fetch_add(1, Ordering::Relaxed);
                tracing::error!(
                    screen_share_session = writer_metrics.session_id,
                    track_ssrc = ssrc,
                    ?frame_kind,
                    error = %error,
                    "TrackLocal recusou amostra H.264 antes do envio RTP"
                );
                let _ = writer_events.send(ScreenShareEvent::Error(format!(
                    "Falha ao enviar um quadro H.264 pela conexão P2P: {error}"
                )));
                break;
            } else {
                writer_metrics
                    .interval_write_sample_bytes
                    .fetch_add(sample_bytes, Ordering::Relaxed);
                writer_metrics.record_sent_frame(frame_kind);
            }
        }
    });

    let feedback_track = Arc::clone(&track);
    let feedback_metrics = Arc::clone(&metrics);
    let feedback_force_keyframe = Arc::clone(&force_keyframe);
    let rtcp_feedback_task = tokio::spawn(async move {
        loop {
            let Some(event) = feedback_track.poll().await else {
                // TrackLocal::poll returns None while the track is not bound yet. This
                // task starts before SDP negotiation, so retry until WebRTC binds it.
                tokio::time::sleep(Duration::from_millis(20)).await;
                continue;
            };
            let packets = match event {
                TrackLocalEvent::OnRtcpPacket(packets) => packets,
                _ => continue,
            };
            let pli_count = packets
                .iter()
                .filter(|packet| packet.as_any().is::<PictureLossIndication>())
                .count();
            if pli_count == 0 {
                continue;
            }
            for _ in 0..pli_count {
                feedback_metrics.record_pli_received();
            }
            feedback_force_keyframe.store(true, Ordering::Relaxed);
            tracing::debug!(
                pli_packets = pli_count,
                "Pedido PLI recebido; será solicitado IDR ao codificador"
            );
        }
    });

    let _ = events.send(ScreenShareEvent::State(
        "Oferta enviada; aguardando conexão P2P com o participante.".to_owned(),
    ));
    Ok(PeerSession {
        connection,
        pending_ice: Vec::new(),
        remote_description_set: false,
        started_at: Instant::now(),
        expects_inbound_video: false,
        no_video_notice_sent: false,
        metrics,
        encoder_stop: Some(encoder_stop),
        encoder_source: Some(encoder_source),
        encoder_task: Some(encoder_task),
        sample_writer_task: Some(sample_writer_task),
        rtcp_feedback_task: Some(rtcp_feedback_task),
        remote_track,
        keyframe_request_limiter: KeyframeRequestLimiter::default(),
    })
}

async fn create_receiver(
    payload: String,
    events: &std_mpsc::Sender<ScreenShareEvent>,
    context: egui::Context,
    remote_frame: RemoteFrameStore,
    remote_track: RemoteTrackStore,
    remote_frame_sequence: Arc<AtomicU64>,
    metrics: Arc<SharedMetrics>,
    udp_address: &str,
    stun_server: Option<&str>,
    turn_credentials: Option<&TurnCredentials>,
    decoder_preference: VideoDecoderPreference,
) -> Result<PeerSession, String> {
    let connection = create_peer(
        events,
        context,
        remote_frame,
        Arc::clone(&remote_track),
        remote_frame_sequence,
        Arc::clone(&metrics),
        udp_address,
        stun_server,
        turn_credentials,
        decoder_preference,
    )
    .await?;
    let offer =
        serde_json::from_str(&payload).map_err(|error| format!("Oferta SDP inválida: {error}"))?;
    connection
        .set_remote_description(offer)
        .await
        .map_err(|error| format!("Não foi possível aplicar a oferta WebRTC: {error}"))?;
    let answer = connection
        .create_answer(None)
        .await
        .map_err(|error| format!("Não foi possível criar a resposta WebRTC: {error}"))?;
    connection
        .set_local_description(answer)
        .await
        .map_err(|error| format!("Não foi possível iniciar a resposta WebRTC: {error}"))?;
    let local_description = connection
        .local_description()
        .await
        .ok_or_else(|| "O WebRTC não gerou a descrição de resposta.".to_owned())?;
    let answer_payload = serde_json::to_string(&local_description)
        .map_err(|error| format!("Não foi possível serializar a resposta WebRTC: {error}"))?;
    events
        .send(ScreenShareEvent::Signal {
            kind: SignalKind::Answer,
            payload: answer_payload,
        })
        .map_err(|_| "A interface encerrou a sessão de tela.".to_owned())?;
    let _ = events.send(ScreenShareEvent::State(
        "Resposta enviada; aguardando conexão P2P com o participante.".to_owned(),
    ));
    Ok(PeerSession {
        connection,
        pending_ice: Vec::new(),
        remote_description_set: true,
        started_at: Instant::now(),
        expects_inbound_video: true,
        no_video_notice_sent: false,
        metrics,
        encoder_stop: None,
        encoder_source: None,
        encoder_task: None,
        sample_writer_task: None,
        rtcp_feedback_task: None,
        remote_track,
        keyframe_request_limiter: KeyframeRequestLimiter::default(),
    })
}

async fn apply_pending_ice(peer: &mut PeerSession, events: &std_mpsc::Sender<ScreenShareEvent>) {
    let candidates = std::mem::take(&mut peer.pending_ice);
    for candidate in candidates {
        if let Err(error) = peer.connection.add_ice_candidate(candidate).await {
            let _ = events.send(ScreenShareEvent::Error(format!(
                "Não foi possível adicionar um candidato ICE: {error}"
            )));
        }
    }
}

async fn close_peer(mut peer: PeerSession) {
    if let Some(stop) = peer.encoder_stop.take() {
        stop.store(true, Ordering::Relaxed);
    }
    if let Some(source) = peer.encoder_source.take() {
        source.wake_waiters();
    }
    if let Some(writer) = peer.sample_writer_task.take() {
        writer.abort();
    }
    if let Some(feedback) = peer.rtcp_feedback_task.take() {
        feedback.abort();
    }
    if let Some(encoder) = peer.encoder_task.take() {
        let _ = timeout(Duration::from_secs(2), encoder).await;
    }
    let _ = timeout(Duration::from_secs(2), peer.connection.close()).await;
}

fn encode_latest_frames(
    source: LatestFrame,
    samples: mpsc::Sender<EncodedFrame>,
    stop: Arc<AtomicBool>,
    metrics: Arc<SharedMetrics>,
    force_keyframe: Arc<AtomicBool>,
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
                    match mf_video::HardwareEncoder::new_gpu(
                        frame.width,
                        frame.height,
                        surface.device(),
                    ) {
                        Ok(hardware) => Ok((hardware, true, None)),
                        Err(gpu_error) => mf_video::HardwareEncoder::new(frame.width, frame.height)
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
                    mf_video::HardwareEncoder::new(frame.width, frame.height)
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
                        encoder = Some(ActiveH264Encoder::OpenH264(openh264_encoder()?));
                    }
                }
            }
            #[cfg(not(windows))]
            {
                let reason = "Media Foundation está disponível apenas no Windows.".to_owned();
                metrics.set_encoder_backend("CPU — OpenH264".to_owned(), Some(reason));
                encoder = Some(ActiveH264Encoder::OpenH264(openh264_encoder()?));
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
            let mut cpu = openh264_encoder()?;
            metrics.record_encoder_input_frame();
            let encode_started_at = Instant::now();
            let encoded_result = encode_frame(&mut cpu, &frame);
            metrics.record_encode_duration(encode_started_at.elapsed());
            let encoded = encoded_result?;
            forward_encoded_access_unit(&encoded, &samples, &metrics, &mut forwarding_gate)?;
            encoder = Some(ActiveH264Encoder::OpenH264(cpu));
        }
    }
    Ok(())
}

enum ActiveH264Encoder {
    OpenH264(Encoder),
    #[cfg(windows)]
    MediaFoundation(mf_video::HardwareEncoder),
}

fn openh264_encoder() -> Result<Encoder, String> {
    let encoder_config = EncoderConfig::new()
        .bitrate(BitRate::from_bps(4_000_000))
        .max_frame_rate(FrameRate::from_hz(30.0))
        .usage_type(UsageType::ScreenContentRealTime)
        .adaptive_quantization(false)
        .background_detection(false)
        .intra_frame_period(IntraFramePeriod::from_num_frames(30));
    Encoder::with_api_config(OpenH264API::from_source(), encoder_config)
        .map_err(|error| format!("Não foi possível iniciar o codificador H.264 OpenH264: {error}"))
}

struct EncodedFrame {
    bytes: Vec<u8>,
    kind: H264FrameKind,
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
        || frame.width % 2 != 0
        || frame.height % 2 != 0
    {
        return Err(
            "O quadro excede o limite 1280×720 ou não tem dimensões H.264 válidas.".to_owned(),
        );
    }
    let expected_len = (frame.width as usize)
        .checked_mul(frame.height as usize)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| "O tamanho do quadro da tela é inválido.".to_owned())?;
    if frame.rgba.len() != expected_len {
        return Err("O quadro da tela tem um tamanho de imagem inválido.".to_owned());
    }
    Ok(())
}

fn encode_frame(encoder: &mut Encoder, frame: &PreviewFrame) -> Result<Vec<u8>, String> {
    validate_encoder_frame(frame)?;

    let rgba = RgbaSliceU8::new(&frame.rgba, (frame.width as usize, frame.height as usize));
    let yuv = YUVBuffer::from_rgba8_source(rgba);
    encoder
        .encode(&yuv)
        .map(|encoded| encoded.to_vec())
        .map_err(|error| format!("Falha ao codificar um quadro da tela em H.264: {error}"))
}

fn unique_ssrc() -> u32 {
    let time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u32;
    time ^ std::process::id().rotate_left(13)
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::{Duration, Instant};

    use super::egui;
    #[cfg(windows)]
    use crate::mf_video;
    use openh264::decoder::Decoder;
    use openh264::formats::YUVSource;

    use super::{
        ActiveH264Decoder, CachedH264Gop, DXVA_ADAPTIVE_MIN_INPUTS, DXVA_ADAPTIVE_MIN_OUTPUT_RATIO,
        DXVA_ADAPTIVE_WARMUP, DXVA_ADAPTIVE_WINDOW, DXVA_FIRST_OUTPUT_WATCHDOG,
        DxvaThroughputMonitor, ENCODER_PACING_JITTER_TOLERANCE, Encoder, FRAME_DURATION,
        H264ForwardDecision, H264ForwardingGate, H264FrameKind, KeyframeRequestLimiter,
        LatestFrame, LatestFramePacer, MediaRoute, PreviewFrame, RtpPacket, RtpSequenceTracker,
        ScreenShareEvent, ScreenShareSession, annex_b_nal_types, assemble_h264_access_unit,
        classify_h264_access_unit, dxva_watchdog_expired, encode_frame,
        media_route_from_candidate_types, media_route_label, peer_stats_snapshot,
        rtp_jitter_ticks_to_ms, should_decode_access_unit, should_log_aggregate_error,
        should_try_hardware_decoder, should_use_adaptive_decoder_fallback, update_cached_idr,
        validate_stun_uri,
    };
    use crate::settings::VideoDecoderPreference;
    use bytes::Bytes;
    use rtc::peer_connection::transport::RTCIceCandidateType;
    use rtc::statistics::report::RTCStatsReport;

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
    fn cpu_fallback_can_publish_the_cached_initial_idr_without_new_network_frames() {
        let mut encoder = super::openh264_encoder().unwrap();
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

    #[test]
    fn rtp_jitter_ticks_are_converted_to_milliseconds() {
        assert!((rtp_jitter_ticks_to_ms(317.0, 90_000.0) - 3.522_222_2).abs() < 0.001);
        assert_eq!(rtp_jitter_ticks_to_ms(10.0, 0.0), 0.0);
    }

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
    fn rtp_sequence_tracker_handles_u16_wrap_without_false_gap() {
        let start = Instant::now();
        let mut tracker = RtpSequenceTracker::default();
        tracker.observe(u16::MAX - 1, start);
        assert_eq!(tracker.observe(u16::MAX, start), Default::default());
        assert_eq!(tracker.observe(0, start), Default::default());
        assert_eq!(tracker.observe(1, start), Default::default());
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
    fn media_interval_snapshot_reports_each_video_pipeline_stage() {
        let metrics = super::SharedMetrics::default();
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
        });
        metrics.record_sequence_gap_resync();
        metrics.record_assembled_access_unit(&annex_b_access_unit(&[
            &[0x67, 1],
            &[0x68, 1],
            &[0x65, 1],
        ]));
        metrics.record_assembly_error("lacuna de sequência no quadro".to_owned());
        metrics.record_decoder_input();
        metrics.record_decoder_no_output();
        metrics.record_decoder_queue_drop();
        metrics.record_pli_sent(super::PliReason::SequenceGap);
        metrics.record_pli_received();
        metrics.record_ui_texture_update();

        let interval = metrics.take_performance_snapshot();
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
        assert_eq!(interval.decoder_no_output_frames, 1);
        assert_eq!(interval.decoder_queue_drops, 1);
        assert_eq!(interval.pli_requests_sent, 1);
        assert_eq!(interval.pli_requests_received, 1);
        assert_eq!(interval.ui_texture_updates, 1);
        assert_eq!(metrics.take_performance_snapshot().received_packets, 0);
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
    fn repeated_media_errors_are_logged_at_doubling_counts() {
        assert!([1, 2, 4, 8, 16].into_iter().all(should_log_aggregate_error));
        assert!(
            [0, 3, 5, 25, 100]
                .into_iter()
                .all(|count| !should_log_aggregate_error(count))
        );
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
    fn loopback_webrtc_requests_pli_and_recovers_with_another_decodable_idr() {
        let context = egui::Context::default();
        let sender = ScreenShareSession::new_loopback(context.clone()).unwrap();
        let receiver = ScreenShareSession::new_loopback(context).unwrap();
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
                    ScreenShareEvent::ConnectionClosed => {
                        panic!("sender connection closed before receiving a frame")
                    }
                    ScreenShareEvent::State(_) => {}
                }
            }
            for event in std::iter::from_fn(|| receiver.try_recv()) {
                match event {
                    ScreenShareEvent::Signal { kind, payload } => {
                        sender.handle_signal(kind, payload).unwrap();
                    }
                    ScreenShareEvent::Error(error) => panic!("receiver session failed: {error}"),
                    ScreenShareEvent::ConnectionClosed => {
                        panic!("receiver connection closed before receiving a frame")
                    }
                    ScreenShareEvent::State(_) => {}
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
            "o receptor deve decodificar o IDR de recuperaÃ§Ã£o; receiver metrics: {:?}",
            receiver_metrics
        );
        assert!(
            receiver_metrics.pli_requests_sent >= 1
                && receiver_metrics.keyframe_resyncs >= 1
                && receiver_metrics.last_recovery_time_millis.is_some(),
            "a recuperaÃ§Ã£o deve registrar PLI, ressincronizaÃ§Ã£o e tempo atÃ© o IDR: {:?}",
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
}
