use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::mpsc as std_mpsc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use signaling_protocol::{ControlMessage, ParticipantInfo};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, mpsc};
use tokio::time::{interval, timeout};
use tokio_tungstenite::tungstenite::Message as WebSocketMessage;
use tokio_tungstenite::{WebSocketStream, accept_async, connect_async};

const CONTROL_LISTEN_ADDRESS: &str = "0.0.0.0:9001";
const PROBE_PERIOD: Duration = Duration::from_secs(1);
const PROBE_WINDOW: Duration = Duration::from_secs(30);
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);
const HOST_TIMEOUT: Duration = Duration::from_secs(5);
const CONTROL_LINK_STARTUP_GRACE: Duration = Duration::from_secs(15);
const EMPTY_ELECTION_RETRY_INTERVAL: Duration = Duration::from_secs(10);
const CANDIDATE_TIMEOUT: Duration = Duration::from_secs(10);
const UNSTABLE_LOSS_PERCENT: f32 = 20.0;
const HEALTHY_LOSS_PERCENT: f32 = 5.0;
const HEALTHY_REENTRY_TIME: Duration = Duration::from_secs(30);
const MIN_HEALTH_SAMPLES: usize = 5;

#[derive(Clone, Debug)]
pub struct QueueEntry {
    pub participant: ParticipantInfo,
    pub loss_percent: f32,
    pub jitter_ms: f32,
    pub latency_ms: f32,
    pub eligible: bool,
}

#[derive(Debug)]
pub enum ControlEvent {
    Ready,
    Error(String),
    QueueUpdated(Vec<QueueEntry>),
    LinksUpdated {
        connected: usize,
        total: usize,
    },
    BecomeHost {
        code: String,
        epoch: u64,
        participants: Vec<ParticipantInfo>,
    },
    LeaderChanged {
        participant_id: String,
        address: String,
        epoch: u64,
    },
    RoomEnded,
    HostUnstable {
        loss_percent: f32,
    },
}

enum Command {
    Roster {
        participants: Vec<ParticipantInfo>,
        leader_id: String,
        leader_address: String,
    },
    LeaveNormally,
    PublishLeader {
        address: String,
        epoch: u64,
    },
    CandidateFailed {
        epoch: u64,
    },
    EndRoom,
    Stop,
}

pub struct ControlMesh {
    commands: mpsc::UnboundedSender<Command>,
    events: std_mpsc::Receiver<ControlEvent>,
    _worker: JoinHandle<()>,
}

impl ControlMesh {
    pub fn start(
        local: ParticipantInfo,
        room_code: String,
        leader_id: String,
        leader_address: String,
    ) -> Result<Self, String> {
        Self::start_on(
            local,
            room_code,
            leader_id,
            leader_address,
            CONTROL_LISTEN_ADDRESS.to_owned(),
        )
    }

    fn start_on(
        local: ParticipantInfo,
        room_code: String,
        leader_id: String,
        leader_address: String,
        listen_address: String,
    ) -> Result<Self, String> {
        let (command_tx, command_rx) = mpsc::unbounded_channel();
        let (event_tx, event_rx) = std_mpsc::channel();
        let worker = thread::Builder::new()
            .name("p2p-control-mesh".to_owned())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        let _ = event_tx.send(ControlEvent::Error(format!(
                            "Não foi possível iniciar a malha de controle: {error}"
                        )));
                        return;
                    }
                };
                runtime.block_on(run_mesh(
                    local,
                    room_code,
                    leader_id,
                    leader_address,
                    listen_address,
                    command_rx,
                    event_tx,
                ));
            })
            .map_err(|error| format!("Não foi possível iniciar o controle direto: {error}"))?;
        Ok(Self {
            commands: command_tx,
            events: event_rx,
            _worker: worker,
        })
    }

    pub fn update_roster(
        &self,
        participants: Vec<ParticipantInfo>,
        leader_id: String,
        leader_address: String,
    ) {
        let _ = self.commands.send(Command::Roster {
            participants,
            leader_id,
            leader_address,
        });
    }

    pub fn leave_normally(&self) -> bool {
        self.commands.send(Command::LeaveNormally).is_ok()
    }

    pub fn publish_leader(&self, address: String, epoch: u64) {
        let _ = self
            .commands
            .send(Command::PublishLeader { address, epoch });
    }

    pub fn candidate_failed(&self, epoch: u64) {
        let _ = self.commands.send(Command::CandidateFailed { epoch });
    }

    pub fn end_room(&self) -> bool {
        self.commands.send(Command::EndRoom).is_ok()
    }

    pub fn try_recv(&self) -> Option<ControlEvent> {
        self.events.try_recv().ok()
    }
}

impl Drop for ControlMesh {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Stop);
    }
}

#[derive(Clone)]
struct PeerLink {
    sender: mpsc::UnboundedSender<ControlMessage>,
    stats: std::sync::Arc<Mutex<PeerStats>>,
    last_seen: Instant,
}

#[derive(Clone)]
struct Probe {
    sequence: u64,
    sent_at: Instant,
    rtt_ms: Option<f32>,
    finalized: bool,
}

#[derive(Default)]
struct PeerStats {
    probes: VecDeque<Probe>,
    consecutive_losses: u32,
}

#[derive(Clone, Copy, Debug)]
struct Metrics {
    loss_percent: f32,
    jitter_ms: f32,
    latency_ms: f32,
    samples: usize,
    consecutive_losses: u32,
}

impl PeerStats {
    fn add_probe(&mut self, sequence: u64, now: Instant) {
        self.probes.push_back(Probe {
            sequence,
            sent_at: now,
            rtt_ms: None,
            finalized: false,
        });
        self.finalize_old(now);
        while self
            .probes
            .front()
            .is_some_and(|probe| now.duration_since(probe.sent_at) > PROBE_WINDOW)
        {
            self.probes.pop_front();
        }
    }

    fn record_pong(&mut self, sequence: u64, now: Instant) {
        if let Some(probe) = self
            .probes
            .iter_mut()
            .find(|probe| probe.sequence == sequence)
        {
            if probe.rtt_ms.is_none() {
                probe.rtt_ms = Some(now.duration_since(probe.sent_at).as_secs_f32() * 1000.0);
                probe.finalized = true;
                self.consecutive_losses = 0;
            }
        }
    }

    fn finalize_old(&mut self, now: Instant) {
        for probe in &mut self.probes {
            if !probe.finalized && now.duration_since(probe.sent_at) >= PROBE_TIMEOUT {
                probe.finalized = true;
                if probe.rtt_ms.is_none() {
                    self.consecutive_losses = self.consecutive_losses.saturating_add(1);
                }
            }
        }
    }

    fn metrics(&mut self, now: Instant) -> Metrics {
        self.finalize_old(now);
        let recent = self
            .probes
            .iter()
            .filter(|probe| now.duration_since(probe.sent_at) <= PROBE_WINDOW && probe.finalized)
            .collect::<Vec<_>>();
        let lost = recent.iter().filter(|probe| probe.rtt_ms.is_none()).count();
        let rtts = recent
            .iter()
            .filter_map(|probe| probe.rtt_ms)
            .collect::<Vec<_>>();
        let latency_ms = if rtts.is_empty() {
            0.0
        } else {
            rtts.iter().sum::<f32>() / rtts.len() as f32
        };
        let jitter_ms = if rtts.len() < 2 {
            0.0
        } else {
            rtts.windows(2)
                .map(|pair| (pair[1] - pair[0]).abs())
                .sum::<f32>()
                / (rtts.len() - 1) as f32
        };
        Metrics {
            loss_percent: if recent.is_empty() {
                0.0
            } else {
                lost as f32 * 100.0 / recent.len() as f32
            },
            jitter_ms,
            latency_ms,
            samples: recent.len(),
            consecutive_losses: self.consecutive_losses,
        }
    }
}

