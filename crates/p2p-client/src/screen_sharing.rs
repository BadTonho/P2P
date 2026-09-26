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
use rtc::media::io::sample_builder::SampleBuilder;
use rtc::media_stream::MediaStreamTrack;
use rtc::peer_connection::configuration::RTCConfigurationBuilder;
use rtc::peer_connection::configuration::interceptor_registry::register_default_interceptors;
use rtc::peer_connection::configuration::media_engine::{MIME_TYPE_H264, MediaEngine};
use rtc::peer_connection::transport::RTCIceCandidateInit;
use rtc::rtp::codec::h264::H264Packet;
use rtc::rtp_transceiver::PayloadType;
use rtc::rtp_transceiver::rtp_sender::{
    RTCRtpCodec, RTCRtpCodecParameters, RTCRtpCodingParameters, RTCRtpEncodingParameters,
    RtpCodecKind,
};
use signaling_protocol::SignalKind;
use tokio::sync::{mpsc, watch};
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

use crate::screen_capture::{LatestFrame, PreviewFrame};

const VIDEO_PAYLOAD_TYPE: PayloadType = 102;
const VIDEO_CLOCK_RATE: u32 = 90_000;
const FRAME_DURATION: Duration = Duration::from_nanos(1_000_000_000 / 30);
const MAX_FRAME_QUEUE_DELAY: Duration = Duration::from_millis(200);

type RemoteFrameStore = Arc<Mutex<Option<Arc<PreviewFrame>>>>;

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
    worker: Option<JoinHandle<()>>,
}

impl ScreenShareSession {
    pub fn new(context: egui::Context) -> Result<Self, String> {
        Self::with_udp_address(context, "0.0.0.0:0".to_owned())
    }

    #[cfg(test)]
    fn new_loopback(context: egui::Context) -> Result<Self, String> {
        Self::with_udp_address(context, "127.0.0.1:0".to_owned())
    }

