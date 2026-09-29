#![windows_subsystem = "windows"]

mod audio_capture;
mod control_mesh;
mod logging;
mod mf_video;
mod screen_capture;
mod screen_sharing;
mod settings;
mod signaling_client;
mod turn_relay;
mod update;

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, Ipv4Addr, ToSocketAddrs};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use audio_capture::MicrophoneTest;
use control_mesh::{ControlEvent, ControlMesh, QueueEntry};
use eframe::egui;
use logging::{DiagnosticSnapshot, LoggingState, safe_signaling_endpoint, safe_stun_endpoint};
use screen_capture::{MonitorOption, PendingScreenCapture, ScreenCapture};
use screen_sharing::{ScreenShareEvent, ScreenShareMetrics, ScreenShareSession};
use settings::{AppSettings, VideoDecoderPreference};
use signaling_client::{SignalingClient, SignalingEvent};
use signaling_protocol::{ParticipantInfo, RoomMode, SignalKind};
use turn_relay::{TurnCredentials, TurnRelayServer, TurnRoomConfig};
use update::{UpdateEvent, UpdateManager, UpdateManifest};

const TURN_CONFIG_SIGNAL_PREFIX: &str = "p2p-turn-room-config-v1:";
const GROUP_SCREEN_MAX_AGGREGATE_BITRATE: u32 = 8_000_000;
const GROUP_SCREEN_MAX_PEER_BITRATE: u32 = 4_000_000;

fn group_share_bitrate(viewer_count: usize) -> u32 {
    (GROUP_SCREEN_MAX_AGGREGATE_BITRATE / viewer_count.max(1) as u32)
        .min(GROUP_SCREEN_MAX_PEER_BITRATE)
        .max(250_000)
}

fn group_screen_share_compatible(room_mode: RoomMode, participants: &[ParticipantInfo]) -> bool {
    room_mode == RoomMode::Local
        && (2..=8).contains(&participants.len())
        && participants
            .iter()
            .all(|participant| participant.supports_group_screen_share)
}

