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
use crate::screen_capture::{LatestFrame, PreviewFrame};

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
    pub local_ice_candidates: u64,
    pub remote_ice_candidates: u64,
    pub local_srflx_candidates: u64,
    pub remote_srflx_candidates: u64,
    pub encoded_frames: u64,
    pub sent_frames: u64,
    pub received_packets: u64,
    pub decoded_frames: u64,
    pub decode_errors: u64,
    pub last_decode_error: Option<String>,
    pub h264_diagnostics: String,
}

#[derive(Default)]
struct SharedMetrics {
    p2p_connected: AtomicBool,
    local_ice_candidates: AtomicU64,
    remote_ice_candidates: AtomicU64,
    local_srflx_candidates: AtomicU64,
    remote_srflx_candidates: AtomicU64,
    encoded_frames: AtomicU64,
    sent_frames: AtomicU64,
    received_packets: AtomicU64,
    decoded_frames: AtomicU64,
    decode_errors: AtomicU64,
    last_decode_error: Mutex<Option<String>>,
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
            local_ice_candidates: self.local_ice_candidates.load(Ordering::Relaxed),
            remote_ice_candidates: self.remote_ice_candidates.load(Ordering::Relaxed),
            local_srflx_candidates: self.local_srflx_candidates.load(Ordering::Relaxed),
            remote_srflx_candidates: self.remote_srflx_candidates.load(Ordering::Relaxed),
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

fn decode_h264_access_unit(
    access_unit: &[u8],
    decoder: &mut Decoder,
    context: &egui::Context,
    remote_frame: &RemoteFrameStore,
    sequence: &AtomicU64,
    metrics: &SharedMetrics,
) {
    let sample_diagnostics = metrics.record_assembled_access_unit(access_unit);
    match decoder.decode(access_unit) {
        Ok(Some(yuv)) => {
            let (width, height) = yuv.dimensions();
            if width == 0 || height == 0 {
                return;
            }
            let mut rgba = vec![0; yuv.rgba8_len()];
            yuv.write_rgba8(&mut rgba);
            metrics.decoded_frames.fetch_add(1, Ordering::Relaxed);
            let next_sequence = sequence.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
            *remote_frame
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) =
                Some(Arc::new(PreviewFrame {
                    sequence: next_sequence,
                    width: width as u32,
                    height: height as u32,
                    rgba,
                }));
            context.request_repaint();
        }
        Ok(None) => {}
        Err(error) => {
            let count = metrics.decode_errors.fetch_add(1, Ordering::Relaxed) + 1;
            let detail = if error.native_code() & 0x10 != 0 {
                format!(
                    "{error} (dsNoParamSets: SPS/PPS ausentes ou incompatíveis; quadro: {sample_diagnostics})"
                )
            } else {
                format!("{error}; quadro: {sample_diagnostics}")
            };
            *metrics
                .last_decode_error
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(detail);
            if should_log_aggregate_error(count) {
                tracing::warn!(
                    decode_errors = count,
                    native_code = error.native_code(),
                    frame = %sample_diagnostics,
                    error = %error,
                    "Decodificador H.264 rejeitou quadro; resumos repetidos registrados em contagens dobradas"
                );
            }
            context.request_repaint();
        }
    }
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
    pub fn new(context: egui::Context, stun_server: Option<String>) -> Result<Self, String> {
        if let Some(server) = stun_server.as_deref() {
            if let Err(error) = validate_stun_uri(server) {
                tracing::error!(reason = %error, "URI STUN recusada antes de iniciar WebRTC");
                return Err(error);
            }
        }
        Self::with_udp_address(context, format!("0.0.0.0:{MEDIA_UDP_PORT}"), stun_server)
    }

    #[cfg(test)]
    fn new_loopback(context: egui::Context) -> Result<Self, String> {
        Self::with_udp_address(context, "127.0.0.1:0".to_owned(), None)
    }

