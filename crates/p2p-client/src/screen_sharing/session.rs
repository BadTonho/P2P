use super::*;

#[derive(Default)]
struct PliForwarder {
    read_queue: VecDeque<TaggedPacket>,
    write_queue: VecDeque<TaggedPacket>,
    metrics: Option<Arc<SharedMetrics>>,
}

impl sansio::Protocol<TaggedPacket, TaggedPacket, ()> for PliForwarder {
    type Rout = TaggedPacket;
    type Wout = TaggedPacket;
    type Eout = ();
    type Error = InterceptorError;
    type Time = Instant;

    fn handle_read(&mut self, mut message: TaggedPacket) -> Result<(), Self::Error> {
        if let InterceptorPacket::Rtcp(packets) = &message.message.packet {
            if let Some(metrics) = &self.metrics {
                for packet in packets {
                    if let Some(nack) = packet.as_any().downcast_ref::<TransportLayerNack>() {
                        metrics.record_inbound_nack_packet(nack.media_ssrc);
                    }
                }
            }
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

#[cfg(test)]
#[derive(Default)]
struct TestRtpLossInjector {
    read_queue: VecDeque<TaggedPacket>,
    write_queue: VecDeque<TaggedPacket>,
    metrics: Option<Arc<SharedMetrics>>,
}

#[cfg(test)]
impl sansio::Protocol<TaggedPacket, TaggedPacket, ()> for TestRtpLossInjector {
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
        if matches!(&message.message.packet, InterceptorPacket::Rtp(_))
            && self.metrics.as_ref().is_some_and(|metrics| {
                metrics
                    .test_drop_next_outbound_rtp
                    .compare_exchange(true, false, Ordering::Relaxed, Ordering::Relaxed)
                    .is_ok()
            })
        {
            if let Some(metrics) = &self.metrics {
                metrics
                    .test_dropped_outbound_rtp
                    .fetch_add(1, Ordering::Relaxed);
            }
            return Ok(());
        }
        self.write_queue.push_back(message);
        Ok(())
    }

    fn poll_write(&mut self) -> Option<Self::Wout> {
        self.write_queue.pop_front()
    }
}

#[cfg(test)]
impl Interceptor for TestRtpLossInjector {
    fn bind_local_stream(&mut self, _info: &StreamInfo) {}
    fn unbind_local_stream(&mut self, _info: &StreamInfo) {}
    fn bind_remote_stream(&mut self, _info: &StreamInfo) {}
    fn unbind_remote_stream(&mut self, _info: &StreamInfo) {}
}

#[derive(Debug)]
pub enum ScreenShareEvent {
    Signal { kind: SignalKind, payload: String },
    State(String),
    AudioState(String),
    Error(String),
    AudioError(String),
    ConnectionClosed,
}

pub(super) fn capture_audio_state(
    callbacks: u64,
    non_silent_samples: u64,
    encoded_frames: u64,
    rtp_samples: u64,
) -> String {
    if callbacks == 0 {
        "Áudio do sistema: o loopback não recebeu callbacks nesta janela; o vídeo segue independente."
            .to_owned()
    } else if non_silent_samples == 0 {
        "Áudio do sistema: captura ativa e silenciosa nesta janela; silêncio não é erro.".to_owned()
    } else if encoded_frames == 0 {
        "Áudio do sistema: amostras audíveis capturadas, mas nenhum quadro Opus foi codificado nesta janela."
            .to_owned()
    } else {
        format!(
            "Áudio do sistema: {encoded_frames} quadros Opus codificados e {rtp_samples} aceitos pela faixa RTP nesta janela."
        )
    }
}

pub(super) fn playback_audio_state(
    rtp_packets: u64,
    decoded_frames: u64,
    decoded_non_silent_samples: u64,
    output_callbacks: u64,
    output_non_silent_samples: u64,
) -> String {
    if rtp_packets == 0 {
        "Áudio remoto: nenhum pacote RTP recebido nesta janela.".to_owned()
    } else if decoded_frames == 0 {
        "Áudio remoto: pacotes recebidos, mas nenhum quadro Opus decodificado nesta janela."
            .to_owned()
    } else if decoded_non_silent_samples == 0 {
        "Áudio remoto: quadros Opus decodificados sem amostras audíveis; pode ser silêncio da fonte."
            .to_owned()
    } else if output_callbacks == 0 {
        "Áudio remoto: áudio decodificado, mas a saída do Windows não executou callbacks nesta janela."
            .to_owned()
    } else if output_non_silent_samples == 0 {
        "Áudio remoto: áudio decodificado, mas nenhuma amostra audível chegou à saída nesta janela."
            .to_owned()
    } else {
        "Áudio remoto: decodificação e saída do Windows ativas.".to_owned()
    }
}

enum Command {
    StartSending {
        source: LatestFrame,
        bitrate_bps: u32,
        include_system_audio: bool,
        audio_source_override: Option<Box<dyn AudioSampleSource>>,
    },
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
        Self::new_with_port(
            context,
            bind_ipv4,
            MEDIA_UDP_PORT,
            stun_server,
            turn_credentials,
            decoder_preference,
        )
    }

    pub fn new_with_port(
        context: egui::Context,
        bind_ipv4: Ipv4Addr,
        udp_port: u16,
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
            format!("{bind_ipv4}:{udp_port}"),
            stun_server,
            turn_credentials,
            decoder_preference,
        )
    }

