use std::collections::HashMap;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use base64::Engine as _;
use futures_util::{SinkExt, StreamExt};
use signaling_protocol::{ClientMessage, ParticipantInfo, RoomMode, ServerMessage};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, mpsc, oneshot, watch};
use tokio::time::{sleep, timeout};
use tokio_tungstenite::tungstenite::Message as WebSocketMessage;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::{WebSocketStream, accept_async_with_config};

const MAX_MESSAGE_BYTES: usize = 64 * 1024;
const MAX_AVATAR_BYTES: usize = 16 * 1024;
const MAX_AVATAR_BASE64_BYTES: usize = MAX_AVATAR_BYTES.div_ceil(3) * 4;
const MAX_DISPLAY_NAME_CHARS: usize = 32;
const HOST_TRANSFER_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_ROOM_PARTICIPANTS: usize = 8;
/// Room codes are 48 random bits (12 hexadecimal digits). Clients accept codes of any length.
const ROOM_CODE_LENGTH: usize = 12;
const ROOM_NOT_FOUND_MESSAGE: &str = "A sala nao existe ou ja foi encerrada.";
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const WRITE_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_QUEUED_OUTBOUND_MESSAGES: usize = 2048;
const MAX_TOTAL_CONNECTIONS: usize = 512;
const MAX_CONNECTIONS_PER_IP: usize = 16;
const FAILED_JOIN_WINDOW: Duration = Duration::from_secs(60);
const MAX_FAILED_JOINS_PER_WINDOW: usize = 10;
const TOO_MANY_JOIN_ATTEMPTS_MESSAGE: &str =
    "Muitas tentativas de entrada com codigos invalidos. Aguarde um minuto e tente novamente.";

type ConnectionId = u64;
type SharedRooms = Arc<Mutex<RoomRegistry>>;

#[derive(Clone)]
struct Outgoing {
    messages: mpsc::Sender<OutboundMessage>,
    overloaded: watch::Sender<bool>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OutgoingSendError {
    Full,
    Closed,
}

impl Outgoing {
    fn channel() -> (Self, mpsc::Receiver<OutboundMessage>, watch::Receiver<bool>) {
        Self::channel_with_capacity(MAX_QUEUED_OUTBOUND_MESSAGES)
    }

    fn channel_with_capacity(
        capacity: usize,
    ) -> (Self, mpsc::Receiver<OutboundMessage>, watch::Receiver<bool>) {
        let (messages, receiver) = mpsc::channel(capacity);
        let (overloaded, overload_events) = watch::channel(false);
        (
            Self {
                messages,
                overloaded,
            },
            receiver,
            overload_events,
        )
    }

