use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use bytes::Bytes;
use eframe::egui;
use openh264::OpenH264API;
use openh264::decoder::Decoder;
use openh264::encoder::{BitRate, Encoder, EncoderConfig, FrameRate, IntraFramePeriod, UsageType};
use openh264::formats::{RgbaSliceU8, YUVBuffer, YUVSource};
use rtc::interceptor::Registry;
use rtc::media::Sample;
use rtc::media_stream::MediaStreamTrack;
use rtc::peer_connection::configuration::interceptor_registry::register_default_interceptors;
use rtc::peer_connection::configuration::media_engine::{MIME_TYPE_H264, MediaEngine};
use rtc::peer_connection::configuration::{RTCConfigurationBuilder, RTCIceServer};
use rtc::peer_connection::transport::RTCIceCandidateInit;
use rtc::rtp::codec::h264::H264Packet;
use rtc::rtp::packet::Packet as RtpPacket;
use rtc::rtp::packetizer::Depacketizer;
use rtc::rtp_transceiver::PayloadType;
use rtc::rtp_transceiver::rtp_sender::{
    RTCRtpCodec, RTCRtpCodecParameters, RTCRtpCodingParameters, RTCRtpEncodingParameters,
    RtpCodecKind,
};
use rtc::statistics::StatsSelector;
use rtc::statistics::report::{RTCStatsReport, RTCStatsReportEntry};
use signaling_protocol::SignalKind;
use tokio::sync::mpsc;
use tokio::task::JoinHandle as TokioJoinHandle;
use tokio::time::timeout;
use webrtc::media_stream::Track;
use webrtc::media_stream::track_local::TrackLocal;
use webrtc::media_stream::track_local::static_sample::TrackLocalStaticSample;
use webrtc::media_stream::track_remote::{TrackRemote, TrackRemoteEvent};
use webrtc::peer_connection::{
    PeerConnection, PeerConnectionBuilder, PeerConnectionEventHandler, RTCPeerConnectionIceEvent,
    RTCPeerConnectionState,
};
use webrtc::runtime::TokioRuntime;

use crate::logging::safe_stun_endpoint;
use crate::mf_video;
use crate::screen_capture::{LatestFrame, PreviewFrame};
use crate::turn_relay::TurnCredentials;

const VIDEO_PAYLOAD_TYPE: PayloadType = 102;
const VIDEO_CLOCK_RATE: u32 = 90_000;
const FRAME_DURATION: Duration = Duration::from_nanos(1_000_000_000 / 30);
const RTP_REORDER_DELAY: Duration = Duration::from_millis(40);
const MAX_RTP_FRAME_AGE: Duration = Duration::from_millis(200);
const MAX_PENDING_RTP_FRAMES: usize = 8;
const MEDIA_UDP_PORT: u16 = 9002;
const PEER_CONNECTION_TIMEOUT: Duration = Duration::from_secs(20);
const INTERNET_PEER_CONNECTION_TIMEOUT: Duration = Duration::from_secs(30);
const FIRST_VIDEO_FRAME_TIMEOUT: Duration = Duration::from_secs(5);
const CONNECTION_CHECK_INTERVAL: Duration = Duration::from_millis(200);

