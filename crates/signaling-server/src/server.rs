use std::collections::HashMap;
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use signaling_protocol::{ClientMessage, ServerMessage};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio::time::sleep;
use tokio_tungstenite::tungstenite::Message as WebSocketMessage;
use tokio_tungstenite::{WebSocketStream, accept_async};

const MAX_MESSAGE_BYTES: usize = 64 * 1024;
const HOST_TRANSFER_TIMEOUT: Duration = Duration::from_secs(30);

type ConnectionId = u64;
type Outgoing = mpsc::UnboundedSender<OutboundMessage>;
type SharedRooms = Arc<Mutex<RoomRegistry>>;

#[derive(Clone, Debug)]
pub struct TransferReservation {
    pub code: String,
    pub token: String,
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
}

#[derive(Default)]
struct RoomRegistry {
    rooms: HashMap<String, Room>,
    connection_rooms: HashMap<ConnectionId, String>,
    transfer_reservation: Option<TransferReservation>,
}

#[derive(Default)]
struct Room {
    participants: Vec<(ConnectionId, Outgoing)>,
    pending_transfer: Option<PendingTransfer>,
}

impl RoomRegistry {
    fn create_room(
        &mut self,
        connection_id: ConnectionId,
        sender: Outgoing,
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
                pending_transfer: None,
            },
        );
        self.connection_rooms.insert(connection_id, code.clone());
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
        if room.participants.len() >= 2 {
            return Err("A sala ja esta cheia.".to_owned());
        }

        let existing_peer = room.participants.first().map(|(_, peer)| peer.clone());
        room.participants.push((connection_id, sender.clone()));
        self.connection_rooms.insert(connection_id, code.to_owned());

        if sender
            .send(OutboundMessage::Protocol(ServerMessage::RoomJoined {
                code: code.to_owned(),
            }))
            .is_err()
        {
            self.leave_room(connection_id);
            return Err("Nao foi possivel confirmar a entrada na sala.".to_owned());
        }
        if let Some(peer) = existing_peer {
            let _ = peer.send(OutboundMessage::Protocol(ServerMessage::PeerJoined));
        }
        Ok(())
    }

    fn request_host_transfer(&mut self, host_id: ConnectionId) -> Result<PendingTransfer, String> {
        let Some(code) = self.connection_rooms.get(&host_id).cloned() else {
            return Err("Entre em uma sala antes de transferir a hospedagem.".to_owned());
        };
        let Some(room) = self.rooms.get_mut(&code) else {
            return Err("A sala foi encerrada.".to_owned());
        };
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

        self.transfer_reservation = None;
        self.rooms.insert(
            code.to_owned(),
            Room {
                participants: vec![(connection_id, sender.clone())],
                pending_transfer: None,
            },
        );
        self.connection_rooms.insert(connection_id, code.to_owned());
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
    ) -> Result<(), String> {
        let Some(code) = self.connection_rooms.get(&connection_id) else {
            return Err("Entre em uma sala antes de enviar um sinal.".to_owned());
        };
        let Some(room) = self.rooms.get(code) else {
            self.connection_rooms.remove(&connection_id);
            return Err("A sala foi encerrada.".to_owned());
        };
        let peer = room
            .participants
            .iter()
            .find(|(peer_id, _)| *peer_id != connection_id)
            .map(|(peer_id, sender)| (*peer_id, sender.clone()));
        let Some((peer_id, peer)) = peer else {
            return Err("Aguardando o outro participante entrar na sala.".to_owned());
        };
        if peer
            .send(OutboundMessage::Protocol(ServerMessage::Signal {
                kind,
                payload,
            }))
            .is_err()
        {
            self.leave_room(peer_id);
            return Err("O outro participante desconectou.".to_owned());
        }
        Ok(())
    }

    fn leave_room(&mut self, connection_id: ConnectionId) -> bool {
        let Some(code) = self.connection_rooms.remove(&connection_id) else {
            return false;
        };

        let mut remove_room = false;
        if let Some(room) = self.rooms.get_mut(&code) {
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
        }
        true
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

    loop {
        let accepted = tokio::select! {
            _ = &mut shutdown => break,
            accepted = listener.accept() => accepted,
        };
        let (stream, address) = accepted?;
        let connection_id = next_connection_id.fetch_add(1, Ordering::Relaxed);
        let rooms = Arc::clone(&rooms);
        tokio::spawn(async move {
            handle_connection(stream, connection_id, rooms).await;
            println!("Cliente desconectado: {address}");
        });
        println!("Cliente conectado: {address}");
    }
    Ok(())
}