    #[cfg(test)]
    fn new_loopback(context: egui::Context) -> Result<Self, String> {
        Self::new_loopback_with_decoder_preference(context, VideoDecoderPreference::Automatic)
    }

    #[cfg(test)]
    fn new_loopback_with_decoder_preference(
        context: egui::Context,
        decoder_preference: VideoDecoderPreference,
    ) -> Result<Self, String> {
        Self::with_udp_address(
            context,
            "127.0.0.1:0".to_owned(),
            None,
            None,
            decoder_preference,
        )
    }

    fn with_udp_address(
        context: egui::Context,
        udp_address: String,
        stun_server: Option<String>,
        turn_credentials: Option<TurnCredentials>,
        decoder_preference: VideoDecoderPreference,
    ) -> Result<Self, String> {
        Self::with_udp_address_and_audio_factory(
            context,
            udp_address,
            stun_server,
            turn_credentials,
            decoder_preference,
            Arc::new(SystemAudioPlaybackFactory),
        )
    }

    fn with_udp_address_and_audio_factory(
        context: egui::Context,
        udp_address: String,
        stun_server: Option<String>,
        turn_credentials: Option<TurnCredentials>,
        decoder_preference: VideoDecoderPreference,
        audio_playback_factory: Arc<dyn AudioPlaybackFactory>,
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
        let worker_audio_playback_factory = Arc::clone(&audio_playback_factory);
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
                    worker_audio_playback_factory,
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
        self.start_sending_with_bitrate(source, MAX_PEER_MEDIA_BITRATE)
    }

    pub fn start_sending_with_bitrate(
        &self,
        source: LatestFrame,
        bitrate_bps: u32,
    ) -> Result<(), String> {
        self.start_sending_with_options(source, bitrate_bps, false)
    }

    pub fn start_sending_with_audio(
        &self,
        source: LatestFrame,
        include_system_audio: bool,
    ) -> Result<(), String> {
        self.start_sending_with_options(source, MAX_PEER_MEDIA_BITRATE, include_system_audio)
    }

    pub fn start_sending_with_options(
        &self,
        source: LatestFrame,
        bitrate_bps: u32,
        include_system_audio: bool,
    ) -> Result<(), String> {
        self.commands
            .send(Command::StartSending {
                source,
                bitrate_bps: bitrate_bps.clamp(250_000, MAX_PEER_MEDIA_BITRATE),
                include_system_audio,
                audio_source_override: None,
            })
            .map_err(|_| "A sessão WebRTC foi encerrada.".to_owned())
    }

