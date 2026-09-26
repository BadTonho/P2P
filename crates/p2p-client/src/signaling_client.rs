use std::sync::mpsc as std_mpsc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use signaling_protocol::{ClientMessage, ServerMessage, SignalKind};
use tokio::sync::mpsc;
use tokio::time::timeout;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message as WebSocketMessage;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const DIAGNOSTIC_PAYLOAD: &str = "diagnostic-ping-v1";
const DIAGNOSTIC_ACK_PAYLOAD: &str = "diagnostic-pong-v1";

#[derive(Clone, Debug)]
pub enum RoomAction {
    Create,
    Join(String),
}

#[derive(Debug)]
pub enum SignalingEvent {
    RoomCreated(String),
    RoomJoined(String),
    PeerJoined,
    PeerLeft,
    Signal { kind: SignalKind, payload: String },
    Error(String),
    Disconnected,
}

enum ClientCommand {
    SendDiagnostic,
    AcknowledgeDiagnostic,
    Leave,
}

pub struct SignalingClient {
    commands: mpsc::UnboundedSender<ClientCommand>,
    events: std_mpsc::Receiver<SignalingEvent>,
    _worker: JoinHandle<()>,
}

impl SignalingClient {
    pub fn start(server_url: String, action: RoomAction) -> Result<Self, String> {
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
                runtime.block_on(run_client(server_url, action, command_rx, event_tx));
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
    action: RoomAction,
    mut commands: mpsc::UnboundedReceiver<ClientCommand>,
    events: std_mpsc::Sender<SignalingEvent>,
) {
    if !server_url.starts_with("ws://") {
        let _ = events.send(SignalingEvent::Error(
            "Informe o servidor no formato ws://IP-DO-NOTEBOOK:9000.".to_owned(),
        ));
        return;
    }

    let connected = match timeout(CONNECT_TIMEOUT, connect_async(server_url.as_str())).await {
        Ok(Ok((websocket, _response))) => websocket,
        Ok(Err(error)) => {
            let _ = events.send(SignalingEvent::Error(format!(
                "Nao foi possivel conectar ao servidor: {error}"
            )));
            return;
        }
        Err(_) => {
            let _ = events.send(SignalingEvent::Error(
                "A conexao expirou. Confira o IP, a porta e o firewall do notebook.".to_owned(),
            ));
            return;
        }
    };

    let (mut writer, mut reader) = connected.split();
    let request = match action {
        RoomAction::Create => ClientMessage::CreateRoom,
        RoomAction::Join(code) => ClientMessage::JoinRoom { code },
    };
    let request = match serde_json::to_string(&request) {
        Ok(request) => request,
        Err(error) => {
            let _ = events.send(SignalingEvent::Error(format!(
                "Nao foi possivel preparar o pedido da sala: {error}"
            )));
            return;
        }
    };
    if let Err(error) = writer.send(WebSocketMessage::Text(request.into())).await {
        let _ = events.send(SignalingEvent::Error(format!(
            "Falha ao enviar o pedido da sala: {error}"
        )));
        return;
    }

    let mut failed = false;
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
                            Ok(ServerMessage::PeerJoined) => {
                                let _ = events.send(SignalingEvent::PeerJoined);
                            }
                            Ok(ServerMessage::PeerLeft) => {
                                let _ = events.send(SignalingEvent::PeerLeft);
                            }
                            Ok(ServerMessage::Signal { kind, payload }) => {
                                let _ = events.send(SignalingEvent::Signal { kind, payload });
                            }
                            Ok(ServerMessage::RoomLeft) => break,
                            Ok(ServerMessage::Error { message }) => {
                                let _ = events.send(SignalingEvent::Error(message));
                                failed = true;
                                break;
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
                    Some(command @ (ClientCommand::SendDiagnostic | ClientCommand::AcknowledgeDiagnostic)) => {
                        let payload = match command {
                            ClientCommand::SendDiagnostic => DIAGNOSTIC_PAYLOAD,
                            ClientCommand::AcknowledgeDiagnostic => DIAGNOSTIC_ACK_PAYLOAD,
                            ClientCommand::Leave => unreachable!(),
                        };
                        let message = ClientMessage::Signal {
                            kind: SignalKind::Diagnostic,
                            payload: payload.to_owned(),
                        };
                        match serde_json::to_string(&message) {
                            Ok(json) => {
                                if writer.send(WebSocketMessage::Text(json.into())).await.is_err() {
                                    break;
                                }
                            }
                            Err(error) => {
                                let _ = events.send(SignalingEvent::Error(format!(
                                    "Nao foi possivel preparar o teste de sinalizacao: {error}"
                                )));
                                failed = true;
                                break;
                            }
                        }
                    }
                    Some(ClientCommand::Leave) | None => {
                        if let Ok(json) = serde_json::to_string(&ClientMessage::LeaveRoom) {
                            let _ = writer.send(WebSocketMessage::Text(json.into())).await;
                        }
                        let _ = writer.close().await;
                        break;
                    }
                }
            }
        }
    }

    if !failed {
        let _ = events.send(SignalingEvent::Disconnected);
    }
}