struct Election {
    epoch: u64,
    candidates: Vec<String>,
    index: usize,
    deadline: Instant,
    departing_id: Option<String>,
}

struct MeshState {
    local: ParticipantInfo,
    room_code: String,
    roster: Vec<ParticipantInfo>,
    known_since: HashMap<String, Instant>,
    connected_once: HashSet<String>,
    link_lost_since: HashMap<String, Instant>,
    leader_id: String,
    leader_address: String,
    epoch: u64,
    sequence: u64,
    links: HashMap<String, PeerLink>,
    connecting: HashSet<String>,
    remote_status: HashMap<String, (Metrics, bool, Instant)>,
    election: Option<Election>,
    displaced_at: Option<Instant>,
    recovery_since: Option<Instant>,
    leader_last_seen: Instant,
    leader_control_seen: bool,
    last_unstable_notice: Option<Instant>,
    eligibility_revision: u64,
    empty_election_retry: Option<(Instant, u64)>,
}

type SharedState = std::sync::Arc<Mutex<MeshState>>;

async fn run_mesh(
    local: ParticipantInfo,
    room_code: String,
    leader_id: String,
    leader_address: String,
    listen_address: String,
    mut commands: mpsc::UnboundedReceiver<Command>,
    events: std_mpsc::Sender<ControlEvent>,
) {
    tracing::info!(listen_address = %listen_address, "Iniciando malha direta de controle");
    let listener = match TcpListener::bind(listen_address).await {
        Ok(listener) => listener,
        Err(error) => {
            tracing::error!(error = %error, "Falha ao abrir o listener TCP da malha de controle");
            let _ = events.send(ControlEvent::Error(format!(
                "Não foi possível abrir a porta de controle 9001. Libere-a no firewall e feche outro aplicativo que a esteja usando: {error}"
            )));
            return;
        }
    };
    let leader_is_local = leader_id == local.id;
    let state = std::sync::Arc::new(Mutex::new(MeshState {
        local,
        room_code,
        roster: Vec::new(),
        known_since: HashMap::new(),
        connected_once: HashSet::new(),
        link_lost_since: HashMap::new(),
        leader_id,
        leader_address,
        epoch: 0,
        sequence: 0,
        links: HashMap::new(),
        connecting: HashSet::new(),
        remote_status: HashMap::new(),
        election: None,
        displaced_at: None,
        recovery_since: None,
        leader_last_seen: Instant::now(),
        leader_control_seen: leader_is_local,
        last_unstable_notice: None,
        eligibility_revision: 0,
        empty_election_retry: None,
    }));
    let _ = events.send(ControlEvent::Ready);
    tracing::info!("Listener da malha de controle pronto");
    let mut ticker = interval(PROBE_PERIOD);

    loop {
        tokio::select! {
            accepted = listener.accept() => {
                match accepted {
                    Ok((stream, address)) => {
                        tracing::debug!(peer = %address, "Conexão TCP recebida para malha de controle");
                        let task_state = state.clone();
                        let task_events = events.clone();
                        tokio::spawn(async move {
                            if let Err(error) = accept_peer(stream, task_state.clone(), task_events).await {
                                tracing::warn!(error = %error, "Falha no canal de controle recebido");
                            }
                        });
                    }
                    Err(error) => tracing::error!(error = %error, "Falha ao aceitar conexão da malha de controle"),
                }
            }
            command = commands.recv() => {
                match command {
                    Some(Command::Roster { participants, leader_id, leader_address }) => {
                        let mut state = state.lock().await;
                        if let Some(local) = participants.iter().find(|participant| participant.id == state.local.id) {
                            state.local = local.clone();
                        }
                        let known_ids = participants.iter().map(|participant| participant.id.as_str()).collect::<HashSet<_>>();
                        let roster_changed = participants.len() != state.roster.len()
                            || participants.iter().zip(&state.roster).any(|(new, old)| {
                                new.id != old.id
                                    || new.may_host != old.may_host
                                    || new.control_address != old.control_address
                            });
                        if roster_changed {
                            state.eligibility_revision = state.eligibility_revision.wrapping_add(1);
                        }
                        for participant in &participants {
                            state.known_since.entry(participant.id.clone()).or_insert_with(Instant::now);
                        }
                        state.known_since.retain(|id, _| known_ids.contains(id.as_str()));
                        state.connected_once.retain(|id| known_ids.contains(id.as_str()));
                        state.link_lost_since.retain(|id, _| known_ids.contains(id.as_str()));
                        state.links.retain(|id, _| known_ids.contains(id.as_str()));
                        state.remote_status.retain(|id, _| known_ids.contains(id.as_str()));
                        tracing::info!(participants = participants.len(), leader_address = %leader_address, "Lista da sala sincronizada na malha de controle");
                        state.roster = participants;
                        if !leader_id.is_empty()
                            && (state.epoch == 0 || state.leader_id == leader_id)
                        {
                            if state.leader_id != leader_id {
                                state.leader_last_seen = Instant::now();
                                state.leader_control_seen = leader_id == state.local.id;
                            }
                            state.leader_id = leader_id;
                            state.leader_address = leader_address;
                        }
                    }
                    Some(Command::LeaveNormally) => {
                        tracing::info!("Iniciando sucessão por saída normal do anfitrião");
                        let local_id = state.lock().await.local.id.clone();
                        initiate_election(&state, &events, true, true, Some(local_id)).await;
                    }
                    Some(Command::PublishLeader { address, epoch }) => {
                        tracing::info!(epoch, address = %address, "Publicando endereço do novo anfitrião");
                        publish_leader(&state, &events, address, epoch).await;
                    }
                    Some(Command::CandidateFailed { epoch }) => {
                        tracing::warn!(epoch, "Candidato da sucessão não ficou pronto; avançando fila");
                        let failed = state.lock().await.election.as_ref()
                            .filter(|election| election.epoch == epoch)
                            .and_then(|election| election.candidates.get(election.index).cloned());
                        if let Some(participant_id) = failed {
                            broadcast(&state, ControlMessage::CandidateFailed { participant_id, epoch }).await;
                        }
                        fail_current_candidate(&state, &events, epoch).await;
                    }
                    Some(Command::EndRoom) => {
                        tracing::warn!("Anfitrião encerrou explicitamente a sala");
                        let epoch = {
                            let mut state = state.lock().await;
                            state.epoch = state.epoch.saturating_add(1);
                            state.election = None;
                            state.epoch
                        };
                        broadcast(&state, ControlMessage::EndRoom { epoch }).await;
                        let _ = events.send(ControlEvent::RoomEnded);
                    }
                    Some(Command::Stop) | None => break,
                }
            }
            _ = ticker.tick() => {
                tick_mesh(&state, &events).await;
            }
        }
    }
}

async fn accept_peer(
    stream: TcpStream,
    state: SharedState,
    events: std_mpsc::Sender<ControlEvent>,
) -> Result<(), String> {
    let websocket = accept_async(stream)
        .await
        .map_err(|error| error.to_string())?;
    attach_socket(websocket, state, events).await
}