async fn handle_connection(stream: TcpStream, connection_id: ConnectionId, rooms: SharedRooms) {
    let websocket = match accept_async(stream).await {
        Ok(websocket) => websocket,
        Err(error) => {
            eprintln!("Falha ao abrir WebSocket: {error}");
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
    let (mut websocket_sender, mut websocket_receiver) = websocket.split();
    let (outgoing, mut outgoing_messages) = mpsc::unbounded_channel::<OutboundMessage>();

    let writer = tokio::spawn(async move {
        while let Some(outgoing) = outgoing_messages.recv().await {
            let message = match outgoing {
                OutboundMessage::Protocol(message) => match serde_json::to_string(&message) {
                    Ok(json) => WebSocketMessage::Text(json.into()),
                    Err(error) => {
                        eprintln!("Falha ao serializar resposta de sinalizacao: {error}");
                        continue;
                    }
                },
                OutboundMessage::Control(message) => message,
            };
            if websocket_sender.send(message).await.is_err() {
                break;
            }
        }
        let _ = websocket_sender.close().await;
    });

    while let Some(frame) = websocket_receiver.next().await {
        let Ok(frame) = frame else {
            break;
        };

        match frame {
            WebSocketMessage::Text(text) => {
                if text.len() > MAX_MESSAGE_BYTES {
                    send_error(&outgoing, "A mensagem excede o limite permitido.");
                    break;
                }
                match serde_json::from_str::<ClientMessage>(text.as_str()) {
                    Ok(message) => {
                        handle_client_message(connection_id, message, &outgoing, &rooms).await;
                    }
                    Err(error) => {
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
            WebSocketMessage::Close(_) => {
                let _ = outgoing.send(OutboundMessage::Control(WebSocketMessage::Close(None)));
                break;
            }
            WebSocketMessage::Binary(_) => {
                send_error(&outgoing, "Envie mensagens JSON em texto.");
            }
            WebSocketMessage::Pong(_) | WebSocketMessage::Frame(_) => {}
        }
    }

    rooms.lock().await.leave_room(connection_id);
    drop(outgoing);
    let _ = writer.await;
}

async fn handle_client_message(
    connection_id: ConnectionId,
    message: ClientMessage,
    outgoing: &Outgoing,
    rooms: &SharedRooms,
) {
    let result = match message {
        ClientMessage::CreateRoom => rooms
            .lock()
            .await
            .create_room(connection_id, outgoing.clone())
            .map(|_| ()),
        ClientMessage::JoinRoom { code } => {
            rooms
                .lock()
                .await
                .join_room(connection_id, &code, outgoing.clone())
        }
        ClientMessage::Signal { kind, payload } => {
            rooms
                .lock()
                .await
                .forward_signal(connection_id, kind, payload)
        }
        ClientMessage::LeaveRoom => {
            rooms.lock().await.leave_room(connection_id);
            send_message(outgoing, ServerMessage::RoomLeft);
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
        send_error(outgoing, &message);
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

    use super::{OutboundMessage, RoomRegistry, TransferReservation, handle_connection, serve};
    use signaling_protocol::{ClientMessage, ServerMessage, SignalKind};

    fn server_message(receiver: &mut mpsc::UnboundedReceiver<OutboundMessage>) -> ServerMessage {
        match receiver.try_recv().expect("expected a server message") {
            OutboundMessage::Protocol(message) => message,
            OutboundMessage::Control(_) => panic!("expected a protocol message"),
        }
    }

    #[test]
    fn rooms_have_two_slots_and_notify_both_participants() {
        let mut registry = RoomRegistry::default();
        let (first_tx, mut first_rx) = mpsc::unbounded_channel();
        let (second_tx, mut second_rx) = mpsc::unbounded_channel();
        let (third_tx, _third_rx) = mpsc::unbounded_channel();

        let code = registry.create_room(1, first_tx).unwrap();
        assert_eq!(code.len(), 8);
        assert_eq!(
            server_message(&mut first_rx),
            ServerMessage::RoomCreated { code: code.clone() }
        );

        registry.join_room(2, &code, second_tx).unwrap();
        assert_eq!(server_message(&mut first_rx), ServerMessage::PeerJoined);
        assert_eq!(
            server_message(&mut second_rx),
            ServerMessage::RoomJoined { code: code.clone() }
        );
        assert!(registry.join_room(3, &code, third_tx).is_err());
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
            .forward_signal(1, SignalKind::Diagnostic, "ping".to_owned())
            .unwrap();
        assert_eq!(
            server_message(&mut second_rx),
            ServerMessage::Signal {
                kind: SignalKind::Diagnostic,
                payload: "ping".to_owned(),
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
        assert!(matches!(
            receive_server_message(&mut third).await,
            ServerMessage::Error { message } if message.contains("cheia")
        ));
        third.close(None).await.unwrap();

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
            },
        )
        .await;
        assert_eq!(
            receive_server_message(&mut second).await,
            ServerMessage::Signal {
                kind: SignalKind::Diagnostic,
                payload: "diagnostic-ping-v1".to_owned(),
            }
        );
        send_client_message(
            &mut second,
            ClientMessage::Signal {
                kind: SignalKind::Diagnostic,
                payload: "diagnostic-pong-v1".to_owned(),
            },
        )
        .await;
        assert_eq!(
            receive_server_message(&mut first).await,
            ServerMessage::Signal {
                kind: SignalKind::Diagnostic,
                payload: "diagnostic-pong-v1".to_owned(),
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
