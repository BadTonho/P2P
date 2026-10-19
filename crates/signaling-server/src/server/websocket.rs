use super::*;
use std::io;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};

use futures_util::{SinkExt, StreamExt};
use signaling_protocol::ClientMessage;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, mpsc, oneshot, watch};
use tokio::time::{sleep, timeout};
use tokio_tungstenite::tungstenite::Message as WebSocketMessage;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::{WebSocketStream, accept_async_with_config};

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
            Err(error) if is_transient_accept_error(&error) => {
                tracing::error!(
                    error = %error,
                    "Listener de sinalização não pôde aceitar esta conexão; continuando o aceite"
                );
                sleep(TRANSIENT_ACCEPT_BACKOFF).await;
                continue;
            }
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

/// Erros de accept transitórios (peers que fecham a conexão antes de o accept
/// concluir, por exemplo) não podem derrubar todo o servidor de sinalização.
pub(in crate::server) fn is_transient_accept_error(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::TimedOut
            | io::ErrorKind::Interrupted
            | io::ErrorKind::WouldBlock
    )
}

pub(super) async fn spawn_admitted_connection(
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

    // Conservação da conexão: pings periódicos e encerramento quando nenhuma
    // resposta (Pong ou qualquer quadro) volta dentro do prazo. Conexões
    // meio-abertas (NAT, queda silenciosa) não ficam penduradas para sempre.
    let mut ping_pending = false;
    let mut last_inbound = tokio::time::Instant::now();
    let mut next_ping_at = last_inbound + keepalive_ping_interval();

    loop {
        let frame = tokio::select! {
            frame = websocket_receiver.next() => frame,
            _ = &mut writer => {
                writer_finished = true;
                break;
            }
            _ = tokio::time::sleep_until(next_ping_at) => {
                let now = tokio::time::Instant::now();
                let idle = now.duration_since(last_inbound);
                if idle > keepalive_max_idle() {
                    tracing::info!(
                        connection_id,
                        idle_secs = idle.as_secs(),
                        "Conexão ociosa sem resposta aos pings; encerrando"
                    );
                    send_error(&outgoing, "Conexão encerrada por inatividade.");
                    break;
                }
                if !ping_pending {
                    ping_pending = true;
                    let _ = outgoing.send(OutboundMessage::Control(
                        WebSocketMessage::Ping(Vec::new().into()),
                    ));
                }
                next_ping_at = now + keepalive_ping_interval();
                continue;
            }
        };
        let Some(frame) = frame else {
            break;
        };
        let frame = match frame {
            Ok(frame) => {
                last_inbound = tokio::time::Instant::now();
                ping_pending = false;
                frame
            }
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

pub(super) async fn write_outgoing_messages<S>(
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