async fn connect_peer(
    id: String,
    address: String,
    state: SharedState,
    events: std_mpsc::Sender<ControlEvent>,
) {
    let url = format!("ws://{address}");
    match timeout(Duration::from_secs(3), connect_async(url)).await {
        Ok(Ok((websocket, _))) => {
            tracing::info!(peer_address = %address, "Conexão direta de controle estabelecida");
            if let Err(error) = attach_socket(websocket, state.clone(), events).await {
                tracing::warn!(peer_address = %address, error = %error, "Falha ao completar handshake de controle");
            }
        }
        Ok(Err(error)) => log_control_connect_failure(&address, error.to_string()),
        Err(_) => log_control_connect_failure(&address, "timeout de 3 segundos".to_owned()),
    }
    // Se a tentativa falhou, libera o endereço para que o próximo pulso tente novamente.
    state.lock().await.connecting.remove(&id);
}

fn log_control_connect_failure(address: &str, error: String) {
    static LAST_FAILURES: std::sync::OnceLock<std::sync::Mutex<HashMap<String, Instant>>> =
        std::sync::OnceLock::new();
    let failures = LAST_FAILURES.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
    let should_log = {
        let mut failures = failures
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let now = Instant::now();
        let should_log = failures
            .get(address)
            .is_none_or(|last| now.duration_since(*last) >= Duration::from_secs(10));
        if should_log {
            failures.insert(address.to_owned(), now);
        }
        should_log
    };
    if should_log {
        tracing::warn!(peer_address = %address, error = %error, "Falha ao conectar ao participante pela malha de controle; repetição limitada a uma linha por 10 segundos");
    }
}

async fn attach_socket<S>(
    mut websocket: WebSocketStream<S>,
    state: SharedState,
    events: std_mpsc::Sender<ControlEvent>,
) -> Result<(), String>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (local, room_code) = {
        let state = state.lock().await;
        (state.local.clone(), state.room_code.clone())
    };
    send_ws(
        &mut websocket,
        ControlMessage::Hello {
            room_code: room_code.clone(),
            participant: local.clone(),
        },
    )
    .await?;
    let incoming = timeout(Duration::from_secs(5), websocket.next())
        .await
        .map_err(|_| "timeout ao apresentar o participante".to_owned())?
        .ok_or_else(|| "canal de controle encerrado durante a entrada".to_owned())?
        .map_err(|error| error.to_string())?;
    let claimed_remote = match incoming {
        WebSocketMessage::Text(text) => match serde_json::from_str::<ControlMessage>(text.as_str())
            .map_err(|error| error.to_string())?
        {
            ControlMessage::Hello {
                room_code: peer_room,
                participant,
            } if peer_room == room_code => participant,
            ControlMessage::Hello { .. } => {
                return Err("o participante informou outra sala".to_owned());
            }
            _ => return Err("mensagem Hello ausente no canal de controle".to_owned()),
        },
        _ => return Err("mensagem Hello ausente no canal de controle".to_owned()),
    };
    tracing::info!(
        participant_order = claimed_remote.order,
        may_host = claimed_remote.may_host,
        "Participante identificado na malha de controle"
    );
    let remote = {
        let state = state.lock().await;
        state
            .roster
            .iter()
            .find(|participant| participant.id == claimed_remote.id)
            .cloned()
            .ok_or_else(|| "o participante não pertence à lista desta sala".to_owned())?
    };
    if remote.id == local.id {
        return Err("tentativa de conexão com o próprio aplicativo".to_owned());
    }

    let (mut writer, mut reader) = websocket.split();
    let (sender, mut outgoing) = mpsc::unbounded_channel::<ControlMessage>();
    let stats = std::sync::Arc::new(Mutex::new(PeerStats::default()));
    {
        let mut state = state.lock().await;
        if !state
            .roster
            .iter()
            .any(|participant| participant.id == remote.id)
        {
            state.roster.push(remote.clone());
        }
        state.connecting.remove(&remote.id);
        state.connected_once.insert(remote.id.clone());
        state.link_lost_since.remove(&remote.id);
        state.links.insert(
            remote.id.clone(),
            PeerLink {
                sender: sender.clone(),
                stats: stats.clone(),
                last_seen: Instant::now(),
            },
        );
    }
    loop {
        tokio::select! {
            incoming = reader.next() => {
                let Some(Ok(frame)) = incoming else { break };
                let WebSocketMessage::Text(text) = frame else { continue };
                let Ok(message) = serde_json::from_str::<ControlMessage>(text.as_str()) else { continue };
                let now = Instant::now();
                let mut should_broadcast_canonical = None;
                match message {
                    ControlMessage::Ping { sequence, epoch } => {
                        let _ = sender.send(ControlMessage::Pong { sequence, epoch });
                    }
                    ControlMessage::Pong { sequence, .. } => {
                        stats.lock().await.record_pong(sequence, now);
                    }
                    ControlMessage::Status { participant_id, leader_id, leader_address, epoch, loss_percent, jitter_ms, latency_ms, probe_count, consecutive_losses, eligible } => {
                        if participant_id == remote.id {
                            let metrics = Metrics { loss_percent, jitter_ms, latency_ms, samples: probe_count as usize, consecutive_losses };
                            let mut state = state.lock().await;
                            if participant_id == state.leader_id {
                                state.leader_last_seen = now;
                                state.leader_control_seen = true;
                            }
                            let eligibility_changed = state
                                .remote_status
                                .get(&participant_id)
                                .is_none_or(|(_, previous, _)| *previous != eligible);
                            if eligibility_changed {
                                state.eligibility_revision = state.eligibility_revision.wrapping_add(1);
                            }
                            state.remote_status.insert(participant_id, (metrics, eligible, now));
                            if epoch > state.epoch || (epoch == state.epoch && canonical_precedes(&state, &leader_id)) {
                                if state.leader_id != leader_id {
                                    state.leader_control_seen = leader_id == state.local.id;
                                    state.leader_last_seen = now;
                                }
                                state.epoch = epoch;
                                state.leader_id = leader_id.clone();
                                state.leader_address = leader_address.clone();
                                let order = state.roster.iter()
                                    .find(|participant| participant.id == leader_id)
                                    .map_or(u8::MAX, |participant| participant.order);
                                should_broadcast_canonical = Some((leader_id, leader_address, epoch, order));
                            }
                        }
                    }
                    ControlMessage::HostLeaving { participant_id, epoch } => {
                        let is_current_leader = {
                            let state = state.lock().await;
                            epoch >= state.epoch && participant_id == state.leader_id
                        };
                        if is_current_leader {
                            initiate_election(&state, &events, false, true, Some(participant_id)).await;
                        }
                    }
                    ControlMessage::ElectionStart { epoch, candidates, departing_id } => {
                        apply_election(&state, &events, epoch, candidates, departing_id).await;
                    }
                    ControlMessage::CandidateFailed { participant_id, epoch } => {
                        let should_advance = state.lock().await.election.as_ref()
                            .is_some_and(|election| election.epoch == epoch && election.candidates.get(election.index) == Some(&participant_id));
                        if should_advance {
                            fail_current_candidate(&state, &events, epoch).await;
                        }
                    }
                    ControlMessage::LeaderElected { participant_id, address, epoch, order } => {
                        let mut state = state.lock().await;
                        if let Some(current) = state.roster.iter().find(|participant| participant.id == participant_id) {
                            let was_local_leader = state.leader_id == state.local.id;
                            let selected_by_this_election = state.election.as_ref().is_some_and(|election| {
                                election.epoch == epoch
                                    && election.candidates.get(election.index) == Some(&participant_id)
                            });
                            if epoch > state.epoch
                                || selected_by_this_election
                                || (epoch == state.epoch && order < current.order)
                            {
                                state.epoch = epoch;
                                state.leader_id = participant_id.clone();
                                state.leader_address = address.clone();
                                state.leader_last_seen = now;
                                state.leader_control_seen = participant_id == state.local.id;
                                state.election = None;
                                if participant_id != state.local.id {
                                    if was_local_leader {
                                        state.displaced_at = Some(now);
                                        state.recovery_since = None;
                                    }
                                    let _ = events.send(ControlEvent::LeaderChanged { participant_id, address, epoch });
                                }
                            }
                        }
                    }
                    ControlMessage::EndRoom { epoch } => {
                        let should_end = {
                            let mut state = state.lock().await;
                            let election_still_has_candidates = state
                                .election
                                .as_ref()
                                .is_some_and(|election| {
                                    election.epoch >= epoch
                                        && election.candidates.get(election.index).is_some()
                                });
                            if epoch < state.epoch || election_still_has_candidates {
                                false
                            } else {
                                state.epoch = epoch;
                                state.election = None;
                                true
                            }
                        };
                        if should_end {
                            let _ = events.send(ControlEvent::RoomEnded);
                        }
                    }
                    ControlMessage::Hello { .. } => {}
                }
                if let Some((leader_id, address, epoch, order)) = should_broadcast_canonical {
                    broadcast(&state, ControlMessage::LeaderElected {
                        participant_id: leader_id,
                        address,
                        epoch,
                        order,
                    }).await;
                }
                let mut state = state.lock().await;
                if let Some(link) = state.links.get_mut(&remote.id) {
                    link.last_seen = now;
                }
                if remote.id == state.leader_id {
                    state.leader_last_seen = now;
                    state.leader_control_seen = true;
                }
            }
            outgoing_message = outgoing.recv() => {
                let Some(message) = outgoing_message else { break };
                let text = serde_json::to_string(&message).map_err(|error| error.to_string())?;
                if writer.send(WebSocketMessage::Text(text.into())).await.is_err() { break; }
            }
        }
    }
    let mut state = state.lock().await;
    state.links.remove(&remote.id);
    state
        .link_lost_since
        .insert(remote.id.clone(), Instant::now());
    if state.remote_status.remove(&remote.id).is_some() {
        state.eligibility_revision = state.eligibility_revision.wrapping_add(1);
    }
    Ok(())
}