    fn with_udp_address(context: egui::Context, udp_address: String) -> Result<Self, String> {
        let (commands_tx, commands_rx) = mpsc::unbounded_channel();
        let (events_tx, events_rx) = std_mpsc::channel();
        let remote_frame = Arc::new(Mutex::new(None));
        let worker_remote_frame = Arc::clone(&remote_frame);
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
                ));
            })
            .map_err(|error| format!("Não foi possível iniciar a sessão de tela: {error}"))?;

        Ok(Self {
            commands: commands_tx,
            events: events_rx,
            remote_frame,
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
}

#[async_trait::async_trait]
impl PeerConnectionEventHandler for PeerEvents {
    async fn on_ice_candidate(&self, event: RTCPeerConnectionIceEvent) {
        if event.candidate.address.is_empty() {
            return;
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

    async fn on_connection_state_change(&self, state: RTCPeerConnectionState) {
        match state {
            RTCPeerConnectionState::Connected => {
                let _ = self.events.send(ScreenShareEvent::State(
                    "Conexão P2P estabelecida.".to_owned(),
                ));
            }
            RTCPeerConnectionState::Failed
            | RTCPeerConnectionState::Disconnected
            | RTCPeerConnectionState::Closed => {
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
        let events = self.events.clone();
        let context = self.context.clone();
        let remote_frame = Arc::clone(&self.remote_frame);
        let sequence = Arc::clone(&self.remote_frame_sequence);
        tokio::spawn(async move {
            let mut decoder = match Decoder::new() {
                Ok(decoder) => decoder,
                Err(error) => {
                    let _ = events.send(ScreenShareEvent::Error(format!(
                        "Não foi possível iniciar o decodificador H.264: {error}"
                    )));
                    return;
                }
            };
            let mut builder = SampleBuilder::new(30, H264Packet::default(), VIDEO_CLOCK_RATE)
                .with_max_time_delay(MAX_FRAME_QUEUE_DELAY);

            while let Some(event) = track.poll().await {
                match event {
                    TrackRemoteEvent::OnRtpPacket(packet) => {
                        builder.push(Instant::now(), packet);
                        while let Some(sample) = builder.pop(Instant::now()) {
                            match decoder.decode(&sample.data) {
                                Ok(Some(yuv)) => {
                                    let (width, height) = yuv.dimensions();
                                    if width == 0 || height == 0 {
                                        continue;
                                    }
                                    let mut rgba = vec![0; yuv.rgba8_len()];
                                    yuv.write_rgba8(&mut rgba);
                                    let next_sequence =
                                        sequence.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
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
                                // Um pacote perdido pode invalidar um quadro; quadros-chave periódicos
                                // permitem que a decodificação se recupere automaticamente.
                                Err(_error) => {}
                            }
                        }
                    }
                    TrackRemoteEvent::OnEnded | TrackRemoteEvent::OnEnding => break,
                    _ => {}
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
) {
    let mut active_peer = None;
    let mut ice_before_peer = Vec::new();
    let remote_frame_sequence = Arc::new(AtomicU64::new(0));

    while let Some(command) = commands.recv().await {
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
                    &udp_address,
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
                        &udp_address,
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

    if let Some(peer) = active_peer {
        close_peer(peer).await;
    }
}

async fn create_peer(
    events: &std_mpsc::Sender<ScreenShareEvent>,
    context: egui::Context,
    remote_frame: RemoteFrameStore,
    remote_frame_sequence: Arc<AtomicU64>,
    udp_address: &str,
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
    });
    let connection = PeerConnectionBuilder::new()
        .with_configuration(RTCConfigurationBuilder::new().build())
        .with_media_engine(media_engine)
        .with_interceptor_registry(interceptors)
        .with_handler(handler)
        .with_runtime(Arc::new(TokioRuntime))
        .with_udp_addrs(vec![udp_address.to_owned()])
        .build()
        .await
        .map_err(|error| format!("Não foi possível criar a conexão WebRTC P2P: {error}"))?;
    Ok(Arc::new(connection))
}

async fn create_sender(
    source: LatestFrame,
    events: &std_mpsc::Sender<ScreenShareEvent>,
    context: egui::Context,
    remote_frame: RemoteFrameStore,
    remote_frame_sequence: Arc<AtomicU64>,
    udp_address: &str,
) -> Result<PeerSession, String> {
    let connection = create_peer(
        events,
        context,
        remote_frame,
        remote_frame_sequence,
        udp_address,
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

    let (sample_tx, sample_rx) = watch::channel(None::<(u64, Vec<u8>)>);
    let encoder_stop = Arc::new(AtomicBool::new(false));
    let encoder_stop_worker = Arc::clone(&encoder_stop);
    let encoder_events = events.clone();
    let encoder_task = tokio::task::spawn_blocking(move || {
        match encode_latest_frames(source, sample_tx, encoder_stop_worker) {
            Ok(()) => Ok(()),
            Err(error) => {
                let _ = encoder_events.send(ScreenShareEvent::Error(error.clone()));
                Err(error)
            }
        }
    });
    let writer_track = Arc::clone(&track);
    let writer_events = events.clone();
    let sample_writer_task = tokio::spawn(async move {
        let Some(ssrc) = writer_track.ssrcs().await.first().copied() else {
            let _ = writer_events.send(ScreenShareEvent::Error(
                "A trilha H.264 não recebeu um identificador RTP.".to_owned(),
            ));
            return;
        };
        let mut samples = sample_rx;
        while samples.changed().await.is_ok() {
            let Some((_, data)) = samples.borrow_and_update().clone() else {
                continue;
            };
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
    udp_address: &str,
) -> Result<PeerSession, String> {
    let connection = create_peer(
        events,
        context,
        remote_frame,
        remote_frame_sequence,
        udp_address,
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
    samples: watch::Sender<Option<(u64, Vec<u8>)>>,
    stop: Arc<AtomicBool>,
) -> Result<(), String> {
    let encoder_config = EncoderConfig::new()
        .bitrate(BitRate::from_bps(4_000_000))
        .max_frame_rate(FrameRate::from_hz(30.0))
        .usage_type(UsageType::ScreenContentRealTime)
        .adaptive_quantization(false)
        .background_detection(false)
        .intra_frame_period(IntraFramePeriod::from_num_frames(60));
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
            samples.send_replace(Some((frame.sequence, encoded)));
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
        encode_frame,
    };

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