type RemoteFrameStore = Arc<Mutex<Option<Arc<PreviewFrame>>>>;

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
    pub p2p_connected: bool,
    pub route: Option<MediaRoute>,
    pub local_ice_candidates: u64,
    pub remote_ice_candidates: u64,
    pub local_srflx_candidates: u64,
    pub remote_srflx_candidates: u64,
    pub local_relay_candidates: u64,
    pub remote_relay_candidates: u64,
    pub encoded_frames: u64,
    pub sent_frames: u64,
    pub received_packets: u64,
    pub decoded_frames: u64,
    pub decode_errors: u64,
    pub last_decode_error: Option<String>,
    pub h264_diagnostics: String,
    pub encoder_backend: String,
    pub encoder_fallback_reason: Option<String>,
    pub decoder_backend: String,
    pub decoder_fallback_reason: Option<String>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ScreenSharePerformanceSnapshot {
    pub encoded_frames: u64,
    pub sent_frames: u64,
    pub encode_nanos: u64,
    pub encode_samples: u64,
    pub queue_wait_nanos: u64,
    pub queue_wait_samples: u64,
    pub write_sample_nanos: u64,
    pub write_sample_samples: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaRoute {
    Direct,
    Turn,
}

#[derive(Default)]
struct SharedMetrics {
    p2p_connected: AtomicBool,
    route: AtomicU64,
    local_ice_candidates: AtomicU64,
    remote_ice_candidates: AtomicU64,
    local_srflx_candidates: AtomicU64,
    remote_srflx_candidates: AtomicU64,
    local_relay_candidates: AtomicU64,
    remote_relay_candidates: AtomicU64,
    encoded_frames: AtomicU64,
    sent_frames: AtomicU64,
    received_packets: AtomicU64,
    decoded_frames: AtomicU64,
    decode_errors: AtomicU64,
    interval_encoded_frames: AtomicU64,
    interval_sent_frames: AtomicU64,
    interval_encode_nanos: AtomicU64,
    interval_encode_samples: AtomicU64,
    interval_queue_wait_nanos: AtomicU64,
    interval_queue_wait_samples: AtomicU64,
    interval_write_sample_nanos: AtomicU64,
    interval_write_sample_samples: AtomicU64,
    last_decode_error: Mutex<Option<String>>,
    encoder_backend: Mutex<String>,
    encoder_fallback_reason: Mutex<Option<String>>,
    decoder_backend: Mutex<String>,
    decoder_fallback_reason: Mutex<Option<String>>,
    connected_at: Mutex<Option<Instant>>,
    h264_flow: Mutex<H264FlowDiagnostics>,
}

#[derive(Default)]
struct H264FlowDiagnostics {
    encoded_sps: u64,
    encoded_pps: u64,
    encoded_idr: u64,
    received_sps: u64,
    received_pps: u64,
    received_idr: u64,
    received_single_nals: u64,
    received_stap_a: u64,
    received_fu_a_start: u64,
    received_fu_a_end: u64,
    sequence_gaps: u64,
    out_of_order_packets: u64,
    assembled_access_units: u64,
    assembled_with_sps: u64,
    assembled_with_pps: u64,
    assembled_with_idr: u64,
    assembly_errors: u64,
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
        ScreenShareMetrics {
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
            encoded_frames: self.encoded_frames.load(Ordering::Relaxed),
            sent_frames: self.sent_frames.load(Ordering::Relaxed),
            received_packets: self.received_packets.load(Ordering::Relaxed),
            decoded_frames: self.decoded_frames.load(Ordering::Relaxed),
            decode_errors: self.decode_errors.load(Ordering::Relaxed),
            last_decode_error: self
                .last_decode_error
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone(),
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
            h264_diagnostics: format!(
                "Codificado: SPS/PPS/IDR {}/{}/{} ({}); RTP: SPS/PPS/IDR {}/{}/{}, single/STAP-A/FU-A início/fim {}/{}/{}/{}, lacunas/reordenação {}/{}; quadros montados: {} (com SPS/PPS/IDR {}/{}/{}), erros de montagem {}; último quadro: {}{}.",
                h264_flow.encoded_sps,
                h264_flow.encoded_pps,
                h264_flow.encoded_idr,
                h264_flow.last_encoded_nals,
                h264_flow.received_sps,
                h264_flow.received_pps,
                h264_flow.received_idr,
                h264_flow.received_single_nals,
                h264_flow.received_stap_a,
                h264_flow.received_fu_a_start,
                h264_flow.received_fu_a_end,
                h264_flow.sequence_gaps,
                h264_flow.out_of_order_packets,
                h264_flow.assembled_access_units,
                h264_flow.assembled_with_sps,
                h264_flow.assembled_with_pps,
                h264_flow.assembled_with_idr,
                h264_flow.assembly_errors,
                h264_flow.last_access_unit,
                h264_flow
                    .last_assembly_error
                    .as_ref()
                    .map(|error| format!("; último erro de montagem: {error}"))
                    .unwrap_or_default(),
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

    fn record_encoded_access_unit(&self, data: &[u8]) {
        let nals = annex_b_nal_types(data);
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
        flow.last_encoded_nals = describe_nal_types(&nals, data.len());
    }

    fn record_received_packet(
        &self,
        packet: &rtc::rtp::packet::Packet,
        previous_sequence: &mut Option<u16>,
    ) {
        let payload = &packet.payload;
        let packet_type = payload.first().map(|byte| byte & 0x1f);
        let nals = rtp_payload_nal_types(payload);
        let mut flow = self
            .h264_flow
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        if let Some(previous) = *previous_sequence {
            let distance = packet.header.sequence_number.wrapping_sub(previous);
            if (2..0x8000).contains(&distance) {
                flow.sequence_gaps += u64::from(distance - 1);
                *previous_sequence = Some(packet.header.sequence_number);
            } else if distance == 0 || distance >= 0x8000 {
                flow.out_of_order_packets += 1;
            } else {
                *previous_sequence = Some(packet.header.sequence_number);
            }
        } else {
            *previous_sequence = Some(packet.header.sequence_number);
        }

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
                _ => {}
            }
        }
    }

    fn record_assembled_access_unit(&self, data: &[u8]) -> String {
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
        flow.last_access_unit = description.clone();
        description
    }

    fn record_assembly_error(&self, error: String) {
        let mut flow = self
            .h264_flow
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        flow.assembly_errors += 1;
        let count = flow.assembly_errors;
        flow.last_assembly_error = Some(error);
        if should_log_aggregate_error(count) {
            tracing::warn!(
                assembly_errors = count,
                "Erro de montagem H.264; resumos repetidos registrados em contagens dobradas"
            );
        }
    }

    fn take_performance_snapshot(&self) -> ScreenSharePerformanceSnapshot {
        ScreenSharePerformanceSnapshot {
            encoded_frames: self.interval_encoded_frames.swap(0, Ordering::Relaxed),
            sent_frames: self.interval_sent_frames.swap(0, Ordering::Relaxed),
            encode_nanos: self.interval_encode_nanos.swap(0, Ordering::Relaxed),
            encode_samples: self.interval_encode_samples.swap(0, Ordering::Relaxed),
            queue_wait_nanos: self.interval_queue_wait_nanos.swap(0, Ordering::Relaxed),
            queue_wait_samples: self.interval_queue_wait_samples.swap(0, Ordering::Relaxed),
            write_sample_nanos: self.interval_write_sample_nanos.swap(0, Ordering::Relaxed),
            write_sample_samples: self
                .interval_write_sample_samples
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
struct H264AccessUnitAssembler {
    frames: HashMap<u32, PendingRtpFrame>,
}

struct PendingRtpFrame {
    packets: Vec<RtpPacket>,
    first_received: Instant,
    marker_seen: bool,
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

fn decode_h264_access_unit(
    access_unit: &[u8],
    decoder: &mut ActiveH264Decoder,
    context: &egui::Context,
    remote_frame: &RemoteFrameStore,
    sequence: &AtomicU64,
    metrics: &SharedMetrics,
) -> Option<String> {
    let sample_diagnostics = metrics.record_assembled_access_unit(access_unit);
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
            let next_sequence = sequence.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
            *remote_frame
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) =
                Some(Arc::new(PreviewFrame {
                    sequence: next_sequence,
                    width,
                    height,
                    rgba,
                }));
            context.request_repaint();
            None
        }
        Ok(None) => None,
        Err((detail, native_code)) => {
            #[cfg(windows)]
            let hardware_decoder = matches!(&*decoder, ActiveH264Decoder::MediaFoundation(_));
            #[cfg(not(windows))]
            let hardware_decoder = false;
            let count = metrics.decode_errors.fetch_add(1, Ordering::Relaxed) + 1;
            *metrics
                .last_decode_error
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(detail.clone());
            if should_log_aggregate_error(count) {
                tracing::warn!(
                    decode_errors = count,
                    native_code = native_code.unwrap_or_default(),
                    frame = %sample_diagnostics,
                    error = %detail,
                    backend = %metrics.decoder_backend.lock().unwrap_or_else(std::sync::PoisonError::into_inner),
                    "Decodificador de vídeo rejeitou quadro; resumos repetidos registrados em contagens dobradas"
                );
            }
            context.request_repaint();
            hardware_decoder.then_some(detail)
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
        "0x887a",
        "0xc00d36b5",
    ]
    .iter()
    .any(|needle| error.contains(needle))
}

fn annex_b_nal_types(data: &[u8]) -> Vec<u8> {
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
    let mut nal_types = Vec::new();

    while nal_start < data.len() {
        if let Some((next_start, next_start_code_len)) = next_start_code(data, nal_start) {
            if next_start > nal_start {
                nal_types.push(data[nal_start] & 0x1f);
            }
            nal_start = next_start + next_start_code_len;
        } else {
            nal_types.push(data[nal_start] & 0x1f);
            break;
        }
    }
    nal_types
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

#[derive(Debug)]
pub enum ScreenShareEvent {
    Signal { kind: SignalKind, payload: String },
    State(String),
    Error(String),
    ConnectionClosed,
}

enum Command {
    StartSending(LatestFrame),
    Signal { kind: SignalKind, payload: String },
    Stop,
}

pub struct ScreenShareSession {
    commands: mpsc::UnboundedSender<Command>,
    events: std_mpsc::Receiver<ScreenShareEvent>,
    remote_frame: RemoteFrameStore,
    metrics: Arc<SharedMetrics>,
    worker: Option<JoinHandle<()>>,
}

impl ScreenShareSession {
    pub fn new(
        context: egui::Context,
        stun_server: Option<String>,
        turn_credentials: Option<TurnCredentials>,
    ) -> Result<Self, String> {
        if let Some(server) = stun_server.as_deref() {
            if let Err(error) = validate_stun_uri(server) {
                tracing::error!(reason = %error, "URI STUN recusada antes de iniciar WebRTC");
                return Err(error);
            }
        }
        Self::with_udp_address(
            context,
            format!("0.0.0.0:{MEDIA_UDP_PORT}"),
            stun_server,
            turn_credentials,
        )
    }

    #[cfg(test)]
    fn new_loopback(context: egui::Context) -> Result<Self, String> {
        Self::with_udp_address(context, "127.0.0.1:0".to_owned(), None, None)
    }

    fn with_udp_address(
        context: egui::Context,
        udp_address: String,
        stun_server: Option<String>,
        turn_credentials: Option<TurnCredentials>,
    ) -> Result<Self, String> {
        tracing::info!(
            udp_address = %udp_address,
            stun_endpoint = %stun_server.as_deref().map(safe_stun_endpoint).unwrap_or_else(|| "(não configurado)".to_owned()),
            "Criando sessão WebRTC para compartilhamento de tela"
        );
        let (commands_tx, commands_rx) = mpsc::unbounded_channel();
        let (events_tx, events_rx) = std_mpsc::channel();
        let remote_frame = Arc::new(Mutex::new(None));
        let metrics = Arc::new(SharedMetrics::default());
        let worker_remote_frame = Arc::clone(&remote_frame);
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
                    udp_address,
                    stun_server,
                    turn_credentials,
                    worker_metrics,
                ));
            })
            .map_err(|error| format!("Não foi possível iniciar a sessão de tela: {error}"))?;

        Ok(Self {
            commands: commands_tx,
            events: events_rx,
            remote_frame,
            metrics,
            worker: Some(worker),
        })
    }

    pub fn start_sending(&self, source: LatestFrame) -> Result<(), String> {
        self.commands
            .send(Command::StartSending(source))
            .map_err(|_| "A sessão WebRTC foi encerrada.".to_owned())
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
    encoder_task: Option<TokioJoinHandle<Result<(), String>>>,
    sample_writer_task: Option<TokioJoinHandle<()>>,
}

#[derive(Clone)]
struct PeerEvents {
    events: std_mpsc::Sender<ScreenShareEvent>,
    context: egui::Context,
    remote_frame: RemoteFrameStore,
    remote_frame_sequence: Arc<AtomicU64>,
    metrics: Arc<SharedMetrics>,
    stun_server: Option<String>,
    turn_enabled: bool,
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
        tracing::info!(state = ?state, "Estado ICE mudou");
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
        tracing::info!(state = ?state, "Estado da conexão WebRTC mudou");
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
        tracing::info!(
            "Faixa de vídeo remota recebida; iniciando depacketizador e worker de codec"
        );
        let events = self.events.clone();
        let context = self.context.clone();
        let remote_frame = Arc::clone(&self.remote_frame);
        let sequence = Arc::clone(&self.remote_frame_sequence);
        let metrics = Arc::clone(&self.metrics);
        tokio::spawn(async move {
            let (decoder_tx, decoder_rx) = std_mpsc::sync_channel::<Vec<u8>>(8);
            let worker_events = events.clone();
            let worker_context = context.clone();
            let worker_remote_frame = Arc::clone(&remote_frame);
            let worker_sequence = Arc::clone(&sequence);
            let worker_metrics = Arc::clone(&metrics);
            let worker = thread::Builder::new()
                .name("p2p-h264-decoder".to_owned())
                .spawn(move || {
                    let mut decoder: Option<ActiveH264Decoder> = None;
                    let mut using_cpu_pending_sps = false;
                    let mut hardware_frames_without_output = 0u32;
                    while let Ok(access_unit) = decoder_rx.recv() {
                        let has_sps_and_idr = {
                            let nals = annex_b_nal_types(&access_unit);
                            nals.contains(&7) && nals.contains(&8) && nals.contains(&5)
                        };

                        #[cfg(windows)]
                        let should_try_hardware = decoder.is_none()
                            || (using_cpu_pending_sps && has_sps_and_idr);
                        #[cfg(not(windows))]
                        let should_try_hardware = false;

                        if should_try_hardware {
                            #[cfg(windows)]
                            {
                                if let Some((width, height)) = mf_video::sps_dimensions(&access_unit) {
                                    match mf_video::HardwareDecoder::new(width, height) {
                                        Ok(hardware) => {
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
                                }
                                Err(error) => {
                                    let _ = worker_events.send(ScreenShareEvent::Error(format!(
                                        "Não foi possível iniciar o decodificador H.264: {error}"
                                    )));
                                    return;
                                }
                            }
                        }

                        if let Some(active_decoder) = decoder.as_mut() {
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
                            #[cfg(windows)]
                            if hardware_error
                                .as_deref()
                                .is_some_and(is_hardware_device_failure)
                            {
                                let reason = hardware_error.unwrap_or_default();
                                tracing::warn!(fallback_reason = %reason, "Falha do dispositivo DXVA; mudando para OpenH264 na CPU");
                                match Decoder::new() {
                                    Ok(cpu) => {
                                        worker_metrics.set_decoder_backend(
                                            "CPU — OpenH264".to_owned(),
                                            Some(reason),
                                        );
                                        decoder = Some(ActiveH264Decoder::OpenH264(cpu));
                                        using_cpu_pending_sps = false;
                                        if let Some(decoder) = decoder.as_mut() {
                                            decode_h264_access_unit(
                                                &access_unit,
                                                decoder,
                                                &worker_context,
                                                &worker_remote_frame,
                                                &worker_sequence,
                                                &worker_metrics,
                                            );
                                        }
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
                                hardware_frames_without_output = hardware_frames_without_output.saturating_add(1);
                                if hardware_frames_without_output >= 5 {
                                    let reason = "DXVA recebeu 5 quadros H.264 sem produzir imagem; usando OpenH264 na CPU.".to_owned();
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
                                            if let Some(decoder) = decoder.as_mut() {
                                                decode_h264_access_unit(
                                                    &access_unit,
                                                    decoder,
                                                    &worker_context,
                                                    &worker_remote_frame,
                                                    &worker_sequence,
                                                    &worker_metrics,
                                                );
                                            }
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
                                hardware_frames_without_output = 0;
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
            let mut previous_sequence = None;
            let mut flush_pending = tokio::time::interval(Duration::from_millis(10));
            flush_pending.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

            loop {
                tokio::select! {
                    _ = flush_pending.tick() => {}
                    event = track.poll() => {
                        let Some(event) = event else { break };
                        match event {
                            TrackRemoteEvent::OnRtpPacket(packet) => {
                                metrics.received_packets.fetch_add(1, Ordering::Relaxed);
                                metrics.record_received_packet(&packet, &mut previous_sequence);
                                if let Some(error) = assembler.push(packet) {
                                    metrics.record_assembly_error(error);
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
                        Ok(access_unit) => match decoder_tx.try_send(access_unit) {
                            Ok(()) => {}
                            Err(std_mpsc::TrySendError::Full(_)) => {
                                metrics.record_assembly_error(
                                    "worker de decodificação atrasado; quadro H.264 descartado"
                                        .to_owned(),
                                );
                                context.request_repaint();
                            }
                            Err(std_mpsc::TrySendError::Disconnected(_)) => {
                                let _ = events.send(ScreenShareEvent::Error(
                                    "O worker do decodificador H.264 foi encerrado.".to_owned(),
                                ));
                                return;
                            }
                        },
                        Err(error) => {
                            metrics.record_assembly_error(error);
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
    udp_address: String,
    stun_server: Option<String>,
    turn_credentials: Option<TurnCredentials>,
    metrics: Arc<SharedMetrics>,
) {
    let mut active_peer: Option<PeerSession> = None;
    let mut ice_before_peer = Vec::new();
    let remote_frame_sequence = Arc::new(AtomicU64::new(0));
    let mut connection_check = tokio::time::interval(CONNECTION_CHECK_INTERVAL);
    connection_check.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut last_route_check = Instant::now();

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
                    if let Some(route) = selected_media_route(&report) {
                        let route_code = match route {
                            MediaRoute::Direct => 1,
                            MediaRoute::Turn => 2,
                        };
                        let previous = peer.metrics.route.swap(route_code, Ordering::Relaxed);
                        if previous != route_code {
                            tracing::info!(?route, "Rota ICE de mídia selecionada");
                            let label = match route {
                                MediaRoute::Direct => "Conexão direta P2P selecionada.",
                                MediaRoute::Turn => "Conexão retransmitida pelo servidor TURN do anfitrião.",
                            };
                            let _ = events.send(ScreenShareEvent::State(label.to_owned()));
                        }
                        context.request_repaint();
                    }
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
                    Arc::clone(&remote_frame_sequence),
                    Arc::clone(&metrics),
                    &udp_address,
                    stun_server.as_deref(),
                    turn_credentials.as_ref(),
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
                        Arc::clone(&remote_frame_sequence),
                        Arc::clone(&metrics),
                        &udp_address,
                        stun_server.as_deref(),
                        turn_credentials.as_ref(),
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
            Command::Stop => break,
        }
            }
        }
    }

    if let Some(peer) = active_peer {
        close_peer(peer).await;
    }
}

fn selected_media_route(report: &RTCStatsReport) -> Option<MediaRoute> {
    let selected_pair_id = report.iter().find_map(|entry| match entry {
        RTCStatsReportEntry::Transport(transport)
            if !transport.selected_candidate_pair_id.is_empty() =>
        {
            Some(transport.selected_candidate_pair_id.as_str())
        }
        _ => None,
    })?;
    let pair = match report.get(selected_pair_id)? {
        RTCStatsReportEntry::IceCandidatePair(pair) => pair,
        _ => return None,
    };
    let relay_candidate_type = rtc::peer_connection::transport::RTCIceCandidateType::Relay;
    let local_is_relay = matches!(
        report.get(&pair.local_candidate_id),
        Some(RTCStatsReportEntry::LocalCandidate(candidate))
            if candidate.candidate_type == relay_candidate_type
    );
    let remote_is_relay = matches!(
        report.get(&pair.remote_candidate_id),
        Some(RTCStatsReportEntry::RemoteCandidate(candidate))
            if candidate.candidate_type == relay_candidate_type
    );
    Some(if local_is_relay || remote_is_relay {
        MediaRoute::Turn
    } else {
        MediaRoute::Direct
    })
}

async fn create_peer(
    events: &std_mpsc::Sender<ScreenShareEvent>,
    context: egui::Context,
    remote_frame: RemoteFrameStore,
    remote_frame_sequence: Arc<AtomicU64>,
    metrics: Arc<SharedMetrics>,
    udp_address: &str,
    stun_server: Option<&str>,
    turn_credentials: Option<&TurnCredentials>,
) -> Result<Arc<dyn PeerConnection>, String> {
    let video_codec = RTCRtpCodecParameters {
        rtp_codec: RTCRtpCodec {
            mime_type: MIME_TYPE_H264.to_owned(),
            clock_rate: VIDEO_CLOCK_RATE,
            channels: 0,
            sdp_fmtp_line: "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f"
                .to_owned(),
            rtcp_feedback: vec![],
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
    let handler = Arc::new(PeerEvents {
        events: events.clone(),
        context,
        remote_frame,
        remote_frame_sequence,
        metrics,
        stun_server: stun_server.map(str::to_owned),
        turn_enabled: turn_credentials.is_some(),
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
    remote_frame_sequence: Arc<AtomicU64>,
    metrics: Arc<SharedMetrics>,
    udp_address: &str,
    stun_server: Option<&str>,
    turn_credentials: Option<&TurnCredentials>,
) -> Result<PeerSession, String> {
    let connection = create_peer(
        events,
        context,
        remote_frame,
        remote_frame_sequence,
        Arc::clone(&metrics),
        udp_address,
        stun_server,
        turn_credentials,
    )
    .await?;
    let codec = RTCRtpCodec {
        mime_type: MIME_TYPE_H264.to_owned(),
        clock_rate: VIDEO_CLOCK_RATE,
        channels: 0,
        sdp_fmtp_line: "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f"
            .to_owned(),
        rtcp_feedback: vec![],
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
    let (sample_tx, sample_rx) = mpsc::channel::<Vec<u8>>(1);
    let encoder_stop = Arc::new(AtomicBool::new(false));
    let encoder_stop_worker = Arc::clone(&encoder_stop);
    let encoder_events = events.clone();
    let encoder_metrics = Arc::clone(&metrics);
    let encoder_task = tokio::task::spawn_blocking(move || {
        match encode_latest_frames(source, sample_tx, encoder_stop_worker, encoder_metrics) {
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
        let mut sample_rx = sample_rx;
        while let Some(data) = sample_rx.recv().await {
            let sample = Sample {
                data: Bytes::from(data),
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
                let _ = writer_events.send(ScreenShareEvent::Error(format!(
                    "Falha ao enviar um quadro H.264 pela conexão P2P: {error}"
                )));
                break;
            } else {
                writer_metrics.sent_frames.fetch_add(1, Ordering::Relaxed);
                writer_metrics
                    .interval_sent_frames
                    .fetch_add(1, Ordering::Relaxed);
            }
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
        encoder_task: Some(encoder_task),
        sample_writer_task: Some(sample_writer_task),
    })
}

async fn create_receiver(
    payload: String,
    events: &std_mpsc::Sender<ScreenShareEvent>,
    context: egui::Context,
    remote_frame: RemoteFrameStore,
    remote_frame_sequence: Arc<AtomicU64>,
    metrics: Arc<SharedMetrics>,
    udp_address: &str,
    stun_server: Option<&str>,
    turn_credentials: Option<&TurnCredentials>,
) -> Result<PeerSession, String> {
    let connection = create_peer(
        events,
        context,
        remote_frame,
        remote_frame_sequence,
        Arc::clone(&metrics),
        udp_address,
        stun_server,
        turn_credentials,
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
        encoder_task: None,
        sample_writer_task: None,
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
    if let Some(writer) = peer.sample_writer_task.take() {
        writer.abort();
    }
    if let Some(encoder) = peer.encoder_task.take() {
        let _ = timeout(Duration::from_secs(2), encoder).await;
    }
    let _ = timeout(Duration::from_secs(2), peer.connection.close()).await;
}

fn encode_latest_frames(
    source: LatestFrame,
    samples: mpsc::Sender<Vec<u8>>,
    stop: Arc<AtomicBool>,
    metrics: Arc<SharedMetrics>,
) -> Result<(), String> {
    let mut encoder: Option<ActiveH264Encoder> = None;
    let mut hardware_warmup_frames = 0u32;
    let mut last_sequence = None;
    let mut next_frame = Instant::now();

    while !stop.load(Ordering::Relaxed) {
        let wait = next_frame.saturating_duration_since(Instant::now());
        if !wait.is_zero() {
            thread::sleep(wait);
        }
        next_frame += FRAME_DURATION;

        // Do not consume the encoder's first IDR/SPS/PPS while ICE/DTLS is still
        // negotiating. RTP packets written before the peer is connected can be
        // discarded; starting with a P-frame then leaves the receiver without
        // the parameter sets needed to decode the stream.
        if !metrics.p2p_connected.load(Ordering::Relaxed) {
            continue;
        }

        let frame = source
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let Some(frame) = frame else {
            continue;
        };
        if last_sequence == Some(frame.sequence) {
            continue;
        }
        last_sequence = Some(frame.sequence);
        validate_encoder_frame(&frame)?;
        if encoder.is_none() {
            #[cfg(windows)]
            {
                match mf_video::HardwareEncoder::new(frame.width, frame.height) {
                    Ok(hardware) => {
                        let name = hardware.name().to_owned();
                        tracing::info!(codec = %name, width = frame.width, height = frame.height, "Codificador H.264 de hardware ativado");
                        metrics
                            .set_encoder_backend(format!("GPU — Media Foundation / {name}"), None);
                        encoder = Some(ActiveH264Encoder::MediaFoundation(hardware));
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

        let active = encoder
            .as_mut()
            .expect("codificador inicializado antes de codificar");
        let hardware_failure = match active {
            ActiveH264Encoder::OpenH264(cpu) => {
                let encode_started_at = Instant::now();
                let encoded_result = encode_frame(cpu, &frame);
                metrics.record_encode_duration(encode_started_at.elapsed());
                let encoded = encoded_result?;
                send_encoded_frame(&encoded, &samples, &metrics)?;
                None
            }
            #[cfg(windows)]
            ActiveH264Encoder::MediaFoundation(hardware) => {
                let encode_started_at = Instant::now();
                let encode_result = hardware.encode_rgba(&frame.rgba);
                metrics.record_encode_duration(encode_started_at.elapsed());
                match encode_result {
                    Ok(encoded) => {
                        if encoded.is_empty() {
                            hardware_warmup_frames = hardware_warmup_frames.saturating_add(1);
                            if hardware_warmup_frames > 90 {
                                Some("O codificador de hardware não produziu H.264 após 3 segundos de aquecimento.".to_owned())
                            } else {
                                None
                            }
                        } else {
                            let nals = annex_b_nal_types(&encoded);
                            if !nals.contains(&7) || !nals.contains(&8) || !nals.contains(&5) {
                                hardware_warmup_frames = hardware_warmup_frames.saturating_add(1);
                                if hardware_warmup_frames > 90 {
                                    Some("O codificador de hardware não enviou SPS/PPS/IDR após 3 segundos; usando OpenH264.".to_owned())
                                } else {
                                    None
                                }
                            } else {
                                send_encoded_frame(&encoded, &samples, &metrics)?;
                                hardware_warmup_frames = 0;
                                None
                            }
                        }
                    }
                    Err(error) => Some(error),
                }
            }
        };

        if let Some(reason) = hardware_failure {
            tracing::warn!(fallback_reason = %reason, "Falha no codificador H.264 de hardware; mudando para OpenH264 na CPU");
            metrics.set_encoder_backend("CPU — OpenH264".to_owned(), Some(reason));
            let mut cpu = openh264_encoder()?;
            let encode_started_at = Instant::now();
            let encoded_result = encode_frame(&mut cpu, &frame);
            metrics.record_encode_duration(encode_started_at.elapsed());
            let encoded = encoded_result?;
            send_encoded_frame(&encoded, &samples, &metrics)?;
            encoder = Some(ActiveH264Encoder::OpenH264(cpu));
        }
        if next_frame < Instant::now() {
            next_frame = Instant::now() + FRAME_DURATION;
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

fn send_encoded_frame(
    encoded: &[u8],
    samples: &mpsc::Sender<Vec<u8>>,
    metrics: &SharedMetrics,
) -> Result<(), String> {
    if encoded.is_empty() {
        return Ok(());
    }
    metrics.record_encoded_access_unit(encoded);
    metrics.encoded_frames.fetch_add(1, Ordering::Relaxed);
    metrics
        .interval_encoded_frames
        .fetch_add(1, Ordering::Relaxed);
    let sample = encoded.to_vec();
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
        Encoder, FRAME_DURATION, LatestFrame, PreviewFrame, ScreenShareEvent, ScreenShareSession,
        annex_b_nal_types, encode_frame, should_log_aggregate_error, validate_stun_uri,
    };

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
    fn rgba_frame_can_be_encoded_and_decoded_as_h264() {
        let mut encoder = Encoder::new().unwrap();
        let frame = PreviewFrame {
            sequence: 1,
            width: 320,
            height: 240,
            rgba: vec![96; 320 * 240 * 4],
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
        let mut access_unit = None;
        for sequence in 0..90u8 {
            let mut rgba = vec![0u8; 320 * 240 * 4];
            for (index, pixel) in rgba.chunks_exact_mut(4).enumerate() {
                pixel[0] = (index as u8).wrapping_add(sequence.wrapping_mul(11));
                pixel[1] = (index / 320) as u8;
                pixel[2] = (index % 320) as u8;
                pixel[3] = 255;
            }
            let encoded = encoder.encode_rgba(&rgba).unwrap();
            if annex_b_nal_types(&encoded).contains(&7)
                && annex_b_nal_types(&encoded).contains(&8)
                && annex_b_nal_types(&encoded).contains(&5)
            {
                access_unit = Some(encoded);
                break;
            }
        }
        let access_unit = access_unit.expect("codificador de hardware precisa emitir SPS/PPS/IDR");
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
    fn loopback_webrtc_negotiates_transfers_a_frame_and_closes() {
        let context = egui::Context::default();
        let sender = ScreenShareSession::new_loopback(context.clone()).unwrap();
        let receiver = ScreenShareSession::new_loopback(context).unwrap();
        let source: LatestFrame = Arc::new(Mutex::new(Some(Arc::new(PreviewFrame {
            sequence: 1,
            width: 320,
            height: 240,
            rgba: vec![128; 320 * 240 * 4],
        }))));
        sender.start_sending(Arc::clone(&source)).unwrap();

        let deadline = Instant::now() + Duration::from_secs(15);
        let mut sequence = 1;
        let mut next_frame = Instant::now() + FRAME_DURATION;
        let mut received_frame = None;
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
                break;
            }
            if Instant::now() >= next_frame {
                sequence += 1;
                *source
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) =
                    Some(Arc::new(PreviewFrame {
                        sequence,
                        width: 320,
                        height: 240,
                        rgba: vec![(sequence % 255) as u8; 320 * 240 * 4],
                    }));
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
    }
}
