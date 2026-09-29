use std::collections::HashMap;
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use signaling_protocol::{ClientMessage, ParticipantInfo, RoomMode, ServerMessage};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio::time::sleep;
use tokio_tungstenite::tungstenite::Message as WebSocketMessage;
use tokio_tungstenite::{WebSocketStream, accept_async};

const MAX_MESSAGE_BYTES: usize = 64 * 1024;
const HOST_TRANSFER_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_ROOM_PARTICIPANTS: usize = 8;

type ConnectionId = u64;
type Outgoing = mpsc::UnboundedSender<OutboundMessage>;
type SharedRooms = Arc<Mutex<RoomRegistry>>;

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
}

#[derive(Default)]
struct Room {
    participants: Vec<(ConnectionId, Outgoing)>,
    members: Vec<ParticipantInfo>,
    leader_connection_id: ConnectionId,
    pending_transfer: Option<PendingTransfer>,
    room_mode: RoomMode,
}

impl RoomRegistry {
    fn create_room(
        &mut self,
        connection_id: ConnectionId,
        sender: Outgoing,
    ) -> Result<String, String> {
        self.create_room_with_mode(connection_id, sender, RoomMode::Local)
    }

    fn create_room_with_mode(
        &mut self,
        connection_id: ConnectionId,
        sender: Outgoing,
        room_mode: RoomMode,
    ) -> Result<String, String> {
        if self.connection_rooms.contains_key(&connection_id) {
            return Err("Este cliente ja esta em uma sala.".to_owned());
        }

        let code = loop {
            let candidate = format!("{:08X}", rand::random::<u32>());
            if !self.rooms.contains_key(&candidate) {
                break candidate;
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
            return Err("A sala nao existe ou ja foi encerrada.".to_owned());
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
    ) -> Result<(), String> {
        self.create_room_with_mode(connection_id, sender, room_mode)?;
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
            return Err("A sala nao existe ou ja foi encerrada.".to_owned());
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
        participant.display_name = format!("Participante {}", participant.order);
        if internet_test {
            participant.may_host = false;
            participant.control_address.clear();
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

fn default_participant(connection_id: ConnectionId, order: u8) -> ParticipantInfo {
    ParticipantInfo {
        id: format!("participant-{connection_id}"),
        display_name: format!("Participante {order}"),
        order,
        may_host: false,
        control_address: String::new(),
        supports_group_screen_share: false,
    }
}

pub async fn serve(
    listener: TcpListener,
    mut shutdown: oneshot::Receiver<()>,
    transfer_reservation: Option<TransferReservation>,
) -> io::Result<()> {
    let mut registry = RoomRegistry::default();
    registry.transfer_reservation = transfer_reservation;
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
        tracing::info!(connection_id, peer_address = %address, "Cliente conectado ao servidor de sinalização");
        let rooms = Arc::clone(&rooms);
        tokio::spawn(async move {
            handle_connection(stream, connection_id, rooms).await;
            tracing::info!(connection_id, peer_address = %address, "Cliente desconectado do servidor de sinalização");
        });
    }
    Ok(())
}

async fn handle_connection(stream: TcpStream, connection_id: ConnectionId, rooms: SharedRooms) {
    let websocket = match accept_async(stream).await {
        Ok(websocket) => websocket,
        Err(error) => {
            tracing::warn!(connection_id, error = %error, "Falha no handshake WebSocket recebido");
            return;
        }
    };
    serve_websocket(websocket, connection_id, rooms).await;
}

async fn serve_websocket(
    websocket: WebSocketStream<TcpStream>,
    connection_id: ConnectionId,
    rooms: SharedRooms,
) {
    let (websocket_sender, mut websocket_receiver) = websocket.split();
    let (outgoing, outgoing_messages) = mpsc::unbounded_channel::<OutboundMessage>();

    let writer = tokio::spawn(write_outgoing_messages(
        websocket_sender,
        outgoing_messages,
        connection_id,
    ));

    while let Some(frame) = websocket_receiver.next().await {
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
                        handle_client_message(connection_id, message, &outgoing, &rooms).await;
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
    let _ = writer.await;
}

async fn write_outgoing_messages<S>(
    mut websocket_sender: S,
    mut outgoing_messages: mpsc::UnboundedReceiver<OutboundMessage>,
    connection_id: ConnectionId,
) where
    S: futures_util::Sink<WebSocketMessage> + Unpin,
    S::Error: std::fmt::Display,
{
    while let Some(outgoing) = outgoing_messages.recv().await {
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
        if let Err(error) = websocket_sender.send(message).await {
            tracing::warn!(connection_id, error = %error, "Falha ao enviar resposta pelo WebSocket");
            break;
        }
        if is_close {
            if !is_acknowledgement {
                // The read half queues a CloseAcknowledgement when the peer
                // answers. Its frame is already on the socket, so do not echo
                // a second close frame.
                while let Some(response) = outgoing_messages.recv().await {
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
    message: ClientMessage,
    outgoing: &Outgoing,
    rooms: &SharedRooms,
) {
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
        } => rooms.lock().await.create_room_identified(
            connection_id,
            outgoing.clone(),
            participant,
            room_mode,
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
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    use futures_util::{SinkExt, StreamExt};
    use tokio::net::TcpListener;
    use tokio::sync::{Mutex, mpsc, oneshot};
    use tokio::time::timeout;
    use tokio_tungstenite::tungstenite::Message as WebSocketMessage;
    use tokio_tungstenite::{WebSocketStream, connect_async};

    use super::{
        OutboundMessage, RoomRegistry, TransferReservation, handle_connection, serve,
        write_outgoing_messages,
    };
    use signaling_protocol::{ClientMessage, ParticipantInfo, RoomMode, ServerMessage, SignalKind};

    struct RecordingSink(Arc<std::sync::Mutex<Vec<WebSocketMessage>>>);

    impl futures_util::Sink<WebSocketMessage> for RecordingSink {
        type Error = std::convert::Infallible;

        fn poll_ready(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), Self::Error>> {
            std::task::Poll::Ready(Ok(()))
        }

        fn start_send(
            self: std::pin::Pin<&mut Self>,
            item: WebSocketMessage,
        ) -> Result<(), Self::Error> {
            self.get_mut()
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(item);
            Ok(())
        }

        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), Self::Error>> {
            std::task::Poll::Ready(Ok(()))
        }

        fn poll_close(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), Self::Error>> {
            std::task::Poll::Ready(Ok(()))
        }
    }

    #[tokio::test]
    async fn websocket_writer_sends_room_left_before_close_and_drops_later_messages() {
        let (outgoing, receiver) = mpsc::unbounded_channel();
        outgoing
            .send(OutboundMessage::Protocol(ServerMessage::RoomLeft))
            .unwrap();
        outgoing
            .send(OutboundMessage::Control(WebSocketMessage::Close(None)))
            .unwrap();
        outgoing
            .send(OutboundMessage::Protocol(ServerMessage::PeerLeft))
            .unwrap();
        drop(outgoing);

        let sent = Arc::new(std::sync::Mutex::new(Vec::new()));
        write_outgoing_messages(RecordingSink(Arc::clone(&sent)), receiver, 22).await;

        let sent = sent
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(
            sent.len(),
            2,
            "nenhuma mensagem pode ser enviada após Close"
        );
        assert!(matches!(
            sent.first(),
            Some(WebSocketMessage::Text(text))
                if serde_json::from_str::<ServerMessage>(text.as_str())
                    .is_ok_and(|message| matches!(message, ServerMessage::RoomLeft))
        ));
        assert!(matches!(sent.get(1), Some(WebSocketMessage::Close(_))));
    }

    fn server_message(receiver: &mut mpsc::UnboundedReceiver<OutboundMessage>) -> ServerMessage {
        match receiver.try_recv().expect("expected a server message") {
            OutboundMessage::Protocol(message) => message,
            OutboundMessage::Control(_) | OutboundMessage::CloseAcknowledgement(_) => {
                panic!("expected a protocol message")
            }
        }
    }

    #[test]
    fn rooms_have_eight_slots_and_reject_the_ninth_participant() {
        let mut registry = RoomRegistry::default();
        let (first_tx, mut first_rx) = mpsc::unbounded_channel();

        let code = registry.create_room(1, first_tx).unwrap();
        assert_eq!(code.len(), 8);
        assert_eq!(
            server_message(&mut first_rx),
            ServerMessage::RoomCreated { code: code.clone() }
        );

        for id in 2..=8 {
            let (sender, mut receiver) = mpsc::unbounded_channel();
            registry.join_room(id, &code, sender).unwrap();
            assert_eq!(
                server_message(&mut receiver),
                ServerMessage::RoomJoined { code: code.clone() }
            );
        }
        let (ninth_tx, _ninth_rx) = mpsc::unbounded_channel();
        assert!(
            registry
                .join_room(9, &code, ninth_tx)
                .unwrap_err()
                .contains("8")
        );
        assert_eq!(registry.rooms.get(&code).unwrap().participants.len(), 8);
    }

    #[test]
    fn internet_test_rooms_limit_to_two_disable_handoff_and_hide_control_addresses() {
        let mut registry = RoomRegistry::default();
        let (host_tx, mut host_rx) = mpsc::unbounded_channel();
        let (guest_tx, mut guest_rx) = mpsc::unbounded_channel();
        let code = registry
            .create_room_with_mode(1, host_tx, RoomMode::InternetTest)
            .unwrap();
        let _ = server_message(&mut host_rx); // RoomCreated
        registry
            .identify_participant(
                1,
                ParticipantInfo {
                    id: "host-id".to_owned(),
                    display_name: "ignored".to_owned(),
                    order: 0,
                    may_host: true,
                    control_address: "192.168.1.2:9001".to_owned(),
                    supports_group_screen_share: false,
                },
            )
            .unwrap();
        let ServerMessage::RoomRoster {
            participants,
            room_mode,
            ..
        } = server_message(&mut host_rx)
        else {
            panic!("expected internet room roster")
        };
        assert_eq!(room_mode, RoomMode::InternetTest);
        assert!(!participants[0].may_host);
        assert!(participants[0].control_address.is_empty());

        registry
            .join_room_identified(
                2,
                &code,
                guest_tx,
                ParticipantInfo {
                    id: "guest-id".to_owned(),
                    display_name: "ignored".to_owned(),
                    order: 0,
                    may_host: true,
                    control_address: "192.168.1.3:9001".to_owned(),
                    supports_group_screen_share: false,
                },
            )
            .unwrap();
        let _ = server_message(&mut guest_rx); // RoomJoined
        let _ = server_message(&mut host_rx); // PeerJoined
        for receiver in [&mut host_rx, &mut guest_rx] {
            let ServerMessage::RoomRoster {
                participants,
                room_mode,
                ..
            } = server_message(receiver)
            else {
                panic!("expected updated internet room roster")
            };
            assert_eq!(room_mode, RoomMode::InternetTest);
            assert_eq!(participants.len(), 2);
            assert!(participants.iter().all(|participant| {
                !participant.may_host && participant.control_address.is_empty()
            }));
        }

        let (third_tx, _third_rx) = mpsc::unbounded_channel();
        let error = registry.join_room(3, &code, third_tx).unwrap_err();
        assert!(error.contains("duas pessoas"));
        assert!(
            registry
                .request_host_transfer(1)
                .unwrap_err()
                .contains("desativada")
        );
    }

    #[test]
    fn roster_keeps_join_order_and_host_permission() {
        let mut registry = RoomRegistry::default();
        let (host_tx, mut host_rx) = mpsc::unbounded_channel();
        let (guest_tx, mut guest_rx) = mpsc::unbounded_channel();
        let code = registry.create_room(1, host_tx).unwrap();
        let _ = server_message(&mut host_rx);
        registry
            .identify_participant(
                1,
                ParticipantInfo {
                    id: "host-id".to_owned(),
                    display_name: "ignored".to_owned(),
                    order: 0,
                    may_host: true,
                    control_address: "192.168.1.2:9001".to_owned(),
                    supports_group_screen_share: false,
                },
            )
            .unwrap();
        let ServerMessage::RoomRoster {
            participants,
            leader_id,
            ..
        } = server_message(&mut host_rx)
        else {
            panic!("expected a participant roster")
        };
        assert_eq!(leader_id, "host-id");
        assert_eq!(participants[0].order, 1);
        assert!(participants[0].may_host);

        registry.join_room(2, &code, guest_tx).unwrap();
        let _ = server_message(&mut guest_rx);
        let _ = server_message(&mut host_rx);
        registry
            .identify_participant(
                2,
                ParticipantInfo {
                    id: "guest-id".to_owned(),
                    display_name: "ignored".to_owned(),
                    order: 0,
                    may_host: false,
                    control_address: "192.168.1.3:9001".to_owned(),
                    supports_group_screen_share: false,
                },
            )
            .unwrap();
        let ServerMessage::RoomRoster {
            participants,
            leader_id,
            ..
        } = server_message(&mut host_rx)
        else {
            panic!("expected updated roster")
        };
        assert_eq!(leader_id, "host-id");
        assert_eq!(
            participants
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            ["host-id", "guest-id"]
        );
        assert_eq!(
            participants
                .iter()
                .map(|item| item.order)
                .collect::<Vec<_>>(),
            [1, 2]
        );
        assert!(!participants[1].may_host);
    }

    #[test]
    fn election_adoption_preserves_roster_and_allows_existing_members_to_rejoin() {
        let original_members = vec![
            ParticipantInfo {
                id: "host-id".to_owned(),
                display_name: "Participante 1".to_owned(),
                order: 1,
                may_host: true,
                control_address: "192.168.1.2:9001".to_owned(),
                supports_group_screen_share: false,
            },
            ParticipantInfo {
                id: "candidate-id".to_owned(),
                display_name: "Participante 2".to_owned(),
                order: 2,
                may_host: true,
                control_address: "192.168.1.3:9001".to_owned(),
                supports_group_screen_share: false,
            },
            ParticipantInfo {
                id: "guest-id".to_owned(),
                display_name: "Participante 3".to_owned(),
                order: 3,
                may_host: false,
                control_address: "192.168.1.4:9001".to_owned(),
                supports_group_screen_share: false,
            },
        ];
        let mut registry = RoomRegistry {
            transfer_reservation: Some(TransferReservation {
                code: "SAMECODE".to_owned(),
                token: "election-token".to_owned(),
                participants: original_members,
            }),
            ..RoomRegistry::default()
        };
        let (candidate_tx, mut candidate_rx) = mpsc::unbounded_channel();
        registry
            .adopt_transferred_room(10, "SAMECODE", "election-token", candidate_tx)
            .unwrap();
        assert_eq!(
            server_message(&mut candidate_rx),
            ServerMessage::RoomAdopted {
                code: "SAMECODE".to_owned()
            }
        );
        registry
            .identify_participant(
                10,
                ParticipantInfo {
                    id: "candidate-id".to_owned(),
                    display_name: "Participante 2".to_owned(),
                    order: 2,
                    may_host: true,
                    control_address: "192.168.1.3:9001".to_owned(),
                    supports_group_screen_share: false,
                },
            )
            .unwrap();
        let ServerMessage::RoomRoster {
            participants,
            leader_id,
            ..
        } = server_message(&mut candidate_rx)
        else {
            panic!("expected the restored participant roster")
        };
        assert_eq!(leader_id, "candidate-id");
        assert_eq!(
            participants
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            ["host-id", "candidate-id", "guest-id"]
        );

        let (host_tx, mut host_rx) = mpsc::unbounded_channel();
        registry
            .join_room_identified(
                11,
                "SAMECODE",
                host_tx,
                ParticipantInfo {
                    id: "host-id".to_owned(),
                    display_name: "Participante 1".to_owned(),
                    order: 1,
                    may_host: true,
                    control_address: "192.168.1.2:9001".to_owned(),
                    supports_group_screen_share: false,
                },
            )
            .unwrap();
        assert_eq!(
            server_message(&mut host_rx),
            ServerMessage::RoomJoined {
                code: "SAMECODE".to_owned()
            }
        );
        let _ = server_message(&mut candidate_rx); // PeerJoined
        let ServerMessage::RoomRoster {
            participants,
            leader_id,
            ..
        } = server_message(&mut host_rx)
        else {
            panic!("expected the roster after the old host reconnects")
        };
        assert_eq!(leader_id, "candidate-id");
        assert_eq!(
            participants
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            ["host-id", "candidate-id", "guest-id"]
        );
        assert_eq!(
            participants
                .iter()
                .map(|item| item.order)
                .collect::<Vec<_>>(),
            [1, 2, 3]
        );
    }

    #[test]
    fn signaling_goes_only_to_the_other_participant_and_leave_frees_room() {
        let mut registry = RoomRegistry::default();
        let (first_tx, mut first_rx) = mpsc::unbounded_channel();
        let (second_tx, mut second_rx) = mpsc::unbounded_channel();
        let code = registry.create_room(1, first_tx).unwrap();
        let _ = server_message(&mut first_rx);
        registry.join_room(2, &code, second_tx).unwrap();
        let _ = server_message(&mut first_rx);
        let _ = server_message(&mut second_rx);

        registry
            .forward_signal(1, SignalKind::Diagnostic, "ping".to_owned(), None, None)
            .unwrap();
        assert_eq!(
            server_message(&mut second_rx),
            ServerMessage::Signal {
                kind: SignalKind::Diagnostic,
                payload: "ping".to_owned(),
                from_participant_id: Some("participant-1".to_owned()),
                stream_id: None,
            }
        );
        assert!(first_rx.try_recv().is_err());

        assert!(registry.leave_room(1));
        assert_eq!(server_message(&mut second_rx), ServerMessage::PeerLeft);
        assert!(registry.rooms.contains_key(&code));
        assert!(registry.leave_room(2));
        assert!(!registry.rooms.contains_key(&code));
    }

    #[test]
    fn targeted_media_signaling_reaches_only_the_named_room_member() {
        let mut registry = RoomRegistry::default();
        let (first_tx, mut first_rx) = mpsc::unbounded_channel();
        let (second_tx, mut second_rx) = mpsc::unbounded_channel();
        let (third_tx, mut third_rx) = mpsc::unbounded_channel();
        let code = registry.create_room(1, first_tx).unwrap();
        registry.join_room(2, &code, second_tx).unwrap();
        registry.join_room(3, &code, third_tx).unwrap();

        // Consume the room-created/joined notifications; this test concerns only
        // delivery of a directed WebRTC message.
        while first_rx.try_recv().is_ok() {}
        while second_rx.try_recv().is_ok() {}
        while third_rx.try_recv().is_ok() {}

        registry
            .forward_signal(
                1,
                SignalKind::Offer,
                "offer-sdp".to_owned(),
                Some("participant-2".to_owned()),
                Some("participant-1".to_owned()),
            )
            .unwrap();

        assert_eq!(
            server_message(&mut second_rx),
            ServerMessage::Signal {
                kind: SignalKind::Offer,
                payload: "offer-sdp".to_owned(),
                from_participant_id: Some("participant-1".to_owned()),
                stream_id: Some("participant-1".to_owned()),
            }
        );
        assert!(first_rx.try_recv().is_err());
        assert!(third_rx.try_recv().is_err());

        assert!(
            registry
                .forward_signal(
                    1,
                    SignalKind::IceCandidate,
                    "candidate".to_owned(),
                    Some("not-in-this-room".to_owned()),
                    Some("participant-1".to_owned()),
                )
                .is_err()
        );
    }

    #[test]
    fn canceled_or_expired_transfer_keeps_room_available_on_original_server() {
        let mut registry = RoomRegistry::default();
        let (host_tx, mut host_rx) = mpsc::unbounded_channel();
        let (peer_tx, mut peer_rx) = mpsc::unbounded_channel();
        let code = registry.create_room(1, host_tx).unwrap();
        let _ = server_message(&mut host_rx);
        registry.join_room(2, &code, peer_tx).unwrap();
        let _ = server_message(&mut host_rx);
        let _ = server_message(&mut peer_rx);

        let canceled = registry.request_host_transfer(1).unwrap();
        assert!(matches!(
            server_message(&mut peer_rx),
            ServerMessage::HostTransferRequested { .. }
        ));
        assert!(matches!(
            server_message(&mut host_rx),
            ServerMessage::HostTransferPending { .. }
        ));
        assert!(registry.cancel_host_transfer(1, &canceled.token));
        assert!(matches!(
            server_message(&mut host_rx),
            ServerMessage::HostTransferCanceled { .. }
        ));
        assert!(matches!(
            server_message(&mut peer_rx),
            ServerMessage::HostTransferCanceled { .. }
        ));
        assert!(registry.rooms.contains_key(&code));

        let expired = registry.request_host_transfer(1).unwrap();
        let _ = server_message(&mut peer_rx);
        let _ = server_message(&mut host_rx);
        registry.expire_host_transfer(&expired);
        assert!(matches!(
            server_message(&mut host_rx),
            ServerMessage::HostTransferCanceled { .. }
        ));
        assert!(matches!(
            server_message(&mut peer_rx),
            ServerMessage::HostTransferCanceled { .. }
        ));
        assert!(registry.rooms.contains_key(&code));
        assert_eq!(registry.rooms[&code].participants.len(), 2);

        let rejected = registry.request_host_transfer(1).unwrap();
        let _ = server_message(&mut peer_rx);
        let _ = server_message(&mut host_rx);
        assert!(registry.reject_host_transfer(2, &rejected.token));
        assert!(matches!(
            server_message(&mut host_rx),
            ServerMessage::HostTransferCanceled { .. }
        ));
        assert!(matches!(
            server_message(&mut peer_rx),
            ServerMessage::HostTransferCanceled { .. }
        ));
        assert!(registry.rooms.contains_key(&code));
    }

    async fn send_client_message<S>(socket: &mut WebSocketStream<S>, message: ClientMessage)
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    {
        let text = serde_json::to_string(&message).unwrap();
        socket
            .send(WebSocketMessage::Text(text.into()))
            .await
            .unwrap();
    }

    async fn receive_server_message<S>(socket: &mut WebSocketStream<S>) -> ServerMessage
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    {
        let frame = timeout(Duration::from_secs(3), socket.next())
            .await
            .expect("server response timed out")
            .expect("server closed the connection")
            .expect("websocket read failed");
        let WebSocketMessage::Text(text) = frame else {
            panic!("expected a JSON text message")
        };
        serde_json::from_str(text.as_str()).unwrap()
    }

    async fn start_test_server(
        reservation: Option<TransferReservation>,
    ) -> (
        String,
        oneshot::Sender<()>,
        tokio::task::JoinHandle<std::io::Result<()>>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let task = tokio::spawn(serve(listener, shutdown_rx, reservation));
        (format!("ws://{address}"), shutdown_tx, task)
    }

    #[tokio::test]
    async fn websocket_host_transfer_moves_room_to_new_server_and_keeps_same_code() {
        let (old_url, stop_old, old_server) = start_test_server(None).await;
        let (mut old_host, _) = connect_async(&old_url).await.unwrap();
        send_client_message(&mut old_host, ClientMessage::CreateRoom).await;
        let ServerMessage::RoomCreated { code } = receive_server_message(&mut old_host).await
        else {
            panic!("expected a new room")
        };

        let (mut old_peer, _) = connect_async(&old_url).await.unwrap();
        send_client_message(
            &mut old_peer,
            ClientMessage::JoinRoom { code: code.clone() },
        )
        .await;
        assert_eq!(
            receive_server_message(&mut old_peer).await,
            ServerMessage::RoomJoined { code: code.clone() }
        );
        assert_eq!(
            receive_server_message(&mut old_host).await,
            ServerMessage::PeerJoined
        );

        send_client_message(&mut old_host, ClientMessage::RequestHostTransfer).await;
        let ServerMessage::HostTransferRequested {
            code: requested_code,
            token,
        } = receive_server_message(&mut old_peer).await
        else {
            panic!("expected a handoff request")
        };
        assert_eq!(requested_code, code);
        assert_eq!(
            receive_server_message(&mut old_host).await,
            ServerMessage::HostTransferPending {
                code: code.clone(),
                token: token.clone()
            }
        );

        let (new_url, stop_new, new_server) = start_test_server(Some(TransferReservation {
            code: code.clone(),
            token: token.clone(),
            participants: Vec::new(),
        }))
        .await;
        let (mut new_host, _) = connect_async(&new_url).await.unwrap();
        send_client_message(
            &mut new_host,
            ClientMessage::AdoptTransferredRoom {
                code: code.clone(),
                token: token.clone(),
            },
        )
        .await;
        assert_eq!(
            receive_server_message(&mut new_host).await,
            ServerMessage::RoomAdopted { code: code.clone() }
        );

        send_client_message(
            &mut old_peer,
            ClientMessage::ConfirmHostTransfer {
                code: code.clone(),
                token,
            },
        )
        .await;
        assert_eq!(
            receive_server_message(&mut old_host).await,
            ServerMessage::HostTransferComplete { code: code.clone() }
        );
        assert_eq!(
            receive_server_message(&mut old_peer).await,
            ServerMessage::HostTransferComplete { code: code.clone() }
        );

        let (mut next_peer, _) = connect_async(&new_url).await.unwrap();
        send_client_message(
            &mut next_peer,
            ClientMessage::JoinRoom { code: code.clone() },
        )
        .await;
        assert_eq!(
            receive_server_message(&mut next_peer).await,
            ServerMessage::RoomJoined { code: code.clone() }
        );
        assert_eq!(
            receive_server_message(&mut new_host).await,
            ServerMessage::PeerJoined
        );

        let (mut stale_join, _) = connect_async(&old_url).await.unwrap();
        send_client_message(&mut stale_join, ClientMessage::JoinRoom { code }).await;
        assert!(matches!(
            receive_server_message(&mut stale_join).await,
            ServerMessage::Error { .. }
        ));

        old_host.close(None).await.unwrap();
        old_peer.close(None).await.unwrap();
        next_peer.close(None).await.unwrap();
        new_host.close(None).await.unwrap();
        stale_join.close(None).await.unwrap();
        let _ = stop_old.send(());
        let _ = stop_new.send(());
        old_server.await.unwrap().unwrap();
        new_server.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn websocket_clients_create_join_and_exchange_diagnostic_signal() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let rooms = Arc::new(Mutex::new(RoomRegistry::default()));
        let next_id = Arc::new(AtomicU64::new(1));
        let accept_task = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    break;
                };
                let connection_id = next_id.fetch_add(1, Ordering::Relaxed);
                let rooms = Arc::clone(&rooms);
                tokio::spawn(async move {
                    handle_connection(stream, connection_id, rooms).await;
                });
            }
        });

        let url = format!("ws://{address}");
        let (mut first, _) = connect_async(&url).await.unwrap();
        send_client_message(&mut first, ClientMessage::CreateRoom).await;
        let ServerMessage::RoomCreated { code } = receive_server_message(&mut first).await else {
            panic!("expected room creation confirmation")
        };

        let (mut second, _) = connect_async(&url).await.unwrap();
        send_client_message(&mut second, ClientMessage::JoinRoom { code: code.clone() }).await;
        assert_eq!(
            receive_server_message(&mut second).await,
            ServerMessage::RoomJoined { code: code.clone() }
        );
        assert_eq!(
            receive_server_message(&mut first).await,
            ServerMessage::PeerJoined
        );

        let (mut third, _) = connect_async(&url).await.unwrap();
        send_client_message(&mut third, ClientMessage::JoinRoom { code: code.clone() }).await;
        assert_eq!(
            receive_server_message(&mut third).await,
            ServerMessage::RoomJoined { code: code.clone() }
        );
        assert_eq!(
            receive_server_message(&mut first).await,
            ServerMessage::PeerJoined
        );
        assert_eq!(
            receive_server_message(&mut second).await,
            ServerMessage::PeerJoined
        );

        let (mut invalid, _) = connect_async(&url).await.unwrap();
        send_client_message(
            &mut invalid,
            ClientMessage::JoinRoom {
                code: "NOT-A-ROOM".to_owned(),
            },
        )
        .await;
        assert!(matches!(
            receive_server_message(&mut invalid).await,
            ServerMessage::Error { message } if message.contains("nao existe")
        ));
        invalid.close(None).await.unwrap();

        send_client_message(
            &mut first,
            ClientMessage::Signal {
                kind: SignalKind::Diagnostic,
                payload: "diagnostic-ping-v1".to_owned(),
                target_participant_id: None,
                stream_id: None,
            },
        )
        .await;
        assert_eq!(
            receive_server_message(&mut second).await,
            ServerMessage::Signal {
                kind: SignalKind::Diagnostic,
                payload: "diagnostic-ping-v1".to_owned(),
                from_participant_id: Some("participant-1".to_owned()),
                stream_id: None,
            }
        );
        assert_eq!(
            receive_server_message(&mut third).await,
            ServerMessage::Signal {
                kind: SignalKind::Diagnostic,
                payload: "diagnostic-ping-v1".to_owned(),
                from_participant_id: Some("participant-1".to_owned()),
                stream_id: None,
            }
        );
        send_client_message(
            &mut second,
            ClientMessage::Signal {
                kind: SignalKind::Diagnostic,
                payload: "diagnostic-pong-v1".to_owned(),
                target_participant_id: None,
                stream_id: None,
            },
        )
        .await;
        assert_eq!(
            receive_server_message(&mut first).await,
            ServerMessage::Signal {
                kind: SignalKind::Diagnostic,
                payload: "diagnostic-pong-v1".to_owned(),
                from_participant_id: Some("participant-2".to_owned()),
                stream_id: None,
            }
        );
        assert_eq!(
            receive_server_message(&mut third).await,
            ServerMessage::Signal {
                kind: SignalKind::Diagnostic,
                payload: "diagnostic-pong-v1".to_owned(),
                from_participant_id: Some("participant-2".to_owned()),
                stream_id: None,
            }
        );

        first.close(None).await.unwrap();
        assert_eq!(
            receive_server_message(&mut second).await,
            ServerMessage::PeerLeft
        );
        second.close(None).await.unwrap();
        accept_task.abort();
    }
}