async fn send_ws<S>(
    websocket: &mut WebSocketStream<S>,
    message: ControlMessage,
) -> Result<(), String>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let text = serde_json::to_string(&message).map_err(|error| error.to_string())?;
    websocket
        .send(WebSocketMessage::Text(text.into()))
        .await
        .map_err(|error| error.to_string())
}

async fn tick_mesh(state: &SharedState, events: &std_mpsc::Sender<ControlEvent>) {
    let now = Instant::now();
    let (
        senders,
        connectors,
        leader_id,
        epoch,
        links_missing,
        remote_host_unstable,
        local_host_unstable,
    ) = {
        let mut state = state.lock().await;
        state.sequence = state.sequence.wrapping_add(1);
        let sequence = state.sequence;
        let mut senders = Vec::new();
        for (id, link) in &state.links {
            if let Ok(mut stats) = link.stats.try_lock() {
                stats.add_probe(sequence, now);
            }
            senders.push((id.clone(), link.sender.clone()));
        }
        let mut connectors = Vec::new();
        let roster = state.roster.clone();
        for participant in &roster {
            if participant.id == state.local.id
                || participant.control_address.is_empty()
                || state.links.contains_key(&participant.id)
                || state.connecting.contains(&participant.id)
            {
                continue;
            }
            // Uma única ponta abre a conexão de cada par para evitar duplicatas.
            if state.local.id < participant.id {
                state.connecting.insert(participant.id.clone());
                connectors.push((participant.id.clone(), participant.control_address.clone()));
            }
        }
        let own_metrics = own_worst_metrics(&mut state, now);
        let recovering = if state.displaced_at.is_some() {
            if own_metrics.loss_percent < HEALTHY_LOSS_PERCENT
                && own_metrics.samples >= MIN_HEALTH_SAMPLES
            {
                let since = state.recovery_since.get_or_insert(now);
                now.duration_since(*since) >= HEALTHY_REENTRY_TIME
            } else {
                state.recovery_since = None;
                false
            }
        } else {
            true
        };
        if recovering {
            state.displaced_at = None;
        }
        let eligible = state.local.may_host
            && recovering
            && !state.local.control_address.is_empty()
            && own_metrics.loss_percent < UNSTABLE_LOSS_PERCENT
            && own_metrics.consecutive_losses < 5;
        let unstable = (own_metrics.samples >= MIN_HEALTH_SAMPLES
            && own_metrics.loss_percent >= UNSTABLE_LOSS_PERCENT)
            || own_metrics.consecutive_losses >= 5;
        let links_missing = state.leader_id != state.local.id
            && state.leader_control_seen
            && now.duration_since(state.leader_last_seen) >= HOST_TIMEOUT;
        let remote_host_unstable =
            state
                .remote_status
                .get(&state.leader_id)
                .is_some_and(|(metrics, _, seen)| {
                    now.duration_since(*seen) < HOST_TIMEOUT
                        && ((metrics.samples >= MIN_HEALTH_SAMPLES
                            && metrics.loss_percent >= UNSTABLE_LOSS_PERCENT)
                            || metrics.consecutive_losses >= 5)
                });
        let leader_id = state.leader_id.clone();
        let local_host_unstable = leader_id == state.local.id && unstable;
        let leader_address = state.leader_address.clone();
        let epoch = state.epoch;
        let participants = state.roster.clone();
        let mut queue = participants
            .into_iter()
            .filter(|participant| participant.id != leader_id)
            .map(|participant| {
                let (metrics, permitted) = if participant.id == state.local.id {
                    (own_metrics, eligible)
                } else {
                    state
                        .remote_status
                        .get(&participant.id)
                        .filter(|(_, _, seen)| now.duration_since(*seen) < HOST_TIMEOUT)
                        .map(|(metrics, eligible, _)| (*metrics, participant.may_host && *eligible))
                        .unwrap_or((
                            Metrics {
                                loss_percent: 0.0,
                                jitter_ms: 0.0,
                                latency_ms: 0.0,
                                samples: 0,
                                consecutive_losses: 0,
                            },
                            false,
                        ))
                };
                QueueEntry {
                    participant,
                    loss_percent: metrics.loss_percent,
                    jitter_ms: metrics.jitter_ms,
                    latency_ms: metrics.latency_ms,
                    eligible: permitted,
                }
            })
            .collect::<Vec<_>>();
        queue.sort_by(compare_queue_entries);
        let status_metrics = own_metrics;
        let status = ControlMessage::Status {
            participant_id: state.local.id.clone(),
            leader_id: leader_id.clone(),
            leader_address: leader_address.clone(),
            epoch,
            loss_percent: status_metrics.loss_percent,
            jitter_ms: status_metrics.jitter_ms,
            latency_ms: status_metrics.latency_ms,
            probe_count: status_metrics.samples as u32,
            consecutive_losses: status_metrics.consecutive_losses,
            eligible,
        };
        let links = senders.clone();
        let _ = events.send(ControlEvent::QueueUpdated(queue));
        let _ = events.send(ControlEvent::LinksUpdated {
            connected: state.links.len(),
            total: state
                .roster
                .iter()
                .filter(|participant| participant.id != state.local.id)
                .count(),
        });
        for (_, sender) in &links {
            let _ = sender.send(status.clone());
        }
        if unstable && leader_id == state.local.id {
            if state
                .last_unstable_notice
                .is_none_or(|previous| now.duration_since(previous) >= Duration::from_secs(10))
            {
                state.last_unstable_notice = Some(now);
                let _ = events.send(ControlEvent::HostUnstable {
                    loss_percent: own_metrics.loss_percent,
                });
            }
        }
        (
            senders,
            connectors,
            leader_id,
            epoch,
            links_missing,
            remote_host_unstable,
            local_host_unstable,
        )
    };

    let sequence = {
        let state = state.lock().await;
        state.sequence
    };
    for (_, sender) in &senders {
        let _ = sender.send(ControlMessage::Ping { sequence, epoch });
    }
    for (id, address) in connectors {
        let task_state = state.clone();
        let task_events = events.clone();
        tokio::spawn(async move {
            connect_peer(id, address, task_state, task_events).await;
        });
    }

    let active_election = state.lock().await.election.as_ref().map(|election| {
        (
            election.epoch,
            election.deadline,
            election.candidates.get(election.index).cloned(),
        )
    });
    if let Some((candidate_epoch, deadline, candidate)) = active_election {
        if now >= deadline {
            if let Some(candidate) = candidate {
                broadcast(
                    state,
                    ControlMessage::CandidateFailed {
                        participant_id: candidate.clone(),
                        epoch: candidate_epoch,
                    },
                )
                .await;
                fail_current_candidate(state, events, candidate_epoch).await;
            }
        }
    } else if !leader_id.is_empty() {
        if links_missing {
            tracing::warn!(epoch, "Anfitrião sem pulsos; timeout de liveness iniciado");
            initiate_election(state, events, false, true, None).await;
        } else if remote_host_unstable || local_host_unstable {
            tracing::warn!(
                epoch,
                remote_host_unstable,
                local_host_unstable,
                "Enlace do anfitrião excedeu limite de estabilidade"
            );
            initiate_election(state, events, false, false, None).await;
        }
    }
}

