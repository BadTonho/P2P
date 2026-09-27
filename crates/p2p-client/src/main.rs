#![windows_subsystem = "windows"]

mod audio_capture;
mod control_mesh;
mod logging;
mod screen_capture;
mod screen_sharing;
mod signaling_client;
mod update;

use std::net::Ipv4Addr;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use audio_capture::MicrophoneTest;
use control_mesh::{ControlEvent, ControlMesh, QueueEntry};
use eframe::egui;
use logging::{DiagnosticSnapshot, LoggingState, safe_signaling_endpoint, safe_stun_endpoint};
use screen_capture::{PendingScreenCapture, ScreenCapture};
use screen_sharing::{ScreenShareEvent, ScreenShareMetrics, ScreenShareSession};
use signaling_client::{SignalingClient, SignalingEvent};
use signaling_protocol::{ParticipantInfo, RoomMode, SignalKind};
use update::{UpdateEvent, UpdateManager, UpdateManifest};

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum SettingsCategory {
    #[default]
    Audio,
    Connection,
    Updates,
}

#[derive(Clone, Default)]
enum UpdateStatus {
    #[default]
    Checking,
    Unconfigured,
    UpToDate,
    Available(UpdateManifest),
    Downloading {
        manifest: UpdateManifest,
        received: u64,
    },
    CancellingDownload(UpdateManifest),
    Downloaded {
        manifest: UpdateManifest,
        path: std::path::PathBuf,
    },
    PreparingToApply,
    Applying,
    Failed(String),
}

#[derive(Default)]
struct ClientUi {
    room_code: Option<String>,
    join_code: String,
    code_copied: bool,
    settings_open: bool,
    settings_category: SettingsCategory,
    server_url: String,
    stun_server_url: String,
    create_room_mode: RoomMode,
    room_mode: RoomMode,
    connecting: bool,
    connection_status: Option<String>,
    connection_error: Option<String>,
    signaling: Option<SignalingClient>,
    pending_signaling: Option<SignalingClient>,
    pending_room_adopted: bool,
    hosting_locally: bool,
    starting_host: bool,
    incoming_transfer: Option<(String, String)>,
    outgoing_transfer: Option<(String, String)>,
    handoff_error: Option<String>,
    close_after_transfer: bool,
    allow_window_close: bool,
    host_addresses: Vec<HostAddress>,
    host_addresses_error: Option<String>,
    selected_host_address: usize,
    addresses_loaded: bool,
    participant_id: String,
    may_host: bool,
    participants: Vec<ParticipantInfo>,
    current_leader_id: String,
    control_mesh: Option<ControlMesh>,
    control_queue: Vec<QueueEntry>,
    control_status: Option<String>,
    control_mesh_failed: bool,
    pending_election_epoch: Option<u64>,
    pending_election_reconnect: bool,
    leave_after_handoff: bool,
    ending_room_explicitly: bool,
    peer_connected: bool,
    diagnostic_status: Option<String>,
    microphone: Option<MicrophoneTest>,
    microphone_level: f32,
    microphone_level_dbfs: f32,
    monitor_gain_db: f32,
    microphone_error: Option<String>,
    microphone_monitor_error: Option<String>,
    microphone_audio_warning: bool,
    microphone_clipping_warning: bool,
    screen_capture: Option<ScreenCapture>,
    screen_picker: Option<PendingScreenCapture>,
    screen_texture: Option<egui::TextureHandle>,
    screen_status: Option<String>,
    screen_share_session: Option<ScreenShareSession>,
    screen_share_role: ScreenShareRole,
    screen_share_status: Option<String>,
    remote_screen_texture: Option<egui::TextureHandle>,
    remote_screen_sequence: u64,
    screen_share_metrics: ScreenShareMetrics,
    logging: LoggingState,
    last_screen_metrics_log_at: Option<Instant>,
    last_control_link_state: Option<(usize, usize)>,
    last_control_metrics_log_at: Option<Instant>,
    last_audio_metrics_log_at: Option<Instant>,
    updates: UpdateManager,
    update_status: UpdateStatus,
}

#[derive(Clone, Default)]
enum ScreenShareRole {
    #[default]
    Idle,
    Requesting {
        request_id: String,
    },
    Sending {
        request_id: String,
    },
    Receiving {
        request_id: String,
    },
}

#[derive(serde::Deserialize, serde::Serialize)]
struct ScreenShareRequest {
    request_id: String,
    participant_id: String,
}

#[derive(Clone)]
struct HostAddress {
    adapter: String,
    ipv4: Ipv4Addr,
}

