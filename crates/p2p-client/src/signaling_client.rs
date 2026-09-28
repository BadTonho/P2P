use std::sync::mpsc as std_mpsc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures_util::{SinkExt, StreamExt};
use signaling_protocol::{ClientMessage, ParticipantInfo, RoomMode, ServerMessage, SignalKind};
use signaling_server::TransferReservation;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot};
use tokio::time::timeout;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message as WebSocketMessage;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const LOCAL_SERVER_ADDRESS: &str = "0.0.0.0:9000";
const LOCAL_CLIENT_URL: &str = "ws://127.0.0.1:9000";
const DIAGNOSTIC_PAYLOAD: &str = "diagnostic-ping-v1";
const DIAGNOSTIC_ACK_PAYLOAD: &str = "diagnostic-pong-v1";

fn default_participant() -> ParticipantInfo {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    ParticipantInfo {
        id: format!("{}-{nonce:x}", std::process::id()),
        display_name: "Participante".to_owned(),
        order: 0,
        may_host: false,
        control_address: String::new(),
    }
}

#[derive(Debug)]
pub enum SignalingEvent {
    RoomCreated(String),
    RoomJoined(String),
    RoomAdopted(String),
    PeerJoined,
    PeerLeft,
    RoomRoster {
        participants: Vec<ParticipantInfo>,
        leader_id: String,
        room_mode: RoomMode,
    },
    HostTransferPending {
        code: String,
        token: String,
    },
    HostTransferRequested {
        code: String,
        token: String,
    },
    HostTransferComplete(String),
    HostTransferCanceled(String),
    ServerError(String),
    Signal {
        kind: SignalKind,
        payload: String,
    },
    Error(String),
    Disconnected,
}

enum ClientCommand {
    SendDiagnostic,
    AcknowledgeDiagnostic,
    SendSignal { kind: SignalKind, payload: String },
    RequestHostTransfer,
    CancelHostTransfer { token: String },
    ConfirmHostTransfer { code: String, token: String },
    RejectHostTransfer { token: String },
    Leave,
}

pub struct SignalingClient {
    commands: mpsc::UnboundedSender<ClientCommand>,
    events: std_mpsc::Receiver<SignalingEvent>,
    _worker: JoinHandle<()>,
}

impl SignalingClient {
    pub fn start_host_with_participant(
        participant: ParticipantInfo,
        room_mode: RoomMode,
    ) -> Result<Self, String> {
        Self::start_worker(
            LOCAL_CLIENT_URL.to_owned(),
            ClientMessage::CreateRoomIdentified {
                participant: participant.clone(),
                room_mode,
            },
            Some(None),
            LOCAL_SERVER_ADDRESS.to_owned(),
            participant,
        )
    }

    #[cfg(test)]
    fn start_host_at(listen_address: &str) -> Result<Self, String> {
        Self::start_worker(
            LOCAL_CLIENT_URL.to_owned(),
            ClientMessage::CreateRoom,
            Some(None),
            listen_address.to_owned(),
            default_participant(),
        )
    }

    pub fn join_with_participant(
        server_url: String,
        code: String,
        participant: ParticipantInfo,
    ) -> Result<Self, String> {
        Self::start_worker(
            server_url,
            ClientMessage::JoinRoomIdentified {
                code,
                participant: participant.clone(),
            },
            None,
            LOCAL_SERVER_ADDRESS.to_owned(),
            participant,
        )
    }

    pub fn adopt_transfer(code: String, token: String) -> Result<Self, String> {
        Self::start_worker(
            LOCAL_CLIENT_URL.to_owned(),
            ClientMessage::AdoptTransferredRoom {
                code: code.clone(),
                token: token.clone(),
            },
            Some(Some(TransferReservation {
                code,
                token,
                participants: Vec::new(),
            })),
            LOCAL_SERVER_ADDRESS.to_owned(),
            default_participant(),
        )
    }