    fn with_udp_address(
        context: egui::Context,
        udp_address: String,
        stun_server: Option<String>,
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
                *self
                    .metrics
                    .connected_at
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
                "Conexão ICE interrompida; aguardando recuperação…".to_owned()
            }
            webrtc::peer_connection::RTCIceConnectionState::Failed => {
                self.metrics.p2p_connected.store(false, Ordering::Relaxed);
                *self
                    .metrics
                    .connected_at
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
                let message = if let Some(stun_server) = &self.stun_server {
                    let safe_stun_server = safe_stun_endpoint(stun_server);
                    let local = self.metrics.local_srflx_candidates.load(Ordering::Relaxed);
                    let remote = self.metrics.remote_srflx_candidates.load(Ordering::Relaxed);
                    format!(
                        "O ICE não encontrou um caminho direto pela internet. Candidatos públicos via STUN: {local} locais e {remote} recebidos do amigo. Confira a URI {safe_stun_server}, o firewall e UDP {MEDIA_UDP_PORT}; esta etapa não usa TURN."
                    )
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
                    stun_configured = self.stun_server.is_some(),
                    "ICE falhou em encontrar caminho UDP direto"
                );
                let _ = self.events.send(ScreenShareEvent::Error(message));
                self.context.request_repaint();
                return;
            }
            webrtc::peer_connection::RTCIceConnectionState::Closed => {
                self.metrics.p2p_connected.store(false, Ordering::Relaxed);
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
        tracing::info!("Faixa de vídeo remota recebida; iniciando depacketizador/decodificador");
        let events = self.events.clone();
        let context = self.context.clone();
        let remote_frame = Arc::clone(&self.remote_frame);
        let sequence = Arc::clone(&self.remote_frame_sequence);
        let metrics = Arc::clone(&self.metrics);
        tokio::spawn(async move {
            let mut decoder = match Decoder::new() {
                Ok(decoder) => decoder,
                Err(error) => {
                    tracing::error!(error = %error, "Falha ao iniciar decodificador OpenH264");
                    let _ = events.send(ScreenShareEvent::Error(format!(
                        "Nao foi possivel iniciar o decodificador H.264: {error}"
                    )));
                    return;
                }
            };
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
                        Ok(access_unit) => decode_h264_access_unit(
                            &access_unit,
                            &mut decoder,
                            &context,
                            &remote_frame,
                            &sequence,
                            &metrics,
                        ),
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
    metrics: Arc<SharedMetrics>,
) {
    let mut active_peer: Option<PeerSession> = None;
    let mut ice_before_peer = Vec::new();
    let remote_frame_sequence = Arc::new(AtomicU64::new(0));
    let mut connection_check = tokio::time::interval(CONNECTION_CHECK_INTERVAL);
    connection_check.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            _ = connection_check.tick() => {
                let Some(peer) = active_peer.as_mut() else {
                    continue;
                };
                let connection_timeout = if stun_server.is_some() {
                    INTERNET_PEER_CONNECTION_TIMEOUT
                } else {
                    PEER_CONNECTION_TIMEOUT
                };
                if !peer.metrics.p2p_connected.load(Ordering::Relaxed)
                    && peer.started_at.elapsed() >= connection_timeout
                {
                    let message = if let Some(stun_server) = &stun_server {
                        let local = peer.metrics.local_srflx_candidates.load(Ordering::Relaxed);
                        let remote = peer.metrics.remote_srflx_candidates.load(Ordering::Relaxed);
                        format!(
                            "A conexão P2P não foi estabelecida em {connection_timeout:?}. Candidatos públicos via STUN: {local} locais e {remote} recebidos do amigo. Confira a URI {stun_server}, as regras de NAT e o firewall/UDP {MEDIA_UDP_PORT} dos dois PCs. Não há TURN nem retransmissão nesta etapa."
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

async fn create_peer(
    events: &std_mpsc::Sender<ScreenShareEvent>,
    context: egui::Context,
    remote_frame: RemoteFrameStore,
    remote_frame_sequence: Arc<AtomicU64>,
    metrics: Arc<SharedMetrics>,
    udp_address: &str,
    stun_server: Option<&str>,
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
    });
    let mut configuration = RTCConfigurationBuilder::new();
    if let Some(stun_server) = stun_server {
        let validated_stun = validate_stun_uri(stun_server)?;
        configuration = configuration.with_ice_servers(vec![RTCIceServer {
            urls: vec![validated_stun],
            ..Default::default()
        }]);
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
) -> Result<PeerSession, String> {
    let connection = create_peer(
        events,
        context,
        remote_frame,
        remote_frame_sequence,
        Arc::clone(&metrics),
        udp_address,
        stun_server,
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
            if let Err(error) = writer_track
                .sample_writer(ssrc, VIDEO_PAYLOAD_TYPE)
                .write_sample(&sample)
                .await
            {
                let _ = writer_events.send(ScreenShareEvent::Error(format!(
                    "Falha ao enviar um quadro H.264 pela conexão P2P: {error}"
                )));
                break;
            } else {
                writer_metrics.sent_frames.fetch_add(1, Ordering::Relaxed);
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
) -> Result<PeerSession, String> {
    let connection = create_peer(
        events,
        context,
        remote_frame,
        remote_frame_sequence,
        Arc::clone(&metrics),
        udp_address,
        stun_server,
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
    let encoder_config = EncoderConfig::new()
        .bitrate(BitRate::from_bps(4_000_000))
        .max_frame_rate(FrameRate::from_hz(30.0))
        .usage_type(UsageType::ScreenContentRealTime)
        .adaptive_quantization(false)
        .background_detection(false)
        .intra_frame_period(IntraFramePeriod::from_num_frames(30));
    let mut encoder = Encoder::with_api_config(OpenH264API::from_source(), encoder_config)
        .map_err(|error| format!("Não foi possível iniciar o codificador H.264: {error}"))?;
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
        let encoded = encode_frame(&mut encoder, &frame)?;
        if !encoded.is_empty() {
            metrics.record_encoded_access_unit(&encoded);
            metrics.encoded_frames.fetch_add(1, Ordering::Relaxed);
            samples
                .blocking_send(encoded)
                .map_err(|_| "O envio de vídeo foi encerrado.".to_owned())?;
        }
        if next_frame < Instant::now() {
            next_frame = Instant::now() + FRAME_DURATION;
        }
    }
    Ok(())
}

fn encode_frame(encoder: &mut Encoder, frame: &PreviewFrame) -> Result<Vec<u8>, String> {
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
    use openh264::decoder::Decoder;
    use openh264::formats::YUVSource;

    use super::{
        Encoder, FRAME_DURATION, LatestFrame, PreviewFrame, ScreenShareEvent, ScreenShareSession,
        encode_frame, should_log_aggregate_error, validate_stun_uri,
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

        receiver.stop();
        sender.stop();
        assert_eq!(received_frame, Some((320, 240)));
    }
}