fn next_group_media_port(used_ports: &HashSet<u16>) -> Option<u16> {
    (9002..=9009).find(|port| !used_ports.contains(port))
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum SettingsCategory {
    #[default]
    Audio,
    Connection,
    Video,
    Updates,
}

#[derive(Clone, Default)]
enum UpdateStatus {
    #[default]
    Checking,
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
    use_turn_on_create: bool,
    room_mode: RoomMode,
    connecting: bool,
    connection_status: Option<String>,
    connection_error: Option<String>,
    signaling: Option<SignalingClient>,
    turn_server: Option<TurnRelayServer>,
    turn_room_config: Option<TurnRoomConfig>,
    turn_config_received: bool,
    turn_config_wait_started_at: Option<Instant>,
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
    preferred_control_ipv4: Option<Ipv4Addr>,
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
    video_decoder_preference: VideoDecoderPreference,
    microphone_error: Option<String>,
    microphone_monitor_error: Option<String>,
    microphone_audio_warning: bool,
    microphone_clipping_warning: bool,
    screen_capture: Option<ScreenCapture>,
    screen_picker: Option<PendingScreenCapture>,
    available_monitors: Vec<MonitorOption>,
    monitors_loaded: bool,
    selected_monitor: usize,
    dxgi_capture_error: Option<String>,
    screen_texture: Option<egui::TextureHandle>,
    show_local_preview: bool,
    capture_preview_enabled: Arc<AtomicBool>,
    screen_status: Option<String>,
    screen_pipeline_summary: String,
    screen_share_session: Option<ScreenShareSession>,
    screen_share_role: ScreenShareRole,
    screen_share_status: Option<String>,
    remote_screen_texture: Option<egui::TextureHandle>,
    remote_screen_sequence: u64,
    group_local_sharing: bool,
    group_available_shares: HashSet<String>,
    group_watched_shares: HashSet<String>,
    group_outbound_sessions: HashMap<String, ScreenShareSession>,
    group_inbound_sessions: HashMap<String, ScreenShareSession>,
    group_outbound_ports: HashMap<String, u16>,
    group_inbound_ports: HashMap<String, u16>,
    group_remote_textures: HashMap<String, egui::TextureHandle>,
    group_remote_sequences: HashMap<String, u64>,
    group_peer_status: HashMap<String, String>,
    focused_group_screen: Option<String>,
    last_group_metrics_log_at: Option<Instant>,
    screen_share_metrics: ScreenShareMetrics,
    logging: LoggingState,
    last_screen_metrics_log_at: Option<Instant>,
    last_control_link_state: Option<(usize, usize)>,
    last_control_metrics_log_at: Option<Instant>,
    last_audio_metrics_log_at: Option<Instant>,
    updates: UpdateManager,
    update_status: UpdateStatus,
    settings_error: Option<String>,
    settings_dirty: bool,
    settings_save_at: Option<Instant>,
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
    fn refresh_monitors(&mut self) {
        if self.monitors_loaded || self.screen_capture.is_some() {
            return;
        }
        self.monitors_loaded = true;
        match ScreenCapture::monitors() {
            Ok(monitors) => {
                tracing::info!(
                    count = monitors.len(),
                    "Monitores ativos enumerados para DXGI"
                );
                self.available_monitors = monitors;
            }
            Err(error) => {
                tracing::warn!(error = %error, "NÃ£o foi possÃ­vel enumerar monitores DXGI");
                self.dxgi_capture_error = Some(error);
            }
        }
    }

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
                    self.update_status = UpdateStatus::Failed(error);
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

    fn show_notice(ui: &mut egui::Ui, prefix: &str, message: &str) {
        ui.horizontal_wrapped(|ui| {
            ui.label(egui::RichText::new(prefix).strong());
            ui.label(message);
        });
    }

    fn apply_monochrome_style(ui: &mut egui::Ui) {
        let dark_mode = ui.visuals().dark_mode;
        let (selection_fill, selection_stroke, foreground) = if dark_mode {
            (
                egui::Color32::from_gray(66),
                egui::Color32::from_gray(210),
                egui::Color32::from_gray(225),
            )
        } else {
            (
                egui::Color32::from_gray(220),
                egui::Color32::from_gray(55),
                egui::Color32::from_gray(45),
            )
        };
        let visuals = ui.visuals_mut();
        visuals.selection.bg_fill = selection_fill;
        visuals.selection.stroke = egui::Stroke::new(1.0, selection_stroke);
        visuals.hyperlink_color = foreground;
        visuals.warn_fg_color = foreground;
        visuals.error_fg_color = foreground;
    }

    fn preferences_snapshot(&self) -> AppSettings {
        let mut settings = AppSettings::default();
        settings.server_url = self.server_url.clone();
        settings.stun_server_url = self.stun_server_url.clone();
        settings.monitor_gain_db = self.monitor_gain_db;
        settings.video_decoder_preference = self.video_decoder_preference;
        settings.show_local_preview = self.show_local_preview;
        settings.create_room_mode = self.create_room_mode;
        settings.use_turn_on_create = self.use_turn_on_create;
        settings.may_host = self.may_host;
        settings.control_ipv4 = self.preferred_control_ipv4;
        settings
    }

    fn apply_preferences(&mut self, preferences: AppSettings) {
        self.server_url = preferences.server_url;
        self.stun_server_url = preferences.stun_server_url;
        self.monitor_gain_db = preferences.monitor_gain_db;
        self.video_decoder_preference = preferences.video_decoder_preference;
        self.show_local_preview = preferences.show_local_preview;
        self.capture_preview_enabled
            .store(self.show_local_preview, Ordering::Relaxed);
        self.create_room_mode = preferences.create_room_mode;
        self.use_turn_on_create = preferences.use_turn_on_create;
        self.may_host = preferences.may_host;
        self.preferred_control_ipv4 = preferences.control_ipv4;
    }

    fn save_preferences(&mut self) {
        self.settings_dirty = true;
        self.settings_save_at = None;
        match settings::save(&self.preferences_snapshot()) {
            Ok(()) => {
                self.settings_dirty = false;
                self.settings_error = None;
                tracing::info!("Preferências do aplicativo salvas");
            }
            Err(error) => {
                tracing::error!(error = %error, "Não foi possível salvar as preferências do aplicativo");
                self.settings_error = Some(error);
            }
        }
    }

    fn show(&mut self, ui: &mut egui::Ui) {
        Self::apply_monochrome_style(ui);
        let context = ui.ctx().clone();
        let preferences_before_frame = self.preferences_snapshot();
        if self.microphone.is_some()
            || (self.screen_capture.is_some()
                && self.show_local_preview
                && (self.group_local_sharing
                    || matches!(&self.screen_share_role, ScreenShareRole::Sending { .. })))
            || self.screen_picker.is_some()
            || self.screen_share_session.is_some()
            || !self.group_inbound_sessions.is_empty()
            || !self.group_outbound_sessions.is_empty()
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
            context.request_repaint_after(Duration::from_millis(100));
        }
        self.refresh_microphone();
        self.refresh_monitors();
        self.refresh_screen(&context);
        self.refresh_signaling(&context);
        self.refresh_turn_state();
        self.refresh_screen_share(&context);
        self.refresh_control_mesh(&context);
        self.refresh_updates(&context);
        self.handle_window_close(&context);

        let mut open_settings = false;
        let mut close_settings = false;
        let mut export_logs = false;
        egui::Panel::top("app-header")
            .frame(egui::Frame::new().inner_margin(egui::Margin::symmetric(12, 8)))
            .show(ui, |ui| {
                Self::apply_monochrome_style(ui);
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.heading("P2P - Voz e tela");
                        ui.label(
                            "Salas locais ou teste pela internet; compartilhamento de tela P2P, sem áudio",
                        );
                    });
                    ui.with_layout(
                        egui::Layout::right_to_left(egui::Align::Center),
                        |ui| {
                            export_logs = ui.button("Exportar logs").clicked();
                            if self.settings_open {
                                close_settings = ui.button("Voltar").clicked();
                            } else {
                                open_settings = ui
                                    .add_enabled(
                                        self.screen_picker.is_none(),
                                        egui::Button::new("Configurações"),
                                    )
                                    .clicked();
                            }
                        },
                    );
                });
            });

        if export_logs {
            self.export_logs();
        }
        if open_settings {
            self.open_settings();
        } else if close_settings {
            self.close_settings();
        }

        if self.room_code.is_some() && !self.settings_open {
            egui::Panel::bottom("room-controls")
                .frame(egui::Frame::new().inner_margin(egui::Margin::symmetric(12, 8)))
                .show(ui, |ui| {
                    Self::apply_monochrome_style(ui);
                    self.show_room_toolbar(ui);
                });
        }

        let mut open_update_settings = false;
        let mut open_logs_directory = false;
        egui::CentralPanel::default().show(ui, |ui| {
            Self::apply_monochrome_style(ui);
            egui::ScrollArea::vertical().show(ui, |ui| {
                if let Some(error) = self.logging.take_write_error() {
                    self.logging.export_message = Some(format!(
                        "Falha ao gravar logs: {error}. Confira a pasta de logs em Diagnóstico."
                    ));
                }
                if let Some(message) = &self.logging.startup_message {
                    Self::show_notice(ui, "Aviso:", message);
                }
                if let Some(message) = &self.logging.export_message {
                    Self::show_notice(ui, "Logs:", message);
                }
                if let Some(message) = self.settings_error.clone() {
                    Self::show_notice(ui, "Preferências:", &message);
                    if ui.button("Tentar salvar preferências").clicked() {
                        self.save_preferences();
                    }
                }

                let update_notice = match &self.update_status {
                    UpdateStatus::Available(manifest) => {
                        Some(format!("A versão {} está disponível.", manifest.version))
                    }
                    UpdateStatus::Downloaded { manifest, .. } => Some(format!(
                        "A versão {} foi baixada; reinicie para aplicar.",
                        manifest.version
                    )),
                    _ => None,
                };
                if let Some(notice) = update_notice {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(notice).strong());
                        if ui.button("Ver atualização").clicked() {
                            open_update_settings = true;
                        }
                    });
                }
                ui.add_space(12.0);

                if open_update_settings {
                    self.open_settings();
                    self.settings_category = SettingsCategory::Updates;
                }
                if !self.settings_open && self.room_code.is_some() {
                    self.show_handoff_panel(ui, &context);
                }
                if self.settings_open {
                    self.show_settings(ui);
                } else if self.room_code.is_some() {
                    self.show_room(ui);
                } else {
                    self.show_home(ui);
                }

                self.show_diagnostics(ui, &mut open_logs_directory);
            });
        });
        if open_logs_directory {
            self.open_logs_directory();
        }

        if self.preferences_snapshot() != preferences_before_frame {
            self.settings_dirty = true;
            self.settings_save_at = Some(Instant::now() + Duration::from_millis(400));
        }
        if self.settings_dirty {
            if let Some(save_at) = self.settings_save_at {
                let now = Instant::now();
                if now >= save_at {
                    self.save_preferences();
                    context.request_repaint();
                } else {
                    context.request_repaint_after(save_at - now);
                }
            }
        }
    }

    fn show_home(&mut self, ui: &mut egui::Ui) {
        if !self.addresses_loaded {
            self.refresh_host_addresses();
        }

        ui.horizontal(|ui| {
            ui.heading("Modo da sala");
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
        ui.add_space(4.0);

        if self.create_room_mode == RoomMode::Local {
            ui.group(|ui| {
                ui.heading("Rede de controle da sala");
                ui.label("Escolha o endereço que seu amigo consegue alcançar.");
                self.show_control_address_picker(ui);
                ui.checkbox(
                    &mut self.may_host,
                    "Permitir que este computador seja escolhido para hospedar futuramente",
                );
                ui.collapsing("Requisitos de rede", |ui| {
                    ui.label("A interface selecionada será usada para controle e vídeo.");
                    ui.label("Libere TCP 9001 e UDP 9002–9009 no firewall do Windows.");
                    ui.label("Use Radmin VPN quando o amigo entrar pelo endereço Radmin; use Ethernet/Wi-Fi na rede local.");
                    ui.label("A permissão para hospedar só permite assumir a sala se o anfitrião sair.");
                });
            });
        } else {
            ui.group(|ui| {
                ui.heading("Teste controlado pela internet");
                ui.label("Até duas pessoas. O anfitrião precisa permanecer online.");
                ui.checkbox(
                    &mut self.use_turn_on_create,
                    "Usar TURN como alternativa se a conexão direta falhar",
                );
                Self::show_notice(
                    ui,
                    "Atenção:",
                    "A sinalização usa ws:// sem criptografia ou autenticação. As credenciais TURN também passam por esse canal. Use apenas em testes controlados com pessoas conhecidas.",
                );
                ui.collapsing("Endereço e portas", |ui| {
                    ui.label("Configure o IPv4 público ou nome DDNS e a URI STUN em Configurações > Conexão.");
                    ui.label("Encaminhe TCP 9000 no roteador e libere a porta no firewall. CGNAT pode impedir conexões de entrada.");
                    if self.use_turn_on_create {
                        ui.label("TURN usa UDP 3478 e UDP 50000–50100; encaminhe e libere essas portas.");
                        ui.label("A tela permanece direta quando possível; retransmitida, ela consome a banda do anfitrião.");
                    } else {
                        ui.label("Sem TURN, a tela depende de o ICE encontrar um caminho UDP direto usando STUN.");
                    }
                });
            });
        }

        ui.add_space(10.0);
        if ui.available_width() >= 620.0 {
            ui.columns(2, |columns| {
                self.show_create_room_card(&mut columns[0]);
                self.show_join_room_card(&mut columns[1]);
            });
        } else {
            self.show_create_room_card(ui);
            ui.add_space(8.0);
            self.show_join_room_card(ui);
        }

        if self.connecting {
            ui.label("Conectando ao servidor de sinalização…");
        }
        if let Some(status) = &self.connection_status {
            ui.label(status);
        }
        if let Some(error) = &self.connection_error {
            Self::show_notice(ui, "Erro de conexão:", error);
        }
        ui.collapsing("Como entrar", |ui| {
            ui.label("Em Configurações > Conexão, informe o IPv4 ou nome DDNS compartilhado pelo anfitrião.");
        });
    }

    fn show_create_room_card(&mut self, ui: &mut egui::Ui) {
        ui.group(|ui| {
            ui.heading("Criar sala");
            ui.label("Hospede neste computador e compartilhe o código com seu amigo.");
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
    }

    fn show_join_room_card(&mut self, ui: &mut egui::Ui) {
        ui.group(|ui| {
            ui.heading("Entrar em uma sala");
            ui.label("Digite o código da sala:");
            ui.horizontal(|ui| {
                let field_width = (ui.available_width() - 72.0).max(100.0);
                ui.add(
                    egui::TextEdit::singleline(&mut self.join_code)
                        .hint_text("Código da sala")
                        .desired_width(field_width),
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
    }

    fn show_diagnostics(&self, ui: &mut egui::Ui, open_logs_directory: &mut bool) {
        egui::CollapsingHeader::new("Diagnóstico")
            .default_open(false)
            .show(ui, |ui| {
                ui.label("Log desta execução");
                ui.horizontal_wrapped(|ui| {
                    ui.label(self.logging.current_log_file_label());
                    if let Some(path) = self.logging.current_log_file() {
                        if ui.small_button("Copiar caminho").clicked() {
                            ui.ctx().copy_text(path.display().to_string());
                        }
                    }
                    if ui.small_button("Abrir pasta de logs").clicked() {
                        *open_logs_directory = true;
                    }
                });

                if self.room_code.is_some() {
                    ui.separator();
                    self.show_screen_diagnostics(ui);
                }
            });
    }

    fn show_screen_diagnostics(&self, ui: &mut egui::Ui) {
        ui.heading("Transmissão de tela");
        if let Some(address) = self.host_addresses.get(self.selected_host_address) {
            ui.label(format!(
                "Adaptador de mídia UDP: {} ({})",
                address.ipv4, address.adapter
            ));
        }
        if self.room_mode == RoomMode::InternetTest {
            ui.label("Rede: TCP 9000 para sinalização; UDP 9002 para mídia direta.");
            ui.label(format!("STUN: {}", self.stun_server_url));
            if self.hosting_locally {
                ui.label(if self.turn_server.is_some() {
                    "TURN ativo neste PC; encaminhe UDP 3478 e UDP 50000–50100."
                } else {
                    "TURN desativado pelo anfitrião; a tela depende de P2P direto."
                });
            } else if self.turn_config_received {
                ui.label(
                    if self
                        .turn_room_config
                        .as_ref()
                        .is_some_and(|config| config.turn.is_some())
                    {
                        "O anfitrião habilitou TURN como alternativa."
                    } else {
                        "O anfitrião desativou TURN; a tela depende de P2P direto."
                    },
                );
            } else {
                ui.label("Aguardando a configuração de rede enviada pelo anfitrião.");
            }
            ui.label("Teste controlado: ws:// não criptografa a sinalização ou as credenciais temporárias do TURN.");
        } else {
            ui.label(
                "Rede local/Radmin: libere TCP 9001 e UDP 9002–9009 no firewall dos participantes.",
            );
        }

        if !self.screen_pipeline_summary.is_empty() {
            ui.label(&self.screen_pipeline_summary);
        }
        if !self.screen_share_metrics.encoder_backend.is_empty()
            || !self.screen_share_metrics.decoder_backend.is_empty()
        {
            ui.label(format!(
                "Encoder: {}. Decoder: {}. Preferência: {}.",
                self.screen_share_metrics.encoder_backend,
                self.screen_share_metrics.decoder_backend,
                self.screen_share_metrics.decoder_preference
            ));
            if let Some(reason) = &self.screen_share_metrics.encoder_fallback_reason {
                ui.label(format!("Fallback do encoder: {reason}"));
            }
            if let Some(reason) = &self.screen_share_metrics.decoder_fallback_reason {
                ui.label(format!("Fallback do decoder: {reason}"));
            }
        }

        match &self.screen_share_role {
            ScreenShareRole::Sending { .. } => {
                ui.label(format!(
                    "Sessão {} · SSRC {} · H.264: {} entradas, {} produzidos, {} enviados (IDR {}, P {}), {} descartados antes do IDR.",
                    self.screen_share_metrics.session_id,
                    self.screen_share_metrics.track_ssrc.unwrap_or_default(),
                    self.screen_share_metrics.encoder_input_frames,
                    self.screen_share_metrics.encoded_frames,
                    self.screen_share_metrics.sent_frames,
                    self.screen_share_metrics.sent_idr_frames,
                    self.screen_share_metrics.sent_delta_frames,
                    self.screen_share_metrics.dropped_before_initial_idr
                ));
                ui.label(&self.screen_share_metrics.selected_ice_pair);
                ui.label(format!(
                    "RTP enviado: {} pacotes, {} bytes.",
                    self.screen_share_metrics.outbound_rtp_packets,
                    self.screen_share_metrics.outbound_rtp_bytes
                ));
                ui.label(&self.screen_share_metrics.rtc_outbound_summary);
                ui.label(&self.screen_share_metrics.h264_diagnostics);
            }
            ScreenShareRole::Receiving { .. } => {
                ui.label(format!(
                    "Sessão {} · SSRC {} · recebidos {}, montados {}, entradas no decoder {}, decodificados {}, publicados {}, atualizações da prévia {}, erros H.264 {}.",
                    self.screen_share_metrics.session_id,
                    self.screen_share_metrics.track_ssrc.unwrap_or_default(),
                    self.screen_share_metrics.received_packets,
                    self.screen_share_metrics.received_delta_frames,
                    self.screen_share_metrics.decoder_input_frames,
                    self.screen_share_metrics.decoded_frames,
                    self.screen_share_metrics.published_frames,
                    self.screen_share_metrics.ui_texture_updates,
                    self.screen_share_metrics.decode_errors
                ));
                ui.label(&self.screen_share_metrics.selected_ice_pair);
                ui.label(format!(
                    "RTP recebido: {} pacotes, {} bytes; perda {}, jitter {:.1} ms.",
                    self.screen_share_metrics.inbound_rtp_packets,
                    self.screen_share_metrics.inbound_rtp_bytes,
                    self.screen_share_metrics.inbound_rtp_lost,
                    self.screen_share_metrics.inbound_rtp_jitter_ms
                ));
                ui.label(&self.screen_share_metrics.rtc_inbound_summary);
                if let Some(error) = &self.screen_share_metrics.last_decode_error {
                    ui.label(format!("Último erro H.264: {error}"));
                }
                ui.label(&self.screen_share_metrics.h264_diagnostics);
                let recovery_time = self
                    .screen_share_metrics
                    .last_recovery_time_millis
                    .map_or_else(
                        || "ainda não disponível".to_owned(),
                        |ms| format!("{ms} ms"),
                    );
                ui.label(format!(
                    "Recuperação: PLI enviado/recebido {}/{}, fila cheia {}, ressincronizações {}, IDRs decodificados {}, último tempo até IDR {}.",
                    self.screen_share_metrics.pli_requests_sent,
                    self.screen_share_metrics.pli_requests_received,
                    self.screen_share_metrics.pli_queue_overflow,
                    self.screen_share_metrics.keyframe_resyncs,
                    self.screen_share_metrics.decoded_idr_frames,
                    recovery_time
                ));
            }
            ScreenShareRole::Idle | ScreenShareRole::Requesting { .. } => {}
        }

        if !matches!(&self.screen_share_role, ScreenShareRole::Idle)
            || self.screen_share_metrics.local_ice_candidates > 0
            || self.screen_share_metrics.remote_ice_candidates > 0
            || self.screen_share_status.is_some()
        {
            ui.label(format!(
                "ICE: {} candidatos locais ({} STUN, {} TURN), {} do amigo ({} STUN, {} TURN).",
                self.screen_share_metrics.local_ice_candidates,
                self.screen_share_metrics.local_srflx_candidates,
                self.screen_share_metrics.local_relay_candidates,
                self.screen_share_metrics.remote_ice_candidates,
                self.screen_share_metrics.remote_srflx_candidates,
                self.screen_share_metrics.remote_relay_candidates
            ));
        }

        if self.room_mode == RoomMode::Local && !self.control_queue.is_empty() {
            ui.separator();
            ui.label("Fila de sucessão · métricas dos enlaces");
            let mut queue = self.control_queue.clone();
            queue.sort_by(|left, right| {
                right
                    .eligible
                    .cmp(&left.eligible)
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
                    if candidate.eligible {
                        ""
                    } else {
                        " (inelegível)"
                    }
                ));
            }
        }
    }

    fn show_room(&mut self, ui: &mut egui::Ui) {
        let mut participants = self.participants.clone();
        participants.sort_by_key(|participant| participant.order);

        ui.horizontal(|ui| {
            ui.heading("Sala");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(if self.peer_connected {
                    "Conectado"
                } else {
                    "Aguardando participante"
                });
                ui.label(if self.room_mode == RoomMode::InternetTest {
                    "Internet · até 2 pessoas"
                } else {
                    "Rede local / Radmin · até 8 pessoas"
                });
            });
        });

        if self.room_mode == RoomMode::InternetTest {
            Self::show_notice(
                ui,
                "Teste controlado:",
                "A sinalização usa ws:// sem criptografia ou autenticação. Use apenas com pessoas conhecidas.",
            );
        }
        if let Some(error) = &self.connection_error {
            Self::show_notice(ui, "Erro de conexão:", error);
        }
        if let Some(status) = &self.screen_share_status {
            Self::show_notice(ui, "Compartilhamento:", status);
        }
        if let Some(status) = &self.screen_status {
            Self::show_notice(ui, "Captura:", status);
        }
        match &self.screen_share_role {
            ScreenShareRole::Sending { .. } => {
                ui.label(self.screen_route_status(
                    "transmitindo a tela.",
                    "Compartilhamento aceito; negociando a conexão WebRTC.",
                ));
            }
            ScreenShareRole::Receiving { .. } => {
                ui.label(self.screen_route_status(
                    "recebendo a tela.",
                    "Compartilhamento aceito; negociando a conexão WebRTC.",
                ));
            }
            ScreenShareRole::Requesting { .. } => {
                ui.label("Pedido de compartilhamento enviado; aguardando resposta.");
            }
            ScreenShareRole::Idle => {}
        }
        if self.screen_share_metrics.decode_errors > 0
            && matches!(&self.screen_share_role, ScreenShareRole::Receiving { .. })
        {
            Self::show_notice(
                ui,
                "Aviso de vídeo:",
                &format!(
                    "{} erros H.264 nesta sessão; veja Diagnóstico.",
                    self.screen_share_metrics.decode_errors
                ),
            );
        }

        egui::CollapsingHeader::new("Detalhes da sala")
            .default_open(false)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(format!("Código: {}", self.room_code.as_deref().unwrap_or("")));
                    if ui.button("Copiar código").clicked() {
                        if let Some(code) = &self.room_code {
                            ui.ctx().copy_text(code.clone());
                            self.code_copied = true;
                        }
                    }
                });
                if self.code_copied {
                    ui.small("Código copiado para a área de transferência.");
                }
                ui.label(
                    self.connection_status
                        .as_deref()
                        .unwrap_or("Conectado ao servidor."),
                );
                ui.label(if self.hosting_locally {
                    "Este computador está hospedando a sala."
                } else {
                    "Você entrou na sala hospedada por outro participante."
                });

                ui.separator();
                ui.heading("Convite");
                if self.hosting_locally && self.room_mode == RoomMode::InternetTest {
                    match signaling_ws_url(&self.server_url) {
                        Ok(url) => {
                            ui.horizontal_wrapped(|ui| {
                                ui.monospace(&url);
                                if ui.button("Copiar endereço").clicked() {
                                    ui.ctx().copy_text(url);
                                }
                            });
                        }
                        Err(_) => Self::show_notice(
                            ui,
                            "Endereço necessário:",
                            "Configure o IPv4 público ou DDNS em Configurações > Conexão.",
                        ),
                    }
                } else if self.hosting_locally {
                    ui.label("Escolha o endereço que seu amigo consegue alcançar.");
                    self.show_host_address_picker(ui);
                } else {
                    ui.label("Use o endereço informado pelo anfitrião em Configurações > Conexão.");
                }

                ui.separator();
                ui.heading("Participantes e sucessão");
                if participants.is_empty() {
                    ui.label("Aguardando participantes…");
                } else {
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
                }
                if self.room_mode == RoomMode::InternetTest {
                    ui.small("Esta sala aceita duas pessoas e termina quando o anfitrião sai ou perde a conexão.");
                } else if participants.iter().all(|participant| !participant.may_host) {
                    ui.small("Ninguém autorizou hospedagem automática; a sala termina se o anfitrião sair.");
                }
                if self.room_mode == RoomMode::Local {
                    if let Some(status) = &self.control_status {
                        ui.small(status);
                    }
                    let mut queue = self.control_queue.clone();
                    queue.sort_by(|left, right| {
                        right
                            .eligible
                            .cmp(&left.eligible)
                            .then_with(|| left.loss_percent.total_cmp(&right.loss_percent))
                            .then_with(|| left.jitter_ms.total_cmp(&right.jitter_ms))
                            .then_with(|| left.latency_ms.total_cmp(&right.latency_ms))
                            .then_with(|| left.participant.order.cmp(&right.participant.order))
                    });
                    for (index, candidate) in queue.iter().enumerate() {
                        ui.label(format!(
                            "{}. {}{}",
                            index + 1,
                            candidate.participant.display_name,
                            if candidate.eligible { "" } else { " (inelegível)" }
                        ));
                    }
                }

                if self.peer_connected && ui.button("Testar sinalização").clicked() {
                    self.diagnostic_status = Some("Enviando sinal de diagnóstico…".to_owned());
                    if let Some(signaling) = &self.signaling {
                        if let Err(error) = signaling.send_diagnostic() {
                            self.diagnostic_status = Some(error);
                        }
                    }
                }
                if let Some(status) = &self.diagnostic_status {
                    ui.small(status);
                }
                if self.hosting_locally && self.room_mode == RoomMode::Local {
                    ui.separator();
                    if ui
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
            });

        ui.add_space(8.0);
        ui.heading("Participantes");
        if participants.is_empty() {
            ui.label("Aguardando participantes…");
        } else {
            egui::ScrollArea::horizontal()
                .id_salt("room-participants")
                .max_height(48.0)
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        for participant in &participants {
                            egui::Frame::group(ui.style()).show(ui, |ui| {
                                ui.set_min_width(150.0);
                                ui.label(&participant.display_name);
                            });
                        }
                    });
                });
        }

        self.show_group_watch_controls(ui, &participants);

        ui.add_space(8.0);
        let stage_width = ui.available_width();
        let stage_height = ui.available_height().clamp(280.0, 680.0);
        egui::Frame::group(ui.style()).show(ui, |stage| {
            stage.set_min_size(egui::vec2(stage_width, stage_height));
            if self.group_sharing_compatible() {
                self.show_group_screen_stage(stage, stage_height);
            } else {
                match &self.screen_share_role {
                    ScreenShareRole::Receiving { .. } => {
                        if let Some(texture) = &self.remote_screen_texture {
                            let available = stage.available_size();
                            let source = texture.size_vec2();
                            let scale = (available.x / source.x)
                                .min(available.y / source.y)
                                .min(1.0);
                            let image_size = source * scale.max(0.01);
                            stage.vertical_centered(|ui| {
                                ui.add_space(((stage_height - image_size.y) * 0.5).max(0.0));
                                ui.add(
                                    egui::Image::new((texture.id(), source))
                                        .fit_to_exact_size(image_size),
                                );
                            });
                        } else {
                            stage.vertical_centered(|ui| {
                                ui.add_space(stage_height * 0.4);
                                ui.heading("Conectando à tela do participante…");
                                ui.label(
                                    "A imagem aparecerá aqui quando o primeiro quadro chegar.",
                                );
                            });
                        }
                    }
                    ScreenShareRole::Sending { .. } if self.show_local_preview => {
                        if let Some(texture) = &self.screen_texture {
                            let available = stage.available_size();
                            let source = texture.size_vec2();
                            let scale = (available.x / source.x)
                                .min(available.y / source.y)
                                .min(1.0);
                            let image_size = source * scale.max(0.01);
                            stage.vertical_centered(|ui| {
                                ui.add_space(((stage_height - image_size.y) * 0.5).max(0.0));
                                ui.add(
                                    egui::Image::new((texture.id(), source))
                                        .fit_to_exact_size(image_size),
                                );
                            });
                        } else {
                            stage.vertical_centered(|ui| {
                                ui.add_space(stage_height * 0.4);
                                ui.heading("Você está compartilhando");
                                ui.label("Preparando sua prévia local…");
                            });
                        }
                    }
                    ScreenShareRole::Sending { .. } => {
                        stage.vertical_centered(|ui| {
                            ui.add_space(stage_height * 0.4);
                            ui.heading("Você está compartilhando");
                            ui.label("Prévia local desativada para economizar recursos.");
                        });
                    }
                    ScreenShareRole::Requesting { .. } => {
                        stage.vertical_centered(|ui| {
                            ui.add_space(stage_height * 0.4);
                            ui.heading("Solicitação enviada");
                            ui.label("Aguardando resposta do outro participante…");
                        });
                    }
                    ScreenShareRole::Idle if self.peer_connected => {
                        stage.vertical_centered(|ui| {
                            ui.add_space(stage_height * 0.4);
                            ui.heading("Sala pronta");
                            ui.label("Compartilhe sua tela para começar.");
                        });
                    }
                    ScreenShareRole::Idle => {
                        stage.vertical_centered(|ui| {
                            ui.add_space(stage_height * 0.4);
                            ui.heading("Aguardando seu amigo");
                            ui.label("A tela compartilhada aparecerá aqui.");
                        });
                    }
                }
            }
        });

        if let Some(reason) = self
            .screen_capture
            .as_ref()
            .and_then(ScreenCapture::fallback_reason)
        {
            Self::show_notice(ui, "Fallback da captura:", &reason);
        }
    }

    fn show_room_toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            self.show_screen_source_menu(ui);
            if self.group_sharing_compatible() {
                let can_share = self.peer_connected
                    && self.screen_capture.is_some()
                    && self.screen_picker.is_none();
                if self.group_local_sharing {
                    if ui.button("Parar compartilhamento").clicked() {
                        self.set_group_share(false);
                    }
                } else if ui
                    .add_enabled(can_share, egui::Button::new("Compartilhar minha tela"))
                    .clicked()
                {
                    self.request_screen_share(ui.ctx());
                }
                let preview_changed = ui
                    .checkbox(&mut self.show_local_preview, "Mostrar minha prévia")
                    .changed();
                if preview_changed {
                    self.capture_preview_enabled
                        .store(self.show_local_preview, Ordering::Relaxed);
                    self.save_preferences();
                }
            } else {
                let share_role = self.screen_share_role.clone();
                match share_role {
                    ScreenShareRole::Idle => {
                        let allowed = self.participants.len() == 2
                            && self.peer_connected
                            && self.turn_configuration_ready()
                            && self.screen_capture.is_some()
                            && self.screen_share_session.is_none();
                        if ui
                            .add_enabled(allowed, egui::Button::new("Compartilhar tela"))
                            .clicked()
                        {
                            self.request_screen_share(ui.ctx());
                        }
                    }
                    ScreenShareRole::Requesting { .. } => {
                        if ui.button("Cancelar pedido").clicked() {
                            self.stop_screen_share(true);
                            self.screen_share_status =
                                Some("Pedido de compartilhamento cancelado.".to_owned());
                        }
                    }
                    ScreenShareRole::Sending { .. } | ScreenShareRole::Receiving { .. } => {
                        if ui.button("Parar compartilhamento").clicked() {
                            self.stop_screen_share(true);
                        }
                    }
                }
            }

            if matches!(&self.screen_share_role, ScreenShareRole::Sending { .. }) {
                let preview_changed = ui
                    .checkbox(&mut self.show_local_preview, "Mostrar minha prévia")
                    .changed();
                if preview_changed {
                    self.capture_preview_enabled
                        .store(self.show_local_preview, Ordering::Relaxed);
                    tracing::info!(
                        enabled = self.show_local_preview,
                        "Prévia local de compartilhamento alterada"
                    );
                }
            }

            let label =
                if self.hosting_locally && self.peer_connected && self.room_mode == RoomMode::Local
                {
                    "Sair e transferir"
                } else if self.hosting_locally
                    && self.peer_connected
                    && self.room_mode == RoomMode::InternetTest
                {
                    "Encerrar sala"
                } else {
                    "Sair da sala"
                };
            if ui
                .add_enabled(
                    self.screen_picker.is_none() && self.outgoing_transfer.is_none(),
                    egui::Button::new(label),
                )
                .clicked()
            {
                self.request_leave(ui.ctx());
            }
        });
    }

    fn show_screen_source_menu(&mut self, ui: &mut egui::Ui) {
        let label = if let Some(capture) = &self.screen_capture {
            format!("Fonte: {}", capture.backend_name())
        } else if self.screen_picker.is_some() {
            "Selecionando tela…".to_owned()
        } else {
            "Capturar tela".to_owned()
        };
        ui.menu_button(label, |ui| {
            if self.screen_capture.is_some() {
                if ui.button("Parar captura da tela").clicked() {
                    self.stop_screen_capture();
                    ui.close();
                }
                return;
            }
            if self.screen_picker.is_some() {
                ui.label("Aguardando o seletor do Windows…");
                return;
            }

            if !self.available_monitors.is_empty() {
                let selected = self.selected_monitor.min(self.available_monitors.len() - 1);
                self.selected_monitor = selected;
                egui::ComboBox::from_id_salt("room-monitor-source")
                    .selected_text(format!(
                        "{} ({}×{})",
                        self.available_monitors[selected].name,
                        self.available_monitors[selected].width,
                        self.available_monitors[selected].height
                    ))
                    .show_ui(ui, |ui| {
                        for (index, monitor) in self.available_monitors.iter().enumerate() {
                            ui.selectable_value(
                                &mut self.selected_monitor,
                                index,
                                format!(
                                    "{} ({}×{})",
                                    monitor.name, monitor.width, monitor.height
                                ),
                            );
                        }
                    });
                if ui.button("Capturar monitor por DXGI").clicked() {
                    self.dxgi_capture_error = None;
                    match ScreenCapture::start_monitor(
                        self.selected_monitor,
                        ui.ctx().clone(),
                        Arc::clone(&self.capture_preview_enabled),
                    ) {
                        Ok(capture) => {
                            self.screen_capture = Some(capture);
                            self.screen_status = Some(
                                "Captura DXGI ativa; o app não adiciona a borda de captura do Windows."
                                    .to_owned(),
                            );
                            tracing::info!(monitor_index = self.selected_monitor + 1, "Captura DXGI iniciada pela barra da sala");
                            ui.close();
                        }
                        Err(error) => {
                            tracing::error!(error = %error, monitor_index = self.selected_monitor + 1, "Falha ao iniciar captura DXGI; Windows Graphics Capture está disponível como alternativa");
                            self.dxgi_capture_error = Some(error.clone());
                            self.screen_status = Some(format!(
                                "Não foi possível capturar este monitor por DXGI: {error}"
                            ));
                        }
                    }
                }
            } else {
                ui.label("Nenhum monitor DXGI disponível.");
            }
            ui.small("A captura de janela usa a borda amarela de privacidade do Windows.");
            if ui.button("Selecionar janela ou monitor pelo Windows").clicked() {
                self.select_screen(ui.ctx());
                ui.close();
            }
            if self.dxgi_capture_error.is_some() {
                ui.small("O seletor do Windows pode ser usado como alternativa.");
            }
        });
    }
    fn show_settings(&mut self, ui: &mut egui::Ui) {
        ui.heading("Configurações");
        ui.add_space(8.0);
        if ui.available_width() >= 620.0 {
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.set_min_width(132.0);
                    self.show_settings_categories(ui, false);
                });
                ui.separator();
                ui.vertical(|ui| self.show_settings_content(ui));
            });
        } else {
            self.show_settings_categories(ui, true);
            ui.separator();
            self.show_settings_content(ui);
        }
    }

    fn show_settings_categories(&mut self, ui: &mut egui::Ui, horizontal: bool) {
        let mut show = |ui: &mut egui::Ui| {
            for (category, label) in [
                (SettingsCategory::Audio, "Áudio"),
                (SettingsCategory::Connection, "Conexão"),
                (SettingsCategory::Video, "Vídeo"),
                (SettingsCategory::Updates, "Atualizações"),
            ] {
                let selected = self.settings_category == category;
                if ui.selectable_label(selected, label).clicked() {
                    self.select_settings_category(category);
                }
            }
        };
        if horizontal {
            ui.horizontal_wrapped(|ui| show(ui));
        } else {
            ui.vertical(|ui| show(ui));
        }
    }

    fn show_settings_content(&mut self, ui: &mut egui::Ui) {
        match self.settings_category {
            SettingsCategory::Audio => self.show_audio_settings(ui),
            SettingsCategory::Connection => self.show_connection_settings(ui),
            SettingsCategory::Video => self.show_video_settings(ui),
            SettingsCategory::Updates => self.show_update_settings(ui),
        }
    }

    fn show_update_settings(&mut self, ui: &mut egui::Ui) {
        ui.heading("Atualizações");
        ui.label(format!("Versão instalada: {}", env!("CARGO_PKG_VERSION")));
        ui.hyperlink_to("Abrir GitHub Releases", UpdateManager::releases_page_url());
        ui.small("O app consulta o último release público e procura o arquivo p2p-client.exe.");
        ui.add_space(8.0);

        match self.update_status.clone() {
            UpdateStatus::Checking => {
                ui.label("Verificando se há uma versão nova…");
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
                    self.updates.apply(path, manifest.version.clone());
                }
            }
            UpdateStatus::PreparingToApply => {
                ui.label("Preparando a atualização ao lado do aplicativo…");
            }
            UpdateStatus::Applying => {
                ui.label("O aplicativo será fechado e reaberto com a nova versão.");
            }
            UpdateStatus::Failed(error) => {
                Self::show_notice(ui, "Erro de atualização:", &error);
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
                !operation_running,
                egui::Button::new("Verificar atualizações"),
            )
            .clicked()
        {
            tracing::info!("Usuário solicitou nova verificação de atualização");
            self.update_status = UpdateStatus::Checking;
            self.updates.check();
        }
        ui.separator();
        ui.small("O download usa HTTPS e valida tamanho e SHA-256 informados pelo GitHub. Não há assinatura digital: essas verificações detectam corrupção, mas não autenticam o publicador.");
        ui.small("As atualizações só são baixadas e aplicadas por sua escolha; o app não faz isso enquanto você está em uma sala.");
    }

    fn show_connection_settings(&mut self, ui: &mut egui::Ui) {
        ui.heading("Conexão");
        ui.group(|ui| {
            ui.label("Adaptador para controle e mídia");
            self.show_control_address_picker(ui);
            ui.small("O endereço escolhido vale para TCP 9001 e UDP 9002–9009.");
        });

        ui.add_space(8.0);
        ui.group(|ui| {
            ui.label("Endereço do anfitrião · porta 9000");
            ui.add(
                egui::TextEdit::singleline(&mut self.server_url)
                    .hint_text("IP público ou minha-sala.ddns.net")
                    .desired_width(ui.available_width().min(420.0)),
            );
            match signaling_ws_url(&self.server_url) {
                Ok(url) => {
                    ui.horizontal_wrapped(|ui| {
                        ui.monospace(&url);
                        if ui.button("Copiar endereço").clicked() {
                            ui.ctx().copy_text(url);
                        }
                    });
                }
                Err(error) if !self.server_url.trim().is_empty() => {
                    Self::show_notice(ui, "Endereço inválido:", &error);
                }
                Err(_) => {}
            }
        });

        ui.add_space(8.0);
        ui.group(|ui| {
            ui.label("Servidor STUN");
            ui.add(
                egui::TextEdit::singleline(&mut self.stun_server_url)
                    .hint_text("stun:stun.l.google.com:19302")
                    .desired_width(ui.available_width().min(420.0)),
            );
            if let Err(error) = screen_sharing::validate_stun_uri(&self.stun_server_url) {
                Self::show_notice(ui, "URI inválida:", &error);
            }
            ui.small("STUN ajuda a encontrar uma conexão direta; não retransmite vídeo.");
        });

        ui.collapsing("Ajuda de conexão", |ui| {
            ui.label("Na rede local/Radmin, informe o endereço compartilhado pelo anfitrião.");
            ui.label("Na Internet, informe o IPv4 público ou DDNS do anfitrião; o app não detecta o IP público.");
            ui.label("Para hospedar na Internet, encaminhe TCP 9000 e libere a porta no firewall. TURN também requer UDP 3478 e UDP 50000–50100; CGNAT pode impedir conexões de entrada.");
            ui.label("TURN é configurado pelo anfitrião. Não é necessário manter um notebook separado ligado.");
            ui.label("ws:// não criptografa a sinalização nem autentica os participantes. Use apenas testes controlados com pessoas conhecidas.");
            ui.label("O endereço fica salvo nas preferências locais deste aplicativo.");
        });
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
                Self::show_notice(ui, "Erro do microfone:", error);
            }
            if let Some(error) = &self.microphone_monitor_error {
                Self::show_notice(ui, "Retorno de áudio:", error);
            }
            if self.microphone_audio_warning {
                Self::show_notice(
                    ui,
                    "Aviso:",
                    "O retorno teve cortes por falta ou excesso de amostras. Pare e inicie o teste novamente.",
                );
            }
            if self.microphone_clipping_warning {
                Self::show_notice(
                    ui,
                    "Aviso:",
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

    fn show_video_settings(&mut self, ui: &mut egui::Ui) {
        ui.heading("Vídeo");
        ui.label("Escolha como decodificar a tela recebida:");

        ui.radio_value(
            &mut self.video_decoder_preference,
            VideoDecoderPreference::Automatic,
            "Automático",
        );
        ui.small("Começa com DXVA; após 2 s de aquecimento, muda para OpenH264 se publicar menos de 80% das entradas em uma janela de 3 s.");

        ui.radio_value(
            &mut self.video_decoder_preference,
            VideoDecoderPreference::PreferDxva,
            "Preferir DXVA",
        );
        ui.small("Mantém o decoder da GPU mesmo com FPS baixo. Usa CPU se DXVA estiver indisponível ou falhar.");

        ui.radio_value(
            &mut self.video_decoder_preference,
            VideoDecoderPreference::Cpu,
            "CPU (OpenH264)",
        );
        ui.small("Decodifica com o processador e não tenta inicializar o DXVA.");
        ui.separator();
        ui.small("A preferência é salva neste computador e vale na próxima sessão de compartilhamento. O decoder ativo e o motivo de fallback aparecem no diagnóstico da sala.");
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
        match PendingScreenCapture::begin(Arc::clone(&self.capture_preview_enabled)) {
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
        let preview_active = self.show_local_preview
            && (self.group_local_sharing
                || matches!(&self.screen_share_role, ScreenShareRole::Sending { .. }));
        self.capture_preview_enabled
            .store(preview_active, Ordering::Relaxed);
        if !preview_active {
            self.screen_texture = None;
        }

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

        if preview_active
            && let Some(frame) = capture.latest_frame()
            && !frame.rgba.is_empty()
        {
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

        let was_dxgi = capture.backend_name() == "DXGI Desktop Duplication";
        if let Some(result) = capture.poll_finished() {
            self.stop_screen_share(true);
            self.screen_capture = None;
            self.screen_texture = None;
            self.screen_status = Some(match result {
                Ok(()) => "A captura da tela foi encerrada pelo Windows.".to_owned(),
                Err(error) => {
                    if was_dxgi {
                        self.dxgi_capture_error = Some(error.clone());
                    }
                    format!("A captura da tela falhou: {error}")
                }
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
        if self.group_sharing_compatible() {
            self.set_group_share(true);
            context.request_repaint();
            return;
        }
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
        from_participant_id: Option<String>,
        stream_id: Option<String>,
        kind: SignalKind,
        payload: String,
        context: &egui::Context,
    ) {
        match kind {
            SignalKind::ScreenShareAvailable => {
                if let Some(peer_id) = from_participant_id.filter(|id| id != &self.participant_id) {
                    if self.group_sharing_compatible() {
                        self.group_available_shares.insert(peer_id);
                    }
                }
            }
            SignalKind::ScreenShareUnavailable => {
                if let Some(peer_id) = from_participant_id {
                    self.group_available_shares.remove(&peer_id);
                    self.group_watched_shares.remove(&peer_id);
                    self.group_inbound_sessions.remove(&peer_id);
                    self.group_inbound_ports.remove(&peer_id);
                    self.group_remote_textures.remove(&peer_id);
                    self.group_remote_sequences.remove(&peer_id);
                }
            }
            SignalKind::ScreenShareWatch => {
                let Some(viewer_id) = from_participant_id else {
                    return;
                };
                if !self.group_local_sharing || !self.group_sharing_compatible() {
                    return;
                }
                if self.group_outbound_sessions.contains_key(&viewer_id) {
                    return;
                }
                self.rebalance_group_outbound(context, Some(viewer_id));
            }
            SignalKind::ScreenShareUnwatch => {
                if let Some(viewer_id) = from_participant_id {
                    self.group_outbound_sessions.remove(&viewer_id);
                    self.group_outbound_ports.remove(&viewer_id);
                    self.rebalance_group_outbound(context, None);
                }
            }
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
                if self.room_mode == RoomMode::InternetTest && !self.turn_config_received {
                    let _ = self
                        .send_screen_share_signal(SignalKind::ScreenShareBusy, request.request_id);
                    self.screen_share_status = Some(
                        "Aguardando a configuração de mídia enviada pelo anfitrião.".to_owned(),
                    );
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

                let bind_ipv4 = match self.selected_media_ipv4() {
                    Ok(address) => address,
                    Err(error) => {
                        let _ = self.send_screen_share_signal(
                            SignalKind::ScreenShareBusy,
                            request.request_id,
                        );
                        self.screen_share_status = Some(error);
                        return;
                    }
                };

                match ScreenShareSession::new(
                    context.clone(),
                    bind_ipv4,
                    self.stun_server_for_room(),
                    self.turn_credentials_for_room(),
                    self.video_decoder_preference,
                ) {
                    Ok(session) => {
                        let metrics = session.metrics();
                        tracing::info!(
                            screen_share_session = metrics.session_id,
                            role = "receiver",
                            "Receptor de compartilhamento preparado"
                        );
                        self.screen_share_session = Some(session);
                        self.screen_share_metrics = ScreenShareMetrics::default();
                        self.last_screen_metrics_log_at = Some(Instant::now());
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
                let bind_ipv4 = match self.selected_media_ipv4() {
                    Ok(address) => address,
                    Err(error) => {
                        self.screen_share_role = ScreenShareRole::Idle;
                        self.screen_share_status = Some(error);
                        return;
                    }
                };
                match ScreenShareSession::new(
                    context.clone(),
                    bind_ipv4,
                    self.stun_server_for_room(),
                    self.turn_credentials_for_room(),
                    self.video_decoder_preference,
                ) {
                    Ok(session) => {
                        if let Some(capture) = self.screen_capture.as_ref() {
                            let _ = capture.take_performance_snapshot();
                        }
                        if let Err(error) = session.start_sending(source) {
                            session.stop();
                            self.screen_share_role = ScreenShareRole::Idle;
                            self.screen_share_status = Some(error);
                            return;
                        }
                        let metrics = session.metrics();
                        tracing::info!(
                            screen_share_session = metrics.session_id,
                            role = "sender",
                            "Emissor de compartilhamento preparado"
                        );
                        self.screen_share_session = Some(session);
                        self.last_screen_metrics_log_at = Some(Instant::now());
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
                if let Some(peer_id) = from_participant_id.as_deref().filter(|id| {
                    self.group_sharing_compatible()
                        && self.group_watched_shares.contains(*id)
                        && stream_id.as_deref() == Some(*id)
                }) {
                    self.group_inbound_sessions.remove(peer_id);
                    self.group_inbound_ports.remove(peer_id);
                    self.group_remote_textures.remove(peer_id);
                    self.group_remote_sequences.remove(peer_id);
                    self.group_peer_status.insert(
                        peer_id.to_owned(),
                        "A transmissão está sendo reconfigurada…".to_owned(),
                    );
                    return;
                }
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
                if self.group_sharing_compatible() {
                    self.handle_group_peer_signal(
                        from_participant_id,
                        stream_id,
                        kind,
                        payload,
                        context,
                    );
                } else if let Some(session) = &self.screen_share_session {
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

    fn send_screen_share_signal_to(
        &self,
        participant_id: &str,
        kind: SignalKind,
        payload: String,
    ) -> Result<(), String> {
        self.signaling
            .as_ref()
            .ok_or_else(|| "A conexão de sinalização não está disponível.".to_owned())?
            .send_signal_to(participant_id.to_owned(), kind, payload)
    }

    fn send_screen_share_signal_to_stream(
        &self,
        participant_id: &str,
        stream_id: String,
        kind: SignalKind,
        payload: String,
    ) -> Result<(), String> {
        self.signaling
            .as_ref()
            .ok_or_else(|| "A conexão de sinalização não está disponível.".to_owned())?
            .send_signal_to_stream(participant_id.to_owned(), stream_id, kind, payload)
    }

    fn send_turn_room_config(&self) {
        let Some(config) = &self.turn_room_config else {
            return;
        };
        let payload = match serde_json::to_string(config) {
            Ok(payload) => format!("{TURN_CONFIG_SIGNAL_PREFIX}{payload}"),
            Err(error) => {
                tracing::error!(error = %error, "Não foi possível serializar a configuração de mídia da sala");
                return;
            }
        };
        let result = self
            .signaling
            .as_ref()
            .ok_or_else(|| "A conexão de sinalização não está disponível.".to_owned())
            .and_then(|signaling| signaling.send_signal(SignalKind::Diagnostic, payload));
        match result {
            Ok(()) => tracing::info!(
                turn_enabled = config.turn.is_some(),
                "Configuração ICE da sala enviada ao outro participante; credenciais omitidas"
            ),
            Err(error) => tracing::error!(
                error = %error,
                "Não foi possível enviar a configuração ICE da sala"
            ),
        }
    }

    fn turn_credentials_for_room(&self) -> Option<TurnCredentials> {
        if self.room_mode != RoomMode::InternetTest {
            return None;
        }
        let mut credentials = self.turn_room_config.as_ref()?.turn.clone()?;
        if self.hosting_locally {
            // The host must not depend on router NAT loopback to reach its own TURN service.
            // The TURN server still advertises the configured public relay address.
            credentials.url = "turn:127.0.0.1:3478?transport=udp".to_owned();
        }
        Some(credentials)
    }

    fn turn_configuration_ready(&self) -> bool {
        self.room_mode != RoomMode::InternetTest || self.turn_config_received
    }

    fn screen_route_status(&self, connected: &str, negotiating: &str) -> String {
        if !self.screen_share_metrics.p2p_connected {
            return negotiating.to_owned();
        }
        match self.screen_share_metrics.route {
            Some(screen_sharing::MediaRoute::Turn) => {
                format!("Conexão retransmitida via TURN; {connected}")
            }
            Some(screen_sharing::MediaRoute::Direct) => {
                format!("Conexão direta P2P estabelecida; {connected}")
            }
            None => format!("Conexão WebRTC estabelecida; {connected}"),
        }
    }

    fn refresh_screen_share(&mut self, context: &egui::Context) {
        self.refresh_group_screen_shares(context);
        if let Some(session) = self.screen_share_session.as_ref() {
            self.screen_share_metrics = session.metrics();
            let should_log_metrics = self
                .last_screen_metrics_log_at
                .is_none_or(|last| last.elapsed() >= Duration::from_secs(5));
            if should_log_metrics {
                let metrics = &self.screen_share_metrics;
                let logged_at = Instant::now();
                let interval = self
                    .last_screen_metrics_log_at
                    .map_or(Duration::from_secs(5), |last| {
                        logged_at.saturating_duration_since(last)
                    });
                let interval_seconds = interval.as_secs_f64().max(0.001);
                let performance = session.take_performance_snapshot();
                let capture_performance =
                    if matches!(&self.screen_share_role, ScreenShareRole::Sending { .. }) {
                        self.screen_capture
                            .as_ref()
                            .map(ScreenCapture::take_performance_snapshot)
                            .unwrap_or_default()
                    } else {
                        Default::default()
                    };
                let capture_fps = capture_performance.processed_frames as f64 / interval_seconds;
                let capture_examined_frames = capture_performance
                    .processed_frames
                    .saturating_add(capture_performance.unchanged_frames);
                let encoder_fps = performance.encoded_frames as f64 / interval_seconds;
                let send_fps = performance.sent_frames as f64 / interval_seconds;
                let receive_fps = performance.assembled_access_units as f64 / interval_seconds;
                let receive_packet_rate = performance.received_packets as f64 / interval_seconds;
                let decode_fps = performance.decoded_frames as f64 / interval_seconds;
                let publish_fps = performance.published_frames as f64 / interval_seconds;
                let backend = self
                    .screen_capture
                    .as_ref()
                    .map(ScreenCapture::backend_name)
                    .unwrap_or("sem captura local");
                let average_ms = |nanos: u64, samples: u64| {
                    if samples == 0 {
                        0.0
                    } else {
                        nanos as f64 / samples as f64 / 1_000_000.0
                    }
                };
                let capture_readback_ms =
                    average_ms(capture_performance.readback_nanos, capture_examined_frames);
                let capture_resize_ms =
                    average_ms(capture_performance.resize_nanos, capture_examined_frames);
                let capture_gpu_ms = average_ms(
                    capture_performance.gpu_convert_nanos,
                    capture_examined_frames,
                );
                self.screen_pipeline_summary = format!(
                    "5 s: captura {capture_fps:.1} FPS únicos ({backend}; {} callbacks, {} imagens iguais, {} descartados); encoder {encoder_fps:.1} FPS (novos {}, repetidos {}, worker atrasado {}, sequências puladas {}); envio {send_fps:.1} FPS; recepção {receive_fps:.1} quadros/s ({receive_packet_rate:.0} pacotes/s), decoder {decode_fps:.1} FPS, prévia publicada {publish_fps:.1} FPS. Captura: leitura {capture_readback_ms:.1} ms, prévia {capture_resize_ms:.1} ms, GPU {capture_gpu_ms:.1} ms.",
                    capture_performance.received_frames,
                    capture_performance.unchanged_frames,
                    capture_performance.skipped_frames,
                    performance.new_capture_frames,
                    performance.repeated_capture_frames,
                    performance.encoder_worker_late_frames,
                    performance.skipped_capture_sequences,
                );
                tracing::info!(
                    screen_share_session = metrics.session_id,
                    role = match &self.screen_share_role {
                        ScreenShareRole::Sending { .. } => "sender",
                        ScreenShareRole::Receiving { .. } => "receiver",
                        ScreenShareRole::Requesting { .. } => "requesting",
                        ScreenShareRole::Idle => "idle",
                    },
                    track_ssrc = metrics.track_ssrc.unwrap_or_default(),
                    selected_ice_pair = %metrics.selected_ice_pair,
                    rtc_outbound = %metrics.rtc_outbound_summary,
                    rtc_inbound = %metrics.rtc_inbound_summary,
                    p2p_connected = metrics.p2p_connected,
                    local_ice = metrics.local_ice_candidates,
                    remote_ice = metrics.remote_ice_candidates,
                    local_relay = metrics.local_relay_candidates,
                    remote_relay = metrics.remote_relay_candidates,
                    route = ?metrics.route,
                    encoder_backend = %metrics.encoder_backend,
                    encoder_fallback = metrics.encoder_fallback_reason.as_deref().unwrap_or(""),
                    decoder_backend = %metrics.decoder_backend,
                    decoder_preference = %metrics.decoder_preference,
                    decoder_fallback = metrics.decoder_fallback_reason.as_deref().unwrap_or(""),
                    encoded = metrics.encoded_frames,
                    sent = metrics.sent_frames,
                    received_delta_frames = metrics.received_delta_frames,
                    decoded_delta_frames = metrics.decoded_delta_frames,
                    received_packets_total = metrics.received_packets,
                    received_packets_interval = performance.received_packets,
                    sequence_gap_packets_interval = performance.observed_sequence_gaps,
                    reordered_packets_recovered_interval = performance.recovered_reordered_packets,
                    unmatched_out_of_order_interval = performance.unmatched_out_of_order_packets,
                    duplicate_packets_interval = performance.duplicate_packets,
                    confirmed_missing_packets_interval = performance.confirmed_missing_packets,
                    late_after_confirmed_packets_interval = performance.late_after_confirmed_packets,
                    sequence_gap_resyncs_interval = performance.sequence_gap_resyncs,
                    rtc_rtp_loss_delta = performance.inbound_rtp_lost_delta,
                    assembled_access_units_interval = performance.assembled_access_units,
                    assembly_errors_interval = performance.assembly_errors,
                    decoded = metrics.decoded_frames,
                    decode_errors = metrics.decode_errors,
                    decode_errors_interval = performance.decode_errors,
                    decoded_interval = performance.decoded_frames,
                    pli_sent_interval = performance.pli_requests_sent,
                    pli_received_interval = performance.pli_requests_received,
                    pli_queue_overflow = metrics.pli_queue_overflow,
                    rtc_rtp_out_packets = metrics.outbound_rtp_packets,
                    rtc_rtp_out_bytes = metrics.outbound_rtp_bytes,
                    rtc_rtp_in_packets = metrics.inbound_rtp_packets,
                    rtc_rtp_in_bytes = metrics.inbound_rtp_bytes,
                    rtc_rtp_in_lost = metrics.inbound_rtp_lost,
                    rtc_rtp_in_jitter_ms = metrics.inbound_rtp_jitter_ms,
                    decoder_input_frames = metrics.decoder_input_frames,
                    decoder_input_interval = performance.decoder_input_frames,
                    decoder_no_output_frames = metrics.decoder_no_output_frames,
                    decoder_no_output_interval = performance.decoder_no_output_frames,
                    decoder_queue_drops = metrics.decoder_queue_drops,
                    decoder_queue_drops_interval = performance.decoder_queue_drops,
                    published_frames = metrics.published_frames,
                    published_interval = performance.published_frames,
                    ui_texture_updates = metrics.ui_texture_updates,
                    ui_texture_updates_interval = performance.ui_texture_updates,
                    h264 = %metrics.h264_diagnostics,
                    last_decode_error = metrics.last_decode_error.as_deref().unwrap_or(""),
                    interval_seconds,
                    capture_fps = capture_performance.processed_frames as f64 / interval_seconds,
                    capture_callbacks = capture_performance.received_frames,
                    capture_processed = capture_performance.processed_frames,
                    capture_unchanged = capture_performance.unchanged_frames,
                    capture_skipped = capture_performance.skipped_frames,
                    capture_readback_avg_ms = average_ms(capture_performance.readback_nanos, capture_examined_frames),
                    capture_resize_avg_ms = average_ms(capture_performance.resize_nanos, capture_examined_frames),
                    capture_gpu_convert_avg_ms = average_ms(capture_performance.gpu_convert_nanos, capture_examined_frames),
                    encoder_new_capture_frames = performance.new_capture_frames,
                    encoder_repeated_capture_frames = performance.repeated_capture_frames,
                    encoder_worker_late_frames = performance.encoder_worker_late_frames,
                    skipped_capture_sequences = performance.skipped_capture_sequences,
                    encoder_input_frames = performance.encoder_input_frames,
                    encode_fps = performance.encoded_frames as f64 / interval_seconds,
                    encoded_frames = performance.encoded_frames,
                    encoded_idr_frames = performance.encoded_idr_frames,
                    encoded_delta_frames = performance.encoded_delta_frames,
                    encode_avg_ms = average_ms(performance.encode_nanos, performance.encode_samples),
                    send_fps = performance.sent_frames as f64 / interval_seconds,
                    send_frames = performance.sent_frames,
                    dropped_before_initial_idr = performance.dropped_before_initial_idr,
                    sent_idr_frames = performance.sent_idr_frames,
                    sent_delta_frames = performance.sent_delta_frames,
                    send_queue_wait_avg_ms = average_ms(performance.queue_wait_nanos, performance.queue_wait_samples),
                    write_sample_avg_ms = average_ms(performance.write_sample_nanos, performance.write_sample_samples),
                    write_sample_bytes = performance.write_sample_bytes,
                    write_sample_failures = performance.write_sample_failures,
                    rtc_rtp_out_packets_delta = performance.outbound_rtp_packets,
                    rtc_rtp_out_bytes_delta = performance.outbound_rtp_bytes,
                    rtc_rtp_in_packets_delta = performance.inbound_rtp_packets,
                    rtc_rtp_in_bytes_delta = performance.inbound_rtp_bytes,
                    "Resumo periódico da mídia de compartilhamento"
                );
                self.last_screen_metrics_log_at = Some(logged_at);
            }
        }
        let events = self
            .screen_share_session
            .as_ref()
            .map(|session| std::iter::from_fn(|| session.try_recv()).collect::<Vec<_>>())
            .unwrap_or_default();
        let screen_share_session = self.screen_share_metrics.session_id;
        let mut stop_session = false;
        for event in events {
            match event {
                ScreenShareEvent::Signal { kind, payload } => {
                    tracing::debug!(screen_share_session, signal_kind = ?kind, "Sinal de negociação de tela gerado; conteúdo omitido");
                    let result = self.send_screen_share_signal(kind, payload);
                    if let Err(error) = result {
                        tracing::error!(screen_share_session, error = %error, "Falha ao encaminhar sinal de tela pela sinalização");
                        self.screen_share_status = Some(error);
                        stop_session = true;
                    }
                }
                ScreenShareEvent::State(status) => {
                    tracing::info!(screen_share_session, state = %status, "Estado WebRTC de compartilhamento alterado");
                    self.screen_share_status = Some(status)
                }
                ScreenShareEvent::Error(error) => {
                    tracing::error!(screen_share_session, error = %error, "Erro na sessão WebRTC de compartilhamento");
                    self.screen_share_status = Some(error);
                    stop_session = true;
                }
                ScreenShareEvent::ConnectionClosed => {
                    tracing::warn!(
                        screen_share_session,
                        "Conexão P2P de compartilhamento encerrada"
                    );
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
                if let Some(session) = self.screen_share_session.as_ref() {
                    session.record_ui_texture_update();
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
        if self.group_local_sharing
            || !self.group_watched_shares.is_empty()
            || !self.group_inbound_sessions.is_empty()
            || !self.group_outbound_sessions.is_empty()
        {
            self.stop_group_media(announce);
        }
        let was_sending = matches!(&self.screen_share_role, ScreenShareRole::Sending { .. });
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
            let metrics = session.metrics();
            tracing::info!(
                screen_share_session = metrics.session_id,
                track_ssrc = metrics.track_ssrc.unwrap_or_default(),
                role = match &self.screen_share_role {
                    ScreenShareRole::Sending { .. } => "sender",
                    ScreenShareRole::Receiving { .. } => "receiver",
                    ScreenShareRole::Requesting { .. } => "requesting",
                    ScreenShareRole::Idle => "idle",
                },
                selected_ice_pair = %metrics.selected_ice_pair,
                rtc_outbound = %metrics.rtc_outbound_summary,
                rtc_inbound = %metrics.rtc_inbound_summary,
                h264 = %metrics.h264_diagnostics,
                "Resumo final do compartilhamento antes de liberar a sessão"
            );
            session.stop();
        }
        self.last_screen_metrics_log_at = None;
        self.screen_share_role = ScreenShareRole::Idle;
        self.remote_screen_texture = None;
        self.remote_screen_sequence = 0;
        if was_sending {
            self.screen_texture = None;
            self.capture_preview_enabled.store(false, Ordering::Relaxed);
        }
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
        self.turn_server = None;
        self.turn_room_config = None;
        self.turn_config_received = false;
        self.turn_config_wait_started_at = None;
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
            if self.use_turn_on_create {
                let external_ipv4 = match resolve_public_ipv4(&self.server_url) {
                    Ok(address) => address,
                    Err(error) => {
                        self.connection_error = Some(format!(
                            "Não foi possível configurar o TURN. Confira o IPv4 público ou DDNS em Configurações > Conexão: {error}"
                        ));
                        return;
                    }
                };
                match TurnRelayServer::start(external_ipv4) {
                    Ok((server, credentials)) => {
                        tracing::info!(
                            "TURN local pronto em UDP 3478; credenciais temporárias omitidas"
                        );
                        self.turn_server = Some(server);
                        self.turn_room_config = Some(TurnRoomConfig {
                            turn: Some(credentials),
                        });
                        self.turn_config_received = true;
                    }
                    Err(error) => {
                        tracing::error!(error = %error, "Não foi possível iniciar o TURN local");
                        self.connection_error = Some(error);
                        return;
                    }
                }
            } else {
                self.turn_room_config = Some(TurnRoomConfig { turn: None });
                self.turn_config_received = true;
            }
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
                self.turn_server = None;
                self.turn_room_config = None;
                self.turn_config_received = false;
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
        self.turn_server = None;
        self.turn_room_config = None;
        self.turn_config_received = false;
        self.turn_config_wait_started_at = Some(Instant::now());
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
            supports_group_screen_share: self.room_mode == RoomMode::Local,
        }
    }

    fn group_sharing_compatible(&self) -> bool {
        group_screen_share_compatible(self.room_mode, &self.participants)
    }

    fn allocate_group_media_port(&self) -> Result<u16, String> {
        let used = self
            .group_outbound_ports
            .values()
            .chain(self.group_inbound_ports.values())
            .copied()
            .collect::<HashSet<_>>();
        next_group_media_port(&used)
            .ok_or_else(|| "Todas as portas de mídia UDP 9002–9009 estão ocupadas.".to_owned())
    }

    fn set_group_share(&mut self, enabled: bool) {
        if enabled && !self.group_sharing_compatible() {
            self.screen_share_status = Some(if self.participants.len() > 2 {
                "O compartilhamento em grupo exige que todos atualizem para uma versão compatível."
                    .to_owned()
            } else {
                "O compartilhamento em grupo não está disponível nesta sala.".to_owned()
            });
            return;
        }
        let kind = if enabled {
            SignalKind::ScreenShareAvailable
        } else {
            SignalKind::ScreenShareUnavailable
        };
        match self.send_screen_share_signal(kind, String::new()) {
            Ok(()) => {
                self.group_local_sharing = enabled;
                if !enabled {
                    self.group_outbound_sessions.clear();
                    self.group_outbound_ports.clear();
                }
                self.screen_share_status = Some(if enabled {
                    "Sua tela está disponível. Cada participante escolhe se quer assistir."
                        .to_owned()
                } else {
                    "Você parou de compartilhar sua tela.".to_owned()
                });
                tracing::info!(enabled, "Estado de compartilhamento em grupo alterado");
            }
            Err(error) => self.screen_share_status = Some(error),
        }
    }

    fn toggle_group_watch(&mut self, peer_id: &str) {
        if self.group_watched_shares.remove(peer_id) {
            if let Err(error) = self.send_screen_share_signal_to(
                peer_id,
                SignalKind::ScreenShareUnwatch,
                String::new(),
            ) {
                self.screen_share_status = Some(error);
            }
            self.group_inbound_sessions.remove(peer_id);
            self.group_inbound_ports.remove(peer_id);
            self.group_remote_textures.remove(peer_id);
            self.group_remote_sequences.remove(peer_id);
            self.group_peer_status.remove(peer_id);
        } else if self.group_available_shares.contains(peer_id) {
            match self.send_screen_share_signal_to(
                peer_id,
                SignalKind::ScreenShareWatch,
                String::new(),
            ) {
                Ok(()) => {
                    self.group_watched_shares.insert(peer_id.to_owned());
                    self.group_peer_status.insert(
                        peer_id.to_owned(),
                        "Pedido para assistir enviado…".to_owned(),
                    );
                }
                Err(error) => self.screen_share_status = Some(error),
            }
        }
    }

    fn start_group_outbound(&mut self, viewer_id: String, context: &egui::Context, bitrate: u32) {
        let Some(source) = self
            .screen_capture
            .as_ref()
            .map(ScreenCapture::frame_source)
        else {
            self.screen_share_status = Some("Selecione uma tela antes de compartilhar.".to_owned());
            return;
        };
        let (address, port) = match (self.selected_media_ipv4(), self.allocate_group_media_port()) {
            (Ok(address), Ok(port)) => (address, port),
            (Err(error), _) | (_, Err(error)) => {
                self.screen_share_status = Some(error);
                return;
            }
        };
        match ScreenShareSession::new_with_port(
            context.clone(),
            address,
            port,
            None,
            None,
            self.video_decoder_preference,
        ) {
            Ok(session) => {
                if let Err(error) = session.start_sending_with_bitrate(source, bitrate) {
                    self.screen_share_status = Some(error);
                } else {
                    self.group_outbound_ports.insert(viewer_id.clone(), port);
                    self.group_outbound_sessions.insert(viewer_id, session);
                }
            }
            Err(error) => self.screen_share_status = Some(error),
        }
    }

    fn rebalance_group_outbound(
        &mut self,
        context: &egui::Context,
        additional_viewer: Option<String>,
    ) {
        let mut viewers = self
            .group_outbound_sessions
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        if let Some(viewer) = additional_viewer {
            if !viewers.contains(&viewer) {
                viewers.push(viewer);
            }
        }
        viewers.sort();
        if viewers.is_empty() {
            return;
        }
        for viewer in self.group_outbound_sessions.keys() {
            let _ = self.send_screen_share_signal_to_stream(
                viewer,
                self.participant_id.clone(),
                SignalKind::ScreenShareStopped,
                String::new(),
            );
        }
        self.group_outbound_sessions.clear();
        self.group_outbound_ports.clear();
        let bitrate = group_share_bitrate(viewers.len());
        for viewer in viewers {
            self.start_group_outbound(viewer, context, bitrate);
        }
        tracing::info!(
            viewers = self.group_outbound_sessions.len(),
            target_bitrate_bps = bitrate,
            "Encoder de grupo reequilibrado dentro do limite de banda"
        );
    }

    fn stop_group_media(&mut self, announce: bool) {
        if announce && self.group_local_sharing {
            let _ =
                self.send_screen_share_signal(SignalKind::ScreenShareUnavailable, String::new());
        }
        if announce {
            let watched = self.group_watched_shares.drain().collect::<Vec<_>>();
            for peer_id in watched {
                let _ = self.send_screen_share_signal_to(
                    &peer_id,
                    SignalKind::ScreenShareUnwatch,
                    String::new(),
                );
            }
        } else {
            self.group_watched_shares.clear();
        }
        self.group_available_shares.clear();
        self.group_outbound_sessions.clear();
        self.group_inbound_sessions.clear();
        self.group_outbound_ports.clear();
        self.group_inbound_ports.clear();
        self.group_remote_textures.clear();
        self.group_remote_sequences.clear();
        self.group_peer_status.clear();
        self.group_local_sharing = false;
        self.focused_group_screen = None;
    }

    fn show_group_watch_controls(&mut self, ui: &mut egui::Ui, participants: &[ParticipantInfo]) {
        if !self.group_sharing_compatible() {
            if self.room_mode == RoomMode::Local && participants.len() > 2 {
                Self::show_notice(
                    ui,
                    "Compartilhamento em grupo indisponível:",
                    "Todos precisam usar uma versão que ofereça suporte ao compartilhamento em grupo.",
                );
            }
            return;
        }
        let mut peers = self
            .group_available_shares
            .iter()
            .filter_map(|id| participants.iter().find(|p| &p.id == id))
            .cloned()
            .collect::<Vec<_>>();
        peers.sort_by_key(|participant| participant.order);
        if peers.is_empty() && !self.group_local_sharing {
            return;
        }
        ui.group(|ui| {
            ui.heading("Telas compartilhadas");
            ui.horizontal_wrapped(|ui| {
                for peer in peers {
                    let watching = self.group_watched_shares.contains(&peer.id);
                    let label = if watching {
                        format!("Parar de assistir {}", peer.display_name)
                    } else {
                        format!("Assistir {}", peer.display_name)
                    };
                    if ui.button(label).clicked() {
                        self.toggle_group_watch(&peer.id);
                    }
                }
                if self.group_local_sharing {
                    ui.label("Sua tela está disponível");
                }
            });
        });
    }

    fn show_group_screen_stage(&mut self, ui: &mut egui::Ui, stage_height: f32) {
        let focused = self.focused_group_screen.clone();
        if let Some(id) = focused.as_deref() {
            if let Some(texture) = self.group_remote_textures.get(id) {
                let source = texture.size_vec2();
                let available = ui.available_size();
                let scale = (available.x / source.x)
                    .min(available.y / source.y)
                    .min(1.0);
                ui.vertical_centered(|ui| {
                    ui.add_space(((stage_height - source.y * scale.max(0.01)) * 0.5).max(0.0));
                    ui.add(
                        egui::Image::new((texture.id(), source))
                            .fit_to_exact_size(source * scale.max(0.01)),
                    );
                    if ui.button("Voltar à grade").clicked() {
                        self.focused_group_screen = None;
                    }
                });
                return;
            }
            if id == "__local" {
                if let Some(texture) = self
                    .screen_texture
                    .as_ref()
                    .filter(|_| self.show_local_preview)
                {
                    let source = texture.size_vec2();
                    let available = ui.available_size();
                    let scale = (available.x / source.x)
                        .min(available.y / source.y)
                        .min(1.0);
                    ui.vertical_centered(|ui| {
                        ui.add_space(((stage_height - source.y * scale.max(0.01)) * 0.5).max(0.0));
                        ui.add(
                            egui::Image::new((texture.id(), source))
                                .fit_to_exact_size(source * scale.max(0.01)),
                        );
                        if ui.button("Voltar à grade").clicked() {
                            self.focused_group_screen = None;
                        }
                    });
                    return;
                }
            }
            self.focused_group_screen = None;
        }
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                if self.group_local_sharing && self.show_local_preview {
                    egui::Frame::group(ui.style()).show(ui, |tile| {
                        tile.set_min_size(egui::vec2(300.0, 195.0));
                        tile.label("Você (prévia local)");
                        if let Some(texture) = &self.screen_texture {
                            tile.add(
                                egui::Image::new((texture.id(), texture.size_vec2()))
                                    .fit_to_exact_size(egui::vec2(280.0, 158.0)),
                            );
                        } else {
                            tile.label("Preparando prévia…");
                        }
                        if tile.button("Ampliar").clicked() {
                            self.focused_group_screen = Some("__local".to_owned());
                        }
                    });
                }
                let watched = self
                    .group_watched_shares
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>();
                for peer_id in watched {
                    let name = self
                        .participants
                        .iter()
                        .find(|p| p.id == peer_id)
                        .map(|p| p.display_name.clone())
                        .unwrap_or_else(|| "Participante".to_owned());
                    egui::Frame::group(ui.style()).show(ui, |tile| {
                        tile.set_min_size(egui::vec2(300.0, 195.0));
                        tile.label(name);
                        if let Some(texture) = self.group_remote_textures.get(&peer_id) {
                            tile.add(
                                egui::Image::new((texture.id(), texture.size_vec2()))
                                    .fit_to_exact_size(egui::vec2(280.0, 158.0)),
                            );
                            if tile.button("Ampliar").clicked() {
                                self.focused_group_screen = Some(peer_id.clone());
                            }
                        } else {
                            tile.label(
                                self.group_peer_status
                                    .get(&peer_id)
                                    .map(String::as_str)
                                    .unwrap_or("Aguardando a tela…"),
                            );
                        }
                    });
                }
                if !self.group_local_sharing && self.group_watched_shares.is_empty() {
                    ui.vertical_centered(|ui| {
                        ui.add_space(stage_height * 0.35);
                        ui.heading("Sala pronta");
                        ui.label("Escolha uma transmissão e pressione Assistir.");
                    });
                }
            });
        });
    }

    fn handle_group_peer_signal(
        &mut self,
        from_participant_id: Option<String>,
        stream_id: Option<String>,
        kind: SignalKind,
        payload: String,
        context: &egui::Context,
    ) {
        let Some(peer_id) = from_participant_id else {
            self.screen_share_status = Some(
                "O servidor não identificou o participante que enviou a negociação WebRTC."
                    .to_owned(),
            );
            return;
        };
        if !self.group_sharing_compatible() || peer_id == self.participant_id {
            return;
        }
        let Some(stream_id) = stream_id else {
            self.group_peer_status.insert(
                peer_id,
                "A negociação WebRTC veio sem identificação da transmissão.".to_owned(),
            );
            return;
        };
        if kind == SignalKind::Offer && stream_id != peer_id {
            tracing::warn!(signal_kind = ?kind, "Oferta de grupo não corresponde à identidade do transmissor");
            return;
        }
        if kind == SignalKind::Offer && !self.group_inbound_sessions.contains_key(&peer_id) {
            let (address, port) =
                match (self.selected_media_ipv4(), self.allocate_group_media_port()) {
                    (Ok(address), Ok(port)) => (address, port),
                    (Err(error), _) | (_, Err(error)) => {
                        self.screen_share_status = Some(error);
                        return;
                    }
                };
            match ScreenShareSession::new_with_port(
                context.clone(),
                address,
                port,
                None,
                None,
                self.video_decoder_preference,
            ) {
                Ok(session) => {
                    self.group_inbound_ports.insert(peer_id.clone(), port);
                    self.group_inbound_sessions.insert(peer_id.clone(), session);
                    self.group_peer_status
                        .insert(peer_id.clone(), "Negociando a tela recebida…".to_owned());
                }
                Err(error) => {
                    self.screen_share_status = Some(error);
                    return;
                }
            }
        }
        let session = match kind {
            SignalKind::Answer if stream_id == self.participant_id => {
                self.group_outbound_sessions.get(&peer_id)
            }
            SignalKind::Offer => self.group_inbound_sessions.get(&peer_id),
            SignalKind::IceCandidate if stream_id == self.participant_id => {
                self.group_outbound_sessions.get(&peer_id)
            }
            SignalKind::IceCandidate if stream_id == peer_id => {
                self.group_inbound_sessions.get(&peer_id)
            }
            _ => None,
        };
        if let Some(session) = session {
            if let Err(error) = session.handle_signal(kind, payload) {
                self.group_peer_status.insert(peer_id, error.clone());
                self.screen_share_status = Some(error);
            }
        } else {
            tracing::debug!(signal_kind = ?kind, "Ignorando sinal WebRTC sem sessão de grupo correspondente");
        }
    }

    fn refresh_group_screen_shares(&mut self, context: &egui::Context) {
        let mut peer_ids = self
            .group_inbound_sessions
            .keys()
            .chain(self.group_outbound_sessions.keys())
            .cloned()
            .collect::<HashSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        peer_ids.sort();
        let mut outbound_membership_changed = false;
        for peer_id in peer_ids {
            let mut signal_events = Vec::new();
            let mut failures = Vec::new();
            let mut states = Vec::new();
            let mut inbound_failed = false;
            let mut outbound_failed = false;
            if let Some(session) = self.group_inbound_sessions.get(&peer_id) {
                while let Some(event) = session.try_recv() {
                    match event {
                        ScreenShareEvent::Signal { kind, payload } => {
                            signal_events.push((true, kind, payload))
                        }
                        ScreenShareEvent::State(state) => states.push(state),
                        ScreenShareEvent::Error(error) => {
                            inbound_failed = true;
                            failures.push(error);
                        }
                        ScreenShareEvent::ConnectionClosed => {
                            inbound_failed = true;
                            failures.push("A conexão P2P foi encerrada.".to_owned());
                        }
                    }
                }
                if let Some(frame) = session.latest_remote_frame() {
                    let sequence = self
                        .group_remote_sequences
                        .get(&peer_id)
                        .copied()
                        .unwrap_or(0);
                    if sequence != frame.sequence {
                        let image = egui::ColorImage::from_rgba_unmultiplied(
                            [frame.width as usize, frame.height as usize],
                            &frame.rgba,
                        );
                        if let Some(texture) = self.group_remote_textures.get_mut(&peer_id) {
                            texture.set(image, egui::TextureOptions::LINEAR);
                        } else {
                            self.group_remote_textures.insert(
                                peer_id.clone(),
                                context.load_texture(
                                    format!("group-screen-{peer_id}"),
                                    image,
                                    egui::TextureOptions::LINEAR,
                                ),
                            );
                        }
                        self.group_remote_sequences
                            .insert(peer_id.clone(), frame.sequence);
                    }
                }
            }
            if let Some(session) = self.group_outbound_sessions.get(&peer_id) {
                while let Some(event) = session.try_recv() {
                    match event {
                        ScreenShareEvent::Signal { kind, payload } => {
                            signal_events.push((false, kind, payload))
                        }
                        ScreenShareEvent::State(state) => states.push(state),
                        ScreenShareEvent::Error(error) => {
                            outbound_failed = true;
                            failures.push(error);
                        }
                        ScreenShareEvent::ConnectionClosed => {
                            outbound_failed = true;
                            failures.push("A conexão P2P foi encerrada.".to_owned());
                        }
                    }
                }
            }
            for (is_inbound, kind, payload) in signal_events {
                let stream_id = if is_inbound {
                    peer_id.clone()
                } else {
                    self.participant_id.clone()
                };
                if let Err(error) =
                    self.send_screen_share_signal_to_stream(&peer_id, stream_id, kind, payload)
                {
                    if is_inbound {
                        inbound_failed = true;
                    } else {
                        outbound_failed = true;
                    }
                    failures.push(error);
                }
            }
            if let Some(state) = states.last() {
                self.group_peer_status
                    .insert(peer_id.clone(), state.clone());
            }
            if let Some(error) = failures.last() {
                tracing::error!(error = %error, "Falha isolada em sessão de tela de participante");
                self.group_peer_status
                    .insert(peer_id.clone(), error.clone());
            }
            if inbound_failed {
                self.group_inbound_sessions.remove(&peer_id);
                self.group_inbound_ports.remove(&peer_id);
                self.group_remote_textures.remove(&peer_id);
                self.group_remote_sequences.remove(&peer_id);
                self.group_watched_shares.remove(&peer_id);
                let _ = self.send_screen_share_signal_to(
                    &peer_id,
                    SignalKind::ScreenShareUnwatch,
                    String::new(),
                );
            }
            if outbound_failed {
                self.group_outbound_sessions.remove(&peer_id);
                self.group_outbound_ports.remove(&peer_id);
                let _ = self.send_screen_share_signal_to_stream(
                    &peer_id,
                    self.participant_id.clone(),
                    SignalKind::ScreenShareStopped,
                    String::new(),
                );
                outbound_membership_changed = true;
            }
        }
        if outbound_membership_changed {
            self.rebalance_group_outbound(context, None);
        }

        if self
            .last_group_metrics_log_at
            .is_none_or(|last| last.elapsed() >= Duration::from_secs(5))
        {
            let outbound = self
                .group_outbound_sessions
                .values()
                .map(ScreenShareSession::metrics)
                .collect::<Vec<_>>();
            let inbound = self
                .group_inbound_sessions
                .values()
                .map(ScreenShareSession::metrics)
                .collect::<Vec<_>>();
            tracing::info!(
                outbound_peers = outbound.len(),
                inbound_peers = inbound.len(),
                encoded_frames = outbound.iter().map(|m| m.encoded_frames).sum::<u64>(),
                sent_frames = outbound.iter().map(|m| m.sent_frames).sum::<u64>(),
                received_packets = inbound.iter().map(|m| m.received_packets).sum::<u64>(),
                decoded_frames = inbound.iter().map(|m| m.decoded_frames).sum::<u64>(),
                decode_errors = inbound.iter().map(|m| m.decode_errors).sum::<u64>(),
                watching = self.group_watched_shares.len(),
                sharing = self.group_local_sharing,
                "Resumo agregado de compartilhamento em grupo"
            );
            self.last_group_metrics_log_at = Some(Instant::now());
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
        let member_ids = self
            .participants
            .iter()
            .map(|participant| participant.id.clone())
            .collect::<HashSet<_>>();
        self.group_available_shares
            .retain(|id| member_ids.contains(id));
        self.group_watched_shares
            .retain(|id| member_ids.contains(id));
        self.group_outbound_sessions
            .retain(|id, _| member_ids.contains(id));
        self.group_inbound_sessions
            .retain(|id, _| member_ids.contains(id));
        self.group_outbound_ports
            .retain(|id, _| member_ids.contains(id));
        self.group_inbound_ports
            .retain(|id, _| member_ids.contains(id));
        self.group_remote_textures
            .retain(|id, _| member_ids.contains(id));
        self.group_remote_sequences
            .retain(|id, _| member_ids.contains(id));
        self.group_peer_status
            .retain(|id, _| member_ids.contains(id));
        if self.room_mode != RoomMode::Local
            && (self.group_local_sharing
                || !self.group_watched_shares.is_empty()
                || !self.group_inbound_sessions.is_empty()
                || !self.group_outbound_sessions.is_empty())
        {
            self.stop_group_media(false);
        } else if self.room_mode == RoomMode::Local
            && !self.group_sharing_compatible()
            && (self.group_local_sharing
                || !self.group_watched_shares.is_empty()
                || !self.group_inbound_sessions.is_empty()
                || !self.group_outbound_sessions.is_empty())
        {
            self.stop_group_media(true);
            self.screen_share_status = Some(
                "O compartilhamento em grupo foi encerrado: todos os participantes precisam de uma versão compatível."
                    .to_owned(),
            );
        }
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
        if self.group_local_sharing && self.group_sharing_compatible() {
            let _ = self.send_screen_share_signal(SignalKind::ScreenShareAvailable, String::new());
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
                if !self.host_addresses.is_empty() {
                    self.selected_host_address = self
                        .preferred_control_ipv4
                        .and_then(|preferred| {
                            self.host_addresses
                                .iter()
                                .position(|address| address.ipv4 == preferred)
                        })
                        .unwrap_or(0);
                    let selected_ipv4 = self.host_addresses[self.selected_host_address].ipv4;
                    if self.preferred_control_ipv4 != Some(selected_ipv4) {
                        self.preferred_control_ipv4 = Some(selected_ipv4);
                        tracing::info!(ipv4 = %selected_ipv4, "Adaptador de controle padrão selecionado porque o IPv4 salvo não está ativo");
                    }
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
                self.preferred_control_ipv4 = Some(selected.ipv4);
                tracing::info!(adapter = %selected.adapter, ipv4 = %selected.ipv4, "Adaptador escolhido para a malha de controle");
            }
            ui.monospace(format!(
                "ws://{}:9001",
                self.host_addresses[self.selected_host_address].ipv4
            ));
            ui.small("Este IPv4 também será usado para vincular o vídeo WebRTC nas portas UDP 9002–9009.");
        } else if let Some(error) = &self.host_addresses_error {
            Self::show_notice(ui, "Erro de adaptador:", error);
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
                self.preferred_control_ipv4 = Some(selected.ipv4);
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
            Self::show_notice(ui, "Erro de adaptador:", error);
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

    fn selected_media_ipv4(&self) -> Result<Ipv4Addr, String> {
        self.host_addresses
            .get(self.selected_host_address)
            .map(|address| address.ipv4)
            .ok_or_else(|| {
                "Nenhum adaptador IPv4 está selecionado. Atualize os adaptadores em Configurações > Conexão antes de compartilhar a tela.".to_owned()
            })
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
                    if self.hosting_locally && self.room_mode == RoomMode::InternetTest {
                        self.send_turn_room_config();
                    }
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
                    from_participant_id: _,
                    stream_id: _,
                    kind: SignalKind::Diagnostic,
                    payload,
                } => {
                    if let Some(serialized) = payload.strip_prefix(TURN_CONFIG_SIGNAL_PREFIX) {
                        match serde_json::from_str::<TurnRoomConfig>(serialized) {
                            Ok(config) => {
                                let turn_enabled = config.turn.is_some();
                                self.turn_room_config = Some(config);
                                self.turn_config_received = true;
                                self.turn_config_wait_started_at = None;
                                tracing::info!(
                                    turn_enabled,
                                    "Configuração ICE recebida do anfitrião; credenciais omitidas"
                                );
                                self.connection_status = Some(if turn_enabled {
                                    "Configuração recebida; TURN está disponível como alternativa."
                                        .to_owned()
                                } else {
                                    "Configuração recebida; esta sala usa somente conexão direta."
                                        .to_owned()
                                });
                            }
                            Err(error) => {
                                tracing::error!(error = %error, "Configuração ICE da sala inválida");
                                self.connection_error = Some(
                                    "O anfitrião enviou uma configuração de mídia inválida."
                                        .to_owned(),
                                );
                            }
                        }
                    } else {
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
                }
                SignalingEvent::Signal {
                    from_participant_id,
                    stream_id,
                    kind,
                    payload,
                } => {
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
                            | SignalKind::ScreenShareAvailable
                            | SignalKind::ScreenShareUnavailable
                            | SignalKind::ScreenShareWatch
                            | SignalKind::ScreenShareUnwatch
                    ) {
                        self.handle_screen_share_signal(
                            from_participant_id.clone(),
                            stream_id.clone(),
                            kind,
                            payload.clone(),
                            context,
                        );
                    } else if matches!(
                        kind,
                        SignalKind::Offer | SignalKind::Answer | SignalKind::IceCandidate
                    ) {
                        self.handle_group_peer_signal(
                            from_participant_id,
                            stream_id,
                            kind,
                            payload,
                            context,
                        );
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
            self.turn_server = None;
            self.turn_room_config = None;
            self.turn_config_received = false;
            self.turn_config_wait_started_at = None;
            self.pending_signaling = None;
            self.screen_picker = None;
            if let Some(mut capture) = self.screen_capture.take() {
                let _ = capture.stop();
            }
            self.screen_texture = None;
        }
    }

    fn refresh_turn_state(&mut self) {
        let failure = self
            .turn_server
            .as_mut()
            .and_then(TurnRelayServer::try_failure);
        if let Some(error) = failure {
            tracing::error!(error = %error, "Servidor TURN integrado encerrou com erro");
            self.turn_server = None;
            self.turn_room_config = Some(TurnRoomConfig { turn: None });
            self.turn_config_received = true;
            self.send_turn_room_config();
            self.connection_error = Some(format!(
                "O servidor TURN parou: {error}. O compartilhamento poderá usar somente conexão direta."
            ));
            if self.screen_share_session.is_some() {
                self.stop_screen_share(false);
                self.screen_share_status = Some(
                    "O servidor TURN parou. Inicie novamente para tentar uma conexão direta."
                        .to_owned(),
                );
            }
        }

        if self.room_mode == RoomMode::InternetTest
            && !self.hosting_locally
            && !self.turn_config_received
            && self
                .turn_config_wait_started_at
                .is_some_and(|started| started.elapsed() >= Duration::from_secs(5))
        {
            tracing::warn!(
                "O anfitrião não enviou configuração TURN em 5 segundos; mantendo tentativa direta"
            );
            self.turn_room_config = Some(TurnRoomConfig { turn: None });
            self.turn_config_received = true;
            self.turn_config_wait_started_at = None;
            self.connection_status = Some(
                "O anfitrião não enviou configuração TURN; a tela tentará somente conexão direta."
                    .to_owned(),
            );
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
                Self::show_notice(ui, "Transferência:", &error);
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
        self.turn_server = None;
        self.turn_room_config = None;
        self.turn_config_received = false;
        self.turn_config_wait_started_at = None;
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

    fn open_logs_directory(&mut self) {
        match self.logging.open_log_directory() {
            Ok(()) => {
                self.logging.export_message = Some(format!(
                    "Pasta de logs: {}",
                    self.logging.actual_log_directory_label()
                ));
            }
            Err(error) => {
                self.logging.export_message =
                    Some(format!("Não foi possível abrir a pasta de logs: {error}"));
                tracing::error!(error = %error, "Falha ao abrir a pasta de logs");
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
            log_directory: self.logging.actual_log_directory_label(),
            current_log_file: self.logging.current_log_file_label(),
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
                "sessão={}, SSRC={}, P2P={}, rota={:?}; par ICE: {}; RTP enviado: {}; RTP recebido: {}; RTP total enviado={} pacotes/{} bytes, recebido={} pacotes/{} bytes, perda={}, jitter={:.1} ms; codec local/remoto={}/{}, fallback local/remoto={}/{}, ICE local/remoto={}/{}, srflx local/remoto={}/{}, relay local/remoto={}/{}, entradas/quadros H.264/enviados/decodificados={}/{}/{}/{}, IDR/P enviados={}/{}, descartados antes do IDR={}, P montados/decodificados={}/{}, pacotes RTP observados={}, decoder entradas/sem saída/fila cheia={}/{}/{}, quadros publicados/atualizações da prévia={}/{}, PLI fila cheia={}, erros de decodificação={}, diagnóstico H.264={}",
                metrics.session_id,
                metrics.track_ssrc.unwrap_or_default(),
                metrics.p2p_connected,
                metrics.route,
                metrics.selected_ice_pair,
                metrics.rtc_outbound_summary,
                metrics.rtc_inbound_summary,
                metrics.outbound_rtp_packets,
                metrics.outbound_rtp_bytes,
                metrics.inbound_rtp_packets,
                metrics.inbound_rtp_bytes,
                metrics.inbound_rtp_lost,
                metrics.inbound_rtp_jitter_ms,
                metrics.encoder_backend,
                metrics.decoder_backend,
                metrics.encoder_fallback_reason.as_deref().unwrap_or(""),
                metrics.decoder_fallback_reason.as_deref().unwrap_or(""),
                metrics.local_ice_candidates,
                metrics.remote_ice_candidates,
                metrics.local_srflx_candidates,
                metrics.remote_srflx_candidates,
                metrics.local_relay_candidates,
                metrics.remote_relay_candidates,
                metrics.encoder_input_frames,
                metrics.encoded_frames,
                metrics.sent_frames,
                metrics.decoded_frames,
                metrics.sent_idr_frames,
                metrics.sent_delta_frames,
                metrics.dropped_before_initial_idr,
                metrics.received_delta_frames,
                metrics.decoded_delta_frames,
                metrics.received_packets,
                metrics.decoder_input_frames,
                metrics.decoder_no_output_frames,
                metrics.decoder_queue_drops,
                metrics.published_frames,
                metrics.ui_texture_updates,
                metrics.pli_queue_overflow,
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

fn resolve_public_ipv4(input: &str) -> Result<Ipv4Addr, String> {
    let endpoint = signaling_ws_url(input)?;
    let authority = endpoint
        .strip_prefix("ws://")
        .and_then(|value| value.strip_suffix(":9000"))
        .ok_or_else(|| {
            "Informe o IPv4 público ou nome DDNS usado para acessar o anfitrião.".to_owned()
        })?;

    let addresses = (authority, 3478)
        .to_socket_addrs()
        .map_err(|error| format!("Não foi possível resolver o endereço do anfitrião: {error}"))?;
    let address = addresses
        .filter_map(|address| match address.ip() {
            IpAddr::V4(ipv4) if is_public_ipv4_candidate(ipv4) => Some(ipv4),
            _ => None,
        })
        .next()
        .ok_or_else(|| {
            "O endereço não resolveu para um IPv4 público. TURN precisa de um endereço público alcançável; CGNAT não permite essa conexão de entrada.".to_owned()
        })?;
    Ok(address)
}

fn is_public_ipv4_candidate(address: Ipv4Addr) -> bool {
    let octets = address.octets();
    let is_shared_address_space = octets[0] == 100 && (64..=127).contains(&octets[1]);
    let is_documentation_range = matches!(
        octets,
        [192, 0, 2, _] | [198, 51, 100, _] | [203, 0, 113, _]
    );
    !address.is_private()
        && !address.is_loopback()
        && !address.is_link_local()
        && !address.is_unspecified()
        && !address.is_multicast()
        && !address.is_broadcast()
        && !is_shared_address_space
        && !is_documentation_range
}

fn signaling_address_for_control(control_address: &str) -> String {
    control_address
        .strip_suffix(":9001")
        .map(|ip| format!("ws://{ip}:9000"))
        .unwrap_or_default()
}

impl Drop for ClientUi {
    fn drop(&mut self) {
        if let Err(error) = settings::save(&self.preferences_snapshot()) {
            tracing::error!(error = %error, "Não foi possível salvar as preferências ao encerrar");
        }
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
    app.microphone_level_dbfs = -60.0;
    let (preferences, settings_warning) = settings::load();
    app.apply_preferences(preferences);
    app.settings_error = settings_warning;
    if let Some(error) = &app.settings_error {
        tracing::warn!(error = %error, "Preferências não puderam ser carregadas; usando valores padrão");
    }

    tracing::info!("Interface gráfica sendo inicializada");

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_icon(application_icon()),
        ..Default::default()
    };

    eframe::run_ui_native("P2P - Voz e tela", native_options, move |ui, _frame| {
        app.show(ui);
    })
}

fn application_icon() -> egui::IconData {
    let icon = image::load_from_memory(include_bytes!("../assets/p2p-icon.png"))
        .expect("ícone PNG do aplicativo inválido")
        .resize_exact(64, 64, image::imageops::FilterType::Lanczos3)
        .to_rgba8();
    let (width, height) = icon.dimensions();
    egui::IconData {
        rgba: icon.into_raw(),
        width,
        height,
    }
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
    use super::{
        group_screen_share_compatible, is_public_ipv4_candidate, next_group_media_port,
        signaling_ws_url,
    };
    use signaling_protocol::{ParticipantInfo, RoomMode};
    use std::collections::HashSet;
    use std::net::Ipv4Addr;

    fn participant(id: usize, supports_group_screen_share: bool) -> ParticipantInfo {
        ParticipantInfo {
            id: format!("participant-{id}"),
            display_name: format!("Participante {id}"),
            order: id as u8,
            may_host: false,
            control_address: String::new(),
            supports_group_screen_share,
        }
    }

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

    #[test]
    fn group_sender_bitrate_stays_within_aggregate_limit() {
        for viewers in [1, 2, 7] {
            let per_peer = super::group_share_bitrate(viewers);
            assert!(per_peer <= 4_000_000);
            assert!(u64::from(per_peer) * viewers as u64 <= 8_000_000);
        }
        assert_eq!(super::group_share_bitrate(1), 4_000_000);
        assert_eq!(super::group_share_bitrate(2), 4_000_000);
        assert_eq!(super::group_share_bitrate(7), 1_142_857);
    }

    #[test]
    fn group_sharing_requires_two_to_eight_compatible_local_participants() {
        let eight = (1..=8).map(|id| participant(id, true)).collect::<Vec<_>>();
        assert!(group_screen_share_compatible(RoomMode::Local, &eight));

        let mut old_client = eight[..2].to_vec();
        old_client[1].supports_group_screen_share = false;
        assert!(!group_screen_share_compatible(RoomMode::Local, &old_client));
        assert!(!group_screen_share_compatible(
            RoomMode::InternetTest,
            &eight[..2]
        ));
        assert!(!group_screen_share_compatible(RoomMode::Local, &eight[..1]));
        assert!(!group_screen_share_compatible(
            RoomMode::Local,
            &(1..=9).map(|id| participant(id, true)).collect::<Vec<_>>()
        ));
    }

    #[test]
    fn group_media_ports_are_unique_and_limited_to_the_documented_range() {
        let mut used = HashSet::new();
        for expected in 9002..=9009 {
            let port = next_group_media_port(&used).unwrap();
            assert_eq!(port, expected);
            assert!(used.insert(port));
        }
        assert_eq!(next_group_media_port(&used), None);
    }

    #[test]
    fn turn_host_address_requires_a_public_ipv4_candidate() {
        assert!(is_public_ipv4_candidate(Ipv4Addr::new(8, 8, 8, 8)));
        assert!(!is_public_ipv4_candidate(Ipv4Addr::new(192, 168, 1, 5)));
        assert!(!is_public_ipv4_candidate(Ipv4Addr::new(100, 80, 2, 3)));
        assert!(!is_public_ipv4_candidate(Ipv4Addr::new(203, 0, 113, 5)));
    }
}