    #[cfg(test)]
    fn start_sending_with_test_audio(
        &self,
        source: LatestFrame,
        audio_source: Box<dyn AudioSampleSource>,
    ) -> Result<(), String> {
        self.commands
            .send(Command::StartSending {
                source,
                bitrate_bps: MAX_PEER_MEDIA_BITRATE,
                include_system_audio: false,
                audio_source_override: Some(audio_source),
            })
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

    #[cfg(test)]
    fn drop_next_outbound_rtp_for_test(&self) {
        self.metrics
            .test_drop_next_outbound_rtp
            .store(true, Ordering::Relaxed);
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

    pub(crate) fn remote_video_track_seen(&self) -> bool {
        self.metrics.remote_video_track_seen.load(Ordering::Relaxed)
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
    audio_task: Option<TokioJoinHandle<()>>,
    rtcp_feedback_task: Option<TokioJoinHandle<()>>,
    #[allow(dead_code)] // Usado pelo teste loopback para emitir o PLI de diagnóstico.
    remote_track: RemoteTrackStore,
    #[allow(dead_code)] // Usado pelo teste loopback para limitar pedidos explícitos de PLI.
    keyframe_request_limiter: KeyframeRequestLimiter,
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
        PeerEvents::receive_remote_track(self, track).await;
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
    audio_playback_factory: Arc<dyn AudioPlaybackFactory>,
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
                        let video_track_seen = peer
                            .metrics
                            .remote_video_track_seen
                            .load(Ordering::Relaxed);
                        let snapshot = peer.metrics.snapshot();
                        tracing::warn!(
                            screen_share_session = peer.metrics.session_id,
                            stage = "first_video_timeout",
                            video_track_seen,
                            received_rtp_packets = packets,
                            assembled_access_units = snapshot.received_delta_frames,
                            decoder_inputs = snapshot.decoder_input_frames,
                            decoded_frames = snapshot.decoded_frames,
                            published_frames = snapshot.published_frames,
                            decode_errors,
                            "Sessão conectada sem publicar vídeo antes do timeout"
                        );
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
            Command::StartSending { source, bitrate_bps, include_system_audio, audio_source_override } => {
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
                    bitrate_bps,
                    include_system_audio,
                    audio_source_override,
                    Arc::clone(&audio_playback_factory),
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
                        Arc::clone(&audio_playback_factory),
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
                        let answer: RTCSessionDescription = serde_json::from_str(&payload)
                            .map_err(|error| format!("Resposta SDP inválida: {error}"))?;
                        let media_summary = summarize_sdp_media(&answer.sdp);
                        tracing::info!(
                            screen_share_session = peer.metrics.session_id,
                            stage = "remote_answer_received",
                            media = %media_summary,
                            "Resumo sanitizado da resposta remota"
                        );
                        peer.connection
                            .set_remote_description(answer)
                            .await
                            .map_err(|error| {
                                format!("Não foi possível aplicar a resposta SDP: {error}")
                            })?;
                        tracing::info!(
                            screen_share_session = peer.metrics.session_id,
                            stage = "remote_answer_applied",
                            media = %media_summary,
                            "Resposta remota aplicada à conexão WebRTC"
                        );
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
                        super::receiver::send_picture_loss_indication(
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
        snapshot.outbound_nack_count = Some(u64::from(outbound.nack_count));
        snapshot.outbound_retransmitted_packets = Some(outbound.retransmitted_packets_sent);
        snapshot.outbound_retransmitted_bytes = Some(outbound.retransmitted_bytes_sent);
        snapshot.outbound_summary = format!(
            "RTP de saída: {} pacotes / {} bytes; frames codificados/enviados {}/{}; pacotes NACK recebidos {}; retransmissões enviadas {} pacotes / {} bytes; SSRC {}; encoder RTC {}",
            snapshot.outbound_packets,
            snapshot.outbound_bytes,
            snapshot.outbound_frames_encoded,
            snapshot.outbound_frames_sent,
            snapshot.outbound_nack_count.unwrap_or_default(),
            snapshot.outbound_retransmitted_packets.unwrap_or_default(),
            snapshot.outbound_retransmitted_bytes.unwrap_or_default(),
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
        snapshot.inbound_nack_count = Some(u64::from(inbound.nack_count));
        snapshot.inbound_retransmitted_packets = Some(inbound.retransmitted_packets_received);
        snapshot.inbound_retransmitted_bytes = Some(inbound.retransmitted_bytes_received);
        snapshot.inbound_pli_count = inbound.pli_count;
        snapshot.inbound_jitter_ms = rtp_jitter_ticks_to_ms(received.jitter, 90_000.0);
        snapshot.inbound_frames_received = inbound.frames_received;
        snapshot.inbound_frames_decoded = inbound.frames_decoded;
        snapshot.inbound_frames_rendered = inbound.frames_rendered;
        snapshot.inbound_frames_dropped = inbound.frames_dropped;
        snapshot.inbound_packets_discarded = inbound.packets_discarded;
        snapshot.inbound_ssrc = received.rtp_stream_stats.ssrc;
        snapshot.inbound_frame_width = inbound.frame_width;
        snapshot.inbound_frame_height = inbound.frame_height;
        snapshot.inbound_summary = format!(
            "RTP de entrada: {} pacotes / {} bytes; perda reportada {}; jitter {:.1} ms; pacotes NACK enviados {}, PLI enviados {}; retransmissões recebidas {} pacotes / {} bytes; frames recebidos/decodificados/renderizados/descartados {}/{}/{}/{}; resolução {}x{}; descartados no jitter buffer {}; SSRC {}; decoder RTC {}",
            snapshot.inbound_packets,
            snapshot.inbound_bytes,
            snapshot.inbound_packets_lost,
            snapshot.inbound_jitter_ms,
            snapshot.inbound_nack_count.unwrap_or_default(),
            snapshot.inbound_pli_count,
            snapshot.inbound_retransmitted_packets.unwrap_or_default(),
            snapshot.inbound_retransmitted_bytes.unwrap_or_default(),
            snapshot.inbound_frames_received,
            snapshot.inbound_frames_decoded,
            snapshot.inbound_frames_rendered,
            snapshot.inbound_frames_dropped,
            snapshot.inbound_frame_width,
            snapshot.inbound_frame_height,
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
    audio_playback_factory: Arc<dyn AudioPlaybackFactory>,
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
    let opus_codec = RTCRtpCodecParameters {
        rtp_codec: RTCRtpCodec {
            mime_type: MIME_TYPE_OPUS.to_owned(),
            clock_rate: OPUS_SAMPLE_RATE,
            channels: OPUS_CHANNELS as u16,
            sdp_fmtp_line: "minptime=10;useinbandfec=1".to_owned(),
            rtcp_feedback: vec![],
        },
        payload_type: AUDIO_PAYLOAD_TYPE,
        ..Default::default()
    };
    media_engine
        .register_codec(opus_codec, RtpCodecKind::Audio)
        .map_err(|error| format!("Não foi possível registrar Opus no WebRTC: {error}"))?;
    let interceptors = register_default_interceptors(Registry::new(), &mut media_engine)
        .map_err(|error| format!("Não foi possível preparar o WebRTC: {error}"))?;
    let interceptors = interceptors.with(
        Slot::Custom(14_000),
        PliForwarder {
            metrics: Some(Arc::clone(&metrics)),
            ..PliForwarder::default()
        },
    );
    #[cfg(test)]
    let interceptors = interceptors.with(
        Slot::Custom(3_000),
        TestRtpLossInjector {
            metrics: Some(Arc::clone(&metrics)),
            ..TestRtpLossInjector::default()
        },
    );
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
        audio_playback_factory,
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
            let port = udp_address
                .rsplit_once(':')
                .and_then(|(_, port)| port.parse::<u16>().ok());
            if port.is_some_and(|port| (9002..=9009).contains(&port)) {
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
    bitrate_bps: u32,
    include_system_audio: bool,
    audio_source_override: Option<Box<dyn AudioSampleSource>>,
    audio_playback_factory: Arc<dyn AudioPlaybackFactory>,
) -> Result<PeerSession, String> {
    let include_audio = include_system_audio || audio_source_override.is_some();
    let (system_audio, audio_setup_error) = if include_audio {
        let mut audio_source_override = audio_source_override;
        let result = (|| {
            let capture: Box<dyn AudioSampleSource> = match audio_source_override.take() {
                Some(capture) => capture,
                None => Box::new(SystemAudioCapture::start(metrics.session_id)?),
            };
            let (input_rate_hz, input_channels) = capture.input_format();
            tracing::info!(
                screen_share_session = metrics.session_id,
                input_rate_hz,
                input_channels,
                opus_rate_hz = OPUS_SAMPLE_RATE,
                "Pipeline de áudio do sistema preparado para envio"
            );
            let encoder = opus::Encoder::new(
                OPUS_SAMPLE_RATE,
                opus::Channels::Stereo,
                opus::Application::Audio,
            )
            .map_err(|error| format!("Não foi possível iniciar o encoder Opus: {error}"))?;
            Ok::<_, String>((capture, encoder))
        })();
        match result {
            Ok(audio) => (Some(audio), None),
            Err(error) => {
                tracing::error!(
                    screen_share_session = metrics.session_id,
                    stage = "system_audio_setup",
                    error = %error,
                    "O som não será enviado; a transmissão de vídeo continuará"
                );
                (None, Some(error))
            }
        }
    } else {
        (None, None)
    };
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
        audio_playback_factory,
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

    let audio_track = if system_audio.is_some() {
        let codec = RTCRtpCodec {
            mime_type: MIME_TYPE_OPUS.to_owned(),
            clock_rate: OPUS_SAMPLE_RATE,
            channels: OPUS_CHANNELS as u16,
            sdp_fmtp_line: "minptime=10;useinbandfec=1".to_owned(),
            rtcp_feedback: vec![],
        };
        let audio_ssrc = unique_ssrc();
        let audio_track = Arc::new(
            TrackLocalStaticSample::new(
                Instant::now(),
                MediaStreamTrack::new(
                    "p2p-screen-audio-stream".to_owned(),
                    "p2p-screen-audio-track".to_owned(),
                    "Áudio do computador".to_owned(),
                    RtpCodecKind::Audio,
                    vec![RTCRtpEncodingParameters {
                        rtp_coding_parameters: RTCRtpCodingParameters {
                            ssrc: Some(audio_ssrc),
                            ..Default::default()
                        },
                        codec,
                        ..Default::default()
                    }],
                ),
            )
            .map_err(|error| format!("Não foi possível criar a faixa Opus: {error}"))?,
        );
        connection
            .add_track(Arc::clone(&audio_track) as Arc<dyn TrackLocal>)
            .await
            .map_err(|error| {
                format!("Não foi possível adicionar o áudio à conexão P2P: {error}")
            })?;
        Some(audio_track)
    } else {
        None
    };

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
    tracing::info!(
        screen_share_session = metrics.session_id,
        stage = "local_offer_created",
        media = %summarize_sdp_media(&local_description.sdp),
        "Resumo sanitizado da oferta local"
    );
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
            bitrate_bps,
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
            media_kind = "video",
            codec = MIME_TYPE_H264,
            payload_type = VIDEO_PAYLOAD_TYPE,
            track_id = "p2p-screen-track",
            stream_id = "p2p-screen-stream",
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

    let audio_task = match (system_audio, audio_track) {
        (Some((capture, encoder)), Some(audio_track)) => {
            let audio_events = events.clone();
            let session_id = metrics.session_id;
            Some(tokio::spawn(async move {
                send_system_audio(capture, encoder, audio_track, audio_events, session_id).await;
            }))
        }
        _ => None,
    };

    let _ = events.send(ScreenShareEvent::State(
        "Oferta enviada; aguardando conexão P2P com o participante.".to_owned(),
    ));
    if let Some(error) = audio_setup_error {
        let _ = events.send(ScreenShareEvent::AudioError(format!(
            "Não foi possível iniciar o som do computador; o vídeo continua: {error}"
        )));
    }
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
        audio_task,
        rtcp_feedback_task: Some(rtcp_feedback_task),
        remote_track,
        keyframe_request_limiter: KeyframeRequestLimiter::default(),
    })
}

async fn send_system_audio(
    mut capture: Box<dyn AudioSampleSource>,
    mut encoder: opus::Encoder,
    track: Arc<TrackLocalStaticSample>,
    events: std_mpsc::Sender<ScreenShareEvent>,
    session_id: u64,
) {
    let Some(ssrc) = track.ssrcs().await.first().copied() else {
        let message = "A faixa Opus não recebeu um identificador RTP.".to_owned();
        tracing::error!(
            screen_share_session = session_id,
            stage = "audio_track_bind",
            "Faixa de áudio sem SSRC"
        );
        let _ = events.send(ScreenShareEvent::AudioError(message));
        return;
    };
    tracing::info!(
        screen_share_session = session_id,
        audio_track_ssrc = ssrc,
        codec = "Opus",
        sample_rate_hz = OPUS_SAMPLE_RATE,
        channels = OPUS_CHANNELS,
        "Faixa RTP de áudio do sistema pronta"
    );

    let mut interval = tokio::time::interval(Duration::from_millis(20));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let frame_len = OPUS_FRAME_SAMPLES_PER_CHANNEL * OPUS_CHANNELS;
    let mut pcm = vec![0.0_f32; frame_len];
    let mut filled = 0_usize;
    let mut encoded = vec![0_u8; 4_000];
    let mut encoded_frames = 0_u64;
    let mut encoded_bytes = 0_u64;
    let mut written_samples = 0_u64;
    let mut write_failures = 0_u64;
    let mut last_report_at = Instant::now();
    let mut last_callbacks = capture.callbacks();
    let mut last_input_frames = capture.input_frames();
    let mut last_non_silent_samples = capture.non_silent_samples();
    let mut last_capture_frames = capture.captured_frames();
    let mut last_dropped_frames = capture.dropped_frames();
    let mut last_xruns = capture.xruns();
    let mut last_device_changes = capture.device_changes();
    let mut last_realtime_denied = capture.realtime_denied();
    let mut last_encoded_frames = 0_u64;
    let mut last_encoded_bytes = 0_u64;
    let mut last_written_samples = 0_u64;
    let mut last_write_failures = 0_u64;
    let mut last_audio_state = String::new();

    loop {
        interval.tick().await;
        if let Some((kind, error)) = capture.take_error() {
            tracing::error!(
                screen_share_session = session_id,
                audio_track_ssrc = ssrc,
                stage = "wasapi_loopback_callback",
                error_kind = ?kind,
                error = %error,
                "Captura do áudio do sistema interrompida"
            );
            let _ = events.send(ScreenShareEvent::AudioError(format!(
                "A captura do som do computador parou ({kind:?}): {error}"
            )));
            break;
        }

        filled += capture.read_samples(&mut pcm[filled..]);
        if filled == frame_len {
            let encoded_len = match encoder.encode_float(&pcm, &mut encoded) {
                Ok(length) if length > 0 => length,
                Ok(_) => {
                    tracing::warn!(
                        screen_share_session = session_id,
                        audio_track_ssrc = ssrc,
                        stage = "opus_encode",
                        "Encoder Opus produziu amostra vazia"
                    );
                    filled = 0;
                    continue;
                }
                Err(error) => {
                    let detail = format!("Falha ao codificar áudio com Opus: {error}");
                    tracing::error!(
                        screen_share_session = session_id,
                        audio_track_ssrc = ssrc,
                        stage = "opus_encode",
                        error = %detail,
                        "Encoder de áudio falhou"
                    );
                    let _ = events.send(ScreenShareEvent::AudioError(detail));
                    break;
                }
            };
            encoded_frames = encoded_frames.saturating_add(1);
            encoded_bytes = encoded_bytes.saturating_add(encoded_len as u64);
            let sample = Sample {
                data: Bytes::copy_from_slice(&encoded[..encoded_len]),
                duration: Duration::from_millis(20),
                ..Sample::new(Instant::now())
            };
            match track
                .sample_writer(ssrc, AUDIO_PAYLOAD_TYPE)
                .write_sample(&sample)
                .await
            {
                Ok(()) => written_samples = written_samples.saturating_add(1),
                Err(error) => {
                    write_failures = write_failures.saturating_add(1);
                    let detail = format!("A faixa WebRTC recusou uma amostra Opus: {error}");
                    tracing::error!(
                        screen_share_session = session_id,
                        audio_track_ssrc = ssrc,
                        stage = "audio_rtp_write",
                        error = %error,
                        encoded_frames,
                        "Falha ao enviar áudio pela faixa WebRTC"
                    );
                    let _ = events.send(ScreenShareEvent::AudioError(detail));
                    break;
                }
            }
            filled = 0;
        }

        if last_report_at.elapsed() >= Duration::from_secs(5) {
            let callbacks = capture.callbacks();
            let input_frames = capture.input_frames();
            let non_silent_samples = capture.non_silent_samples();
            let captured = capture.captured_frames();
            let dropped = capture.dropped_frames();
            let xruns = capture.xruns();
            let device_changes = capture.device_changes();
            let realtime_denied = capture.realtime_denied();
            let callback_delta = callbacks.saturating_sub(last_callbacks);
            let input_frame_delta = input_frames.saturating_sub(last_input_frames);
            let non_silent_delta = non_silent_samples.saturating_sub(last_non_silent_samples);
            let xrun_delta = xruns.saturating_sub(last_xruns);
            let device_change_delta = device_changes.saturating_sub(last_device_changes);
            let realtime_denied_delta = realtime_denied.saturating_sub(last_realtime_denied);
            let interval_seconds = last_report_at.elapsed().as_secs_f64();
            let encoded_delta = encoded_frames.saturating_sub(last_encoded_frames);
            let written_delta = written_samples.saturating_sub(last_written_samples);
            let audio_state = capture_audio_state(
                callback_delta,
                non_silent_delta,
                encoded_delta,
                written_delta,
            );
            if audio_state != last_audio_state {
                let _ = events.send(ScreenShareEvent::AudioState(audio_state.clone()));
                last_audio_state = audio_state;
            }
            if xrun_delta + device_change_delta + realtime_denied_delta > 0 {
                tracing::warn!(
                    screen_share_session = session_id,
                    audio_track_ssrc = ssrc,
                    interval_seconds,
                    capture_callbacks = callback_delta,
                    input_frames = input_frame_delta,
                    non_silent_samples = non_silent_delta,
                    captured_frames = captured.saturating_sub(last_capture_frames),
                    capture_queue_drops = dropped.saturating_sub(last_dropped_frames),
                    xrun_warnings = xrun_delta,
                    device_change_warnings = device_change_delta,
                    realtime_priority_warnings = realtime_denied_delta,
                    encoded_frames = encoded_frames.saturating_sub(last_encoded_frames),
                    encoded_bytes = encoded_bytes.saturating_sub(last_encoded_bytes),
                    rtp_samples_accepted = written_samples.saturating_sub(last_written_samples),
                    rtp_write_failures = write_failures.saturating_sub(last_write_failures),
                    pending_pcm_samples = filled,
                    "Resumo de áudio com avisos recuperáveis; captura continua ativa"
                );
            } else {
                tracing::info!(
                    screen_share_session = session_id,
                    audio_track_ssrc = ssrc,
                    interval_seconds,
                    capture_callbacks = callback_delta,
                    input_frames = input_frame_delta,
                    non_silent_samples = non_silent_delta,
                    captured_frames = captured.saturating_sub(last_capture_frames),
                    capture_queue_drops = dropped.saturating_sub(last_dropped_frames),
                    xrun_warnings = xrun_delta,
                    device_change_warnings = device_change_delta,
                    realtime_priority_warnings = realtime_denied_delta,
                    encoded_frames = encoded_frames.saturating_sub(last_encoded_frames),
                    encoded_bytes = encoded_bytes.saturating_sub(last_encoded_bytes),
                    rtp_samples_accepted = written_samples.saturating_sub(last_written_samples),
                    rtp_write_failures = write_failures.saturating_sub(last_write_failures),
                    pending_pcm_samples = filled,
                    "Resumo periódico do envio de áudio do sistema"
                );
            }
            last_report_at = Instant::now();
            last_callbacks = callbacks;
            last_input_frames = input_frames;
            last_non_silent_samples = non_silent_samples;
            last_capture_frames = captured;
            last_dropped_frames = dropped;
            last_xruns = xruns;
            last_device_changes = device_changes;
            last_realtime_denied = realtime_denied;
            last_encoded_frames = encoded_frames;
            last_encoded_bytes = encoded_bytes;
            last_written_samples = written_samples;
            last_write_failures = write_failures;
        }
    }

    tracing::info!(
        screen_share_session = session_id,
        audio_track_ssrc = ssrc,
        encoded_frames,
        encoded_bytes,
        capture_callbacks = capture.callbacks(),
        input_frames = capture.input_frames(),
        non_silent_samples = capture.non_silent_samples(),
        xrun_warnings = capture.xruns(),
        device_change_warnings = capture.device_changes(),
        realtime_priority_warnings = capture.realtime_denied(),
        captured_frames = capture.captured_frames(),
        rtp_samples_accepted = written_samples,
        rtp_write_failures = write_failures,
        capture_queue_drops = capture.dropped_frames(),
        "Envio de áudio do sistema encerrado"
    );
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
    audio_playback_factory: Arc<dyn AudioPlaybackFactory>,
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
        audio_playback_factory,
    )
    .await?;
    let offer: RTCSessionDescription =
        serde_json::from_str(&payload).map_err(|error| format!("Oferta SDP inválida: {error}"))?;
    let remote_offer_summary = summarize_sdp_media(&offer.sdp);
    tracing::info!(
        screen_share_session = metrics.session_id,
        stage = "remote_offer_received",
        media = %remote_offer_summary,
        "Resumo sanitizado da oferta remota"
    );
    connection
        .set_remote_description(offer)
        .await
        .map_err(|error| format!("Não foi possível aplicar a oferta WebRTC: {error}"))?;
    tracing::info!(
        screen_share_session = metrics.session_id,
        stage = "remote_offer_applied",
        media = %remote_offer_summary,
        "Oferta remota aplicada à conexão WebRTC"
    );
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
    tracing::info!(
        screen_share_session = metrics.session_id,
        stage = "local_answer_created",
        media = %summarize_sdp_media(&local_description.sdp),
        "Resumo sanitizado da resposta local"
    );
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
        audio_task: None,
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
    if let Some(audio_task) = peer.audio_task.take() {
        audio_task.abort();
    }
    if let Some(feedback) = peer.rtcp_feedback_task.take() {
        feedback.abort();
    }
    if let Some(encoder) = peer.encoder_task.take() {
        let _ = timeout(Duration::from_secs(2), encoder).await;
    }
    let _ = timeout(Duration::from_secs(2), peer.connection.close()).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio_capture::AudioPlaybackSink;
    use rtc::peer_connection::transport::RTCIceCandidateType;
    use std::f32::consts::TAU;

    #[derive(Default)]
    struct SyntheticAudioProbe {
        source_callbacks: AtomicU64,
        source_frames: AtomicU64,
        source_audible_samples: AtomicU64,
        source_silent_samples: AtomicU64,
        playback_factory_starts: AtomicU64,
        decoded_frames: AtomicU64,
        decoded_audible_samples: AtomicU64,
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

        fn take_error(&self) -> Option<(cpal::ErrorKind, String)> {
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
        ) -> Result<Box<dyn AudioPlaybackSink>, String> {
            self.probe
                .playback_factory_starts
                .fetch_add(1, Ordering::Relaxed);
            Ok(Box::new(SyntheticAudioPlayback {
                probe: Arc::clone(&self.probe),
            }))
        }
    }

    struct SyntheticAudioPlayback {
        probe: Arc<SyntheticAudioProbe>,
    }

    impl AudioPlaybackSink for SyntheticAudioPlayback {
        fn push_decoded(&mut self, samples: &[f32], frames_per_channel: usize) {
            self.probe.decoded_frames.fetch_add(1, Ordering::Relaxed);
            let audible = samples
                .iter()
                .take(frames_per_channel * OPUS_CHANNELS)
                .filter(|sample| sample.abs() > 0.01)
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
        assert!(
            playback_audio_state(250, 250, 1500, 300, 6000).contains("saída do Windows ativas")
        );
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
        let mut playback = playback_factory.start(1, 1).unwrap();
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
        while Instant::now() < deadline {
            pump_signals(&a_sender, &b_receiver, "A to B offer");
            pump_signals(&b_receiver, &a_sender, "B to A answer");
            pump_signals(&b_sender, &a_receiver, "B to A offer");
            pump_signals(&a_receiver, &b_sender, "A to B answer");

            a_received |= a_receiver.latest_remote_frame().is_some();
            b_received |= b_receiver.latest_remote_frame().is_some();
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
                    && b_receive_audio_probe.decoded_frames.load(Ordering::Relaxed) > 0
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
            "o receptor deve decodificar o IDR de recuperaÃ§Ã£o; receiver metrics: {:?}",
            receiver_metrics
        );
        assert!(
            receiver_metrics.pli_requests_sent >= 1,
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
}