fn own_worst_metrics(state: &mut MeshState, now: Instant) -> Metrics {
    // O enlace com o anfitrião que já expirou não deve tornar inelegível o
    // sucessor: ele está medindo a conexão que será substituída pela eleição.
    let ignore_timed_out_leader = state.leader_id != state.local.id
        && state.leader_control_seen
        && now.duration_since(state.leader_last_seen) >= HOST_TIMEOUT;
    let mut metrics = state
        .links
        .iter()
        .filter(|(participant_id, _)| {
            !(ignore_timed_out_leader && *participant_id == &state.leader_id)
        })
        .filter_map(|(_, link)| {
            link.stats
                .try_lock()
                .ok()
                .map(|mut stats| stats.metrics(now))
        })
        .collect::<Vec<_>>();
    for participant in state.roster.iter().filter(|participant| {
        participant.id != state.local.id
            && !(ignore_timed_out_leader && participant.id == state.leader_id)
    }) {
        if !state.links.contains_key(&participant.id)
            && control_link_timeout_elapsed(state, &participant.id, now)
        {
            metrics.push(Metrics {
                loss_percent: 100.0,
                jitter_ms: 0.0,
                latency_ms: 0.0,
                samples: MIN_HEALTH_SAMPLES,
                consecutive_losses: 5,
            });
        }
    }
    metrics.sort_by(|left, right| compare_metrics(right, left));
    metrics.into_iter().next().unwrap_or(Metrics {
        loss_percent: 0.0,
        jitter_ms: 0.0,
        latency_ms: 0.0,
        samples: 0,
        consecutive_losses: 0,
    })
}

fn control_link_timeout_elapsed(state: &MeshState, participant_id: &str, now: Instant) -> bool {
    if state.connected_once.contains(participant_id) {
        state
            .link_lost_since
            .get(participant_id)
            .or_else(|| state.known_since.get(participant_id))
            .is_some_and(|since| now.duration_since(*since) >= HOST_TIMEOUT)
    } else {
        state
            .known_since
            .get(participant_id)
            .is_some_and(|since| now.duration_since(*since) >= CONTROL_LINK_STARTUP_GRACE)
    }
}

fn compare_queue_entries(left: &QueueEntry, right: &QueueEntry) -> std::cmp::Ordering {
    right
        .eligible
        .cmp(&left.eligible)
        .then_with(|| left.loss_percent.total_cmp(&right.loss_percent))
        .then_with(|| left.jitter_ms.total_cmp(&right.jitter_ms))
        .then_with(|| left.latency_ms.total_cmp(&right.latency_ms))
        .then_with(|| left.participant.order.cmp(&right.participant.order))
}

fn compare_metrics(left: &Metrics, right: &Metrics) -> std::cmp::Ordering {
    left.loss_percent
        .total_cmp(&right.loss_percent)
        .then_with(|| left.jitter_ms.total_cmp(&right.jitter_ms))
        .then_with(|| left.latency_ms.total_cmp(&right.latency_ms))
}