    fn send(&self, message: OutboundMessage) -> Result<(), OutgoingSendError> {
        match self.messages.try_send(message) {
            Ok(()) => Ok(()),
            Err(mpsc::error::TrySendError::Full(_)) => {
                self.overloaded.send_replace(true);
                Err(OutgoingSendError::Full)
            }
            Err(mpsc::error::TrySendError::Closed(_)) => Err(OutgoingSendError::Closed),
        }
    }
}

#[derive(Clone, Debug)]
pub struct TransferReservation {
    pub code: String,
    pub token: String,
    pub participants: Vec<ParticipantInfo>,
}

#[derive(Clone, Debug)]
struct PendingTransfer {
    host_id: ConnectionId,
    target_id: ConnectionId,
    code: String,
    token: String,
}

enum OutboundMessage {
    Protocol(ServerMessage),
    Control(WebSocketMessage),
    CloseAcknowledgement(WebSocketMessage),
}

#[derive(Default)]
struct RoomRegistry {
    rooms: HashMap<String, Room>,
    connection_rooms: HashMap<ConnectionId, String>,
    participant_info: HashMap<ConnectionId, ParticipantInfo>,
    transfer_reservation: Option<TransferReservation>,
    total_connections: usize,
    connections_per_ip: HashMap<IpAddr, usize>,
    failed_joins: HashMap<IpAddr, Vec<Instant>>,
}

#[derive(Default)]
struct Room {
    participants: Vec<(ConnectionId, Outgoing)>,
    members: Vec<ParticipantInfo>,
    leader_connection_id: ConnectionId,
    pending_transfer: Option<PendingTransfer>,
    room_mode: RoomMode,
}

pub fn is_valid_room_code(code: &str) -> bool {
    let trimmed = code.trim();
    (4..=32).contains(&trimmed.len())
        && trimmed
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

impl RoomRegistry {
    fn admit_connection(&mut self, ip: Option<IpAddr>) -> Result<(), &'static str> {
        if self.total_connections >= MAX_TOTAL_CONNECTIONS {
            return Err("limite global de conexoes atingido");
        }
        if let Some(ip) = ip {
            let count = self.connections_per_ip.entry(ip).or_insert(0);
            if *count >= MAX_CONNECTIONS_PER_IP {
                return Err("limite de conexoes por endereco atingido");
            }
            *count += 1;
        }
        self.total_connections += 1;
        Ok(())
    }

    fn release_connection(&mut self, ip: Option<IpAddr>) {
        self.total_connections = self.total_connections.saturating_sub(1);
        if let Some(ip) = ip
            && let Some(count) = self.connections_per_ip.get_mut(&ip)
        {
            *count = count.saturating_sub(1);
            if *count == 0 {
                self.connections_per_ip.remove(&ip);
            }
        }
    }

    fn join_attempts_blocked(&mut self, ip: Option<IpAddr>, now: Instant) -> bool {
        let Some(ip) = ip else {
            return false;
        };
        let Some(failures) = self.failed_joins.get_mut(&ip) else {
            return false;
        };
        failures.retain(|at| now.duration_since(*at) < FAILED_JOIN_WINDOW);
        if failures.is_empty() {
            self.failed_joins.remove(&ip);
            return false;
        }
        failures.len() >= MAX_FAILED_JOINS_PER_WINDOW
    }

    fn record_failed_join(&mut self, ip: Option<IpAddr>, now: Instant) {
        let Some(ip) = ip else {
            return;
        };
        if self.failed_joins.len() > 1024 {
            self.failed_joins.retain(|_, failures| {
                failures.retain(|at| now.duration_since(*at) < FAILED_JOIN_WINDOW);
                !failures.is_empty()
            });
        }
        self.failed_joins.entry(ip).or_default().push(now);
    }

    fn create_room(
        &mut self,
        connection_id: ConnectionId,
        sender: Outgoing,
    ) -> Result<String, String> {
        self.create_room_with_mode(connection_id, sender, RoomMode::Local, None)
    }

    fn create_room_with_mode(
        &mut self,
        connection_id: ConnectionId,
        sender: Outgoing,
        room_mode: RoomMode,
        requested_code: Option<String>,
    ) -> Result<String, String> {
        if self.connection_rooms.contains_key(&connection_id) {
            return Err("Este cliente ja esta em uma sala.".to_owned());
        }

        let code = if let Some(code) = requested_code {
            let sanitized = code.trim().to_ascii_uppercase();
            if !is_valid_room_code(&sanitized) {
                return Err(
                    "O código de sala fornecido é inválido. Utilize de 4 a 32 caracteres alfanuméricos."
                        .to_owned(),
                );
            }
            if self.rooms.contains_key(&sanitized) {
                return Err("Este código de sala já está em uso.".to_owned());
            }
            sanitized
        } else {
            loop {
                let candidate = format!(
                    "{:0width$X}",
                    rand::random::<u64>() & ((1_u64 << (ROOM_CODE_LENGTH * 4)) - 1),
                    width = ROOM_CODE_LENGTH
                );
                if !self.rooms.contains_key(&candidate) {
                    break candidate;
                }
            }
        };

        self.rooms.insert(
            code.clone(),
            Room {
                participants: vec![(connection_id, sender.clone())],
                members: vec![default_participant(connection_id, 1)],
                leader_connection_id: connection_id,
                pending_transfer: None,
                room_mode,
            },
        );
        self.connection_rooms.insert(connection_id, code.clone());
        self.participant_info
            .insert(connection_id, default_participant(connection_id, 1));
        if sender
            .send(OutboundMessage::Protocol(ServerMessage::RoomCreated {
                code: code.clone(),
            }))
            .is_err()
        {
            self.leave_room(connection_id);
            return Err("Nao foi possivel enviar a confirmacao da sala.".to_owned());
        }
        Ok(code)
    }

    fn join_room(
        &mut self,
        connection_id: ConnectionId,
        code: &str,
        sender: Outgoing,
    ) -> Result<(), String> {
        if self.connection_rooms.contains_key(&connection_id) {
            return Err("Este cliente ja esta em uma sala.".to_owned());
        }

        let Some(room) = self.rooms.get_mut(code) else {
            return Err(ROOM_NOT_FOUND_MESSAGE.to_owned());
        };
        let capacity = match room.room_mode {
            RoomMode::Local => MAX_ROOM_PARTICIPANTS,
            RoomMode::InternetTest => 2,
        };
        if room.participants.len() >= capacity {
            return Err(match room.room_mode {
                RoomMode::Local => "A sala ja atingiu o limite de 8 participantes.".to_owned(),
                RoomMode::InternetTest => {
                    "Esta sala de teste pela internet aceita somente duas pessoas.".to_owned()
                }
            });
        }

        let existing_peers = room
            .participants
            .iter()
            .map(|(_, peer)| peer.clone())
            .collect::<Vec<_>>();
        let order = room
            .members
            .iter()
            .map(|participant| participant.order)
            .max()
            .unwrap_or(0)
            .saturating_add(1);
        room.participants.push((connection_id, sender.clone()));
        self.connection_rooms.insert(connection_id, code.to_owned());
        let participant = default_participant(connection_id, order);
        room.members.push(participant.clone());
        self.participant_info.insert(connection_id, participant);

        if sender
            .send(OutboundMessage::Protocol(ServerMessage::RoomJoined {
                code: code.to_owned(),
            }))
            .is_err()
        {
            self.leave_room(connection_id);
            return Err("Nao foi possivel confirmar a entrada na sala.".to_owned());
        }
        for peer in existing_peers {
            let _ = peer.send(OutboundMessage::Protocol(ServerMessage::PeerJoined));
        }
        Ok(())
    }

    fn create_room_identified(
        &mut self,
        connection_id: ConnectionId,
        sender: Outgoing,
        participant: ParticipantInfo,
        room_mode: RoomMode,
        fixed_code: Option<String>,
    ) -> Result<(), String> {
        self.create_room_with_mode(connection_id, sender, room_mode, fixed_code)?;
        self.identify_participant(connection_id, participant)
    }

    fn join_room_identified(
        &mut self,
        connection_id: ConnectionId,
        code: &str,
        sender: Outgoing,
        participant: ParticipantInfo,
    ) -> Result<(), String> {
        let Some(room) = self.rooms.get(code) else {
            return Err(ROOM_NOT_FOUND_MESSAGE.to_owned());
        };
        let is_known_member = room.members.iter().any(|known| known.id == participant.id);
        let is_already_connected = room.participants.iter().any(|(other_id, _)| {
            *other_id != connection_id
                && self
                    .participant_info
                    .get(other_id)
                    .is_some_and(|known| known.id == participant.id)
        });
        if is_already_connected {
            return Err("Este participante ja esta conectado a sala.".to_owned());
        }
        let capacity = match room.room_mode {
            RoomMode::Local => MAX_ROOM_PARTICIPANTS,
            RoomMode::InternetTest => 2,
        };
        if !is_known_member && room.members.len() >= capacity {
            return Err(match room.room_mode {
                RoomMode::Local => "A sala ja atingiu o limite de 8 participantes.".to_owned(),
                RoomMode::InternetTest => {
                    "Esta sala de teste pela internet aceita somente duas pessoas.".to_owned()
                }
            });
        }
        self.join_room(connection_id, code, sender)?;
        self.identify_participant(connection_id, participant)
    }

    fn identify_participant(
        &mut self,
        connection_id: ConnectionId,
        mut participant: ParticipantInfo,
    ) -> Result<(), String> {
        let Some(code) = self.connection_rooms.get(&connection_id).cloned() else {
            return Err("Entre em uma sala antes de informar sua identidade.".to_owned());
        };
        let Some(room) = self.rooms.get(&code) else {
            return Err("A sala foi encerrada.".to_owned());
        };
        let internet_test = room.room_mode == RoomMode::InternetTest;
        if room.participants.iter().any(|(other_id, _)| {
            *other_id != connection_id
                && self
                    .participant_info
                    .get(other_id)
                    .is_some_and(|known| known.id == participant.id)
        }) {
            return Err("Essa identidade ja esta sendo usada na sala.".to_owned());
        }
        let existing = self.participant_info.get(&connection_id).cloned();
        let existing_order = self
            .participant_info
            .get(&connection_id)
            .map_or(1, |known| known.order);
        participant.order = if participant.order == 0 {
            existing_order
        } else {
            participant.order
        };
        participant.display_name =
            sanitize_display_name(&participant.display_name, participant.order);
        participant.avatar_jpeg_base64 = sanitize_avatar(participant.avatar_jpeg_base64);
        participant.public_key = sanitize_public_key(participant.public_key);
        if let Some(known) = room.members.iter().find(|known| known.id == participant.id)
            && known.public_key.is_some()
            && known.public_key != participant.public_key
        {
            return Err("Essa identidade ja foi registrada na sala com outra chave.".to_owned());
        }
        if internet_test {
            participant.may_host = false;
            participant.control_address.clear();
            participant.avatar_jpeg_base64 = None;
        }
        self.participant_info.insert(connection_id, participant);
        if let Some(room) = self.rooms.get_mut(&code) {
            let previous_id = existing.as_ref().map(|known| known.id.as_str());
            room.members.retain(|known| {
                known.id != self.participant_info[&connection_id].id
                    && previous_id != Some(known.id.as_str())
            });
            room.members
                .push(self.participant_info[&connection_id].clone());
            room.members.sort_by_key(|known| known.order);
        }
        self.broadcast_roster(&code);
        Ok(())
    }

    fn broadcast_roster(&self, code: &str) {
        let Some(room) = self.rooms.get(code) else {
            return;
        };
        let mut participants = room.members.clone();
        for (id, _) in &room.participants {
            if let Some(active) = self.participant_info.get(id) {
                if let Some(known) = participants.iter_mut().find(|known| known.id == active.id) {
                    *known = active.clone();
                } else {
                    participants.push(active.clone());
                }
            }
        }
        participants.sort_by_key(|participant| participant.order);
        let leader_id = self
            .participant_info
            .get(&room.leader_connection_id)
            .map(|participant| participant.id.clone())
            .unwrap_or_default();
        let message = OutboundMessage::Protocol(ServerMessage::RoomRoster {
            participants,
            leader_id,
            room_mode: room.room_mode,
        });
        for (_, sender) in &room.participants {
            let _ = sender.send(match &message {
                OutboundMessage::Protocol(message) => OutboundMessage::Protocol(message.clone()),
                OutboundMessage::Control(message) => OutboundMessage::Control(message.clone()),
                OutboundMessage::CloseAcknowledgement(message) => {
                    OutboundMessage::CloseAcknowledgement(message.clone())
                }
            });
        }
    }

    fn request_host_transfer(&mut self, host_id: ConnectionId) -> Result<PendingTransfer, String> {
        let Some(code) = self.connection_rooms.get(&host_id).cloned() else {
            return Err("Entre em uma sala antes de transferir a hospedagem.".to_owned());
        };
        let Some(room) = self.rooms.get_mut(&code) else {
            return Err("A sala foi encerrada.".to_owned());
        };
        if room.room_mode == RoomMode::InternetTest {
            return Err("A sucessao de anfitriao esta desativada no modo Internet.".to_owned());
        }
        if room.participants.len() != 2 {
            return Err("Ainda nao ha outro participante para assumir a sala.".to_owned());
        }
        if room.pending_transfer.is_some() {
            return Err("Ja existe uma transferencia de anfitriao em andamento.".to_owned());
        }

        let (target_id, target_sender) = room
            .participants
            .iter()
            .find(|(participant_id, _)| *participant_id != host_id)
            .map(|(participant_id, sender)| (*participant_id, sender.clone()))
            .ok_or_else(|| "Nao foi encontrado o outro participante.".to_owned())?;
        let transfer = PendingTransfer {
            host_id,
            target_id,
            code: code.clone(),
            token: format!("{:032X}", rand::random::<u128>()),
        };
        room.pending_transfer = Some(transfer.clone());
        if target_sender
            .send(OutboundMessage::Protocol(
                ServerMessage::HostTransferRequested {
                    code: code.clone(),
                    token: transfer.token.clone(),
                },
            ))
            .is_err()
        {
            room.pending_transfer = None;
            return Err("O outro participante desconectou.".to_owned());
        }
        if let Some((_, host_sender)) = room
            .participants
            .iter()
            .find(|(participant_id, _)| *participant_id == host_id)
        {
            let _ = host_sender.send(OutboundMessage::Protocol(
                ServerMessage::HostTransferPending {
                    code,
                    token: transfer.token.clone(),
                },
            ));
        }
        Ok(transfer)
    }

    fn adopt_transferred_room(
        &mut self,
        connection_id: ConnectionId,
        code: &str,
        token: &str,
        sender: Outgoing,
    ) -> Result<(), String> {
        if self.connection_rooms.contains_key(&connection_id) {
            return Err("Este cliente ja esta em uma sala.".to_owned());
        }
        let Some(reservation) = self.transfer_reservation.as_ref() else {
            return Err("Este servidor nao esta aguardando uma transferencia.".to_owned());
        };
        if reservation.code != code || reservation.token != token {
            return Err("Os dados da transferencia nao correspondem.".to_owned());
        }
        if self.rooms.contains_key(code) {
            return Err("O codigo da sala ja esta em uso neste servidor.".to_owned());
        }

        let mut members = reservation.participants.clone();
        self.transfer_reservation = None;
        let host_placeholder = default_participant(connection_id, 1);
        members.retain(|member| member.id != host_placeholder.id);
        members.push(host_placeholder.clone());
        self.rooms.insert(
            code.to_owned(),
            Room {
                participants: vec![(connection_id, sender.clone())],
                members,
                leader_connection_id: connection_id,
                pending_transfer: None,
                room_mode: RoomMode::Local,
            },
        );
        self.connection_rooms.insert(connection_id, code.to_owned());
        self.participant_info
            .insert(connection_id, host_placeholder);
        if sender
            .send(OutboundMessage::Protocol(ServerMessage::RoomAdopted {
                code: code.to_owned(),
            }))
            .is_err()
        {
            self.leave_room(connection_id);
            return Err("Nao foi possivel confirmar a sala transferida.".to_owned());
        }
        Ok(())
    }

    fn confirm_host_transfer(
        &mut self,
        connection_id: ConnectionId,
        code: &str,
        token: &str,
    ) -> Result<(), String> {
        let (host_id, target_id, host_sender, target_sender) = {
            let Some(room) = self.rooms.get(code) else {
                return Err("A sala original nao existe mais.".to_owned());
            };
            let Some(transfer) = room.pending_transfer.as_ref() else {
                return Err("Nao ha transferencia de anfitriao pendente.".to_owned());
            };
            if transfer.target_id != connection_id || transfer.token != token {
                return Err("A confirmacao da transferencia nao e valida.".to_owned());
            }
            let host_sender = room
                .participants
                .iter()
                .find(|(participant_id, _)| *participant_id == transfer.host_id)
                .map(|(_, sender)| sender.clone())
                .ok_or_else(|| "O anfitriao original desconectou.".to_owned())?;
            let target_sender = room
                .participants
                .iter()
                .find(|(participant_id, _)| *participant_id == transfer.target_id)
                .map(|(_, sender)| sender.clone())
                .ok_or_else(|| "O novo anfitriao desconectou.".to_owned())?;
            (
                transfer.host_id,
                transfer.target_id,
                host_sender,
                target_sender,
            )
        };
        self.rooms.remove(code);
        self.connection_rooms.remove(&host_id);
        self.connection_rooms.remove(&target_id);
        let completion = OutboundMessage::Protocol(ServerMessage::HostTransferComplete {
            code: code.to_owned(),
        });
        let _ = host_sender.send(completion);
        let _ = target_sender.send(OutboundMessage::Protocol(
            ServerMessage::HostTransferComplete {
                code: code.to_owned(),
            },
        ));
        Ok(())
    }

    fn reject_host_transfer(&mut self, connection_id: ConnectionId, token: &str) -> bool {
        let Some(code) = self.connection_rooms.get(&connection_id).cloned() else {
            return false;
        };
        let Some(room) = self.rooms.get_mut(&code) else {
            return false;
        };
        let Some(transfer) = room.pending_transfer.as_ref() else {
            return false;
        };
        if transfer.target_id != connection_id || transfer.token != token {
            return false;
        }
        let host_id = transfer.host_id;
        let host_sender = room
            .participants
            .iter()
            .find(|(participant_id, _)| *participant_id == host_id)
            .map(|(_, sender)| sender.clone());
        room.pending_transfer = None;
        if let Some(host_sender) = host_sender {
            let message = ServerMessage::HostTransferCanceled {
                message: "O outro participante recusou a transferencia.".to_owned(),
            };
            let _ = host_sender.send(OutboundMessage::Protocol(message.clone()));
            if let Some((_, target_sender)) = room
                .participants
                .iter()
                .find(|(participant_id, _)| *participant_id == connection_id)
            {
                let _ = target_sender.send(OutboundMessage::Protocol(message));
            }
        }
        true
    }

    fn cancel_host_transfer(&mut self, connection_id: ConnectionId, token: &str) -> bool {
        let Some(code) = self.connection_rooms.get(&connection_id).cloned() else {
            return false;
        };
        let Some(room) = self.rooms.get_mut(&code) else {
            return false;
        };
        let Some(transfer) = room.pending_transfer.as_ref() else {
            return false;
        };
        if transfer.host_id != connection_id || transfer.token != token {
            return false;
        }
        room.pending_transfer = None;
        let message = ServerMessage::HostTransferCanceled {
            message: "O anfitriao cancelou a transferencia.".to_owned(),
        };
        for (_, sender) in &room.participants {
            let _ = sender.send(OutboundMessage::Protocol(message.clone()));
        }
        true
    }

    fn expire_host_transfer(&mut self, transfer: &PendingTransfer) {
        let Some(room) = self.rooms.get_mut(&transfer.code) else {
            return;
        };
        if room
            .pending_transfer
            .as_ref()
            .is_none_or(|current| current.token != transfer.token)
        {
            return;
        }
        room.pending_transfer = None;
        let message = ServerMessage::HostTransferCanceled {
            message: "A transferencia expirou antes de ser concluida.".to_owned(),
        };
        for (participant_id, sender) in &room.participants {
            if *participant_id == transfer.host_id || *participant_id == transfer.target_id {
                let _ = sender.send(OutboundMessage::Protocol(message.clone()));
            }
        }
    }

    fn forward_signal(
        &mut self,
        connection_id: ConnectionId,
        kind: signaling_protocol::SignalKind,
        payload: String,
        target_participant_id: Option<String>,
        stream_id: Option<String>,
    ) -> Result<(), String> {
        let Some(code) = self.connection_rooms.get(&connection_id) else {
            return Err("Entre em uma sala antes de enviar um sinal.".to_owned());
        };
        let Some(room) = self.rooms.get(code) else {
            self.connection_rooms.remove(&connection_id);
            return Err("A sala foi encerrada.".to_owned());
        };
        let from_participant_id = self
            .participant_info
            .get(&connection_id)
            .map(|participant| participant.id.clone())
            .ok_or_else(|| "A identidade do participante ainda nao foi registrada.".to_owned())?;
        if stream_id
            .as_ref()
            .is_some_and(|id| id.is_empty() || id.len() > 128)
        {
            return Err("O identificador da transmissão é inválido.".to_owned());
        }
        let peers = if let Some(target_id) = target_participant_id {
            let target = room
                .participants
                .iter()
                .find(|(peer_id, _)| {
                    *peer_id != connection_id
                        && self
                            .participant_info
                            .get(peer_id)
                            .is_some_and(|participant| participant.id == target_id)
                })
                .map(|(_, sender)| sender.clone())
                .ok_or_else(|| {
                    "O destinatario do sinal nao esta conectado nesta sala.".to_owned()
                })?;
            vec![target]
        } else {
            room.participants
                .iter()
                .filter(|(peer_id, _)| *peer_id != connection_id)
                .map(|(_, sender)| sender.clone())
                .collect::<Vec<_>>()
        };
        if peers.is_empty()
            && !matches!(
                kind,
                signaling_protocol::SignalKind::ScreenShareAvailable
                    | signaling_protocol::SignalKind::ScreenShareUnavailable
            )
        {
            return Err("Aguardando o outro participante entrar na sala.".to_owned());
        }
        let message = ServerMessage::Signal {
            kind,
            payload,
            from_participant_id: Some(from_participant_id),
            stream_id,
        };
        for peer in peers {
            let _ = peer.send(OutboundMessage::Protocol(message.clone()));
        }
        Ok(())
    }

    fn leave_room(&mut self, connection_id: ConnectionId) -> bool {
        let Some(code) = self.connection_rooms.remove(&connection_id) else {
            return false;
        };
        let departing = self.participant_info.remove(&connection_id);

        let mut remove_room = false;
        if let Some(room) = self.rooms.get_mut(&code) {
            if let Some(departing) = &departing {
                room.members.retain(|member| member.id != departing.id);
            }
            if room.pending_transfer.take().is_some() {
                let message = ServerMessage::HostTransferCanceled {
                    message: "A transferencia foi cancelada porque um participante desconectou."
                        .to_owned(),
                };
                for (participant_id, sender) in &room.participants {
                    if *participant_id != connection_id {
                        let _ = sender.send(OutboundMessage::Protocol(message.clone()));
                    }
                }
            }
            room.participants
                .retain(|(participant_id, _)| *participant_id != connection_id);
            for (_, peer) in &room.participants {
                let _ = peer.send(OutboundMessage::Protocol(ServerMessage::PeerLeft));
            }
            if room.leader_connection_id == connection_id
                && let Some((next_leader_id, _)) = room.participants.iter().min_by_key(|(id, _)| {
                    self.participant_info
                        .get(id)
                        .map_or(u8::MAX, |info| info.order)
                })
            {
                room.leader_connection_id = *next_leader_id;
            }
            remove_room = room.participants.is_empty();
        }
        if remove_room {
            self.rooms.remove(&code);
        } else {
            self.broadcast_roster(&code);
        }
        true
    }
}

fn sanitize_display_name(name: &str, order: u8) -> String {
    let cleaned = name
        .trim()
        .chars()
        .filter(|character| !character.is_control())
        .take(MAX_DISPLAY_NAME_CHARS)
        .collect::<String>()
        .trim()
        .to_owned();
    if cleaned.is_empty() {
        format!("Participante {order}")
    } else {
        cleaned
    }
}

/// Aceita apenas chaves Ed25519 de exatamente 32 bytes em base64 padrão.
fn sanitize_public_key(encoded: Option<String>) -> Option<String> {
    let encoded = encoded?;
    if encoded.len() != 44 {
        return None;
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded.as_bytes())
        .ok()?;
    (bytes.len() == 32).then_some(encoded)
}

fn sanitize_avatar(encoded: Option<String>) -> Option<String> {
    let encoded = encoded?;
    if encoded.len() > MAX_AVATAR_BASE64_BYTES {
        return None;
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded.as_bytes())
        .ok()?;
    if bytes.is_empty() || bytes.len() > MAX_AVATAR_BYTES || !is_small_baseline_jpeg(&bytes) {
        return None;
    }
    Some(encoded)
}

fn is_small_baseline_jpeg(bytes: &[u8]) -> bool {
    if !bytes.starts_with(&[0xff, 0xd8]) {
        return false;
    }

    let mut offset = 2;
    while offset < bytes.len() {
        if bytes[offset] != 0xff {
            return false;
        }
        while offset < bytes.len() && bytes[offset] == 0xff {
            offset += 1;
        }
        let Some(&marker) = bytes.get(offset) else {
            return false;
        };
        offset += 1;
        if marker == 0xd9 || marker == 0xda {
            return false;
        }
        if marker == 0xd8 || marker == 0x01 || (0xd0..=0xd7).contains(&marker) {
            continue;
        }
        let Some(length_bytes) = bytes.get(offset..offset + 2) else {
            return false;
        };
        let segment_length = usize::from(u16::from_be_bytes([length_bytes[0], length_bytes[1]]));
        if segment_length < 2 || offset + segment_length > bytes.len() {
            return false;
        }
        if marker == 0xc0 {
            if segment_length < 8 {
                return false;
            }
            let height = u16::from_be_bytes([bytes[offset + 3], bytes[offset + 4]]);
            let width = u16::from_be_bytes([bytes[offset + 5], bytes[offset + 6]]);
            return width > 0 && height > 0 && width <= 96 && height <= 96;
        }
        offset += segment_length;
    }
    false
}

fn default_participant(connection_id: ConnectionId, order: u8) -> ParticipantInfo {
    ParticipantInfo {
        id: format!("participant-{connection_id}"),
        display_name: format!("Participante {order}"),
        order,
        may_host: false,
        control_address: String::new(),
        avatar_jpeg_base64: None,
        supports_group_screen_share: false,
        supports_group_session_ids: false,
        public_key: None,
    }
}

pub async fn serve(
    listener: TcpListener,
    mut shutdown: oneshot::Receiver<()>,
    transfer_reservation: Option<TransferReservation>,
) -> io::Result<()> {
    let registry = RoomRegistry {
        transfer_reservation,
        ..Default::default()
    };
    let rooms = Arc::new(Mutex::new(registry));
    let next_connection_id = Arc::new(AtomicU64::new(1));
    tracing::info!(local_address = ?listener.local_addr().ok(), "Servidor de sinalização pronto para aceitar conexões");

    loop {
        let accepted = tokio::select! {
            _ = &mut shutdown => break,
            accepted = listener.accept() => accepted,
        };
        let (stream, address) = match accepted {
            Ok(accepted) => accepted,
            Err(error) => {
                tracing::error!(error = %error, "Listener de sinalização falhou ao aceitar conexão");
                return Err(error);
            }
        };
        let connection_id = next_connection_id.fetch_add(1, Ordering::Relaxed);
        let rooms = Arc::clone(&rooms);
        let _ = spawn_admitted_connection(stream, connection_id, address, rooms).await;
    }
    Ok(())
}

async fn spawn_admitted_connection(
    stream: TcpStream,
    connection_id: ConnectionId,
    peer_address: SocketAddr,
    rooms: SharedRooms,
) -> Option<tokio::task::JoinHandle<()>> {
    let peer_ip = Some(peer_address.ip());
    if let Err(reason) = rooms.lock().await.admit_connection(peer_ip) {
        tracing::warn!(
            connection_id,
            peer_address = %peer_address,
            reason,
            "Conexão recusada pelo limite do servidor"
        );
        return None;
    }

    tracing::info!(connection_id, peer_address = %peer_address, "Cliente conectado ao servidor de sinalização");
    Some(tokio::spawn(async move {
        handle_connection(stream, connection_id, peer_ip, Arc::clone(&rooms)).await;
        tracing::info!(connection_id, peer_address = %peer_address, "Cliente desconectado do servidor de sinalização");
    }))
}

fn websocket_config() -> WebSocketConfig {
    WebSocketConfig::default()
        .max_message_size(Some(MAX_MESSAGE_BYTES))
        .max_frame_size(Some(MAX_MESSAGE_BYTES))
}

async fn handle_connection(
    stream: TcpStream,
    connection_id: ConnectionId,
    peer_ip: Option<IpAddr>,
    rooms: SharedRooms,
) {
    let handshake = timeout(
        HANDSHAKE_TIMEOUT,
        accept_async_with_config(stream, Some(websocket_config())),
    )
    .await;
    match handshake {
        Ok(Ok(websocket)) => {
            serve_websocket(websocket, connection_id, peer_ip, Arc::clone(&rooms)).await;
        }
        Ok(Err(error)) => {
            tracing::warn!(connection_id, error = %error, "Falha no handshake WebSocket recebido");
        }
        Err(_) => {
            tracing::warn!(connection_id, "Handshake WebSocket excedeu o tempo limite");
        }
    }
    rooms.lock().await.release_connection(peer_ip);
}

async fn serve_websocket(
    websocket: WebSocketStream<TcpStream>,
    connection_id: ConnectionId,
    peer_ip: Option<IpAddr>,
    rooms: SharedRooms,
) {
    let (websocket_sender, mut websocket_receiver) = websocket.split();
    let (outgoing, outgoing_messages, overload_events) = Outgoing::channel();

    let mut writer = tokio::spawn(write_outgoing_messages(
        websocket_sender,
        outgoing_messages,
        overload_events,
        connection_id,
    ));
    let mut writer_finished = false;

    loop {
        let frame = tokio::select! {
            frame = websocket_receiver.next() => frame,
            _ = &mut writer => {
                writer_finished = true;
                break;
            }
        };
        let Some(frame) = frame else {
            break;
        };
        let frame = match frame {
            Ok(frame) => frame,
            Err(error) => {
                tracing::warn!(connection_id, error = %error, "Falha ao receber mensagem WebSocket");
                break;
            }
        };

        match frame {
            WebSocketMessage::Text(text) => {
                if text.len() > MAX_MESSAGE_BYTES {
                    tracing::warn!(
                        connection_id,
                        payload_bytes = text.len(),
                        "Mensagem recebida excedeu o limite; conteúdo omitido"
                    );
                    send_error(&outgoing, "A mensagem excede o limite permitido.");
                    break;
                }
                match serde_json::from_str::<ClientMessage>(text.as_str()) {
                    Ok(message) => {
                        handle_client_message(connection_id, peer_ip, message, &outgoing, &rooms)
                            .await;
                    }
                    Err(error) => {
                        tracing::warn!(connection_id, error = %error, "Mensagem JSON inválida recebida; conteúdo omitido");
                        send_error(
                            &outgoing,
                            &format!("Mensagem de sinalizacao invalida: {error}"),
                        );
                    }
                }
            }
            WebSocketMessage::Ping(payload) => {
                let _ = outgoing.send(OutboundMessage::Control(WebSocketMessage::Pong(payload)));
            }
            WebSocketMessage::Close(close_frame) => {
                let _ = outgoing.send(OutboundMessage::CloseAcknowledgement(
                    WebSocketMessage::Close(close_frame),
                ));
                break;
            }
            WebSocketMessage::Binary(_) => {
                send_error(&outgoing, "Envie mensagens JSON em texto.");
            }
            WebSocketMessage::Pong(_) | WebSocketMessage::Frame(_) => {}
        }
    }

    let left_room = rooms.lock().await.leave_room(connection_id);
    if left_room {
        tracing::info!(connection_id, "Cliente removido da sala ao fechar conexão");
    }
    drop(outgoing);
    if !writer_finished {
        let _ = writer.await;
    }
}

async fn write_outgoing_messages<S>(
    mut websocket_sender: S,
    mut outgoing_messages: mpsc::Receiver<OutboundMessage>,
    mut overload_events: watch::Receiver<bool>,
    connection_id: ConnectionId,
) where
    S: futures_util::Sink<WebSocketMessage> + Unpin,
    S::Error: std::fmt::Display,
{
    let mut overload_watch_open = true;
    loop {
        let outgoing = tokio::select! {
            biased;
            changed = overload_events.changed(), if overload_watch_open => {
                match changed {
                    Ok(()) if *overload_events.borrow_and_update() => {
                        tracing::warn!(
                            connection_id,
                            queued = outgoing_messages.len(),
                            capacity = MAX_QUEUED_OUTBOUND_MESSAGES,
                            "Cliente lento removido: fila de saída atingiu o limite"
                        );
                        break;
                    }
                    Ok(()) => continue,
                    Err(_) => {
                        overload_watch_open = false;
                        continue;
                    }
                }
            }
            message = outgoing_messages.recv() => match message {
                Some(message) => message,
                None => break,
            }
        };
        let is_close = matches!(
            &outgoing,
            OutboundMessage::Control(WebSocketMessage::Close(_))
                | OutboundMessage::CloseAcknowledgement(WebSocketMessage::Close(_))
        );
        let is_acknowledgement = matches!(&outgoing, OutboundMessage::CloseAcknowledgement(_));
        let message = match outgoing {
            OutboundMessage::Protocol(message) => match serde_json::to_string(&message) {
                Ok(json) => WebSocketMessage::Text(json.into()),
                Err(error) => {
                    tracing::error!(connection_id, error = %error, "Falha ao serializar resposta do servidor");
                    continue;
                }
            },
            OutboundMessage::Control(message) => message,
            OutboundMessage::CloseAcknowledgement(message) => message,
        };
        match timeout(WRITE_TIMEOUT, websocket_sender.send(message)).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                tracing::warn!(connection_id, error = %error, "Falha ao enviar resposta pelo WebSocket");
                break;
            }
            Err(_) => {
                tracing::warn!(connection_id, "Envio pelo WebSocket excedeu o tempo limite");
                break;
            }
        }
        if is_close {
            if !is_acknowledgement {
                // The read half queues a CloseAcknowledgement when the peer
                // answers. Its frame is already on the socket, so do not echo
                // a second close frame.
                while let Ok(Some(response)) =
                    timeout(WRITE_TIMEOUT, outgoing_messages.recv()).await
                {
                    if matches!(response, OutboundMessage::CloseAcknowledgement(_)) {
                        break;
                    }
                    tracing::debug!(
                        connection_id,
                        "Ignoring queued WebSocket message after close started"
                    );
                }
            }
            break;
        }
    }
    let _ = websocket_sender.close().await;
}