    pub fn start_elected_host(
        code: String,
        participant: ParticipantInfo,
        participants: Vec<ParticipantInfo>,
    ) -> Result<Self, String> {
        let token = format!(
            "election-{:x}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        );
        Self::start_worker(
            LOCAL_CLIENT_URL.to_owned(),
            ClientMessage::AdoptTransferredRoom {
                code: code.clone(),
                token: token.clone(),
            },
            Some(Some(TransferReservation {
                code,
                token,
                participants,
            })),
            LOCAL_SERVER_ADDRESS.to_owned(),
            participant,
        )
    }

    fn start_worker(
        server_url: String,
        initial_message: ClientMessage,
        local_server: Option<Option<TransferReservation>>,
        local_server_address: String,
        participant: ParticipantInfo,
    ) -> Result<Self, String> {
        let (command_tx, command_rx) = mpsc::unbounded_channel();
        let (event_tx, event_rx) = std_mpsc::channel();
        let worker = thread::Builder::new()
            .name("p2p-signaling-client".to_owned())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        let _ = event_tx.send(SignalingEvent::Error(format!(
                            "Nao foi possivel iniciar o runtime de rede: {error}"
                        )));
                        return;
                    }
                };
                runtime.block_on(run_client(
                    server_url,
                    initial_message,
                    local_server,
                    local_server_address,
                    participant,
                    command_rx,
                    event_tx,
                ));
            })
            .map_err(|error| format!("Nao foi possivel iniciar a conexao: {error}"))?;

        Ok(Self {
            commands: command_tx,
            events: event_rx,
            _worker: worker,
        })
    }

    pub fn send_diagnostic(&self) -> Result<(), String> {
        self.commands
            .send(ClientCommand::SendDiagnostic)
            .map_err(|_| "A conexao com o servidor foi encerrada.".to_owned())
    }

    pub fn acknowledge_diagnostic(&self) -> Result<(), String> {
        self.commands
            .send(ClientCommand::AcknowledgeDiagnostic)
            .map_err(|_| "A conexao com o servidor foi encerrada.".to_owned())
    }

    pub fn send_signal(&self, kind: SignalKind, payload: String) -> Result<(), String> {
        self.commands
            .send(ClientCommand::SendSignal { kind, payload })
            .map_err(|_| "A conexao com o servidor foi encerrada.".to_owned())
    }

    pub fn request_host_transfer(&self) -> Result<(), String> {
        self.commands
            .send(ClientCommand::RequestHostTransfer)
            .map_err(|_| "A conexao com o servidor foi encerrada.".to_owned())
    }

    pub fn cancel_host_transfer(&self, token: String) -> Result<(), String> {
        self.commands
            .send(ClientCommand::CancelHostTransfer { token })
            .map_err(|_| "A conexao com o servidor foi encerrada.".to_owned())
    }

    pub fn confirm_host_transfer(&self, code: String, token: String) -> Result<(), String> {
        self.commands
            .send(ClientCommand::ConfirmHostTransfer { code, token })
            .map_err(|_| "A conexao com o servidor foi encerrada.".to_owned())
    }

    pub fn reject_host_transfer(&self, token: String) -> Result<(), String> {
        self.commands
            .send(ClientCommand::RejectHostTransfer { token })
            .map_err(|_| "A conexao com o servidor foi encerrada.".to_owned())
    }

    pub fn try_recv(&self) -> Option<SignalingEvent> {
        self.events.try_recv().ok()
    }
}

impl Drop for SignalingClient {
    fn drop(&mut self) {
        let _ = self.commands.send(ClientCommand::Leave);
    }
}

