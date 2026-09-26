use std::sync::mpsc as std_mpsc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use signaling_protocol::{ClientMessage, ServerMessage, SignalKind};
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

#[derive(Debug)]
pub enum SignalingEvent {
    RoomCreated(String),
    RoomJoined(String),
    RoomAdopted(String),
    PeerJoined,
    PeerLeft,
    HostTransferPending { code: String, token: String },
    HostTransferRequested { code: String, token: String },
    HostTransferComplete(String),
    HostTransferCanceled(String),
    ServerError(String),
    Signal { kind: SignalKind, payload: String },
    Error(String),
    Disconnected,
}

enum ClientCommand {
    SendDiagnostic,
    AcknowledgeDiagnostic,
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
    pub fn start_host() -> Result<Self, String> {
        Self::start_host_at(LOCAL_SERVER_ADDRESS)
    }

    fn start_host_at(listen_address: &str) -> Result<Self, String> {
        Self::start_worker(
            LOCAL_CLIENT_URL.to_owned(),
            ClientMessage::CreateRoom,
            Some(None),
            listen_address.to_owned(),
        )
    }

    pub fn join(server_url: String, code: String) -> Result<Self, String> {
        Self::start_worker(
            server_url,
            ClientMessage::JoinRoom { code },
            None,
            LOCAL_SERVER_ADDRESS.to_owned(),
        )
    }

    pub fn adopt_transfer(code: String, token: String) -> Result<Self, String> {
        Self::start_worker(
            LOCAL_CLIENT_URL.to_owned(),
            ClientMessage::AdoptTransferredRoom {
                code: code.clone(),
                token: token.clone(),
            },
            Some(Some(TransferReservation { code, token })),
            LOCAL_SERVER_ADDRESS.to_owned(),
        )
    }

    fn start_worker(
        server_url: String,
        initial_message: ClientMessage,
        local_server: Option<Option<TransferReservation>>,
        local_server_address: String,
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
    mut commands: mpsc::UnboundedReceiver<ClientCommand>,
    events: std_mpsc::Sender<SignalingEvent>,
) {
    let mut server_shutdown = None;
    let mut server_task = None;
    if let Some(reservation) = local_server {
        let listener = match TcpListener::bind(local_server_address).await {
            Ok(listener) => listener,
            Err(error) => {
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
        let _ = events.send(SignalingEvent::Error(
            "Informe o servidor no formato ws://IP-DO-ANFITRIAO:9000.".to_owned(),
        ));
        stop_local_server(server_shutdown, server_task).await;
        return;
    }

    let connected = match timeout(CONNECT_TIMEOUT, connect_async(server_url.as_str())).await {
        Ok(Ok((websocket, _response))) => websocket,
        Ok(Err(error)) => {
            let _ = events.send(SignalingEvent::Error(format!(
                "Nao foi possivel conectar ao servidor: {error}"
            )));
            stop_local_server(server_shutdown, server_task).await;
            return;
        }
        Err(_) => {
            let _ = events.send(SignalingEvent::Error(
                "A conexao expirou. Confira o IP, a porta e o firewall.".to_owned(),
            ));
            stop_local_server(server_shutdown, server_task).await;
            return;
        }
    };

    let (mut writer, mut reader) = connected.split();
    let request = match serde_json::to_string(&initial_message) {
        Ok(request) => request,
        Err(error) => {
            let _ = events.send(SignalingEvent::Error(format!(
                "Nao foi possivel preparar o pedido da sala: {error}"
            )));
            stop_local_server(server_shutdown, server_task).await;
            return;
        }
    };
    if let Err(error) = writer.send(WebSocketMessage::Text(request.into())).await {
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
                                let _ = events.send(SignalingEvent::RoomCreated(code));
                            }
                            Ok(ServerMessage::RoomJoined { code }) => {
                                let _ = events.send(SignalingEvent::RoomJoined(code));
                            }
                            Ok(ServerMessage::RoomAdopted { code }) => {
                                let _ = events.send(SignalingEvent::RoomAdopted(code));
                            }
                            Ok(ServerMessage::PeerJoined) => {
                                let _ = events.send(SignalingEvent::PeerJoined);
                            }
                            Ok(ServerMessage::PeerLeft) => {
                                let _ = events.send(SignalingEvent::PeerLeft);
                            }
                            Ok(ServerMessage::HostTransferPending { code, token }) => {
                                let _ = events.send(SignalingEvent::HostTransferPending { code, token });
                            }
                            Ok(ServerMessage::HostTransferRequested { code, token }) => {
                                let _ = events.send(SignalingEvent::HostTransferRequested { code, token });
                            }
                            Ok(ServerMessage::HostTransferComplete { code }) => {
                                let _ = events.send(SignalingEvent::HostTransferComplete(code));
                                transfer_complete = true;
                                break;
                            }
                            Ok(ServerMessage::HostTransferCanceled { message }) => {
                                let _ = events.send(SignalingEvent::HostTransferCanceled(message));
                            }
                            Ok(ServerMessage::Signal { kind, payload }) => {
                                let _ = events.send(SignalingEvent::Signal { kind, payload });
                            }
                            Ok(ServerMessage::RoomLeft) => break,
                            Ok(ServerMessage::Error { message }) => {
                                let _ = events.send(SignalingEvent::ServerError(message));
                            }
                            Err(error) => {
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
                    Some(Ok(WebSocketMessage::Close(_))) | None | Some(Err(_)) => break,
                    Some(Ok(WebSocketMessage::Binary(_))) => {
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
                    Some(ClientCommand::RequestHostTransfer) => {
                        if let Err(error) = send_client_message(&mut writer, ClientMessage::RequestHostTransfer).await {
                            let _ = events.send(SignalingEvent::Error(error));
                            failed = true;
                            break;
                        }
                    }
                    Some(ClientCommand::CancelHostTransfer { token }) => {
                        if let Err(error) = send_client_message(&mut writer, ClientMessage::CancelHostTransfer { token }).await {
                            let _ = events.send(SignalingEvent::Error(error));
                            failed = true;
                            break;
                        }
                    }
                    Some(ClientCommand::ConfirmHostTransfer { code, token }) => {
                        let message = ClientMessage::ConfirmHostTransfer { code, token };
                        if let Err(error) = send_client_message(&mut writer, message).await {
                            let _ = events.send(SignalingEvent::Error(error));
                            failed = true;
                            break;
                        }
                    }
                    Some(ClientCommand::RejectHostTransfer { token }) => {
                        let message = ClientMessage::RejectHostTransfer { token };
                        if let Err(error) = send_client_message(&mut writer, message).await {
                            let _ = events.send(SignalingEvent::Error(error));
                            failed = true;
                            break;
                        }
                    }
                    Some(ClientCommand::Leave) | None => {
                        let _ = send_client_message(&mut writer, ClientMessage::LeaveRoom).await;
                        let _ = writer.close().await;
                        break;
                    }
                }
            }
        }
    }

    if !failed && !transfer_complete {
        let _ = events.send(SignalingEvent::Disconnected);
    }
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