async fn initiate_election(
    state: &SharedState,
    events: &std_mpsc::Sender<ControlEvent>,
    orderly: bool,
    end_if_no_candidate: bool,
    departing_id: Option<String>,
) {
    let (epoch, candidates, leader_id, departing_id, election_revision) = {
        let mut state = state.lock().await;
        if state.election.is_some() {
            return;
        }
        let now = Instant::now();
        if !orderly
            && state
                .empty_election_retry
                .is_some_and(|(last_attempt, revision)| {
                    revision == state.eligibility_revision
                        && now.duration_since(last_attempt) < EMPTY_ELECTION_RETRY_INTERVAL
                })
        {
            return;
        }
        let own = own_worst_metrics(&mut state, now);
        let local_eligible = state.local.may_host
            && !state.local.control_address.is_empty()
            && own.loss_percent < UNSTABLE_LOSS_PERCENT
            && own.consecutive_losses < 5;
        let mut queue = state
            .roster
            .iter()
            .filter(|participant| participant.id != state.leader_id)
            .map(|participant| {
                let (metrics, eligible) = if participant.id == state.local.id {
                    (own, local_eligible)
                } else {
                    let recent_status = state
                        .remote_status
                        .get(&participant.id)
                        .filter(|(_, _, seen)| now.duration_since(*seen) < HOST_TIMEOUT)
                        .map(|(metrics, eligible, _)| (*metrics, *eligible));
                    match recent_status {
                        Some((metrics, eligible)) => (metrics, participant.may_host && eligible),
                        None => (
                            Metrics {
                                // A newly joined participant may not have sent its first health
                                // status yet. Keep it as a fallback candidate; its attempt to
                                // open the signaling server will either succeed or time out.
                                loss_percent: 100.0,
                                jitter_ms: 0.0,
                                latency_ms: 0.0,
                                samples: 0,
                                consecutive_losses: 0,
                            },
                            participant.may_host && !participant.control_address.is_empty(),
                        ),
                    }
                };
                QueueEntry {
                    participant: participant.clone(),
                    loss_percent: metrics.loss_percent,
                    jitter_ms: metrics.jitter_ms,
                    latency_ms: metrics.latency_ms,
                    eligible,
                }
            })
            .filter(|entry| entry.eligible)
            .collect::<Vec<_>>();
        queue.sort_by(compare_queue_entries);
        let candidates = queue
            .into_iter()
            .map(|entry| entry.participant.id)
            .collect::<Vec<_>>();
        if !candidates.is_empty() {
            state.empty_election_retry = None;
        }
        let epoch = state.epoch.saturating_add(1);
        state.epoch = epoch;
        state.election = Some(Election {
            epoch,
            candidates: candidates.clone(),
            index: 0,
            deadline: now + CANDIDATE_TIMEOUT,
            departing_id: departing_id.clone(),
        });
        (
            epoch,
            candidates,
            state.leader_id.clone(),
            departing_id,
            state.eligibility_revision,
        )
    };
    if orderly {
        broadcast(
            state,
            ControlMessage::HostLeaving {
                participant_id: leader_id.clone(),
                epoch,
            },
        )
        .await;
    }
    tracing::warn!(
        epoch,
        orderly,
        candidate_count = candidates.len(),
        "Eleição de anfitrião iniciada"
    );
    if candidates.is_empty() {
        let mut mesh = state.lock().await;
        mesh.election = None;
        if !orderly {
            mesh.empty_election_retry = Some((Instant::now(), election_revision));
        }
        drop(mesh);
        if end_if_no_candidate {
            tracing::error!(
                epoch,
                "Sala encerrada porque não há candidato elegível para hospedagem"
            );
            broadcast(state, ControlMessage::EndRoom { epoch }).await;
            let _ = events.send(ControlEvent::RoomEnded);
        }
        return;
    }
    broadcast(
        state,
        ControlMessage::ElectionStart {
            epoch,
            candidates: candidates.clone(),
            departing_id: departing_id.clone(),
        },
    )
    .await;
    let local_id = state.lock().await.local.id.clone();
    if candidates.first() == Some(&local_id) {
        let state = state.lock().await;
        let participants = state
            .roster
            .iter()
            .filter(|participant| departing_id.as_deref() != Some(participant.id.as_str()))
            .cloned()
            .collect();
        let _ = events.send(ControlEvent::BecomeHost {
            code: state.room_code.clone(),
            epoch,
            participants,
        });
    }
}

async fn apply_election(
    state: &SharedState,
    events: &std_mpsc::Sender<ControlEvent>,
    epoch: u64,
    candidates: Vec<String>,
    departing_id: Option<String>,
) {
    let should_apply = {
        let mut state = state.lock().await;
        if epoch <= state.epoch {
            false
        } else {
            state.epoch = epoch;
            state.election = Some(Election {
                epoch,
                candidates: candidates.clone(),
                index: 0,
                deadline: Instant::now() + CANDIDATE_TIMEOUT,
                departing_id: departing_id.clone(),
            });
            true
        }
    };
    if should_apply {
        tracing::warn!(
            epoch,
            candidate_count = candidates.len(),
            "Eleição recebida e aplicada"
        );
        broadcast(
            state,
            ControlMessage::ElectionStart {
                epoch,
                candidates: candidates.clone(),
                departing_id: departing_id.clone(),
            },
        )
        .await;
        if candidates.is_empty() {
            tracing::error!(epoch, "Eleição recebida sem candidatos disponíveis");
            let _ = events.send(ControlEvent::RoomEnded);
        } else if candidates.first() == Some(&state.lock().await.local.id) {
            let state = state.lock().await;
            let participants = state
                .roster
                .iter()
                .filter(|participant| departing_id.as_deref() != Some(participant.id.as_str()))
                .cloned()
                .collect();
            let _ = events.send(ControlEvent::BecomeHost {
                code: state.room_code.clone(),
                epoch,
                participants,
            });
        }
    }
}

async fn fail_current_candidate(
    state: &SharedState,
    events: &std_mpsc::Sender<ControlEvent>,
    epoch: u64,
) {
    let (next, code, candidates, departing_id) = {
        let mut state = state.lock().await;
        let code = state.room_code.clone();
        let Some(election) = state.election.as_mut() else {
            return;
        };
        if election.epoch != epoch {
            return;
        }
        election.index += 1;
        if election.index >= election.candidates.len() {
            state.election = None;
            (None, code, Vec::new(), None)
        } else {
            election.deadline = Instant::now() + CANDIDATE_TIMEOUT;
            let remaining = election.candidates[election.index..].to_vec();
            election.candidates = remaining.clone();
            election.index = 0;
            (
                election.candidates.first().cloned(),
                code,
                remaining,
                election.departing_id.clone(),
            )
        }
    };
    if let Some(candidate) = next {
        tracing::warn!(
            epoch,
            remaining_candidates = candidates.len(),
            "Tentativa de hospedagem passou ao próximo candidato"
        );
        broadcast(
            state,
            ControlMessage::ElectionStart {
                epoch,
                candidates,
                departing_id: departing_id.clone(),
            },
        )
        .await;
        if candidate == state.lock().await.local.id {
            let state = state.lock().await;
            let participants = state
                .roster
                .iter()
                .filter(|participant| departing_id.as_deref() != Some(participant.id.as_str()))
                .cloned()
                .collect();
            let _ = events.send(ControlEvent::BecomeHost {
                code,
                epoch,
                participants,
            });
        }
    } else {
        tracing::error!(epoch, "Todos os candidatos falharam; encerrando sala");
        broadcast(state, ControlMessage::EndRoom { epoch }).await;
        let _ = events.send(ControlEvent::RoomEnded);
    }
}

async fn publish_leader(
    state: &SharedState,
    events: &std_mpsc::Sender<ControlEvent>,
    address: String,
    epoch: u64,
) {
    tracing::info!(epoch, address = %address, "Novo anfitrião publicou endereço de sinalização");
    let (local_id, order) = {
        let mut state = state.lock().await;
        if epoch < state.epoch {
            return;
        }
        state.epoch = epoch;
        state.leader_id = state.local.id.clone();
        state.leader_address = address.clone();
        state.leader_last_seen = Instant::now();
        state.leader_control_seen = true;
        state.election = None;
        (state.local.id.clone(), state.local.order)
    };
    broadcast(
        state,
        ControlMessage::LeaderElected {
            participant_id: local_id,
            address,
            epoch,
            order,
        },
    )
    .await;
    let _ = events.send(ControlEvent::LeaderChanged {
        participant_id: state.lock().await.local.id.clone(),
        address: state.lock().await.leader_address.clone(),
        epoch,
    });
}

async fn broadcast(state: &SharedState, message: ControlMessage) {
    let senders = state
        .lock()
        .await
        .links
        .values()
        .map(|link| link.sender.clone())
        .collect::<Vec<_>>();
    for sender in senders {
        let _ = sender.send(message.clone());
    }
}

fn canonical_precedes(state: &MeshState, incoming_leader_id: &str) -> bool {
    let incoming = state
        .roster
        .iter()
        .find(|participant| participant.id == incoming_leader_id)
        .map(|participant| participant.order)
        .unwrap_or(u8::MAX);
    let current = state
        .roster
        .iter()
        .find(|participant| participant.id == state.leader_id)
        .map(|participant| participant.order)
        .unwrap_or(u8::MAX);
    incoming < current
}