async fn run_client(
    server_url: String,
    initial_message: ClientMessage,
    local_server: Option<Option<TransferReservation>>,
    local_server_address: String,
    participant: ParticipantInfo,
    mut commands: mpsc::UnboundedReceiver<ClientCommand>,
    events: std_mpsc::Sender<SignalingEvent>,
) {
    let mut server_shutdown = None;
    let mut server_task = None;
    if let Some(reservation) = local_server {
        tracing::info!(listen_address = %local_server_address, "Iniciando servidor de sinalização integrado");
        let listener = match TcpListener::bind(local_server_address).await {
            Ok(listener) => listener,
            Err(error) => {
                tracing::error!(error = %error, "Falha ao abrir a porta TCP do servidor de sinalização");
                let _ = events.send(SignalingEvent::Error(format!(
                    "Nao foi possivel hospedar a sala na porta 9000: {error}. Feche outro servidor ou aplicativo que esteja usando essa porta."
                )));
                return;
            }
        };
        let (shutdown_sender, shutdown_receiver) = oneshot::channel();
        server_shutdown = Some(shutdown_sender);
        server_task = Some(tokio::spawn(signaling_server::serve(
            listener,
            shutdown_receiver,
            reservation,
        )));
    }

    if !server_url.starts_with("ws://") {
        tracing::error!("Endereço de sinalização rejeitado: esquema inválido");
        let _ = events.send(SignalingEvent::Error(
            "Informe o servidor no formato ws://IP-DO-ANFITRIAO:9000.".to_owned(),
        ));
        stop_local_server(server_shutdown, server_task).await;
        return;
    }

    tracing::info!(endpoint = %server_url, "Conectando ao servidor WebSocket de sinalização");
    let connected = match timeout(CONNECT_TIMEOUT, connect_async(server_url.as_str())).await {
        Ok(Ok((websocket, _response))) => {
            tracing::info!(endpoint = %server_url, "Conexão WebSocket de sinalização estabelecida");
            websocket
        }
        Ok(Err(error)) => {
            tracing::error!(endpoint = %server_url, error = %error, "Falha ao conectar ao servidor de sinalização");
            let _ = events.send(SignalingEvent::Error(format!(
                "Nao foi possivel chegar ao servidor de sinalizacao TCP 9000. Confira o endereco, o encaminhamento da porta no roteador e o firewall do anfitriao: {error}"
            )));
            stop_local_server(server_shutdown, server_task).await;
            return;
        }
        Err(_) => {
            tracing::error!(endpoint = %server_url, timeout_seconds = CONNECT_TIMEOUT.as_secs(), "Tempo limite ao conectar ao servidor de sinalização");
            let _ = events.send(SignalingEvent::Error(
                "A conexao ao servidor de sinalizacao TCP 9000 expirou. Confira o endereco, o encaminhamento da porta no roteador e o firewall do anfitriao.".to_owned(),
            ));
            stop_local_server(server_shutdown, server_task).await;
            return;
        }
    };

    let (mut writer, mut reader) = connected.split();
    let request = match serde_json::to_string(&initial_message) {
        Ok(request) => request,
        Err(error) => {
            tracing::error!(error = %error, "Falha ao serializar pedido de entrada/criação da sala");
            let _ = events.send(SignalingEvent::Error(format!(
                "Nao foi possivel preparar o pedido da sala: {error}"
            )));
            stop_local_server(server_shutdown, server_task).await;
            return;
        }
    };
    if let Err(error) = writer.send(WebSocketMessage::Text(request.into())).await {
        tracing::error!(error = %error, "Falha ao enviar pedido inicial de sinalização");
        let _ = events.send(SignalingEvent::Error(format!(
            "Falha ao enviar o pedido da sala: {error}"
        )));
        stop_local_server(server_shutdown, server_task).await;
        return;
    }

    let mut failed = false;
    let mut transfer_complete = false;
    loop {
        tokio::select! {
            incoming = reader.next() => {
                match incoming {
                    Some(Ok(WebSocketMessage::Text(text))) => {
                        match serde_json::from_str::<ServerMessage>(text.as_str()) {
                            Ok(ServerMessage::RoomCreated { code }) => {
                                tracing::info!("Servidor confirmou criação da sala; código omitido");
                                let _ = send_client_message(&mut writer, ClientMessage::IdentifyParticipant { participant: participant.clone() }).await;
                                let _ = events.send(SignalingEvent::RoomCreated(code));
                            }
                            Ok(ServerMessage::RoomJoined { code }) => {
                                tracing::info!("Servidor confirmou entrada na sala; código omitido");
                                let _ = send_client_message(&mut writer, ClientMessage::IdentifyParticipant { participant: participant.clone() }).await;
                                let _ = events.send(SignalingEvent::RoomJoined(code));
                            }
                            Ok(ServerMessage::RoomAdopted { code }) => {
                                tracing::info!("Servidor confirmou adoção de sala; código omitido");
                                let _ = send_client_message(&mut writer, ClientMessage::IdentifyParticipant { participant: participant.clone() }).await;
                                let _ = events.send(SignalingEvent::RoomAdopted(code));
                            }
                            Ok(ServerMessage::PeerJoined) => {
                                tracing::info!("Servidor notificou entrada de outro participante");
                                let _ = events.send(SignalingEvent::PeerJoined);
                            }
                            Ok(ServerMessage::PeerLeft) => {
                                tracing::warn!("Servidor notificou saída de outro participante");
                                let _ = events.send(SignalingEvent::PeerLeft);
                            }
                            Ok(ServerMessage::HostTransferPending { code, token }) => {
                                tracing::info!("Servidor confirmou pedido de transferência; código e token omitidos");
                                let _ = events.send(SignalingEvent::HostTransferPending { code, token });
                            }
                            Ok(ServerMessage::HostTransferRequested { code, token }) => {
                                tracing::info!("Servidor enviou pedido de transferência; código e token omitidos");
                                let _ = events.send(SignalingEvent::HostTransferRequested { code, token });
                            }
                            Ok(ServerMessage::HostTransferComplete { code }) => {
                                tracing::info!("Servidor confirmou transferência concluída; código omitido");
                                let _ = events.send(SignalingEvent::HostTransferComplete(code));
                                transfer_complete = true;
                                break;
                            }
                            Ok(ServerMessage::HostTransferCanceled { message }) => {
                                tracing::warn!(reason = %message, "Servidor cancelou a transferência de hospedagem");
                                let _ = events.send(SignalingEvent::HostTransferCanceled(message));
                            }
                            Ok(ServerMessage::Signal { kind, payload }) => {
                                tracing::debug!(signal_kind = ?kind, payload_bytes = payload.len(), "Sinal recebido; payload omitido");
                                let _ = events.send(SignalingEvent::Signal { kind, payload });
                            }
                            Ok(ServerMessage::RoomRoster { participants, leader_id, room_mode }) => {
                                tracing::info!(participants = participants.len(), room_mode = ?room_mode, "Servidor atualizou lista de participantes");
                                let _ = events.send(SignalingEvent::RoomRoster { participants, leader_id, room_mode });
                            }
                            Ok(ServerMessage::RoomLeft) => {
                                let _ = writer.send(WebSocketMessage::Close(None)).await;
                                let _ = writer.close().await;
                                break;
                            }
                            Ok(ServerMessage::Error { message }) => {
                                tracing::warn!(reason = %message, "Servidor recusou operação de sinalização");
                                let _ = events.send(SignalingEvent::ServerError(message));
                            }
                            Err(error) => {
                                tracing::error!(error = %error, "Resposta JSON inválida do servidor");
                                let _ = events.send(SignalingEvent::Error(format!(
                                    "Resposta invalida do servidor: {error}"
                                )));
                                failed = true;
                                break;
                            }
                        }
                    }
                    Some(Ok(WebSocketMessage::Ping(payload))) => {
                        if writer.send(WebSocketMessage::Pong(payload)).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(WebSocketMessage::Close(_))) => {
                        tracing::warn!("Servidor encerrou a conexão WebSocket");
                        let _ = writer.send(WebSocketMessage::Close(None)).await;
                        let _ = writer.close().await;
                        break;
                    }
                    None => {
                        tracing::warn!("Fluxo WebSocket terminou sem mensagem de encerramento");
                        break;
                    }
                    Some(Err(error)) => {
                        tracing::error!(error = %error, "Erro no fluxo WebSocket de sinalização");
                        break;
                    }
                    Some(Ok(WebSocketMessage::Binary(_))) => {
                        tracing::error!("Servidor enviou formato binário inesperado");
                        let _ = events.send(SignalingEvent::Error(
                            "O servidor enviou uma mensagem em formato inesperado.".to_owned()
                        ));
                        failed = true;
                        break;
                    }
                    Some(Ok(WebSocketMessage::Pong(_))) | Some(Ok(WebSocketMessage::Frame(_))) => {}
                }
            }
            command = commands.recv() => {
                match command {
                    Some(ClientCommand::SendDiagnostic) => {
                        tracing::debug!("Enviando sinal de diagnóstico sem dados pessoais");
                        if let Err(error) = send_signal(&mut writer, SignalKind::Diagnostic, DIAGNOSTIC_PAYLOAD).await {
                            let _ = events.send(SignalingEvent::Error(error));
                            failed = true;
                            break;
                        }
                    }
                    Some(ClientCommand::AcknowledgeDiagnostic) => {
                        if let Err(error) = send_signal(&mut writer, SignalKind::Diagnostic, DIAGNOSTIC_ACK_PAYLOAD).await {
                            let _ = events.send(SignalingEvent::Error(error));
                            failed = true;
                            break;
                        }
                    }
                    Some(ClientCommand::SendSignal { kind, payload }) => {
                        tracing::debug!(signal_kind = ?kind, payload_bytes = payload.len(), "Enviando sinal; conteúdo omitido");
                        if let Err(error) = send_signal(&mut writer, kind, &payload).await {
                            let _ = events.send(SignalingEvent::Error(error));
                            failed = true;
                            break;
                        }
                    }
                    Some(ClientCommand::RequestHostTransfer) => {
                        tracing::info!("Solicitando transferência de hospedagem");
                        if let Err(error) = send_client_message(&mut writer, ClientMessage::RequestHostTransfer).await {
                            let _ = events.send(SignalingEvent::Error(error));
                            failed = true;
                            break;
                        }
                    }
                    Some(ClientCommand::CancelHostTransfer { token }) => {
                        tracing::info!("Cancelando transferência de hospedagem; token omitido");
                        if let Err(error) = send_client_message(&mut writer, ClientMessage::CancelHostTransfer { token }).await {
                            let _ = events.send(SignalingEvent::Error(error));
                            failed = true;
                            break;
                        }
                    }
                    Some(ClientCommand::ConfirmHostTransfer { code, token }) => {
                        tracing::info!("Confirmando transferência de hospedagem; código e token omitidos");
                        let message = ClientMessage::ConfirmHostTransfer { code, token };
                        if let Err(error) = send_client_message(&mut writer, message).await {
                            let _ = events.send(SignalingEvent::Error(error));
                            failed = true;
                            break;
                        }
                    }
                    Some(ClientCommand::RejectHostTransfer { token }) => {
                        tracing::warn!("Recusando transferência de hospedagem; token omitido");
                        let message = ClientMessage::RejectHostTransfer { token };
                        if let Err(error) = send_client_message(&mut writer, message).await {
                            let _ = events.send(SignalingEvent::Error(error));
                            failed = true;
                            break;
                        }
                    }
                    Some(ClientCommand::Leave) | None => {
                        tracing::info!("Enviando saída da sala e fechando conexão WebSocket");
                        let _ = send_client_message(&mut writer, ClientMessage::LeaveRoom).await;
                        let close_handshake = timeout(Duration::from_secs(3), async {
                            while let Some(message) = reader.next().await {
                                match message {
                                    Ok(WebSocketMessage::Text(text)) => {
                                        if matches!(
                                            serde_json::from_str::<ServerMessage>(text.as_str()),
                                            Ok(ServerMessage::RoomLeft)
                                        ) {
                                            tracing::debug!("Servidor confirmou a saída antes do fechamento WebSocket");
                                        }
                                    }
                                    Ok(WebSocketMessage::Close(_)) => {
                                        let _ = writer.send(WebSocketMessage::Close(None)).await;
                                        break;
                                    }
                                    Ok(WebSocketMessage::Ping(payload)) => {
                                        if writer.send(WebSocketMessage::Pong(payload)).await.is_err() {
                                            break;
                                        }
                                    }
                                    Err(error) => {
                                        tracing::debug!(error = %error, "Fluxo terminou durante o fechamento WebSocket");
                                        break;
                                    }
                                    _ => {}
                                }
                            }
                        })
                        .await;
                        if close_handshake.is_err() {
                            tracing::warn!("O fechamento WebSocket excedeu o prazo; encerrando a conexão localmente");
                        }
                        let _ = writer.close().await;
                        break;
                    }
                }
            }
        }
    }

    if !failed && !transfer_complete {
        tracing::warn!("Cliente de sinalização desconectado sem transferência concluída");
        let _ = events.send(SignalingEvent::Disconnected);
    }
    tracing::debug!(
        failed,
        transfer_complete,
        "Encerrando tarefas do servidor integrado"
    );
    stop_local_server(server_shutdown, server_task).await;
}

