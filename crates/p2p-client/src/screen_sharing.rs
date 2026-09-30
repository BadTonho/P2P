use std::borrow::Cow;
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
use rtc::peer_connection::configuration::media_engine::{
    MIME_TYPE_H264, MIME_TYPE_OPUS, MediaEngine,
};
use rtc::peer_connection::configuration::{RTCConfigurationBuilder, RTCIceServer};
use rtc::peer_connection::sdp::RTCSessionDescription;
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

use crate::audio_capture::{
    AudioPlaybackFactory, AudioSampleSource, OPUS_CHANNELS, OPUS_FRAME_SAMPLES_PER_CHANNEL,
    OPUS_SAMPLE_RATE, SystemAudioCapture, SystemAudioPlaybackFactory,
};
use crate::logging::safe_stun_endpoint;
use crate::mf_video;
use crate::screen_capture::{LatestFrame, PreviewFrame};
use crate::settings::VideoDecoderPreference;
use crate::turn_relay::TurnCredentials;

#[path = "screen_sharing/h264.rs"]
mod h264;
#[path = "screen_sharing/metrics.rs"]
mod metrics;
#[path = "screen_sharing/receiver.rs"]
mod receiver;
#[path = "screen_sharing/sdp_diagnostics.rs"]
mod sdp_diagnostics;
#[path = "screen_sharing/sender.rs"]
mod sender;
#[path = "screen_sharing/session.rs"]
mod session;
use h264::*;
use metrics::should_log_aggregate_error;
pub use metrics::{MediaRoute, ScreenShareMetrics, ScreenSharePerformanceSnapshot};
use receiver::{KeyframeRequestLimiter, PliReason, RtpSequenceUpdate};
use sdp_diagnostics::summarize_sdp_media;
use sender::{EncodedFrame, encode_latest_frames, unique_ssrc};
#[cfg(test)]
use sender::{encode_frame, openh264_encoder};
pub use session::{ScreenShareEvent, ScreenShareSession};

const VIDEO_PAYLOAD_TYPE: PayloadType = 102;
const AUDIO_PAYLOAD_TYPE: PayloadType = 111;
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
const INITIAL_RTP_REORDER_DELAY: Duration = Duration::from_millis(40);
const MIN_RTP_REORDER_DELAY: Duration = Duration::from_millis(40);
const MAX_RTP_REORDER_DELAY: Duration = Duration::from_millis(120);
const RTP_REORDER_DELAY_SAFETY_MARGIN: Duration = Duration::from_millis(10);
const RTP_REORDER_DELAY_SAMPLE_CAPACITY: usize = 32;
const RTP_REORDER_DELAY_MIN_SAMPLES: usize = 4;
const MAX_RTP_FRAME_AGE: Duration = Duration::from_millis(200);
const MAX_PENDING_RTP_FRAMES: usize = 8;
const MEDIA_UDP_PORT: u16 = 9002;
const MAX_PEER_MEDIA_BITRATE: u32 = 4_000_000;
const PEER_CONNECTION_TIMEOUT: Duration = Duration::from_secs(20);
const INTERNET_PEER_CONNECTION_TIMEOUT: Duration = Duration::from_secs(30);
const FIRST_VIDEO_FRAME_TIMEOUT: Duration = Duration::from_secs(5);
const CONNECTION_CHECK_INTERVAL: Duration = Duration::from_millis(200);
const KEYFRAME_REQUEST_MIN_INTERVAL: Duration = Duration::from_millis(750);
const RTP_SEQUENCE_HISTORY: usize = 4096;

static NEXT_SCREEN_SHARE_SESSION_ID: AtomicU64 = AtomicU64::new(1);

type RemoteFrameStore = Arc<Mutex<Option<Arc<PreviewFrame>>>>;
type RemoteTrackStore = Arc<Mutex<Option<Arc<dyn TrackRemote>>>>;

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
    outbound_nack_count: u32,
    inbound_nack_count: u32,
    inbound_pli_count: u32,
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
    inbound_frame_width: u32,
    inbound_frame_height: u32,
    outbound_summary: String,
    inbound_summary: String,
}

#[derive(Default)]
struct SharedMetrics {
    session_id: u64,
    track_ssrc: AtomicU64,
    remote_video_track_seen: AtomicBool,
    p2p_connected: AtomicBool,
    route: AtomicU64,
    local_ice_candidates: AtomicU64,
    remote_ice_candidates: AtomicU64,
    local_srflx_candidates: AtomicU64,
    remote_srflx_candidates: AtomicU64,
    local_relay_candidates: AtomicU64,
    remote_relay_candidates: AtomicU64,
    encoder_input_frames: AtomicU64,
    capture_width: AtomicU64,
    capture_height: AtomicU64,
    encoder_width: AtomicU64,
    encoder_height: AtomicU64,
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
    rtp_reorder_window_millis: AtomicU64,
    rtp_reorder_delay_samples: AtomicU64,
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
    interval_nack_requests_sent: AtomicU64,
    interval_nack_requests_received: AtomicU64,
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
    audio_playback_factory: Arc<dyn AudioPlaybackFactory>,
}