impl ClientUi {
    fn refresh_updates(&mut self, context: &egui::Context) {
        if self.room_code.is_some() {
            if let UpdateStatus::Downloading { manifest, .. } = &self.update_status {
                let manifest = manifest.clone();
                self.updates.cancel_download();
                self.update_status = UpdateStatus::CancellingDownload(manifest);
            }
        }

        while let Some(event) = self.updates.try_recv() {
            match event {
                UpdateEvent::CheckFinished(Ok(Some(manifest))) => {
                    self.update_status = UpdateStatus::Available(manifest);
                }
                UpdateEvent::CheckFinished(Ok(None)) => {
                    self.update_status = UpdateStatus::UpToDate;
                }
                UpdateEvent::CheckFinished(Err(error)) => {
                    self.update_status = if UpdateManager::is_configured() {
                        UpdateStatus::Failed(error)
                    } else {
                        UpdateStatus::Unconfigured
                    };
                }
                UpdateEvent::DownloadProgress {
                    version, received, ..
                } => {
                    if let UpdateStatus::Downloading {
                        manifest,
                        received: current,
                    } = &mut self.update_status
                    {
                        if manifest.version == version {
                            *current = received;
                        }
                    }
                }
                UpdateEvent::DownloadFinished { manifest, path } => {
                    self.update_status = UpdateStatus::Downloaded { manifest, path };
                }
                UpdateEvent::DownloadCancelled { version } => {
                    let manifest = match &self.update_status {
                        UpdateStatus::Downloading { manifest, .. }
                        | UpdateStatus::CancellingDownload(manifest)
                            if manifest.version == version =>
                        {
                            Some(manifest.clone())
                        }
                        _ => None,
                    };
                    if let Some(manifest) = manifest {
                        self.update_status = UpdateStatus::Available(manifest);
                    }
                }
                UpdateEvent::DownloadFailed { version, error } => {
                    self.update_status = UpdateStatus::Failed(format!(
                        "Falha ao baixar a versão {version}: {error}"
                    ));
                }
                UpdateEvent::ApplyStarted => {
                    self.update_status = UpdateStatus::Applying;
                    tracing::info!("Fechando o aplicativo para instalar a atualização");
                    context.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                UpdateEvent::ApplyFailed(error) => {
                    tracing::error!(error = %error, "Não foi possível preparar a atualização");
                    self.update_status = UpdateStatus::Failed(error);
                }
            }
        }
    }

    fn update_blocks_room_actions(&self) -> bool {
        matches!(
            &self.update_status,
            UpdateStatus::Downloading { .. }
                | UpdateStatus::CancellingDownload(_)
                | UpdateStatus::PreparingToApply
                | UpdateStatus::Applying
        )
    }

    fn show(&mut self, ui: &mut egui::Ui) {
        let context = ui.ctx().clone();
        if self.microphone.is_some()
            || self.screen_capture.is_some()
            || self.screen_picker.is_some()
            || self.screen_share_session.is_some()
            || self.connecting
            || self.signaling.is_some()
            || self.control_mesh.is_some()
            || matches!(
                &self.update_status,
                UpdateStatus::Checking
                    | UpdateStatus::Downloading { .. }
                    | UpdateStatus::CancellingDownload(_)
                    | UpdateStatus::PreparingToApply
                    | UpdateStatus::Applying
            )
        {
            ui.ctx().request_repaint_after(Duration::from_millis(100));
        }
        self.refresh_microphone();
        self.refresh_screen(ui.ctx());
        self.refresh_signaling(ui.ctx());
        self.refresh_screen_share(ui.ctx());
        self.refresh_control_mesh(ui.ctx());
        self.refresh_updates(ui.ctx());
        self.handle_window_close(ui.ctx());

        egui::ScrollArea::vertical().show(ui, |ui| {
            let mut open_settings = false;
            let mut open_update_settings = false;
            let mut close_settings = false;
            let mut export_logs = false;
            let settings_open = self.settings_open;

            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.add_space(16.0);
                    ui.heading("P2P - Voz e tela");
                    ui.label("Salas locais ou teste pela internet; compartilhamento de tela P2P, sem áudio");
                });

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    export_logs = ui.button("Exportar logs").clicked();
                    if settings_open {
                        close_settings = ui.button("Voltar").clicked();
                    } else {
                        open_settings = ui
                            .add_enabled(
                                self.screen_picker.is_none(),
                                egui::Button::new("Configurações"),
                            )
                            .clicked();
                    }
                });
            });

            if export_logs {
                self.export_logs();
            }

            if let Some(error) = self.logging.take_write_error() {
                self.logging.export_message = Some(format!(
                    "Falha ao gravar logs: {error}. Confira a pasta de logs abaixo."
                ));
            }
            if let Some(message) = &self.logging.startup_message {
                ui.colored_label(egui::Color32::from_rgb(190, 95, 35), message);
            }
            if let Some(message) = &self.logging.export_message {
                ui.small(message);
            }

            let update_notice = match &self.update_status {
                UpdateStatus::Available(manifest) => Some(format!(
                    "A versão {} está disponível.",
                    manifest.version
                )),
                UpdateStatus::Downloaded { manifest, .. } => Some(format!(
                    "A versão {} foi baixada; reinicie para aplicar.",
                    manifest.version
                )),
                _ => None,
            };
            if let Some(notice) = update_notice {
                ui.horizontal(|ui| {
                    ui.colored_label(egui::Color32::from_rgb(85, 170, 110), notice);
                    if ui.button("Ver atualização").clicked() {
                        open_update_settings = true;
                    }
                });
            }

            ui.add_space(20.0);

            if open_update_settings {
                self.open_settings();
                self.settings_category = SettingsCategory::Updates;
            } else if open_settings {
                self.open_settings();
            } else if close_settings {
                self.close_settings();
            }

            if self.settings_open {
                self.show_settings(ui);
            } else if self.room_code.is_some() {
                self.show_room(ui);
            } else {
                self.show_home(ui);
            }
            self.show_handoff_panel(ui, &context);
        });
    }

    fn show_home(&mut self, ui: &mut egui::Ui) {
        if !self.addresses_loaded {
            self.refresh_host_addresses();
        }

        ui.horizontal(|ui| {
            ui.heading("Modo ao criar:");
            ui.selectable_value(
                &mut self.create_room_mode,
                RoomMode::Local,
                "Rede local / Radmin",
            );
            ui.selectable_value(
                &mut self.create_room_mode,
                RoomMode::InternetTest,
                "Internet (teste)",
            );
        });

        if self.create_room_mode == RoomMode::Local {
            ui.group(|ui| {
                ui.heading("Rede de controle da sala");
                ui.label("Todos os participantes mantêm uma conexão direta de controle pela porta TCP 9001.");
                ui.small("Permita a porta 9001 no firewall do Windows e escolha um IPv4 que seus amigos consigam alcançar (LAN ou Radmin).");
                self.show_control_address_picker(ui);
                ui.checkbox(&mut self.may_host, "Permitir que este computador seja escolhido para hospedar futuramente");
                ui.small("Opcional: isso não impede entrar na sala nem compartilhar a tela. Só permite que este PC assuma a hospedagem se o anfitrião sair.");
                ui.small("Para a sucessão funcionar, a malha TCP 9001 também precisa conectar entre os participantes.");
            });
        } else {
            ui.group(|ui| {
                ui.heading("Teste controlado pela internet");
                ui.label("A sala aceitará somente você e mais uma pessoa. O anfitrião precisa permanecer online; não há sucessão pela internet.");
                ui.small("O anfitrião encaminha TCP 9000 no roteador para este PC e permite a porta no firewall do Windows. CGNAT pode impedir conexões de entrada.");
                ui.small("A mídia tenta uma conexão UDP P2P na porta 9002 com STUN. Não há TURN nem retransmissão de vídeo.");
                ui.colored_label(
                    egui::Color32::from_rgb(190, 95, 35),
                    "A sinalização usa ws:// sem criptografia ou autenticação. Use apenas testes controlados com pessoas conhecidas; não use para distribuição regular.",
                );
                ui.small("Configure o IPv4 público ou nome DDNS e a URI STUN em Configurações > Conexão antes de criar a sala.");
            });
        }
        ui.add_space(12.0);

        ui.group(|ui| {
            ui.heading("Criar uma sala");
            ui.label(
                "Crie uma sala hospedada neste computador e compartilhe o código com seu amigo.",
            );

            if ui
                .add_enabled(
                    !self.connecting && !self.update_blocks_room_actions(),
                    egui::Button::new("Criar sala"),
                )
                .clicked()
            {
                self.start_hosting();
            }
        });

        ui.add_space(12.0);

        ui.group(|ui| {
            ui.heading("Entrar em uma sala");
            ui.label("Digite o código da sala:");

            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.join_code)
                        .hint_text("Código da sala")
                        .desired_width(220.0),
                );

                let has_code = !self.join_code.trim().is_empty()
                    && !self.connecting
                    && !self.update_blocks_room_actions();
                if ui
                    .add_enabled(has_code, egui::Button::new("Entrar"))
                    .clicked()
                {
                    let code = self.join_code.trim().to_ascii_uppercase();
                    self.start_join(code);
                }
            });
        });

        if self.connecting {
            ui.label("Conectando ao servidor de sinalizacao...");
        }
        if let Some(status) = &self.connection_status {
            ui.label(status);
        }
        if let Some(error) = &self.connection_error {
            ui.colored_label(egui::Color32::from_rgb(190, 55, 55), error);
        }
        ui.small("Para entrar, informe em Configurações > Conexão o IPv4 ou nome DDNS compartilhado pelo anfitrião.");
    }

    fn show_room(&mut self, ui: &mut egui::Ui) {
        let code = self.room_code.clone().unwrap_or_default();

        ui.group(|ui| {
            ui.heading("Sala");
            ui.horizontal(|ui| {
                ui.label(format!("Código: {code}"));
                if ui.button("Copiar código").clicked() {
                    ui.ctx().copy_text(code.clone());
                    self.code_copied = true;
                }
            });

            if self.code_copied {
                ui.label("Código copiado para a área de transferência.");
            }

            ui.label(
                self.connection_status
                    .as_deref()
                    .unwrap_or("Conectado ao servidor."),
            );
            if let Some(error) = &self.connection_error {
                ui.colored_label(egui::Color32::from_rgb(190, 55, 55), error);
            }
            if !self.participants.is_empty() {
                ui.separator();
                ui.heading(if self.room_mode == RoomMode::InternetTest {
                    "Participantes da sala de teste"
                } else {
                    "Participantes e fila de sucessão"
                });
                let mut participants = self.participants.clone();
                participants.sort_by_key(|participant| participant.order);
                for participant in &participants {
                    ui.label(format!(
                        "{} — ordem {}, {}",
                        participant.display_name,
                        participant.order,
                        if self.room_mode == RoomMode::InternetTest {
                            "sucessão desativada no modo Internet"
                        } else if participant.id == self.current_leader_id {
                            "anfitrião atual"
                        } else if participant.may_host {
                            "autorizado a hospedar"
                        } else {
                            "não autorizado a assumir"
                        }
                    ));
                }
                if self.room_mode == RoomMode::InternetTest {
                    ui.small("Esta sala aceita duas pessoas e termina quando o anfitrião sai ou perde a conexão.");
                } else if participants.iter().all(|participant| !participant.may_host) {
                    ui.small("Ninguém autorizou a hospedagem automática. Isso não bloqueia a entrada; só significa que a sala termina se o anfitrião sair.");
                } else {
                    ui.small("A fila será ordenada pela estabilidade dos canais diretos.");
                }
            }
            if self.room_mode == RoomMode::Local && !self.control_queue.is_empty() {
                ui.separator();
                ui.label("Fila atual (perda, jitter e latência dos enlaces)");
                let mut queue = self.control_queue.clone();
                queue.sort_by(|left, right| {
                    right.eligible.cmp(&left.eligible)
                        .then_with(|| left.loss_percent.total_cmp(&right.loss_percent))
                        .then_with(|| left.jitter_ms.total_cmp(&right.jitter_ms))
                        .then_with(|| left.latency_ms.total_cmp(&right.latency_ms))
                        .then_with(|| left.participant.order.cmp(&right.participant.order))
                });
                for (index, candidate) in queue.iter().enumerate() {
                    ui.label(format!(
                        "{}. {} — perda {:.1}%, jitter {:.1} ms, latência {:.1} ms{}",
                        index + 1,
                        candidate.participant.display_name,
                        candidate.loss_percent,
                        candidate.jitter_ms,
                        candidate.latency_ms,
                        if candidate.eligible { "" } else { " (inelegível)" }
                    ));
                }
            }
            if self.room_mode == RoomMode::Local {
                if let Some(status) = &self.control_status {
                ui.small(status);
                }
            }
            if self.peer_connected {
                ui.label("Seu amigo está conectado.");
                if ui.button("Testar sinalização").clicked() {
                    self.diagnostic_status = Some("Enviando sinal de diagnóstico…".to_owned());
                    if let Some(signaling) = &self.signaling {
                        if let Err(error) = signaling.send_diagnostic() {
                            self.diagnostic_status = Some(error);
                        }
                    }
                }
            } else {
                ui.label("Aguardando seu amigo entrar na sala…");
            }
            if self.hosting_locally && self.room_mode == RoomMode::InternetTest {
                ui.separator();
                ui.heading("Endereço para seu amigo");
                match signaling_ws_url(&self.server_url) {
                    Ok(url) => {
                        ui.label(&url);
                        if ui.button("Copiar endereço do servidor").clicked() {
                            ui.ctx().copy_text(url);
                        }
                    }
                    Err(_) => {
                        ui.colored_label(
                            egui::Color32::from_rgb(190, 55, 55),
                            "Configure o IPv4 público ou DDNS em Configurações > Conexão.",
                        );
                    }
                }
                ui.small("O roteador precisa encaminhar TCP 9000 para este PC; libere também o aplicativo no firewall.");
            } else if self.hosting_locally {
                ui.separator();
                ui.heading("Endereço para seu amigo");
                ui.label("Escolha o adaptador que seu amigo consegue alcançar, como sua rede local ou Radmin VPN.");
                self.show_host_address_picker(ui);
            }
            if let Some(status) = &self.diagnostic_status {
                ui.small(status);
            }
        });

        ui.add_space(12.0);

        ui.group(|ui| {
            ui.heading("Prévia local da tela");
            ui.label("A prévia fica na memória. A tela só é enviada diretamente ao amigo depois que você iniciar o compartilhamento.");
            if self.room_mode == RoomMode::InternetTest {
                ui.small("O vídeo P2P usa UDP 9002. Os dois PCs precisam permitir o aplicativo ou essa porta no firewall do perfil de rede em uso.");
                ui.small("Teste controlado: a sinalização usa ws:// sem criptografia nem autenticação. Somente duas pessoas; sem TURN ou retransmissão. O anfitrião precisa permanecer online.");
                ui.small(format!("STUN configurado: {}", self.stun_server_url));
                ui.small("O operador do STUN pode ver o IP público de quem consulta; ele não recebe os quadros da tela.");
            } else {
                ui.small("O vídeo P2P usa UDP 9002. Os dois PCs precisam permitir o aplicativo ou essa porta no firewall do Windows, na rede privada.");
                ui.small("Nesta etapa, os PCs precisam estar na mesma rede local ou na mesma Radmin VPN. Conexões entre redes diferentes pela internet ainda não estão disponíveis.");
            }

            if self.screen_capture.is_some() {
                ui.label("Captura de tela ativa.");
                if ui.button("Parar captura da tela").clicked() {
                    self.stop_screen_capture();
                }
                if let Some(texture) = &self.screen_texture {
                    ui.add(egui::Image::new((texture.id(), texture.size_vec2())).max_width(640.0));
                } else {
                    ui.label("Aguardando o primeiro quadro…");
                }
            } else if self.screen_picker.is_some() {
                ui.label("Aguardando o seletor do Windows...");
            } else if ui.button("Selecionar tela ou janela").clicked() {
                self.select_screen(ui.ctx());
            }

            if let Some(status) = &self.screen_status {
                ui.label(status);
            }

            match self.screen_share_role.clone() {
                ScreenShareRole::Idle => {
                    let allowed = self.participants.len() == 2
                        && self.peer_connected
                        && self.screen_capture.is_some()
                        && self.screen_share_session.is_none();
                    if ui
                        .add_enabled(allowed, egui::Button::new("Compartilhar tela com meu amigo"))
                        .clicked()
                    {
                        self.request_screen_share(ui.ctx());
                    }
                    if self.participants.len() > 2 {
                        ui.small("O compartilhamento de tela está limitado a salas de duas pessoas nesta etapa.");
                    } else if self.screen_capture.is_none() {
                        ui.small("Selecione uma tela ou janela para habilitar o compartilhamento.");
                    } else if !self.peer_connected {
                        ui.small("Aguardando o outro participante entrar na sala.");
                    }
                }
                ScreenShareRole::Requesting { .. } => {
                    ui.label("Pedido de compartilhamento enviado; aguardando resposta do amigo.");
                    if ui.button("Cancelar pedido").clicked() {
                        self.stop_screen_share(true);
                        self.screen_share_status = Some("Pedido de compartilhamento cancelado.".to_owned());
                    }
                }
                ScreenShareRole::Sending { .. } => {
                    ui.label(if self.screen_share_metrics.p2p_connected {
                        "Conexão P2P estabelecida; transmitindo a tela."
                    } else {
                        "Pedido aceito; negociando a conexão P2P da tela."
                    });
                    ui.small(format!(
                        "H.264: {} quadros codificados, {} quadros enviados.",
                        self.screen_share_metrics.encoded_frames,
                        self.screen_share_metrics.sent_frames
                    ));
                    ui.small(&self.screen_share_metrics.h264_diagnostics);
                    if ui.button("Parar compartilhamento").clicked() {
                        self.stop_screen_share(true);
                    }
                }
                ScreenShareRole::Receiving { .. } => {
                    ui.label(if self.screen_share_metrics.p2p_connected {
                        "Conexão P2P estabelecida; aguardando ou recebendo vídeo."
                    } else {
                        "Pedido aceito; negociando a conexão P2P da tela."
                    });
                    ui.small(format!(
                        "Vídeo: {} pacotes recebidos, {} quadros decodificados, {} erros H.264.",
                        self.screen_share_metrics.received_packets,
                        self.screen_share_metrics.decoded_frames,
                        self.screen_share_metrics.decode_errors
                    ));
                    if let Some(error) = &self.screen_share_metrics.last_decode_error {
                        ui.small(format!("Último erro H.264: {error}"));
                    }
                    ui.small(&self.screen_share_metrics.h264_diagnostics);
                    if ui.button("Parar de receber a tela").clicked() {
                        self.stop_screen_share(true);
                    }
                    if let Some(texture) = &self.remote_screen_texture {
                        ui.add(egui::Image::new((texture.id(), texture.size_vec2())).max_width(640.0));
                    } else {
                        ui.label("Aguardando o primeiro quadro da tela remota…");
                    }
                }
            }
            if let Some(status) = &self.screen_share_status {
                ui.small(status);
            }
            if !matches!(&self.screen_share_role, ScreenShareRole::Idle)
                || self.screen_share_metrics.local_ice_candidates > 0
                || self.screen_share_metrics.remote_ice_candidates > 0
                || self.screen_share_status.is_some()
            {
                ui.small(format!(
                    "Última tentativa ICE: {} candidatos locais ({} públicos via STUN), {} recebidos do amigo ({} públicos via STUN).",
                    self.screen_share_metrics.local_ice_candidates,
                    self.screen_share_metrics.local_srflx_candidates,
                    self.screen_share_metrics.remote_ice_candidates,
                    self.screen_share_metrics.remote_srflx_candidates
                ));
            }
        });

        ui.add_space(12.0);
        if ui
            .add_enabled(
                self.screen_picker.is_none() && self.outgoing_transfer.is_none(),
                egui::Button::new(
                    if self.hosting_locally
                        && self.peer_connected
                        && self.room_mode == RoomMode::Local
                    {
                        "Sair e transferir automaticamente"
                    } else if self.hosting_locally
                        && self.peer_connected
                        && self.room_mode == RoomMode::InternetTest
                    {
                        "Encerrar sala e sair"
                    } else {
                        "Sair da sala"
                    },
                ),
            )
            .clicked()
        {
            self.request_leave(ui.ctx());
        }

        if self.hosting_locally
            && self.room_mode == RoomMode::Local
            && ui
                .add_enabled(
                    !self.ending_room_explicitly,
                    egui::Button::new(if self.ending_room_explicitly {
                        "Encerrando sala…"
                    } else {
                        "Encerrar sala sem sucessor"
                    }),
                )
                .clicked()
        {
            self.end_room_explicitly(ui.ctx());
        }
    }

    fn show_settings(&mut self, ui: &mut egui::Ui) {
        ui.heading("Configurações do aplicativo");
        ui.add_space(8.0);

        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.set_min_width(130.0);
                ui.heading("Categorias");
                let selected = self.settings_category == SettingsCategory::Audio;
                if ui.selectable_label(selected, "Áudio").clicked() {
                    self.select_settings_category(SettingsCategory::Audio);
                }
                let selected = self.settings_category == SettingsCategory::Connection;
                if ui.selectable_label(selected, "Conexão").clicked() {
                    self.select_settings_category(SettingsCategory::Connection);
                }
                let selected = self.settings_category == SettingsCategory::Updates;
                if ui.selectable_label(selected, "Atualizações").clicked() {
                    self.select_settings_category(SettingsCategory::Updates);
                }
            });

            ui.separator();

            ui.vertical(|ui| match self.settings_category {
                SettingsCategory::Audio => self.show_audio_settings(ui),
                SettingsCategory::Connection => self.show_connection_settings(ui),
                SettingsCategory::Updates => self.show_update_settings(ui),
            });
        });
    }

    fn show_update_settings(&mut self, ui: &mut egui::Ui) {
        ui.heading("Atualizações");
        ui.label(format!("Versão instalada: {}", env!("CARGO_PKG_VERSION")));
        ui.add_space(8.0);

        if !UpdateManager::is_configured() {
            ui.colored_label(
                egui::Color32::from_rgb(190, 95, 35),
                "Esta versão foi compilada sem um link de manifesto. Configure P2P_UPDATE_MANIFEST_URL e compile novamente para habilitar atualizações.",
            );
        } else {
            match self.update_status.clone() {
                UpdateStatus::Checking => {
                    ui.label("Verificando se há uma versão nova…");
                }
                UpdateStatus::Unconfigured => {
                    ui.label("Configure o link do manifesto para habilitar atualizações.");
                }
                UpdateStatus::UpToDate => {
                    ui.label("Você está usando a versão mais recente.");
                }
                UpdateStatus::Available(manifest) => {
                    ui.label(format!("A versão {} está disponível.", manifest.version));
                    if self.room_code.is_some() {
                        ui.small("Saia da sala para baixar a atualização.");
                    }
                    if ui
                        .add_enabled(
                            self.room_code.is_none(),
                            egui::Button::new("Baixar atualização"),
                        )
                        .clicked()
                    {
                        tracing::info!(version = %manifest.version, "Usuário iniciou download de atualização");
                        self.updates.download(manifest.clone());
                        self.update_status = UpdateStatus::Downloading {
                            manifest,
                            received: 0,
                        };
                    }
                }
                UpdateStatus::Downloading { manifest, received } => {
                    let progress = if manifest.size_bytes == 0 {
                        0.0
                    } else {
                        received as f32 / manifest.size_bytes as f32
                    };
                    ui.add(
                        egui::ProgressBar::new(progress.clamp(0.0, 1.0)).text(format!(
                            "Baixando {}: {} de {}",
                            manifest.version,
                            format_bytes(received),
                            format_bytes(manifest.size_bytes)
                        )),
                    );
                    if ui.button("Cancelar download").clicked() {
                        self.updates.cancel_download();
                        self.update_status = UpdateStatus::CancellingDownload(manifest);
                    }
                    if self.room_code.is_some() {
                        ui.small("O download será cancelado porque há uma sala ativa.");
                    }
                }
                UpdateStatus::CancellingDownload(_) => {
                    ui.label("Cancelando o download…");
                }
                UpdateStatus::Downloaded { manifest, path } => {
                    ui.label(format!(
                        "A versão {} foi baixada e validada.",
                        manifest.version
                    ));
                    if self.room_code.is_some() {
                        ui.small("Saia da sala antes de reiniciar para aplicar a atualização.");
                    }
                    if ui
                        .add_enabled(
                            self.room_code.is_none(),
                            egui::Button::new("Reiniciar para atualizar"),
                        )
                        .clicked()
                    {
                        tracing::info!(version = %manifest.version, "Usuário solicitou aplicação da atualização");
                        self.update_status = UpdateStatus::PreparingToApply;
                        self.updates.apply(path);
                    }
                }
                UpdateStatus::PreparingToApply => {
                    ui.label("Preparando a atualização ao lado do aplicativo…");
                }
                UpdateStatus::Applying => {
                    ui.label("O aplicativo será fechado e reaberto com a nova versão.");
                }
                UpdateStatus::Failed(error) => {
                    ui.colored_label(egui::Color32::from_rgb(190, 55, 55), error);
                }
            }
        }

        if self.room_code.is_some() {
            ui.small(
                "Downloads e reinicializações ficam bloqueados enquanto você está em uma sala.",
            );
        }
        let operation_running = matches!(
            &self.update_status,
            UpdateStatus::Checking
                | UpdateStatus::Downloading { .. }
                | UpdateStatus::CancellingDownload(_)
                | UpdateStatus::PreparingToApply
                | UpdateStatus::Applying
        );
        if ui
            .add_enabled(
                UpdateManager::is_configured() && !operation_running,
                egui::Button::new("Verificar atualizações"),
            )
            .clicked()
        {
            tracing::info!("Usuário solicitou nova verificação de atualização");
            self.update_status = UpdateStatus::Checking;
            self.updates.check();
        }
        ui.separator();
        ui.small("O download usa HTTPS e valida tamanho e SHA-256. Não há assinatura digital: essas verificações detectam corrupção, mas não confirmam quem publicou os arquivos.");
        ui.small("As atualizações só são baixadas e aplicadas por sua escolha; o app não faz isso enquanto você está em uma sala.");
    }

    fn show_connection_settings(&mut self, ui: &mut egui::Ui) {
        ui.heading("Conexão");
        ui.label("Endereço IPv4 ou nome DDNS do anfitrião (a porta é sempre 9000):");
        ui.add(
            egui::TextEdit::singleline(&mut self.server_url)
                .hint_text("IP público ou minha-sala.ddns.net")
                .desired_width(340.0),
        );
        match signaling_ws_url(&self.server_url) {
            Ok(url) => {
                ui.horizontal(|ui| {
                    ui.label(format!("Endereço para conectar: {url}"));
                    if ui.button("Copiar endereço").clicked() {
                        ui.ctx().copy_text(url);
                    }
                });
            }
            Err(error) if !self.server_url.trim().is_empty() => {
                ui.colored_label(egui::Color32::from_rgb(190, 55, 55), error);
            }
            Err(_) => {}
        }
        ui.small("No modo Internet, configure aqui o IPv4 público ou nome DDNS deste PC para copiar e enviar ao amigo. Não há detecção automática do IP público.");
        ui.small("No modo Rede local/Radmin, informe o IPv4 da rede local ou VPN que o anfitrião compartilhou.");
        ui.small("Os valores ficam somente na memória enquanto o aplicativo estiver aberto.");
        ui.colored_label(
            egui::Color32::from_rgb(190, 95, 35),
            "Esta versão usa ws:// sem criptografia nem autenticação. No modo Internet, faça somente testes controlados com pessoas conhecidas; wss:// e proteção de acesso ficam para antes da distribuição.",
        );
        ui.separator();
        ui.heading("STUN para conexão direta de mídia");
        ui.label("Uma única URI stun:; não use endereço turn: nesta etapa.");
        ui.add(
            egui::TextEdit::singleline(&mut self.stun_server_url)
                .hint_text("stun:stun.l.google.com:19302")
                .desired_width(340.0),
        );
        if let Err(error) = screen_sharing::validate_stun_uri(&self.stun_server_url) {
            ui.colored_label(egui::Color32::from_rgb(190, 55, 55), error);
        }
        ui.small("STUN ajuda os PCs a tentar encontrar um caminho UDP direto; não retransmite vídeo e não funciona em todas as redes.");
        ui.small("O servidor integrado continua apenas na sinalização TCP 9000. Para receber pela internet, encaminhe essa porta no roteador e permita o app no firewall.");
        ui.small("Não é necessário manter um notebook separado ligado.");
    }

    fn show_audio_settings(&mut self, ui: &mut egui::Ui) {
        ui.heading("Áudio");
        ui.label("Teste o microfone padrão do Windows sem entrar em uma sala.");

        ui.add_space(8.0);
        ui.group(|ui| {
            ui.heading("Teste do microfone");
            ui.label("Durante o teste, sua voz é reproduzida ao vivo na saída padrão do Windows. O áudio não é gravado nem transmitido.");
            ui.small("Use fones de ouvido para evitar que o som dos alto-falantes volte ao microfone.");
            ui.small("O medidor mostra o nível em dBFS, sem aplicar o ganho do retorno.");

            let gain_changed = ui
                .add(
                    egui::Slider::new(&mut self.monitor_gain_db, 0.0..=18.0)
                        .text("Ganho do retorno")
                        .suffix(" dB")
                        .step_by(1.0),
                )
                .changed();
            if gain_changed {
                if let Some(microphone) = &self.microphone {
                    microphone.set_monitor_gain_db(self.monitor_gain_db);
                }
            }
            ui.small("O ganho altera apenas o som ouvido. Valores altos podem distorcer.");

            if let Some(error) = &self.microphone_error {
                ui.colored_label(egui::Color32::from_rgb(190, 55, 55), error);
            }
            if let Some(error) = &self.microphone_monitor_error {
                ui.colored_label(egui::Color32::from_rgb(190, 95, 35), error);
            }
            if self.microphone_audio_warning {
                ui.colored_label(
                    egui::Color32::from_rgb(190, 95, 35),
                    "O retorno teve cortes por falta ou excesso de amostras. Pare e inicie o teste novamente.",
                );
            }
            if self.microphone_clipping_warning {
                ui.colored_label(
                    egui::Color32::from_rgb(190, 95, 35),
                    "O retorno está distorcendo; reduza o ganho.",
                );
            }

            if self.microphone.is_some() {
                if self.microphone_monitor_error.is_none() {
                    ui.label("Medidor e retorno ao vivo ativos.");
                } else {
                    ui.label("Medidor ativo; o retorno de áudio está indisponível.");
                }
                ui.add(
                    egui::ProgressBar::new(self.microphone_level)
                        .text(format!("Nível: {:.1} dBFS", self.microphone_level_dbfs)),
                );
                if ui.button("Parar teste do microfone").clicked() {
                    self.stop_microphone();
                }
            } else if ui.button("Testar microfone").clicked() {
                self.start_microphone();
            }

            ui.small("Se o acesso estiver bloqueado: Configurações > Privacidade e segurança > Microfone (no Windows 10, Privacidade > Microfone) > permitir acesso a aplicativos de área de trabalho.");
        });
    }

    fn open_settings(&mut self) {
        if self.room_code.is_some() && self.screen_capture.is_some() {
            self.stop_screen_capture();
        }
        self.settings_category = SettingsCategory::Audio;
        self.settings_open = true;
    }

    fn close_settings(&mut self) {
        if self.settings_category == SettingsCategory::Audio {
            self.stop_microphone();
        }
        self.settings_open = false;
    }

    fn select_settings_category(&mut self, category: SettingsCategory) {
        if self.settings_category == SettingsCategory::Audio && category != SettingsCategory::Audio
        {
            self.stop_microphone();
        }
        self.settings_category = category;
    }

    fn start_microphone(&mut self) {
        tracing::info!("Iniciando teste local de microfone e retorno de áudio");
        self.last_audio_metrics_log_at = None;
        self.microphone_error = None;
        self.microphone_monitor_error = None;
        self.microphone_audio_warning = false;
        self.microphone_clipping_warning = false;
        self.microphone_level = 0.0;
        self.microphone_level_dbfs = -60.0;
        match MicrophoneTest::start(self.monitor_gain_db) {
            Ok(test) => {
                self.microphone = Some(test);
                tracing::info!(
                    gain_db = self.monitor_gain_db,
                    "Teste do microfone iniciado"
                );
            }
            Err(error) => {
                tracing::error!(error = %error, "Falha ao iniciar teste do microfone");
                self.microphone_error = Some(error);
            }
        }
    }

    fn stop_microphone(&mut self) {
        let was_active = self.microphone.is_some();
        self.microphone = None;
        if was_active {
            tracing::info!("Teste local do microfone encerrado");
        }
        self.microphone_level = 0.0;
        self.microphone_level_dbfs = -60.0;
        self.microphone_clipping_warning = false;
    }

    fn refresh_microphone(&mut self) {
        let Some(microphone) = self.microphone.as_mut() else {
            return;
        };

        let rms = microphone.level();
        let level_dbfs = if rms > 0.0 { 20.0 * rms.log10() } else { -60.0 };
        let level_dbfs = level_dbfs.clamp(-60.0, 0.0);
        if self
            .last_audio_metrics_log_at
            .is_none_or(|last| last.elapsed() >= Duration::from_secs(5))
        {
            tracing::info!(
                level_dbfs,
                gain_db = self.monitor_gain_db,
                "Resumo periódico do teste de microfone"
            );
            self.last_audio_metrics_log_at = Some(Instant::now());
        }
        let level = (level_dbfs + 60.0) / 60.0;
        let microphone_error = microphone.take_microphone_error();
        let monitor_error = microphone.take_monitor_error();
        let audio_warning = microphone.take_audio_warning();
        let clipping_warning = microphone.take_clipping_warning();
        if monitor_error.is_some() {
            microphone.stop_monitoring();
        }

        self.microphone_level = level;
        self.microphone_level_dbfs = level_dbfs;
        if clipping_warning && !self.microphone_clipping_warning {
            tracing::warn!(
                gain_db = self.monitor_gain_db,
                "Retorno local de microfone atingiu limitação digital"
            );
        }
        self.microphone_clipping_warning = clipping_warning;
        if audio_warning {
            if !self.microphone_audio_warning {
                tracing::warn!("Fila de áudio local reportou excesso ou falta de amostras");
            }
            self.microphone_audio_warning = true;
        }
        if let Some(error) = monitor_error {
            tracing::error!(error = %error, "Falha no retorno local de áudio; medidor continua ativo");
            self.microphone_monitor_error = Some(error);
        }
        if let Some(error) = microphone_error {
            tracing::error!(error = %error, "Falha na captura do microfone; teste encerrado");
            self.microphone = None;
            self.microphone_level = 0.0;
            self.microphone_error = Some(error);
        }
    }

    fn select_screen(&mut self, _context: &egui::Context) {
        tracing::info!("Usuário abriu o seletor de tela ou janela do Windows");
        self.screen_status = None;
        match PendingScreenCapture::begin() {
            Ok(picker) => {
                self.screen_picker = Some(picker);
                self.screen_status =
                    Some("Selecione uma tela ou janela no seletor do Windows.".to_owned());
            }
            Err(error) => {
                self.screen_status = Some(format!("Não foi possível abrir o seletor: {error}"));
            }
        }
    }
    fn refresh_screen(&mut self, context: &egui::Context) {
        let picker_result = if self.settings_open {
            self.screen_picker = None;
            None
        } else {
            self.screen_picker
                .as_mut()
                .and_then(|picker| picker.poll(context.clone()))
        };
        if let Some(result) = picker_result {
            self.screen_picker = None;
            match result {
                Ok(Some(capture)) => {
                    self.screen_capture = Some(capture);
                    self.screen_status =
                        Some("A prévia atualiza enquanto a captura estiver ativa.".to_owned());
                }
                Ok(None) => {
                    self.screen_status =
                        Some("Seleção cancelada; a captura permaneceu desligada.".to_owned());
                }
                Err(error) => {
                    tracing::error!(error = %error, "Falha ao iniciar captura de tela após seleção");
                    self.screen_status = Some(format!(
                        "Não foi possível iniciar a captura da tela: {error}"
                    ));
                }
            }
        }

        let Some(capture) = self.screen_capture.as_mut() else {
            return;
        };

        if let Some(frame) = capture.latest_frame() {
            let image = egui::ColorImage::from_rgba_unmultiplied(
                [frame.width as usize, frame.height as usize],
                &frame.rgba,
            );
            if let Some(texture) = self.screen_texture.as_mut() {
                texture.set(image, egui::TextureOptions::LINEAR);
            } else {
                self.screen_texture = Some(context.load_texture(
                    "screen-preview",
                    image,
                    egui::TextureOptions::LINEAR,
                ));
            }
        }

        if capture.source_closed() {
            self.stop_screen_share(true);
            self.stop_screen_capture();
            self.screen_status =
                Some("A tela ou janela escolhida foi fechada; a captura terminou.".to_owned());
            return;
        }

        if let Some(result) = capture.poll_finished() {
            self.stop_screen_share(true);
            self.screen_capture = None;
            self.screen_texture = None;
            self.screen_status = Some(match result {
                Ok(()) => "A captura da tela foi encerrada pelo Windows.".to_owned(),
                Err(error) => format!("A captura da tela falhou: {error}"),
            });
        }
    }

    fn stop_screen_capture(&mut self) {
        self.stop_screen_share(true);
        let result = self
            .screen_capture
            .take()
            .map_or(Ok(()), |mut capture| capture.stop());
        self.screen_texture = None;
        self.screen_status = Some(match result {
            Ok(()) => "Captura da tela parada.".to_owned(),
            Err(error) => {
                format!("A captura parou, mas houve um erro ao liberar o recurso: {error}")
            }
        });
    }

    fn request_screen_share(&mut self, context: &egui::Context) {
        if self.participants.len() != 2
            || !self.peer_connected
            || self.screen_capture.is_none()
            || !matches!(&self.screen_share_role, ScreenShareRole::Idle)
        {
            return;
        }
        if self.room_mode == RoomMode::InternetTest {
            if let Err(error) = screen_sharing::validate_stun_uri(&self.stun_server_url) {
                self.screen_share_status = Some(error);
                return;
            }
        }
        self.screen_share_metrics = ScreenShareMetrics::default();
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let request_id = format!("{}-{nonce:x}", self.participant_id);
        let request = ScreenShareRequest {
            request_id: request_id.clone(),
            participant_id: self.participant_id.clone(),
        };
        let payload = match serde_json::to_string(&request) {
            Ok(payload) => payload,
            Err(error) => {
                self.screen_share_status =
                    Some(format!("Não foi possível preparar o pedido: {error}"));
                return;
            }
        };
        let result = self
            .signaling
            .as_ref()
            .ok_or_else(|| "A sala não está conectada ao servidor de sinalização.".to_owned())
            .and_then(|signaling| signaling.send_signal(SignalKind::ScreenShareRequest, payload));
        match result {
            Ok(()) => {
                self.screen_share_role = ScreenShareRole::Requesting { request_id };
                self.screen_share_status =
                    Some("Pedido enviado; a transmissão começa quando o amigo aceitar.".to_owned());
                context.request_repaint();
            }
            Err(error) => self.screen_share_status = Some(error),
        }
    }

    fn handle_screen_share_signal(
        &mut self,
        kind: SignalKind,
        payload: String,
        context: &egui::Context,
    ) {
        match kind {
            SignalKind::ScreenShareRequest => {
                let request: ScreenShareRequest = match serde_json::from_str(&payload) {
                    Ok(request) => request,
                    Err(error) => {
                        self.screen_share_status =
                            Some(format!("Pedido de compartilhamento inválido: {error}"));
                        return;
                    }
                };
                let remote_order = self
                    .participants
                    .iter()
                    .find(|participant| participant.id == request.participant_id);
                let Some(remote_order) = remote_order.map(|participant| participant.order) else {
                    return;
                };
                if self.participants.len() != 2 {
                    let _ = self
                        .send_screen_share_signal(SignalKind::ScreenShareBusy, request.request_id);
                    self.screen_share_status =
                        Some("O compartilhamento ainda está limitado a duas pessoas.".to_owned());
                    return;
                }

                let local_order = self
                    .participants
                    .iter()
                    .find(|participant| participant.id == self.participant_id)
                    .map(|participant| participant.order)
                    .unwrap_or(u8::MAX);
                match self.screen_share_role.clone() {
                    ScreenShareRole::Requesting { .. } if local_order < remote_order => {
                        // Quando os dois pedem ao mesmo tempo, vence quem entrou primeiro.
                        let _ = self.send_screen_share_signal(
                            SignalKind::ScreenShareBusy,
                            request.request_id,
                        );
                        return;
                    }
                    ScreenShareRole::Requesting { .. } if local_order > remote_order => {
                        // O pedido de quem entrou antes vence; aceitamos e descartamos o nosso.
                        self.screen_share_role = ScreenShareRole::Idle;
                    }
                    ScreenShareRole::Idle if self.screen_share_session.is_none() => {}
                    _ => {
                        let _ = self.send_screen_share_signal(
                            SignalKind::ScreenShareBusy,
                            request.request_id,
                        );
                        return;
                    }
                }

                match ScreenShareSession::new(context.clone(), self.stun_server_for_room()) {
                    Ok(session) => {
                        self.screen_share_session = Some(session);
                        self.screen_share_metrics = ScreenShareMetrics::default();
                        self.screen_share_role = ScreenShareRole::Receiving {
                            request_id: request.request_id.clone(),
                        };
                        self.screen_share_status =
                            Some("Aceitando a tela do amigo; preparando a conexão P2P…".to_owned());
                        if let Err(error) = self.send_screen_share_signal(
                            SignalKind::ScreenShareAccept,
                            request.request_id,
                        ) {
                            self.stop_screen_share(false);
                            self.screen_share_status = Some(error);
                        }
                    }
                    Err(error) => {
                        let _ = self.send_screen_share_signal(
                            SignalKind::ScreenShareBusy,
                            request.request_id,
                        );
                        self.screen_share_status = Some(error);
                    }
                }
            }
            SignalKind::ScreenShareAccept => {
                let Some(request_id) = (match self.screen_share_role.clone() {
                    ScreenShareRole::Requesting { request_id } if request_id == payload => {
                        Some(request_id)
                    }
                    _ => None,
                }) else {
                    return;
                };
                let Some(source) = self
                    .screen_capture
                    .as_ref()
                    .map(ScreenCapture::frame_source)
                else {
                    self.screen_share_role = ScreenShareRole::Idle;
                    self.screen_share_status = Some(
                        "A captura local terminou antes do início do compartilhamento.".to_owned(),
                    );
                    return;
                };
                match ScreenShareSession::new(context.clone(), self.stun_server_for_room()) {
                    Ok(session) => {
                        if let Err(error) = session.start_sending(source) {
                            session.stop();
                            self.screen_share_role = ScreenShareRole::Idle;
                            self.screen_share_status = Some(error);
                            return;
                        }
                        self.screen_share_session = Some(session);
                        self.screen_share_role = ScreenShareRole::Sending { request_id };
                        self.screen_share_status =
                            Some("Iniciando a codificação e a conexão direta…".to_owned());
                    }
                    Err(error) => {
                        self.screen_share_role = ScreenShareRole::Idle;
                        self.screen_share_status = Some(error);
                    }
                }
            }
            SignalKind::ScreenShareBusy => {
                if matches!(
                    &self.screen_share_role,
                    ScreenShareRole::Requesting { request_id } if request_id == &payload
                ) {
                    self.screen_share_role = ScreenShareRole::Idle;
                    self.screen_share_status = Some(
                        "O amigo já está compartilhando ou não pode receber outra solicitação agora."
                            .to_owned(),
                    );
                }
            }
            SignalKind::ScreenShareStopped => {
                let request_id = payload;
                let active_request = match &self.screen_share_role {
                    ScreenShareRole::Requesting { request_id }
                    | ScreenShareRole::Sending { request_id }
                    | ScreenShareRole::Receiving { request_id } => Some(request_id),
                    ScreenShareRole::Idle => None,
                };
                if active_request.is_some_and(|active| active == &request_id) {
                    self.stop_screen_share(false);
                    self.screen_share_status =
                        Some("O compartilhamento de tela foi encerrado.".to_owned());
                }
            }
            SignalKind::Offer | SignalKind::Answer | SignalKind::IceCandidate => {
                if let Some(session) = &self.screen_share_session {
                    if let Err(error) = session.handle_signal(kind, payload) {
                        self.stop_screen_share(true);
                        self.screen_share_status = Some(error);
                    }
                }
            }
            SignalKind::Diagnostic => {}
        }
    }

    fn send_screen_share_signal(&self, kind: SignalKind, payload: String) -> Result<(), String> {
        tracing::debug!(
            signal_kind = ?kind,
            payload_bytes = payload.len(),
            "Enviando sinal de compartilhamento; conteúdo omitido"
        );
        self.signaling
            .as_ref()
            .ok_or_else(|| "A conexão de sinalização não está disponível.".to_owned())?
            .send_signal(kind, payload)
    }

    fn refresh_screen_share(&mut self, context: &egui::Context) {
        if let Some(session) = self.screen_share_session.as_ref() {
            self.screen_share_metrics = session.metrics();
            let should_log_metrics = self
                .last_screen_metrics_log_at
                .is_none_or(|last| last.elapsed() >= Duration::from_secs(5));
            if should_log_metrics {
                let metrics = &self.screen_share_metrics;
                tracing::info!(
                    p2p_connected = metrics.p2p_connected,
                    local_ice = metrics.local_ice_candidates,
                    remote_ice = metrics.remote_ice_candidates,
                    encoded = metrics.encoded_frames,
                    sent = metrics.sent_frames,
                    received_packets = metrics.received_packets,
                    decoded = metrics.decoded_frames,
                    decode_errors = metrics.decode_errors,
                    h264 = %metrics.h264_diagnostics,
                    last_decode_error = metrics.last_decode_error.as_deref().unwrap_or(""),
                    "Resumo periódico da mídia de compartilhamento"
                );
                self.last_screen_metrics_log_at = Some(Instant::now());
            }
        }
        let events = self
            .screen_share_session
            .as_ref()
            .map(|session| std::iter::from_fn(|| session.try_recv()).collect::<Vec<_>>())
            .unwrap_or_default();
        let mut stop_session = false;
        for event in events {
            match event {
                ScreenShareEvent::Signal { kind, payload } => {
                    tracing::debug!(signal_kind = ?kind, "Sinal de negociação de tela gerado; conteúdo omitido");
                    let result = self.send_screen_share_signal(kind, payload);
                    if let Err(error) = result {
                        tracing::error!(error = %error, "Falha ao encaminhar sinal de tela pela sinalização");
                        self.screen_share_status = Some(error);
                        stop_session = true;
                    }
                }
                ScreenShareEvent::State(status) => {
                    tracing::info!(state = %status, "Estado WebRTC de compartilhamento alterado");
                    self.screen_share_status = Some(status)
                }
                ScreenShareEvent::Error(error) => {
                    tracing::error!(error = %error, "Erro na sessão WebRTC de compartilhamento");
                    self.screen_share_status = Some(error);
                    stop_session = true;
                }
                ScreenShareEvent::ConnectionClosed => {
                    tracing::warn!("Conexão P2P de compartilhamento encerrada");
                    self.screen_share_status =
                        Some("A conexão P2P de tela foi encerrada ou perdida.".to_owned());
                    stop_session = true;
                }
            }
        }
        if stop_session {
            let status = self.screen_share_status.clone();
            self.stop_screen_share(true);
            self.screen_share_status = status;
            return;
        }

        let remote_frame = self
            .screen_share_session
            .as_ref()
            .and_then(ScreenShareSession::latest_remote_frame);
        if let Some(frame) = remote_frame {
            if self.remote_screen_sequence != frame.sequence {
                let image = egui::ColorImage::from_rgba_unmultiplied(
                    [frame.width as usize, frame.height as usize],
                    &frame.rgba,
                );
                if let Some(texture) = self.remote_screen_texture.as_mut() {
                    texture.set(image, egui::TextureOptions::LINEAR);
                } else {
                    self.remote_screen_texture = Some(context.load_texture(
                        "remote-screen",
                        image,
                        egui::TextureOptions::LINEAR,
                    ));
                }
                if self.remote_screen_sequence == 0 {
                    self.screen_share_status =
                        Some("Primeiro quadro da tela recebido e decodificado.".to_owned());
                }
                self.remote_screen_sequence = frame.sequence;
            }
        }
    }

    fn stop_screen_share(&mut self, announce: bool) {
        let request_id = match &self.screen_share_role {
            ScreenShareRole::Requesting { request_id }
            | ScreenShareRole::Sending { request_id }
            | ScreenShareRole::Receiving { request_id } => Some(request_id.clone()),
            ScreenShareRole::Idle => None,
        };
        let was_active = request_id.is_some();
        if was_active {
            tracing::info!(
                announce,
                "Encerrando sessão de compartilhamento de tela; identificador omitido"
            );
        }
        if announce {
            if let Some(request_id) = request_id {
                let _ = self.send_screen_share_signal(SignalKind::ScreenShareStopped, request_id);
            }
        }
        if let Some(session) = self.screen_share_session.take() {
            session.stop();
        }
        self.screen_share_role = ScreenShareRole::Idle;
        self.remote_screen_texture = None;
        self.remote_screen_sequence = 0;
        if was_active {
            self.screen_share_status = Some("Compartilhamento de tela encerrado.".to_owned());
        }
    }

    fn stun_server_for_room(&self) -> Option<String> {
        (self.room_mode == RoomMode::InternetTest).then(|| self.stun_server_url.trim().to_owned())
    }

    fn enter_room(&mut self, code: String) {
        self.room_code = Some(code);
        self.code_copied = false;
        self.handoff_error = None;
        self.microphone_error = None;
        self.connection_error = None;
        self.screen_status = None;
        self.screen_share_status = None;
    }

    fn start_hosting(&mut self) {
        if self.signaling.is_some() || self.connecting || self.update_blocks_room_actions() {
            return;
        }
        tracing::info!(room_mode = ?self.create_room_mode, "Iniciando criação de sala");
        if self.create_room_mode == RoomMode::InternetTest {
            if let Err(error) = signaling_ws_url(&self.server_url) {
                tracing::error!(reason = %error, "Configuração do endereço Internet inválida para criar sala");
                self.connection_error = Some(format!(
                    "Configure um IPv4 público ou nome DDNS válido em Configurações > Conexão: {error}"
                ));
                return;
            }
            if let Err(error) = screen_sharing::validate_stun_uri(&self.stun_server_url) {
                tracing::error!(reason = %error, "Configuração STUN inválida para criar sala");
                self.connection_error = Some(error);
                return;
            }
            tracing::info!(
                signaling_endpoint = %safe_signaling_endpoint(&self.server_url),
                stun_endpoint = %safe_stun_endpoint(&self.stun_server_url),
                "Configuração de rede do teste Internet validada"
            );
        }
        if self.create_room_mode == RoomMode::Local {
            tracing::info!(
                signaling_endpoint = ?self.selected_signaling_address(),
                "Endereço local escolhido para anunciar a sala"
            );
        }
        self.refresh_host_addresses();
        self.room_mode = self.create_room_mode;
        self.connection_error = None;
        self.connection_status = Some("Iniciando servidor de sala neste computador…".to_owned());
        self.connecting = true;
        self.starting_host = true;
        self.peer_connected = false;
        self.diagnostic_status = None;
        self.room_code = None;
        self.hosting_locally = false;

        let participant = self.local_participant_info();
        match SignalingClient::start_host_with_participant(participant, self.room_mode) {
            Ok(client) => self.signaling = Some(client),
            Err(error) => {
                tracing::error!(error = %error, "Falha ao iniciar sala hospedada localmente");
                self.connecting = false;
                self.starting_host = false;
                self.connection_status = Some("Desconectado.".to_owned());
                self.connection_error = Some(error);
            }
        }
    }

    fn start_join(&mut self, code: String) {
        if self.signaling.is_some() || self.connecting || self.update_blocks_room_actions() {
            return;
        }
        let server_url = match signaling_ws_url(&self.server_url) {
            Ok(url) => url,
            Err(error) => {
                tracing::error!(reason = %error, "Endereço do anfitrião inválido ao entrar em sala");
                self.connection_error = Some(format!(
                    "Informe o IPv4 ou nome DDNS do anfitrião em Configurações > Conexão: {error}"
                ));
                return;
            }
        };
        tracing::info!(endpoint = %server_url, "Conectando para entrar em sala; código omitido");
        self.room_mode = RoomMode::Local;
        self.connection_error = None;
        self.connection_status = Some("Conectando ao anfitrião…".to_owned());
        self.connecting = true;
        self.starting_host = false;
        self.hosting_locally = false;
        self.peer_connected = false;
        self.diagnostic_status = None;
        self.room_code = None;

        let participant = self.local_participant_info();
        match SignalingClient::join_with_participant(server_url, code, participant) {
            Ok(client) => self.signaling = Some(client),
            Err(error) => {
                tracing::error!(error = %error, "Falha ao iniciar a conexão de entrada na sala");
                self.connecting = false;
                self.connection_status = Some("Desconectado.".to_owned());
                self.connection_error = Some(error);
            }
        }
    }

    fn local_participant_info(&mut self) -> ParticipantInfo {
        if self.participant_id.is_empty() {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            self.participant_id = format!("{}-{nonce:x}", std::process::id());
        }
        let control_address = if self.room_mode == RoomMode::InternetTest {
            String::new()
        } else {
            self.host_addresses
                .get(self.selected_host_address)
                .map(|address| format!("{}:9001", address.ipv4))
                .unwrap_or_default()
        };
        let order = self
            .participants
            .iter()
            .find(|participant| participant.id == self.participant_id)
            .map_or(0, |participant| participant.order);
        ParticipantInfo {
            id: self.participant_id.clone(),
            display_name: "Participante".to_owned(),
            order,
            may_host: self.room_mode == RoomMode::Local && self.may_host,
            control_address,
        }
    }

    fn update_room_roster(
        &mut self,
        participants: Vec<ParticipantInfo>,
        leader_id: String,
        room_mode: RoomMode,
    ) {
        self.room_mode = room_mode;
        self.participants = participants;
        self.current_leader_id = leader_id.clone();
        if self.participants.len() != 2 && !matches!(&self.screen_share_role, ScreenShareRole::Idle)
        {
            self.stop_screen_share(true);
            self.screen_share_status = Some(
                "O compartilhamento foi encerrado porque esta sala não tem exatamente duas pessoas."
                    .to_owned(),
            );
        }
        if self.room_mode == RoomMode::InternetTest {
            self.control_mesh = None;
            self.control_queue.clear();
            self.control_status = Some(
                "Malha TCP 9001 e sucessão automática desativadas nesta sala de Internet."
                    .to_owned(),
            );
            return;
        }
        let Some(local) = self
            .participants
            .iter()
            .find(|participant| participant.id == self.participant_id)
            .cloned()
        else {
            return;
        };
        let code = self.room_code.clone().unwrap_or_default();
        let leader_address = self
            .participants
            .iter()
            .find(|participant| participant.id == leader_id)
            .map(|participant| signaling_address_for_control(&participant.control_address))
            .unwrap_or_default();
        if let Some(mesh) = &self.control_mesh {
            mesh.update_roster(self.participants.clone(), leader_id, leader_address);
        } else if !code.is_empty() {
            match ControlMesh::start(local, code, leader_id.clone(), leader_address.clone()) {
                Ok(mesh) => {
                    mesh.update_roster(self.participants.clone(), leader_id, leader_address);
                    self.control_mesh = Some(mesh);
                    self.control_mesh_failed = false;
                    self.control_status =
                        Some("Conectando a malha direta na porta 9001…".to_owned());
                }
                Err(error) => self.control_status = Some(error),
            }
        }
    }

    fn refresh_control_mesh(&mut self, context: &egui::Context) {
        let pending = self
            .control_mesh
            .as_ref()
            .map(|mesh| std::iter::from_fn(|| mesh.try_recv()).collect::<Vec<_>>())
            .unwrap_or_default();
        for event in pending {
            match event {
                ControlEvent::Ready => {
                    tracing::info!("Malha de controle TCP pronta");
                    self.control_mesh_failed = false;
                    self.control_status = Some("Canal de controle ativo na porta 9001.".to_owned());
                }
                ControlEvent::Error(error) => {
                    tracing::error!(error = %error, "Erro na malha de controle");
                    self.control_mesh_failed = true;
                    self.control_status = Some(error);
                }
                ControlEvent::QueueUpdated(queue) => {
                    self.control_queue = queue;
                    if self
                        .last_control_metrics_log_at
                        .is_none_or(|last| last.elapsed() >= Duration::from_secs(10))
                    {
                        let summary = self
                            .control_queue
                            .iter()
                            .map(|entry| {
                                format!(
                                    "ordem {}: perda {:.1}%, jitter {:.1} ms, latência {:.1} ms, elegível {}",
                                    entry.participant.order,
                                    entry.loss_percent,
                                    entry.jitter_ms,
                                    entry.latency_ms,
                                    entry.eligible
                                )
                            })
                            .collect::<Vec<_>>()
                            .join("; ");
                        tracing::debug!(candidates = %summary, "Resumo periódico de estabilidade da fila");
                        self.last_control_metrics_log_at = Some(Instant::now());
                    }
                }
                ControlEvent::LinksUpdated { connected, total } => {
                    if self.last_control_link_state != Some((connected, total)) {
                        tracing::info!(
                            connected,
                            total,
                            "Quantidade de enlaces de controle conectados mudou"
                        );
                        self.last_control_link_state = Some((connected, total));
                    }
                    self.control_status = Some(if total == 0 {
                        "Canal de controle ativo; aguardando outros participantes.".to_owned()
                    } else if connected < total {
                        format!(
                            "Malha direta parcial ({connected}/{total}). Confira o IPv4 escolhido e permita a porta 9001 no firewall de todos."
                        )
                    } else {
                        format!(
                            "Malha direta completa ({connected}/{total} participantes conectados)."
                        )
                    });
                }
                ControlEvent::HostUnstable { loss_percent } => {
                    tracing::warn!(
                        loss_percent,
                        "Anfitrião marcado instável; iniciando sucessão"
                    );
                    self.control_status = Some(format!(
                        "Anfitrião instável ({loss_percent:.1}% de perda). Elegendo o próximo participante elegível…"
                    ));
                }
                ControlEvent::BecomeHost {
                    code,
                    epoch,
                    participants,
                } => {
                    tracing::warn!(epoch, "Participante local eleito para hospedar a sala");
                    let participant = self.local_participant_info();
                    match SignalingClient::start_elected_host(code, participant, participants) {
                        Ok(client) => {
                            self.pending_signaling = Some(client);
                            self.pending_election_epoch = Some(epoch);
                            self.pending_election_reconnect = false;
                            self.control_status = Some("Você foi escolhido para assumir; iniciando o servidor na porta 9000…".to_owned());
                        }
                        Err(error) => {
                            tracing::error!(error = %error, epoch, "Candidato eleito não conseguiu iniciar servidor");
                            self.control_status =
                                Some(format!("Não foi possível iniciar o servidor: {error}"));
                            if let Some(mesh) = &self.control_mesh {
                                mesh.candidate_failed(epoch);
                            }
                        }
                    }
                }
                ControlEvent::LeaderChanged {
                    participant_id,
                    address,
                    epoch,
                } => {
                    tracing::warn!(
                        epoch,
                        is_local_leader = participant_id == self.participant_id,
                        "Liderança da sala mudou"
                    );
                    self.current_leader_id = participant_id.clone();
                    if participant_id == self.participant_id {
                        self.hosting_locally = true;
                        self.peer_connected = false;
                        self.control_status =
                            Some("Este computador está hospedando a sala.".to_owned());
                        continue;
                    }
                    if self.leave_after_handoff {
                        let should_close = self.close_after_transfer;
                        self.leave_room();
                        self.connection_status =
                            Some("A hospedagem foi transferida; você saiu da sala.".to_owned());
                        if should_close {
                            self.allow_window_close = true;
                            context.send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                        continue;
                    }
                    let code = self.room_code.clone().unwrap_or_default();
                    let participant = self.local_participant_info();
                    match SignalingClient::join_with_participant(address, code, participant) {
                        Ok(client) => {
                            self.pending_signaling = Some(client);
                            self.pending_election_epoch = Some(epoch);
                            self.pending_election_reconnect = true;
                            self.hosting_locally = false;
                            self.connection_status = Some(
                                "A sala mudou de anfitrião; reconectando ao novo servidor…"
                                    .to_owned(),
                            );
                        }
                        Err(error) => {
                            self.control_status =
                                Some(format!("Falha ao reconectar ao novo anfitrião: {error}"))
                        }
                    }
                }
                ControlEvent::RoomEnded => {
                    tracing::warn!("Malha de controle informou que a sala terminou");
                    let should_close = self.close_after_transfer;
                    let ended_explicitly = self.ending_room_explicitly;
                    let has_authorized_successor = self.participants.iter().any(|participant| {
                        participant.id != self.current_leader_id && participant.may_host
                    });
                    self.leave_room();
                    self.connection_status = Some(if ended_explicitly {
                        "Sala encerrada por você.".to_owned()
                    } else if has_authorized_successor {
                        "A sala foi encerrada porque o participante autorizado não conseguiu assumir a hospedagem. Confira a conexão direta pela porta TCP 9001 e se a porta TCP 9000 está livre no computador escolhido.".to_owned()
                    } else {
                        "A sala foi encerrada porque nenhum participante restante autorizou a hospedagem. Para manter a sala ativa, marque essa opção antes de entrar na próxima vez.".to_owned()
                    });
                    if should_close {
                        self.allow_window_close = true;
                        context.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                }
            }
        }
    }

    fn refresh_host_addresses(&mut self) {
        self.addresses_loaded = true;
        match enumerate_host_addresses() {
            Ok(addresses) => {
                tracing::info!(
                    adapter_count = addresses.len(),
                    "Lista de adaptadores IPv4 atualizada"
                );
                for address in &addresses {
                    tracing::info!(adapter = %address.adapter, ipv4 = %address.ipv4, "Adaptador IPv4 disponível");
                }
                self.host_addresses = addresses;
                self.host_addresses_error = None;
                if self.selected_host_address >= self.host_addresses.len() {
                    self.selected_host_address = 0;
                }
            }
            Err(error) => {
                tracing::error!(error = %error, "Falha ao enumerar adaptadores IPv4");
                self.host_addresses.clear();
                self.host_addresses_error = Some(error);
            }
        }
    }

    fn show_control_address_picker(&mut self, ui: &mut egui::Ui) {
        if !self.host_addresses.is_empty() {
            let previous_selection = self.selected_host_address;
            self.selected_host_address = self
                .selected_host_address
                .min(self.host_addresses.len().saturating_sub(1));
            let selected = &self.host_addresses[self.selected_host_address];
            egui::ComboBox::from_id_salt("control-ip-address")
                .selected_text(format!("{} — {}", selected.adapter, selected.ipv4))
                .show_ui(ui, |ui| {
                    for (index, address) in self.host_addresses.iter().enumerate() {
                        ui.selectable_value(
                            &mut self.selected_host_address,
                            index,
                            format!("{} — {}", address.adapter, address.ipv4),
                        );
                    }
                });
            if previous_selection != self.selected_host_address {
                let selected = &self.host_addresses[self.selected_host_address];
                tracing::info!(adapter = %selected.adapter, ipv4 = %selected.ipv4, "Adaptador escolhido para a malha de controle");
            }
            ui.monospace(format!(
                "ws://{}:9001",
                self.host_addresses[self.selected_host_address].ipv4
            ));
        } else if let Some(error) = &self.host_addresses_error {
            ui.colored_label(egui::Color32::from_rgb(190, 55, 55), error);
        } else {
            ui.label("Nenhum IPv4 ativo disponível para a conexão direta de controle.");
        }
        if ui.button("Atualizar adaptadores de controle").clicked() {
            self.refresh_host_addresses();
        }
    }

    fn show_host_address_picker(&mut self, ui: &mut egui::Ui) {
        if !self.host_addresses.is_empty() {
            let previous_selection = self.selected_host_address;
            self.selected_host_address = self
                .selected_host_address
                .min(self.host_addresses.len().saturating_sub(1));
            let selected = &self.host_addresses[self.selected_host_address];
            egui::ComboBox::from_id_salt("host-ip-address")
                .selected_text(format!("{} — {}", selected.adapter, selected.ipv4))
                .show_ui(ui, |ui| {
                    for (index, address) in self.host_addresses.iter().enumerate() {
                        ui.selectable_value(
                            &mut self.selected_host_address,
                            index,
                            format!("{} — {}", address.adapter, address.ipv4),
                        );
                    }
                });
            if previous_selection != self.selected_host_address {
                let selected = &self.host_addresses[self.selected_host_address];
                tracing::info!(adapter = %selected.adapter, ipv4 = %selected.ipv4, "Adaptador escolhido para anunciar a sala");
            }
            let url = format!(
                "ws://{}:9000",
                self.host_addresses[self.selected_host_address].ipv4
            );
            ui.horizontal(|ui| {
                ui.monospace(&url);
                if ui.button("Copiar endereço").clicked() {
                    ui.ctx().copy_text(url.clone());
                }
            });
        } else if let Some(error) = &self.host_addresses_error {
            ui.colored_label(egui::Color32::from_rgb(190, 55, 55), error);
        } else {
            ui.label("Nenhum IPv4 ativo foi encontrado. O servidor ainda pode funcionar em redes acessíveis por outro endereço.");
        }
        if ui.button("Atualizar adaptadores").clicked() {
            self.refresh_host_addresses();
        }
    }

    fn selected_signaling_address(&self) -> Option<String> {
        self.host_addresses
            .get(self.selected_host_address)
            .map(|address| format!("ws://{}:9000", address.ipv4))
    }

    fn refresh_signaling(&mut self, context: &egui::Context) {
        let pending_events = self
            .pending_signaling
            .as_ref()
            .map(|client| std::iter::from_fn(|| client.try_recv()).collect::<Vec<_>>())
            .unwrap_or_default();
        for event in pending_events {
            match event {
                SignalingEvent::RoomAdopted(code) => {
                    tracing::info!(
                        "Servidor informou que a sala transferida foi adotada; código omitido"
                    );
                    if let Some(epoch) = self.pending_election_epoch.take() {
                        self.pending_room_adopted = true;
                        self.signaling = self.pending_signaling.take();
                        self.pending_election_reconnect = false;
                        self.hosting_locally = true;
                        self.peer_connected = false;
                        self.enter_room(code);
                        if let Some(address) = self.selected_signaling_address() {
                            if let Some(mesh) = &self.control_mesh {
                                mesh.publish_leader(address, epoch);
                            }
                            self.control_status = Some(
                                "Servidor pronto; anunciando o novo anfitrião aos participantes."
                                    .to_owned(),
                            );
                        } else {
                            self.control_status = Some(
                                "Servidor iniciado, mas nenhum IPv4 pode ser anunciado.".to_owned(),
                            );
                            if let Some(mesh) = &self.control_mesh {
                                mesh.candidate_failed(epoch);
                            }
                        }
                        continue;
                    }
                    self.pending_room_adopted = true;
                    if let (Some(signaling), Some((_, token))) =
                        (&self.signaling, &self.incoming_transfer)
                    {
                        if let Err(error) = signaling.confirm_host_transfer(code, token.clone()) {
                            self.handoff_error = Some(error);
                            self.pending_signaling = None;
                            self.pending_room_adopted = false;
                        } else {
                            self.connection_status = Some(
                                "Novo servidor pronto; confirmando a transferência…".to_owned(),
                            );
                        }
                    }
                }
                SignalingEvent::RoomJoined(code) if self.pending_election_reconnect => {
                    tracing::info!(
                        "Reconexão à sala concluída após eleição de anfitrião; código omitido"
                    );
                    self.signaling = self.pending_signaling.take();
                    self.pending_election_reconnect = false;
                    self.pending_election_epoch = None;
                    self.pending_room_adopted = false;
                    self.hosting_locally = false;
                    self.peer_connected = true;
                    self.enter_room(code);
                    self.connection_status = Some(
                        "Reconectado ao novo anfitrião; a identidade e a ordem foram mantidas."
                            .to_owned(),
                    );
                }
                SignalingEvent::Error(error) | SignalingEvent::ServerError(error) => {
                    tracing::error!(error = %error, "Erro recebido durante conexão ou transferência de sala");
                    if let Some(epoch) = self.pending_election_epoch.take() {
                        if !self.pending_election_reconnect {
                            if let Some(mesh) = &self.control_mesh {
                                mesh.candidate_failed(epoch);
                            }
                        }
                        self.pending_signaling = None;
                        self.pending_election_reconnect = false;
                        self.control_status = Some(format!(
                            "A tentativa de mudança de anfitrião falhou: {error}"
                        ));
                        continue;
                    }
                    if let (Some(signaling), Some((_, token))) =
                        (&self.signaling, &self.incoming_transfer)
                    {
                        let _ = signaling.reject_host_transfer(token.clone());
                    }
                    self.pending_signaling = None;
                    self.pending_room_adopted = false;
                    self.handoff_error =
                        Some(format!("Não foi possível assumir a hospedagem: {error}"));
                }
                SignalingEvent::Disconnected => {
                    tracing::warn!("Conexão pendente de sala foi desconectada");
                    if let Some(epoch) = self.pending_election_epoch.take() {
                        if !self.pending_election_reconnect {
                            if let Some(mesh) = &self.control_mesh {
                                mesh.candidate_failed(epoch);
                            }
                        }
                        self.pending_signaling = None;
                        self.pending_election_reconnect = false;
                        self.control_status = Some(
                            "A tentativa de mudança de anfitrião foi desconectada.".to_owned(),
                        );
                        continue;
                    }
                    self.pending_signaling = None;
                    self.pending_room_adopted = false;
                    self.handoff_error =
                        Some("A tentativa de iniciar o novo servidor foi encerrada.".to_owned());
                }
                _ => {}
            }
        }

        let events = self
            .signaling
            .as_ref()
            .map(|client| std::iter::from_fn(|| client.try_recv()).collect::<Vec<_>>())
            .unwrap_or_default();
        let mut disconnect = false;
        let mut acknowledge_diagnostic = false;
        for event in events {
            match event {
                SignalingEvent::RoomCreated(code) => {
                    tracing::info!(room_mode = ?self.room_mode, "Sala criada no servidor integrado; código omitido");
                    self.connecting = false;
                    self.starting_host = false;
                    self.hosting_locally = true;
                    self.connection_status =
                        Some("Sala hospedada neste computador; aguardando seu amigo.".to_owned());
                    self.enter_room(code);
                }
                SignalingEvent::RoomJoined(code) => {
                    tracing::info!("Entrada na sala concluída; código omitido");
                    self.connecting = false;
                    self.peer_connected = true;
                    self.connection_status = Some("Você entrou na sala do anfitrião.".to_owned());
                    self.enter_room(code);
                }
                SignalingEvent::RoomAdopted(_) => {
                    tracing::info!("Sala adotada pelo participante local após transferência");
                    self.room_mode = RoomMode::Local;
                }
                SignalingEvent::PeerJoined => {
                    tracing::info!("Outro participante entrou na sala");
                    self.connecting = false;
                    self.peer_connected = true;
                    self.connection_status = Some("Seu amigo entrou na sala.".to_owned());
                }
                SignalingEvent::PeerLeft => {
                    tracing::warn!("Outro participante saiu ou desconectou da sala");
                    self.stop_screen_share(false);
                    self.peer_connected = false;
                    self.connection_status =
                        Some("Seu amigo desconectou; a sala aguarda outra conexão.".to_owned());
                    self.diagnostic_status = None;
                    if self.outgoing_transfer.is_some() {
                        self.outgoing_transfer = None;
                        self.handoff_error = Some(
                            "O outro participante desconectou antes da transferência.".to_owned(),
                        );
                    }
                }
                SignalingEvent::RoomRoster {
                    participants,
                    leader_id,
                    room_mode,
                } => {
                    tracing::info!(participants = participants.len(), room_mode = ?room_mode, "Lista de participantes atualizada");
                    self.update_room_roster(participants, leader_id, room_mode);
                }
                SignalingEvent::HostTransferPending { code, token } => {
                    tracing::info!(
                        "Transferência de hospedagem solicitada; códigos e tokens omitidos"
                    );
                    self.outgoing_transfer = Some((code, token));
                    self.handoff_error = None;
                }
                SignalingEvent::HostTransferRequested { code, token } => {
                    tracing::info!("Pedido de transferência recebido; códigos e tokens omitidos");
                    self.incoming_transfer = Some((code, token));
                    self.handoff_error = None;
                }
                SignalingEvent::HostTransferComplete(code) => {
                    tracing::info!("Transferência de hospedagem concluída; código omitido");
                    if self.hosting_locally {
                        let should_close = self.close_after_transfer;
                        self.leave_room();
                        self.connection_status =
                            Some(format!("A hospedagem da sala {code} foi transferida."));
                        if should_close {
                            self.allow_window_close = true;
                            context.send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                    } else if self.pending_signaling.is_some() && self.pending_room_adopted {
                        self.signaling = None;
                        self.signaling = self.pending_signaling.take();
                        self.pending_room_adopted = false;
                        self.incoming_transfer = None;
                        self.handoff_error = None;
                        self.hosting_locally = true;
                        self.peer_connected = false;
                        self.refresh_host_addresses();
                        self.enter_room(code);
                        self.connection_status = Some("Você assumiu a hospedagem. A sala está pronta para outro participante.".to_owned());
                    }
                }
                SignalingEvent::HostTransferCanceled(message) => {
                    tracing::warn!(reason = %message, "Transferência de hospedagem cancelada");
                    if self.outgoing_transfer.is_some()
                        || self.incoming_transfer.is_some()
                        || self.pending_signaling.is_some()
                    {
                        self.outgoing_transfer = None;
                        self.incoming_transfer = None;
                        self.pending_signaling = None;
                        self.pending_room_adopted = false;
                        self.handoff_error = Some(message);
                    }
                }
                SignalingEvent::ServerError(error) => {
                    tracing::error!(error = %error, "Servidor recusou a operação solicitada");
                    if self.connecting {
                        self.connecting = false;
                        self.starting_host = false;
                        self.hosting_locally = false;
                        self.peer_connected = false;
                        self.room_code = None;
                        self.connection_status = Some("Desconectado.".to_owned());
                        self.connection_error = Some(error);
                        disconnect = true;
                    } else if self.pending_signaling.is_some() {
                        if let (Some(signaling), Some((_, token))) =
                            (&self.signaling, &self.incoming_transfer)
                        {
                            let _ = signaling.reject_host_transfer(token.clone());
                        }
                        self.pending_signaling = None;
                        self.pending_room_adopted = false;
                        self.incoming_transfer = None;
                        self.handoff_error = Some(error);
                    } else if self.outgoing_transfer.is_some() || self.incoming_transfer.is_some() {
                        if self
                            .outgoing_transfer
                            .as_ref()
                            .is_some_and(|(_, token)| token.is_empty())
                        {
                            self.outgoing_transfer = None;
                        }
                        self.handoff_error = Some(error);
                    } else if !matches!(&self.screen_share_role, ScreenShareRole::Idle) {
                        self.stop_screen_share(false);
                        self.screen_share_status = Some(error);
                    } else if self
                        .diagnostic_status
                        .as_deref()
                        .is_some_and(|status| status.starts_with("Enviando sinal"))
                    {
                        self.diagnostic_status = Some(error);
                    } else {
                        self.connection_error = Some(error);
                    }
                }
                SignalingEvent::Signal {
                    kind: SignalKind::Diagnostic,
                    payload,
                } => {
                    tracing::debug!("Sinal de diagnóstico recebido; payload omitido");
                    match payload.as_str() {
                        "diagnostic-ping-v1" => {
                            self.diagnostic_status = Some(
                                "Sinal recebido; enviando confirmação ao seu amigo.".to_owned(),
                            );
                            acknowledge_diagnostic = true;
                        }
                        "diagnostic-pong-v1" => {
                            self.diagnostic_status = Some(
                                "Seu amigo confirmou o recebimento do sinal de diagnóstico."
                                    .to_owned(),
                            );
                        }
                        _ => {}
                    }
                }
                SignalingEvent::Signal { kind, payload } => {
                    tracing::debug!(signal_kind = ?kind, payload_bytes = payload.len(), "Sinal de negociação recebido; payload omitido");
                    if matches!(
                        kind,
                        SignalKind::Offer
                            | SignalKind::Answer
                            | SignalKind::IceCandidate
                            | SignalKind::ScreenShareRequest
                            | SignalKind::ScreenShareAccept
                            | SignalKind::ScreenShareBusy
                            | SignalKind::ScreenShareStopped
                    ) {
                        self.handle_screen_share_signal(kind, payload, context);
                    } else {
                        self.connection_status = Some("Sinal de conexão recebido.".to_owned());
                    }
                }
                SignalingEvent::Error(error) => {
                    tracing::error!(error = %error, "Conexão de sinalização falhou durante a sala");
                    if self.control_mesh.is_some() && self.room_code.is_some() {
                        self.connecting = false;
                        self.starting_host = false;
                        self.signaling = None;
                        self.connection_status = Some("Servidor de sinalização desconectado; mantendo os canais diretos para eleger outro anfitrião.".to_owned());
                        self.connection_error = Some(error);
                        continue;
                    }
                    self.connecting = false;
                    self.starting_host = false;
                    self.hosting_locally = false;
                    self.peer_connected = false;
                    self.room_code = None;
                    self.connection_status = Some("Desconectado.".to_owned());
                    self.connection_error = Some(error);
                    disconnect = true;
                }
                SignalingEvent::Disconnected => {
                    tracing::warn!("Conexão de sinalização encerrada durante a sala");
                    if self.control_mesh.is_some() && self.room_code.is_some() {
                        self.connecting = false;
                        self.signaling = None;
                        self.connection_status = Some("Servidor de sinalização desconectado; aguardando a eleição pela malha direta.".to_owned());
                        continue;
                    }
                    self.connecting = false;
                    self.starting_host = false;
                    self.hosting_locally = false;
                    self.peer_connected = false;
                    self.room_code = None;
                    self.connection_status = Some("Conexão com o servidor encerrada.".to_owned());
                    disconnect = true;
                }
            }
        }

        if acknowledge_diagnostic {
            if let Some(signaling) = &self.signaling {
                if let Err(error) = signaling.acknowledge_diagnostic() {
                    self.diagnostic_status = Some(error);
                }
            }
        }

        if disconnect {
            self.stop_screen_share(false);
            self.signaling = None;
            self.pending_signaling = None;
            self.screen_picker = None;
            if let Some(mut capture) = self.screen_capture.take() {
                let _ = capture.stop();
            }
            self.screen_texture = None;
        }
    }

    fn request_leave(&mut self, context: &egui::Context) {
        self.stop_screen_share(true);
        if self.hosting_locally && self.peer_connected && self.room_mode == RoomMode::InternetTest {
            self.leave_room();
            self.connection_status = Some(
                "Sala de Internet encerrada; o anfitrião saiu e ela não terá sucessor.".to_owned(),
            );
        } else if self.hosting_locally && self.peer_connected {
            if let Some(mesh) = self
                .control_mesh
                .as_ref()
                .filter(|_| !self.control_mesh_failed)
            {
                if !mesh.leave_normally() {
                    self.leave_room();
                    self.connection_status = Some(
                        "Voce saiu; nao foi possivel iniciar a eleicao automatica.".to_owned(),
                    );
                    if self.close_after_transfer {
                        self.allow_window_close = true;
                        context.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    return;
                }
                self.leave_after_handoff = true;
                self.connection_status = Some(
                    "Elegendo automaticamente o próximo anfitrião; esta sala continuará ativa…"
                        .to_owned(),
                );
            } else {
                self.leave_room();
                self.connection_status = Some(
                    "Você saiu. A malha de controle estava indisponível, então não foi possível transferir a hospedagem.".to_owned(),
                );
            }
        } else if self.incoming_transfer.is_some() {
            self.reject_incoming_transfer();
            self.leave_room();
        } else {
            self.leave_room();
        }
        if self.close_after_transfer && self.room_code.is_none() {
            self.allow_window_close = true;
            context.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    fn end_room_explicitly(&mut self, context: &egui::Context) {
        self.stop_screen_share(true);
        self.ending_room_explicitly = true;
        let end_requested = self
            .control_mesh
            .as_ref()
            .filter(|_| !self.control_mesh_failed)
            .is_some_and(ControlMesh::end_room);
        if end_requested {
            self.connection_status =
                Some("Encerrando a sala para todos os participantes…".to_owned());
        } else {
            self.leave_room();
            self.connection_status = Some("Sala encerrada por você.".to_owned());
        }
        if self.close_after_transfer && self.room_code.is_none() {
            self.allow_window_close = true;
            context.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    fn request_host_transfer(&mut self) {
        let Some(code) = self.room_code.clone() else {
            self.handoff_error = Some("Você não está em uma sala.".to_owned());
            return;
        };
        let Some(signaling) = &self.signaling else {
            self.handoff_error = Some("A conexão da sala não está ativa.".to_owned());
            return;
        };
        match signaling.request_host_transfer() {
            Ok(()) => {
                self.outgoing_transfer = Some((code, String::new()));
                self.handoff_error = None;
                self.connection_status = Some(
                    "Solicitação enviada; aguardando o aceite do outro participante.".to_owned(),
                );
            }
            Err(error) => self.handoff_error = Some(error),
        }
    }

    fn accept_incoming_transfer(&mut self) {
        let Some((code, token)) = self.incoming_transfer.clone() else {
            return;
        };
        self.close_after_transfer = false;
        match SignalingClient::adopt_transfer(code, token) {
            Ok(client) => {
                self.pending_signaling = Some(client);
                self.pending_room_adopted = false;
                self.handoff_error = None;
                self.connection_status = Some("Iniciando o servidor neste computador…".to_owned());
            }
            Err(error) => self.handoff_error = Some(error),
        }
    }

    fn reject_incoming_transfer(&mut self) {
        if let (Some(signaling), Some((_, token))) = (&self.signaling, &self.incoming_transfer) {
            let _ = signaling.reject_host_transfer(token.clone());
        }
        self.pending_signaling = None;
        self.pending_room_adopted = false;
        self.incoming_transfer = None;
    }

    fn handle_window_close(&mut self, context: &egui::Context) {
        let close_requested = context.input(|input| input.viewport().close_requested());
        if !close_requested || self.allow_window_close {
            return;
        }

        if (self.hosting_locally && self.peer_connected)
            || self.outgoing_transfer.is_some()
            || self.incoming_transfer.is_some()
            || self.pending_signaling.is_some()
        {
            context.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.close_after_transfer = true;
            if self.hosting_locally && self.peer_connected && self.outgoing_transfer.is_none() {
                self.request_leave(context);
            }
        } else {
            self.allow_window_close = true;
        }
    }

    fn show_handoff_panel(&mut self, ui: &mut egui::Ui, context: &egui::Context) {
        if let Some((code, _)) = &self.incoming_transfer {
            let code = code.clone();
            let accepting = self.pending_signaling.is_some();
            ui.add_space(10.0);
            ui.group(|ui| {
                ui.heading("Pedido para assumir a hospedagem");
                ui.label(format!("O anfitrião quer sair da sala {code} e está pedindo que você inicie o servidor neste computador."));
                if accepting {
                    ui.label("Iniciando o novo servidor e aguardando confirmação…");
                } else {
                    ui.horizontal(|ui| {
                        if ui.button("Aceitar e assumir").clicked() {
                            self.accept_incoming_transfer();
                        }
                        if ui.button("Recusar").clicked() {
                            self.reject_incoming_transfer();
                            if self.close_after_transfer {
                                self.leave_room();
                                self.allow_window_close = true;
                                context.send_viewport_cmd(egui::ViewportCommand::Close);
                            } else {
                                self.handoff_error = Some("Transferência recusada; você continua na sala.".to_owned());
                            }
                        }
                    });
                }
            });
        }

        if self.outgoing_transfer.is_some() {
            ui.add_space(10.0);
            ui.group(|ui| {
                ui.heading("Transferindo a hospedagem");
                ui.label("A sala continua ativa neste computador enquanto o outro participante aceita e inicia o novo servidor.");
                ui.horizontal(|ui| {
                    let can_cancel = self.outgoing_transfer.as_ref().is_some_and(|(_, token)| !token.is_empty());
                    if ui.add_enabled(can_cancel, egui::Button::new("Cancelar transferência")).clicked() {
                        if let (Some(signaling), Some((_, token))) = (&self.signaling, &self.outgoing_transfer) {
                            if let Err(error) = signaling.cancel_host_transfer(token.clone()) {
                                self.handoff_error = Some(error);
                            }
                        }
                    }
                    if ui.button("Encerrar sala").clicked() {
                        self.end_room_explicitly(context);
                    }
                });
            });
        }

        if let Some(error) = self.handoff_error.clone() {
            ui.add_space(10.0);
            ui.group(|ui| {
                ui.colored_label(
                    egui::Color32::from_rgb(190, 55, 55),
                    format!("Transferência: {error}"),
                );
                ui.horizontal(|ui| {
                    if self.hosting_locally
                        && self.peer_connected
                        && ui.button("Tentar novamente").clicked()
                    {
                        self.request_host_transfer();
                    }
                    if ui
                        .button(if self.close_after_transfer {
                            "Cancelar fechamento e ficar"
                        } else {
                            "Continuar na sala"
                        })
                        .clicked()
                    {
                        if let (Some(signaling), Some((_, token))) =
                            (&self.signaling, &self.outgoing_transfer)
                        {
                            let _ = signaling.cancel_host_transfer(token.clone());
                        }
                        self.outgoing_transfer = None;
                        self.handoff_error = None;
                        self.close_after_transfer = false;
                    }
                    if self.hosting_locally
                        && ui
                            .button(if self.close_after_transfer {
                                "Encerrar sala e fechar"
                            } else {
                                "Encerrar sala e sair"
                            })
                            .clicked()
                    {
                        self.end_room_explicitly(context);
                    }
                });
            });
        }
    }

    fn leave_room(&mut self) {
        tracing::info!("Saindo da sala e liberando recursos locais");
        self.stop_microphone();
        self.stop_screen_share(true);
        self.screen_picker = None;
        if let Some(mut capture) = self.screen_capture.take() {
            let _ = capture.stop();
        }
        self.screen_texture = None;
        self.room_code = None;
        self.participants.clear();
        self.current_leader_id.clear();
        self.room_mode = RoomMode::Local;
        self.control_queue.clear();
        self.control_mesh = None;
        self.pending_election_epoch = None;
        self.pending_election_reconnect = false;
        self.leave_after_handoff = false;
        self.ending_room_explicitly = false;
        self.join_code.clear();
        self.code_copied = false;
        self.microphone_error = None;
        self.screen_status = None;
        self.screen_share_status = None;
        self.pending_signaling = None;
        self.signaling = None;
        self.pending_room_adopted = false;
        self.hosting_locally = false;
        self.starting_host = false;
        self.incoming_transfer = None;
        self.outgoing_transfer = None;
        self.connecting = false;
        self.peer_connected = false;
        self.connection_status = Some("Desconectado.".to_owned());
        self.connection_error = None;
        self.diagnostic_status = None;
        self.handoff_error = None;
    }

    fn export_logs(&mut self) {
        tracing::info!("Usuário solicitou exportação dos logs");
        let now = time::OffsetDateTime::now_utc();
        let timestamp = format!(
            "{:04}{:02}{:02}-{:02}{:02}{:02}",
            now.year(),
            u8::from(now.month()),
            now.day(),
            now.hour(),
            now.minute(),
            now.second()
        );
        let selected = rfd::FileDialog::new()
            .set_file_name(format!("p2p-diagnostico-{timestamp}.log"))
            .add_filter("Arquivo de log", &["log"])
            .save_file();
        let Some(selected) = selected else {
            self.logging.export_message = None;
            tracing::debug!("Exportação de logs cancelada pelo usuário");
            return;
        };

        let mut destination = selected;
        if !destination
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("log"))
        {
            destination.set_extension("log");
        }
        if self.host_addresses.is_empty() {
            self.refresh_host_addresses();
        }
        let snapshot = self.diagnostic_snapshot();
        match self.logging.export_to(&destination, &snapshot) {
            Ok(()) => {
                self.logging.export_message =
                    Some(format!("Logs exportados em {}", destination.display()));
                tracing::info!("Exportação de logs concluída; caminho não registrado");
            }
            Err(error) => {
                self.logging.export_message =
                    Some(format!("Não foi possível exportar os logs: {error}"));
                tracing::error!(error = %error, "Falha ao exportar logs");
            }
        }
    }

    fn diagnostic_snapshot(&self) -> DiagnosticSnapshot {
        let share_state = match &self.screen_share_role {
            ScreenShareRole::Idle => "inativo",
            ScreenShareRole::Requesting { .. } => "solicitando",
            ScreenShareRole::Sending { .. } => "enviando",
            ScreenShareRole::Receiving { .. } => "recebendo",
        };
        let metrics = &self.screen_share_metrics;
        DiagnosticSnapshot {
            log_directory: self.logging.log_directory_label().to_owned(),
            logging_status: self
                .logging
                .startup_message
                .clone()
                .unwrap_or_else(|| "ativo".to_owned()),
            room_mode: match self.room_mode {
                RoomMode::Local => "local/Radmin",
                RoomMode::InternetTest => "internet (teste)",
            }
            .to_owned(),
            connected_to_signaling: self.signaling.is_some() && !self.connecting,
            participant_count: self.participants.len(),
            signaling_address: safe_signaling_endpoint(&self.server_url),
            stun_server: safe_stun_endpoint(&self.stun_server_url),
            microphone_active: self.microphone.is_some(),
            microphone_level_dbfs: self.microphone_level_dbfs,
            microphone_gain_db: self.monitor_gain_db,
            adapters: self
                .host_addresses
                .iter()
                .map(|address| (address.adapter.clone(), address.ipv4.to_string()))
                .collect(),
            screen_share_state: share_state.to_owned(),
            screen_metrics: format!(
                "P2P={}, ICE local/remoto={}/{}, ICE srflx local/remoto={}/{}, quadros codificados/enviados/decodificados={}/{}/{}, pacotes recebidos={}, erros de decodificação={}, diagnóstico H.264={} ",
                metrics.p2p_connected,
                metrics.local_ice_candidates,
                metrics.remote_ice_candidates,
                metrics.local_srflx_candidates,
                metrics.remote_srflx_candidates,
                metrics.encoded_frames,
                metrics.sent_frames,
                metrics.decoded_frames,
                metrics.received_packets,
                metrics.decode_errors,
                metrics.h264_diagnostics
            ),
        }
    }
}

fn enumerate_host_addresses() -> Result<Vec<HostAddress>, String> {
    #[cfg(target_os = "windows")]
    {
        use std::net::IpAddr;

        let adapters = ipconfig::get_adapters()
            .map_err(|error| format!("Não foi possível listar adaptadores de rede: {error}"))?;
        let mut addresses = Vec::new();
        for adapter in adapters {
            if adapter.oper_status() != ipconfig::OperStatus::IfOperStatusUp {
                continue;
            }
            for address in adapter.ip_addresses() {
                let IpAddr::V4(ipv4) = address else {
                    continue;
                };
                if ipv4.is_unspecified() || ipv4.is_loopback() || ipv4.is_link_local() {
                    continue;
                }
                addresses.push(HostAddress {
                    adapter: adapter.friendly_name().to_owned(),
                    ipv4: *ipv4,
                });
            }
        }
        addresses.sort_by(|left, right| {
            left.adapter
                .cmp(&right.adapter)
                .then_with(|| left.ipv4.octets().cmp(&right.ipv4.octets()))
        });
        addresses.dedup_by(|left, right| left.ipv4 == right.ipv4);
        Ok(addresses)
    }
    #[cfg(not(target_os = "windows"))]
    {
        Ok(Vec::new())
    }
}

fn signaling_ws_url(input: &str) -> Result<String, String> {
    let value = input.trim();
    if value.is_empty() {
        return Err("O endereço do anfitrião está vazio.".to_owned());
    }
    let address = if let Some(address) = value.strip_prefix("ws://") {
        address
    } else if value.contains("://") {
        return Err("Use ws://; wss:// ainda não está disponível nesta etapa.".to_owned());
    } else {
        value
    };

    let address = address.trim_end_matches('/');
    if address.contains('/') || address.contains('?') || address.contains('#') {
        return Err("Informe somente o IPv4 ou nome do anfitrião, sem caminho.".to_owned());
    }

    let host = match address.rsplit_once(':') {
        Some((host, port)) if !host.contains(':') => {
            if port != "9000" {
                return Err("A porta do servidor é fixa em 9000.".to_owned());
            }
            host
        }
        Some(_) => {
            return Err(
                "IPv6 não está disponível nesta etapa; informe um IPv4 ou nome DDNS.".to_owned(),
            );
        }
        None => address,
    };

    if host.parse::<Ipv4Addr>().is_err() {
        if host.len() > 253 || host.is_empty() || !host.is_ascii() {
            return Err("Informe um IPv4 ou nome DDNS válido.".to_owned());
        }
        let valid_name = host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        });
        if !valid_name {
            return Err("Informe um IPv4 ou nome DDNS válido.".to_owned());
        }
    }

    Ok(format!("ws://{host}:9000"))
}

fn signaling_address_for_control(control_address: &str) -> String {
    control_address
        .strip_suffix(":9001")
        .map(|ip| format!("ws://{ip}:9000"))
        .unwrap_or_default()
}

impl Drop for ClientUi {
    fn drop(&mut self) {
        tracing::info!("Encerrando aplicativo e liberando capturas e conexões");
        self.updates.cancel_download();
        self.stop_microphone();
        self.stop_screen_share(true);
        self.screen_picker = None;
        if let Some(mut capture) = self.screen_capture.take() {
            let _ = capture.stop();
        }
        self.signaling = None;
    }
}

fn main() -> eframe::Result {
    let arguments: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    if let Some(exit_code) = update::helper_arguments(&arguments) {
        std::process::exit(exit_code);
    }
    let mut app = ClientUi::default();
    app.logging = LoggingState::initialize();
    logging::install_panic_hook();
    app.updates.check();
    app.monitor_gain_db = 6.0;
    app.microphone_level_dbfs = -60.0;
    app.stun_server_url = "stun:stun.l.google.com:19302".to_owned();

    tracing::info!("Interface gráfica sendo inicializada");

    eframe::run_ui_native(
        "P2P - Voz e tela",
        eframe::NativeOptions::default(),
        move |ui, _frame| {
            egui::CentralPanel::default().show(ui, |ui| app.show(ui));
        },
    )
}

fn format_bytes(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = KIB * 1024.0;
    if bytes >= MIB as u64 {
        format!("{:.1} MiB", bytes as f64 / MIB)
    } else if bytes >= KIB as u64 {
        format!("{:.1} KiB", bytes as f64 / KIB)
    } else {
        format!("{bytes} bytes")
    }
}

#[cfg(test)]
mod tests {
    use super::signaling_ws_url;

    #[test]
    fn signaling_address_accepts_ipv4_or_ddns_with_fixed_port() {
        assert_eq!(
            signaling_ws_url("203.0.113.10").unwrap(),
            "ws://203.0.113.10:9000"
        );
        assert_eq!(
            signaling_ws_url("ws://my-room.ddns.net:9000").unwrap(),
            "ws://my-room.ddns.net:9000"
        );
    }

    #[test]
    fn signaling_address_rejects_tls_ipv6_wrong_port_and_paths() {
        assert!(signaling_ws_url("wss://my-room.ddns.net").is_err());
        assert!(signaling_ws_url("2001:db8::1").is_err());
        assert!(signaling_ws_url("192.0.2.10:9001").is_err());
        assert!(signaling_ws_url("192.0.2.10/room").is_err());
    }
}
