use std::collections::HashMap;
use std::error::Error;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use futures_util::{SinkExt, StreamExt};
use signaling_protocol::{ClientMessage, ServerMessage};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, mpsc};
use tokio_tungstenite::tungstenite::Message as WebSocketMessage;
use tokio_tungstenite::{WebSocketStream, accept_async};

const LISTEN_ADDRESS: &str = "0.0.0.0:9000";
const MAX_MESSAGE_BYTES: usize = 64 * 1024;

type ConnectionId = u64;
type Outgoing = mpsc::UnboundedSender<OutboundMessage>;
type SharedRooms = Arc<Mutex<RoomRegistry>>;

enum OutboundMessage {
    Protocol(ServerMessage),
    Control(WebSocketMessage),
}

#[derive(Default)]
struct RoomRegistry {
    rooms: HashMap<String, Room>,
    connection_rooms: HashMap<ConnectionId, String>,
}

#[derive(Default)]
struct Room {
    participants: Vec<(ConnectionId, Outgoing)>,
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

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let listener = TcpListener::bind(LISTEN_ADDRESS).await?;
    let rooms = Arc::new(Mutex::new(RoomRegistry::default()));
    let next_connection_id = Arc::new(AtomicU64::new(1));

    println!("Servidor de sinalizacao escutando em {LISTEN_ADDRESS}.");
    println!("No Windows, use ipconfig para encontrar o IPv4 do notebook.");
    println!("Configure os clientes com ws://<IP-IPv4-DO-NOTEBOOK>:9000.");

    loop {
        let (stream, address) = listener.accept().await?;
        let connection_id = next_connection_id.fetch_add(1, Ordering::Relaxed);
        let rooms = Arc::clone(&rooms);
        tokio::spawn(async move {
            handle_connection(stream, connection_id, rooms).await;
            println!("Cliente desconectado: {address}");
        });
        println!("Cliente conectado: {address}");
    }
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
    use tokio::sync::{Mutex, mpsc};
    use tokio::time::timeout;
    use tokio_tungstenite::tungstenite::Message as WebSocketMessage;
    use tokio_tungstenite::{WebSocketStream, connect_async};

    use super::{OutboundMessage, RoomRegistry, handle_connection};
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