async fn handle_client_message(
    connection_id: ConnectionId,
    peer_ip: Option<IpAddr>,
    message: ClientMessage,
    outgoing: &Outgoing,
    rooms: &SharedRooms,
) {
    if matches!(
        message,
        ClientMessage::JoinRoom { .. } | ClientMessage::JoinRoomIdentified { .. }
    ) && rooms
        .lock()
        .await
        .join_attempts_blocked(peer_ip, Instant::now())
    {
        tracing::warn!(
            connection_id,
            "Entrada recusada: muitas tentativas com códigos inválidos"
        );
        send_error(outgoing, TOO_MANY_JOIN_ATTEMPTS_MESSAGE);
        return;
    }
    let operation = match &message {
        ClientMessage::CreateRoom => "create_room",
        ClientMessage::CreateRoomIdentified { .. } => "create_room_identified",
        ClientMessage::JoinRoom { .. } => "join_room",
        ClientMessage::JoinRoomIdentified { .. } => "join_room_identified",
        ClientMessage::Signal { .. } => "signal",
        ClientMessage::IdentifyParticipant { .. } => "identify_participant",
        ClientMessage::LeaveRoom => "leave_room",
        ClientMessage::RequestHostTransfer => "request_host_transfer",
        ClientMessage::AdoptTransferredRoom { .. } => "adopt_transferred_room",
        ClientMessage::ConfirmHostTransfer { .. } => "confirm_host_transfer",
        ClientMessage::RejectHostTransfer { .. } => "reject_host_transfer",
        ClientMessage::CancelHostTransfer { .. } => "cancel_host_transfer",
    };
    if let ClientMessage::Signal { kind, payload, .. } = &message {
        tracing::debug!(connection_id, signal_kind = ?kind, payload_bytes = payload.len(), "Encaminhando sinal sem registrar conteúdo");
    }
    let result = match message {
        ClientMessage::CreateRoom => rooms
            .lock()
            .await
            .create_room(connection_id, outgoing.clone())
            .map(|_| ()),
        ClientMessage::CreateRoomIdentified {
            participant,
            room_mode,
            fixed_code,
        } => rooms.lock().await.create_room_identified(
            connection_id,
            outgoing.clone(),
            participant,
            room_mode,
            fixed_code,
        ),
        ClientMessage::JoinRoom { code } => {
            rooms
                .lock()
                .await
                .join_room(connection_id, &code, outgoing.clone())
        }
        ClientMessage::JoinRoomIdentified { code, participant } => rooms
            .lock()
            .await
            .join_room_identified(connection_id, &code, outgoing.clone(), participant),
        ClientMessage::Signal {
            kind,
            payload,
            target_participant_id,
            stream_id,
        } => rooms.lock().await.forward_signal(
            connection_id,
            kind,
            payload,
            target_participant_id,
            stream_id,
        ),
        ClientMessage::IdentifyParticipant { participant } => rooms
            .lock()
            .await
            .identify_participant(connection_id, participant),
        ClientMessage::LeaveRoom => {
            rooms.lock().await.leave_room(connection_id);
            send_message(outgoing, ServerMessage::RoomLeft);
            let _ = outgoing.send(OutboundMessage::Control(WebSocketMessage::Close(None)));
            Ok(())
        }
        ClientMessage::RequestHostTransfer => {
            let transfer = rooms.lock().await.request_host_transfer(connection_id);
            match transfer {
                Ok(transfer) => {
                    let timeout_rooms = Arc::clone(rooms);
                    tokio::spawn(async move {
                        sleep(HOST_TRANSFER_TIMEOUT).await;
                        timeout_rooms.lock().await.expire_host_transfer(&transfer);
                    });
                    Ok(())
                }
                Err(error) => Err(error),
            }
        }
        ClientMessage::AdoptTransferredRoom { code, token } => rooms
            .lock()
            .await
            .adopt_transferred_room(connection_id, &code, &token, outgoing.clone()),
        ClientMessage::ConfirmHostTransfer { code, token } => rooms
            .lock()
            .await
            .confirm_host_transfer(connection_id, &code, &token),
        ClientMessage::RejectHostTransfer { token } => {
            if rooms
                .lock()
                .await
                .reject_host_transfer(connection_id, &token)
            {
                Ok(())
            } else {
                Err("Nao ha uma transferencia pendente para recusar.".to_owned())
            }
        }
        ClientMessage::CancelHostTransfer { token } => {
            if rooms
                .lock()
                .await
                .cancel_host_transfer(connection_id, &token)
            {
                Ok(())
            } else {
                Err("Nao ha uma transferencia pendente que possa ser cancelada.".to_owned())
            }
        }
    };

    if let Err(message) = result {
        if message == ROOM_NOT_FOUND_MESSAGE && operation.starts_with("join_room") {
            rooms
                .lock()
                .await
                .record_failed_join(peer_ip, Instant::now());
        }
        tracing::warn!(connection_id, operation, reason = %message, "Operação do protocolo recusada");
        send_error(outgoing, &message);
    } else if operation != "signal" {
        tracing::debug!(connection_id, operation, "Operação do protocolo concluída");
    }
}

fn send_message(outgoing: &Outgoing, message: ServerMessage) {
    let _ = outgoing.send(OutboundMessage::Protocol(message));
}

fn send_error(outgoing: &Outgoing, message: &str) {
    send_message(
        outgoing,
        ServerMessage::Error {
            message: message.to_owned(),
        },
    );
}

#[cfg(test)]
#[path = "server/tests.rs"]
mod tests;