#[cfg(test)]
mod tests {
    use super::{
        CONTROL_LINK_STARTUP_GRACE, ControlEvent, ControlMesh, EMPTY_ELECTION_RETRY_INTERVAL,
        Election, HOST_TIMEOUT, MeshState, Metrics, PeerLink, PeerStats, Probe, QueueEntry,
        compare_queue_entries, control_link_timeout_elapsed, fail_current_candidate,
        initiate_election, own_worst_metrics,
    };
    use signaling_protocol::ParticipantInfo;
    use std::collections::{HashMap, HashSet, VecDeque};
    use std::sync::Arc;
    use std::sync::mpsc as std_mpsc;
    use std::time::{Duration, Instant};
    use tokio::sync::{Mutex, mpsc};

    fn participant(id: &str, order: u8, may_host: bool) -> ParticipantInfo {
        ParticipantInfo {
            id: id.to_owned(),
            display_name: format!("Participante {order}"),
            order,
            may_host,
            control_address: format!("192.168.1.{order}:9001"),
            avatar_jpeg_base64: None,
            supports_group_screen_share: false,
        }
    }

    fn free_local_address() -> String {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        format!("127.0.0.1:{}", listener.local_addr().unwrap().port())
    }

    fn receive_until(
        receiver: &std_mpsc::Receiver<ControlEvent>,
        limit: Duration,
        mut predicate: impl FnMut(&ControlEvent) -> bool,
    ) -> Option<ControlEvent> {
        let deadline = Instant::now() + limit;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return None;
            }
            match receiver.recv_timeout(remaining.min(Duration::from_millis(100))) {
                Ok(event) if predicate(&event) => return Some(event),
                Ok(_) => {}
                Err(std_mpsc::RecvTimeoutError::Timeout) => {}
                Err(std_mpsc::RecvTimeoutError::Disconnected) => return None,
            }
        }
    }

    fn test_state(local: ParticipantInfo, has_successor: bool) -> MeshState {
        let now = Instant::now();
        let host = participant("host", 1, false);
        let mut stats = PeerStats::default();
        stats.probes = VecDeque::from([
            Probe {
                sequence: 1,
                sent_at: now,
                rtt_ms: Some(50.0),
                finalized: true,
            },
            Probe {
                sequence: 2,
                sent_at: now,
                rtt_ms: Some(100.0),
                finalized: true,
            },
        ]);
        let (link_tx, _link_rx) = mpsc::unbounded_channel();
        let mut roster = vec![host.clone(), local.clone()];
        let mut remote_status = HashMap::new();
        if has_successor {
            let candidate = participant("candidate", 2, true);
            roster.push(candidate.clone());
            remote_status.insert(
                candidate.id.clone(),
                (
                    Metrics {
                        loss_percent: 0.0,
                        jitter_ms: 1.0,
                        latency_ms: 10.0,
                        samples: 5,
                        consecutive_losses: 0,
                    },
                    true,
                    now,
                ),
            );
        }
        MeshState {
            local,
            room_code: "SAMECODE".to_owned(),
            roster,
            known_since: HashMap::new(),
            connected_once: HashSet::new(),
            link_lost_since: HashMap::new(),
            leader_id: host.id,
            leader_address: "ws://192.168.1.1:9000".to_owned(),
            epoch: 0,
            sequence: 0,
            links: HashMap::from([(
                "host".to_owned(),
                PeerLink {
                    sender: link_tx,
                    stats: Arc::new(Mutex::new(stats)),
                    last_seen: now,
                },
            )]),
            connecting: HashSet::new(),
            remote_status,
            election: None,
            displaced_at: None,
            recovery_since: None,
            leader_last_seen: now,
            leader_control_seen: true,
            last_unstable_notice: None,
            eligibility_revision: 0,
            empty_election_retry: None,
        }
    }

    fn entry(id: &str, order: u8, loss: f32, jitter: f32, latency: f32) -> QueueEntry {
        QueueEntry {
            participant: ParticipantInfo {
                id: id.to_owned(),
                display_name: format!("Participante {order}"),
                order,
                may_host: true,
                control_address: "127.0.0.1:9001".to_owned(),
                avatar_jpeg_base64: None,
                supports_group_screen_share: false,
            },
            loss_percent: loss,
            jitter_ms: jitter,
            latency_ms: latency,
            eligible: true,
        }
    }

    #[test]
    fn queue_prioritizes_loss_then_jitter_then_latency_then_join_order() {
        let mut entries = [
            entry("late", 4, 1.0, 4.0, 40.0),
            entry("latency", 3, 1.0, 4.0, 50.0),
            entry("jitter", 2, 1.0, 3.0, 90.0),
            entry("loss", 1, 0.0, 20.0, 200.0),
            entry("arrival-tie", 5, 1.0, 4.0, 40.0),
        ];
        entries.sort_by(compare_queue_entries);
        assert_eq!(
            entries.map(|entry| entry.participant.id),
            ["loss", "jitter", "late", "arrival-tie", "latency"]
        );
    }

    #[test]
    fn timed_out_host_link_does_not_disqualify_the_only_successor() {
        let now = Instant::now();
        let mut state = test_state(participant("successor", 2, true), false);
        state.leader_last_seen = now - HOST_TIMEOUT - Duration::from_millis(1);
        state.known_since.insert(
            "host".to_owned(),
            now - HOST_TIMEOUT - Duration::from_millis(1),
        );
        state.links.clear();

        let metrics = own_worst_metrics(&mut state, now);

        assert!(metrics.loss_percent < 20.0);
        assert!(metrics.consecutive_losses < 5);
    }

    #[test]
    fn first_control_link_has_fifteen_second_grace_but_reconnected_links_keep_five_second_timeout()
    {
        let now = Instant::now();
        let local = participant("local", 2, true);
        let remote = participant("remote", 3, true);
        let mut state = test_state(local, false);
        state.roster.push(remote.clone());
        state.known_since.insert(remote.id.clone(), now);
        assert!(!control_link_timeout_elapsed(
            &state,
            &remote.id,
            now + HOST_TIMEOUT
        ));
        assert!(control_link_timeout_elapsed(
            &state,
            &remote.id,
            now + CONTROL_LINK_STARTUP_GRACE
        ));

        state.connected_once.insert(remote.id.clone());
        state.link_lost_since.insert(remote.id.clone(), now);
        assert!(!control_link_timeout_elapsed(
            &state,
            &remote.id,
            now + HOST_TIMEOUT - Duration::from_millis(1)
        ));
        assert!(control_link_timeout_elapsed(
            &state,
            &remote.id,
            now + HOST_TIMEOUT
        ));
    }

    #[test]
    fn probe_window_counts_lost_pulses_and_keeps_success_latency() {
        let now = Instant::now();
        let mut stats = PeerStats::default();
        stats.add_probe(1, now);
        stats.record_pong(1, now + Duration::from_millis(12));
        stats.add_probe(2, now + Duration::from_secs(1));
        let metrics = stats.metrics(now + Duration::from_secs(4));
        assert_eq!(metrics.loss_percent, 50.0);
        assert!((metrics.latency_ms - 12.0).abs() < 0.1);
        assert_eq!(metrics.consecutive_losses, 1);
    }

    #[tokio::test]
    async fn failed_first_candidate_advances_to_the_next_in_join_order() {
        let local = participant("local", 3, true);
        let state = Arc::new(Mutex::new(test_state(local, true)));
        let (event_tx, event_rx) = std::sync::mpsc::channel();
        initiate_election(&state, &event_tx, false, false, None).await;
        {
            let state = state.lock().await;
            let election = state.election.as_ref().unwrap();
            assert_eq!(election.candidates, ["candidate", "local"]);
            assert_eq!(
                election.deadline.duration_since(Instant::now()).as_secs(),
                9
            );
        }
        fail_current_candidate(&state, &event_tx, 1).await;
        assert!(matches!(
            event_rx.try_recv().unwrap(),
            ControlEvent::BecomeHost { code, epoch: 1, .. } if code == "SAMECODE"
        ));
        let state = state.lock().await;
        let election: &Election = state.election.as_ref().unwrap();
        assert_eq!(election.candidates, ["local"]);
        assert_eq!(election.index, 0);
    }

    #[tokio::test]
    async fn participant_without_host_permission_is_never_elected() {
        let now = Instant::now();
        let mut mesh_state = test_state(participant("local", 2, false), false);
        let unauthorized = participant("unauthorized", 3, false);
        mesh_state.roster.push(unauthorized.clone());
        mesh_state.remote_status.insert(
            unauthorized.id.clone(),
            (
                Metrics {
                    loss_percent: 0.0,
                    jitter_ms: 0.0,
                    latency_ms: 1.0,
                    samples: 5,
                    consecutive_losses: 0,
                },
                true,
                now,
            ),
        );
        let state = Arc::new(Mutex::new(mesh_state));
        let (event_tx, _event_rx) = std::sync::mpsc::channel();

        initiate_election(&state, &event_tx, false, false, None).await;

        assert!(state.lock().await.election.is_none());
    }

    #[tokio::test]
    async fn no_candidate_keeps_an_unstable_but_online_host_and_ends_after_host_exit() {
        let local = participant("local", 2, false);
        let state = Arc::new(Mutex::new(test_state(local.clone(), false)));
        let (event_tx, event_rx) = std::sync::mpsc::channel();
        initiate_election(&state, &event_tx, false, false, None).await;
        assert!(event_rx.try_recv().is_err());
        assert!(state.lock().await.election.is_none());

        let state = Arc::new(Mutex::new(test_state(local, false)));
        initiate_election(&state, &event_tx, true, true, Some("host".to_owned())).await;
        assert!(matches!(
            event_rx.try_recv().unwrap(),
            ControlEvent::RoomEnded
        ));
    }

    #[tokio::test]
    async fn empty_election_is_suppressed_until_eligibility_changes_or_ten_seconds_pass() {
        let local = participant("local", 2, false);
        let state = Arc::new(Mutex::new(test_state(local, false)));
        let (event_tx, _event_rx) = std::sync::mpsc::channel();

        initiate_election(&state, &event_tx, false, false, None).await;
        let first_epoch = state.lock().await.epoch;
        initiate_election(&state, &event_tx, false, false, None).await;
        assert_eq!(state.lock().await.epoch, first_epoch);

        {
            let mut state = state.lock().await;
            state.eligibility_revision = state.eligibility_revision.wrapping_add(1);
        }
        initiate_election(&state, &event_tx, false, false, None).await;
        assert!(state.lock().await.epoch > first_epoch);

        {
            let mut state = state.lock().await;
            state.empty_election_retry = Some((
                Instant::now() - EMPTY_ELECTION_RETRY_INTERVAL,
                state.eligibility_revision,
            ));
        }
        let before_safety_retry = state.lock().await.epoch;
        initiate_election(&state, &event_tx, false, false, None).await;
        assert!(state.lock().await.epoch > before_safety_retry);

        let ending_state = Arc::new(Mutex::new(test_state(
            participant("ending", 2, false),
            false,
        )));
        let (ending_events_tx, ending_events_rx) = std::sync::mpsc::channel();
        initiate_election(&ending_state, &ending_events_tx, false, true, None).await;
        assert!(matches!(
            ending_events_rx.try_recv().unwrap(),
            ControlEvent::RoomEnded
        ));
        let ended_epoch = ending_state.lock().await.epoch;
        initiate_election(&ending_state, &ending_events_tx, false, true, None).await;
        assert_eq!(ending_state.lock().await.epoch, ended_epoch);
        assert!(ending_events_rx.try_recv().is_err());
    }

    #[test]
    fn direct_mesh_connects_and_orders_a_normal_host_handoff() {
        let host_address = free_local_address();
        let mut host = participant("host", 1, false);
        host.control_address = host_address.clone();
        let host_mesh = ControlMesh::start_on(
            host.clone(),
            "SAMECODE".to_owned(),
            host.id.clone(),
            "ws://127.0.0.1:9000".to_owned(),
            host_address,
        )
        .unwrap();
        assert!(
            receive_until(&host_mesh.events, Duration::from_secs(3), |event| matches!(
                event,
                ControlEvent::Ready
            ))
            .is_some()
        );

        let guest_address = free_local_address();
        let mut guest = participant("guest", 2, true);
        guest.control_address = guest_address.clone();
        let roster = vec![host.clone(), guest.clone()];
        host_mesh.update_roster(
            roster.clone(),
            host.id.clone(),
            "ws://127.0.0.1:9000".to_owned(),
        );
        let guest_mesh = ControlMesh::start_on(
            guest.clone(),
            "SAMECODE".to_owned(),
            host.id.clone(),
            "ws://127.0.0.1:9000".to_owned(),
            guest_address,
        )
        .unwrap();
        guest_mesh.update_roster(roster, host.id.clone(), "ws://127.0.0.1:9000".to_owned());

        let has_host_link = receive_until(&host_mesh.events, Duration::from_secs(7), |event| {
            matches!(
                event,
                ControlEvent::LinksUpdated {
                    connected: 1,
                    total: 1
                }
            )
        });
        let has_guest_link = receive_until(&guest_mesh.events, Duration::from_secs(7), |event| {
            matches!(
                event,
                ControlEvent::LinksUpdated {
                    connected: 1,
                    total: 1
                }
            )
        });
        assert!(
            has_host_link.is_some(),
            "host did not establish the direct control link"
        );
        assert!(
            has_guest_link.is_some(),
            "guest did not establish the direct control link"
        );
        assert!(
            receive_until(&host_mesh.events, Duration::from_secs(4), |event| {
                matches!(event, ControlEvent::QueueUpdated(queue)
                if queue.iter().any(|entry| entry.participant.id == "guest" && entry.eligible))
            })
            .is_some(),
            "host did not receive the guest's host eligibility status"
        );

        let _ = host_mesh.leave_normally();
        let elected = receive_until(&guest_mesh.events, Duration::from_secs(4), |event| {
            matches!(event, ControlEvent::BecomeHost { .. })
        })
        .expect("the authorized guest should be elected");
        let ControlEvent::BecomeHost {
            code,
            epoch,
            participants,
        } = elected
        else {
            unreachable!()
        };
        assert_eq!(code, "SAMECODE");
        assert_eq!(
            participants
                .iter()
                .map(|participant| participant.id.as_str())
                .collect::<Vec<_>>(),
            ["guest"]
        );
        guest_mesh.publish_leader("ws://127.0.0.1:9002".to_owned(), epoch);
        assert!(receive_until(&host_mesh.events, Duration::from_secs(5), |event| {
            matches!(event, ControlEvent::LeaderChanged { participant_id, .. } if participant_id == "guest")
        }).is_some());
    }
}