async fn send_client_message<W>(writer: &mut W, message: ClientMessage) -> Result<(), String>
where
    W: futures_util::Sink<WebSocketMessage> + Unpin,
    W::Error: std::fmt::Display,
{
    let json = serde_json::to_string(&message)
        .map_err(|error| format!("Nao foi possivel preparar a mensagem: {error}"))?;
    writer
        .send(WebSocketMessage::Text(json.into()))
        .await
        .map_err(|error| format!("Falha ao enviar a mensagem: {error}"))
}

async fn send_signal<W>(writer: &mut W, kind: SignalKind, payload: &str) -> Result<(), String>
where
    W: futures_util::Sink<WebSocketMessage> + Unpin,
    W::Error: std::fmt::Display,
{
    send_client_message(
        writer,
        ClientMessage::Signal {
            kind,
            payload: payload.to_owned(),
        },
    )
    .await
}

async fn stop_local_server(
    shutdown: Option<oneshot::Sender<()>>,
    task: Option<tokio::task::JoinHandle<std::io::Result<()>>>,
) {
    if let Some(shutdown) = shutdown {
        let _ = shutdown.send(());
    }
    if let Some(task) = task {
        if timeout(Duration::from_secs(2), task).await.is_err() {
            // The runtime also cancels this task when the worker exits.
        }
    }
}

#[cfg(test)]
mod tests {
    use std::net::TcpListener;
    use std::thread;
    use std::time::{Duration, Instant};

    use super::{SignalingClient, SignalingEvent};

    #[test]
    fn hosting_reports_a_clear_error_when_the_port_is_occupied() {
        let occupied = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = occupied.local_addr().unwrap().to_string();
        let client = SignalingClient::start_host_at(&address).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);

        loop {
            if let Some(event) = client.try_recv() {
                assert!(matches!(
                    event,
                    SignalingEvent::Error(message) if message.contains("porta 9000")
                ));
                break;
            }
            assert!(Instant::now() < deadline, "the bind error was not reported");
            thread::sleep(Duration::from_millis(10));
        }
        drop(client);
        drop(occupied);
    }
}
