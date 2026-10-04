use std::borrow::Cow;
use std::collections::{HashMap, HashSet, VecDeque};
use std::net::{IpAddr, Ipv4Addr, ToSocketAddrs};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::audio_capture::{AudioApplication, MicrophoneTest};
use crate::control_mesh::{ControlEvent, ControlMesh, QueueEntry};
use crate::logging::{
    DiagnosticSnapshot, LoggingState, safe_signaling_endpoint, safe_stun_endpoint,
};
use crate::profile;
use crate::screen_capture::{MonitorOption, PendingScreenCapture, PreviewFrame, ScreenCapture};
use crate::screen_sharing::{ScreenShareEvent, ScreenShareMetrics, ScreenShareSession};
use crate::settings::{
    AppSettings, LoggingLevel, MAX_SAVED_HOSTS, SavedHostProfile, VideoDecoderPreference,
};
use crate::signaling_client::{SignalingClient, SignalingEvent};
use crate::turn_relay::{TurnCredentials, TurnRelayServer, TurnRoomConfig};
use crate::update::{UpdateEvent, UpdateManager, UpdateManifest};
use crate::{logging, screen_sharing, settings, update};
use eframe::egui;
use signaling_protocol::{ParticipantInfo, RoomMode, SignalKind};

#[path = "app/group_sharing.rs"]
mod group_sharing;
#[path = "app/room_lifecycle.rs"]
mod room_lifecycle;
#[path = "app/room_participants.rs"]
mod room_participants;
#[path = "ui/mod.rs"]
mod ui;
#[path = "app/updates.rs"]
mod updates;

pub(crate) use updates::*;

const TURN_CONFIG_SIGNAL_PREFIX: &str = "p2p-turn-room-config-v1:";
const GROUP_SCREEN_MAX_AGGREGATE_BITRATE: u32 = 8_000_000;
const GROUP_SCREEN_MAX_PEER_BITRATE: u32 = 4_000_000;
const GROUP_SIGNAL_ID_PREFIX: &str = "p2p-group-session-v1";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LocalPreviewFrameKey {
    sequence: u64,
    width: u32,
    height: u32,
}

impl From<&PreviewFrame> for LocalPreviewFrameKey {
    fn from(frame: &PreviewFrame) -> Self {
        Self {
            sequence: frame.sequence,
            width: frame.width,
            height: frame.height,
        }
    }
}

fn local_preview_image(frame: &PreviewFrame) -> Result<Option<egui::ColorImage>, String> {
    if frame.rgba.is_empty() {
        #[cfg(windows)]
        if frame.cpu_nv12.is_none() {
            return Ok(None);
        }
        #[cfg(not(windows))]
        return Ok(None);
    }
    let width = frame.width as usize;
    let height = frame.height as usize;
    let expected_bytes = width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(4))
        .filter(|bytes| *bytes > 0)
        .ok_or_else(|| "O quadro da prévia tem dimensões inválidas.".to_owned())?;
    let rgba = if !frame.rgba.is_empty() {
        Cow::Borrowed(frame.rgba.as_slice())
    } else {
        #[cfg(windows)]
        {
            if !width.is_multiple_of(2) || !height.is_multiple_of(2) {
                return Err("O quadro NV12 da prévia precisa de dimensões pares.".to_owned());
            }
            let nv12 = frame.cpu_nv12.as_ref().expect("NV12 availability checked");
            // Reopening a static preview must not wait for another DXGI frame.
            Cow::Owned(crate::mf_video::cpu_nv12_to_rgba(
                nv12,
                frame.width,
                frame.height,
            )?)
        }
        #[cfg(not(windows))]
        return Ok(None);
    };
    if rgba.len() != expected_bytes {
        return Err("Os dados RGBA da prévia estão incompletos.".to_owned());
    }
    Ok(Some(egui::ColorImage::from_rgba_unmultiplied(
        [width, height],
        &rgba,
    )))
}

fn settings_navigation_button(settings_open: bool) -> (&'static str, &'static str) {
    if settings_open {
        ("←", "Voltar")
    } else {
        ("⚙", "Configurações")
    }
}

fn room_header_code_label(room_code: Option<&str>) -> Option<String> {
    room_code.map(|code| format!("Código: {code}"))
}

fn copy_confirmation_visible(expires_at: Option<Instant>, now: Instant) -> bool {
    expires_at.is_some_and(|expires_at| now < expires_at)
}

fn allocate_app_header_row(ui: &mut egui::Ui, height: f32) -> egui::Rect {
    let width = ui.available_width();
    ui.allocate_space(egui::vec2(width, height)).1
}

fn centered_header_rect(row: egui::Rect, size: egui::Vec2) -> egui::Rect {
    egui::Rect::from_center_size(row.center(), size)
}

fn right_aligned_header_rect(
    row: egui::Rect,
    size: egui::Vec2,
    offset_from_right: f32,
) -> egui::Rect {
    egui::Rect::from_min_size(
        egui::pos2(
            row.right() - offset_from_right - size.x,
            row.center().y - size.y / 2.0,
        ),
        size,
    )
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum SettingsCategory {
    #[default]
    Audio,
    General,
    Connection,
    Video,
    Updates,
}

#[derive(Default)]
struct ClientUi {
    room_code: Option<String>,
    join_code: String,
    code_copied_until: Option<Instant>,
    settings_open: bool,
    settings_category: SettingsCategory,
    fullscreen_video: bool,
    server_url: String,
    public_server_url: String,
    saved_hosts: Vec<SavedHostProfile>,
    selected_host_index: Option<usize>,
    new_host_name: String,
    new_host_address: String,
    saved_host_error: Option<String>,
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
    control_network_window_open: bool,
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
    remote_audio_default_volume_percent: u8,
    video_decoder_preference: VideoDecoderPreference,
    microphone_error: Option<String>,
    microphone_monitor_error: Option<String>,
    microphone_audio_warning: bool,
    microphone_clipping_warning: bool,
    profile_display_name: String,
    profile_avatar_jpeg: Option<Vec<u8>>,
    profile_avatar_texture: Option<egui::TextureHandle>,
    profile_avatar_error: Option<String>,
    profile_window_open: bool,
    participant_avatar_textures: HashMap<String, (Option<String>, Option<egui::TextureHandle>)>,
    screen_capture: Option<ScreenCapture>,
    screen_picker: Option<PendingScreenCapture>,
    available_monitors: Vec<MonitorOption>,
    monitors_loaded: bool,
    monitor_menu_open: bool,
    dxgi_capture_error: Option<String>,
    screen_texture: Option<egui::TextureHandle>,
    local_preview_frame_key: Option<LocalPreviewFrameKey>,
    local_preview_failure: Option<(LocalPreviewFrameKey, String)>,
    show_local_preview: bool,
    capture_preview_enabled: Arc<AtomicBool>,
    screen_status: Option<String>,
    screen_pipeline_summary: String,
    screen_share_session: Option<ScreenShareSession>,
    screen_share_role: ScreenShareRole,
    screen_share_status: Option<String>,
    audio_status: Option<String>,
    remote_screen_texture: Option<egui::TextureHandle>,
    remote_screen_sequence: u64,
    group_local_sharing: bool,
    include_system_audio: bool,
    excluded_audio_application_path: Option<String>,
    available_audio_applications: Vec<AudioApplication>,
    audio_applications_error: Option<String>,
    audio_applications_loaded: bool,
    group_available_shares: HashSet<String>,
    group_watched_shares: HashSet<String>,
    group_outbound_sessions: HashMap<String, ScreenShareSession>,
    group_inbound_sessions: HashMap<String, ScreenShareSession>,
    group_outbound_generations: HashMap<String, String>,
    group_inbound_generations: HashMap<String, String>,
    pending_group_ice: HashMap<(String, String), VecDeque<PendingGroupIce>>,
    closed_group_generations: HashMap<(String, String), Instant>,
    group_outbound_ports: HashMap<String, u16>,
    group_outbound_target_bitrate_bps: Option<u32>,
    group_inbound_ports: HashMap<String, u16>,
    group_remote_textures: HashMap<String, egui::TextureHandle>,
    group_remote_sequences: HashMap<String, u64>,
    group_auto_focus_pending: HashSet<String>,
    group_peer_status: HashMap<String, String>,
    group_audio_status: HashMap<String, String>,
    focused_group_screen: Option<String>,
    last_group_metrics_log_at: Option<Instant>,
    screen_share_metrics: ScreenShareMetrics,
    logging: LoggingState,
    logging_level: LoggingLevel,
    pub(super) log_performance_metrics: bool,
    pub(super) log_errors_file: bool,
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

struct PendingGroupIce {
    queued_at: Instant,
    payload: String,
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
    fn join_address(&self) -> &str {
        self.selected_host_index
            .and_then(|index| self.saved_hosts.get(index))
            .map(|profile| profile.address.as_str())
            .unwrap_or(&self.server_url)
    }

    fn refresh_monitors(&mut self, force: bool) {
        if self.screen_capture.is_some() || (self.monitors_loaded && !force) {
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
                self.available_monitors.clear();
                tracing::warn!(error = %error, "Não foi possível enumerar monitores DXGI");
                self.dxgi_capture_error = Some(error);
            }
        }
    }

    fn show_notice(ui: &mut egui::Ui, prefix: &str, message: &str) {
        ui.horizontal_wrapped(|ui| {
            ui.label(egui::RichText::new(prefix).strong());
            ui.label(message);
        });
    }

    fn apply_monochrome_style(ui: &mut egui::Ui) {
        let dark_mode = ui.visuals().dark_mode;
        let visuals = ui.visuals_mut();
        if dark_mode {
            visuals.panel_fill = egui::Color32::from_rgb(18, 20, 24);
            visuals.window_fill = egui::Color32::from_rgb(25, 27, 34);
            visuals.extreme_bg_color = egui::Color32::from_rgb(13, 14, 17);
            visuals.faint_bg_color = egui::Color32::from_rgb(28, 31, 38);
            visuals.selection.bg_fill = egui::Color32::from_rgb(42, 54, 76);
            visuals.selection.stroke =
                egui::Stroke::new(1.0, egui::Color32::from_rgb(90, 130, 200));
            visuals.hyperlink_color = egui::Color32::from_rgb(96, 165, 250);
            visuals.warn_fg_color = egui::Color32::from_rgb(251, 191, 36);
            visuals.error_fg_color = egui::Color32::from_rgb(248, 113, 113);
        } else {
            visuals.selection.bg_fill = egui::Color32::from_gray(220);
            visuals.selection.stroke = egui::Stroke::new(1.0, egui::Color32::from_gray(55));
            visuals.hyperlink_color = egui::Color32::from_rgb(29, 78, 216);
            visuals.warn_fg_color = egui::Color32::from_rgb(180, 83, 9);
            visuals.error_fg_color = egui::Color32::from_rgb(185, 28, 28);
        }

        visuals.widgets.noninteractive.corner_radius = egui::CornerRadius::same(6);
        visuals.widgets.inactive.corner_radius = egui::CornerRadius::same(6);
        visuals.widgets.hovered.corner_radius = egui::CornerRadius::same(6);
        visuals.widgets.active.corner_radius = egui::CornerRadius::same(6);
        visuals.widgets.open.corner_radius = egui::CornerRadius::same(6);
        visuals.window_corner_radius = egui::CornerRadius::same(8);
        visuals.menu_corner_radius = egui::CornerRadius::same(6);
    }

    fn preferences_snapshot(&self) -> AppSettings {
        AppSettings {
            server_url: self.server_url.clone(),
            public_server_url: Some(self.public_server_url.clone()),
            saved_hosts: self.saved_hosts.clone(),
            selected_host_index: self.selected_host_index,
            stun_server_url: self.stun_server_url.clone(),
            monitor_gain_db: self.monitor_gain_db,
            remote_audio_default_volume_percent: self.remote_audio_default_volume_percent,
            logging_level: self.logging_level,
            log_performance_metrics: self.log_performance_metrics,
            log_errors_file: self.log_errors_file,
            video_decoder_preference: self.video_decoder_preference,
            show_local_preview: self.show_local_preview,
            include_system_audio: self.include_system_audio,
            excluded_audio_application_path: self.excluded_audio_application_path.clone(),
            profile_display_name: self.profile_display_name.clone(),
            create_room_mode: self.create_room_mode,
            use_turn_on_create: self.use_turn_on_create,
            may_host: self.may_host,
            control_ipv4: self.preferred_control_ipv4,
            ..Default::default()
        }
    }

    fn apply_preferences(&mut self, preferences: AppSettings) {
        self.server_url = preferences.server_url;
        self.public_server_url = preferences.public_server_url.unwrap_or_default();
        self.saved_hosts = preferences.saved_hosts;
        self.selected_host_index = preferences
            .selected_host_index
            .filter(|index| *index < self.saved_hosts.len());
        self.stun_server_url = preferences.stun_server_url;
        self.monitor_gain_db = preferences.monitor_gain_db;
        self.remote_audio_default_volume_percent = preferences.remote_audio_default_volume_percent;
        self.logging_level = preferences.logging_level;
        self.log_performance_metrics = preferences.log_performance_metrics;
        self.log_errors_file = preferences.log_errors_file;
        self.logging.set_errors_enabled(self.log_errors_file);
        self.video_decoder_preference = preferences.video_decoder_preference;
        self.show_local_preview = preferences.show_local_preview;
        self.include_system_audio = preferences.include_system_audio;
        self.excluded_audio_application_path = preferences.excluded_audio_application_path;
        self.profile_display_name = preferences.profile_display_name;
        self.capture_preview_enabled
            .store(self.show_local_preview, Ordering::Relaxed);
        self.create_room_mode = preferences.create_room_mode;
        self.use_turn_on_create = preferences.use_turn_on_create;
        self.may_host = preferences.may_host;
        self.preferred_control_ipv4 = preferences.control_ipv4;
    }

    pub(super) fn choose_profile_avatar(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("Imagens PNG ou JPEG", &["png", "jpg", "jpeg"])
            .pick_file()
        else {
            return;
        };

        let result = profile::normalize_avatar_file(&path);
        match result {
            Ok(avatar) => match profile::store_avatar(&avatar) {
                Ok(()) => {
                    self.profile_avatar_jpeg = Some(avatar);
                    self.profile_avatar_texture = None;
                    self.profile_avatar_error = None;
                }
                Err(error) => self.profile_avatar_error = Some(error),
            },
            Err(error) => self.profile_avatar_error = Some(error),
        }
    }

    pub(super) fn remove_profile_avatar(&mut self) {
        match profile::remove_avatar() {
            Ok(()) => {
                self.profile_avatar_jpeg = None;
                self.profile_avatar_texture = None;
                self.profile_avatar_error = None;
            }
            Err(error) => self.profile_avatar_error = Some(error),
        }
    }

    pub(super) fn local_profile_avatar_texture(
        &mut self,
        context: &egui::Context,
    ) -> Option<egui::TextureHandle> {
        if let Some(texture) = &self.profile_avatar_texture {
            return Some(texture.clone());
        }
        let bytes = self.profile_avatar_jpeg.as_deref()?;
        match profile::avatar_color_image(bytes) {
            Ok(image) => {
                let texture = context.load_texture(
                    "local-profile-avatar",
                    image,
                    egui::TextureOptions::LINEAR,
                );
                self.profile_avatar_texture = Some(texture.clone());
                Some(texture)
            }
            Err(error) => {
                self.profile_avatar_error = Some(error);
                None
            }
        }
    }

    pub(super) fn participant_avatar_texture(
        &mut self,
        context: &egui::Context,
        participant: &ParticipantInfo,
    ) -> Option<egui::TextureHandle> {
        let encoded = participant.avatar_jpeg_base64.clone();
        if let Some((cached, texture)) = self.participant_avatar_textures.get(&participant.id)
            && *cached == encoded
        {
            return texture.clone();
        }

        let texture = encoded
            .as_deref()
            .and_then(|encoded| profile::decode_avatar_payload(encoded).ok())
            .and_then(|bytes| profile::avatar_color_image(&bytes).ok())
            .map(|image| {
                context.load_texture(
                    format!("participant-avatar-{}", participant.id),
                    image,
                    egui::TextureOptions::LINEAR,
                )
            });
        self.participant_avatar_textures
            .insert(participant.id.clone(), (encoded, texture.clone()));
        texture
    }

    pub(super) fn save_preferences(&mut self) {
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

    pub(super) fn set_fullscreen(&mut self, context: &egui::Context, enabled: bool) {
        self.fullscreen_video = enabled;
        context.send_viewport_cmd(egui::ViewportCommand::Fullscreen(enabled));
    }

    pub(super) fn toggle_fullscreen(&mut self, context: &egui::Context) {
        self.set_fullscreen(context, !self.fullscreen_video);
    }

    fn show(&mut self, ui: &mut egui::Ui) {
        Self::apply_monochrome_style(ui);
        let context = ui.ctx().clone();
        if context.input(|i| i.key_pressed(egui::Key::F11)) {
            self.toggle_fullscreen(&context);
        }
        if self.fullscreen_video && context.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.set_fullscreen(&context, false);
        }
        let preferences_before_frame = self.preferences_snapshot();
        if self
            .code_copied_until
            .is_some_and(|expires_at| Instant::now() >= expires_at)
        {
            self.code_copied_until = None;
        }
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
        self.refresh_monitors(false);
        self.refresh_screen(&context);
        self.refresh_signaling(&context);
        self.refresh_turn_state();
        self.refresh_screen_share(&context);
        self.refresh_control_mesh(&context);
        self.refresh_updates(&context);
        self.handle_window_close(&context);

        let mut open_settings = false;
        let mut close_settings = false;
        let mut open_update_settings = false;
        let mut export_logs = false;

        if self.fullscreen_video && self.room_code.is_some() && !self.settings_open {
            egui::CentralPanel::default()
                .frame(egui::Frame::new().fill(egui::Color32::from_rgb(10, 10, 12)))
                .show(ui, |ui| {
                    Self::apply_monochrome_style(ui);
                    ui::show_fullscreen_video(self, ui);
                });
            return;
        }

        if self.room_code.is_some() && !self.settings_open {
            egui::Panel::bottom("room-controls")
                .frame(
                    egui::Frame::new()
                        .fill(egui::Color32::from_rgb(20, 22, 27))
                        .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(38, 42, 52)))
                        .inner_margin(egui::Margin::symmetric(12, 8)),
                )
                .show(ui, |ui| {
                    Self::apply_monochrome_style(ui);
                    ui::show_room_toolbar(self, ui);
                });
        } else if !self.settings_open {
            egui::Panel::bottom("home-bottom-bar")
                .frame(
                    egui::Frame::new()
                        .fill(egui::Color32::from_rgb(17, 19, 24))
                        .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(32, 36, 45)))
                        .inner_margin(egui::Margin::symmetric(12, 6)),
                )
                .show(ui, |ui| {
                    Self::apply_monochrome_style(ui);
                    ui::show_home_bottom_bar(
                        self,
                        ui,
                        &mut open_settings,
                        &mut open_update_settings,
                    );
                });
        }

        let mut open_logs_directory = false;
        egui::CentralPanel::default().show(ui, |ui| {
            Self::apply_monochrome_style(ui);
            egui::ScrollArea::vertical().show(ui, |ui| {
                let room_code = self.room_code.clone();
                let code_label = room_header_code_label(room_code.as_deref());
                let show_copy_confirmation =
                    copy_confirmation_visible(self.code_copied_until, Instant::now());
                let header_update_status = self.update_status.clone();
                let header_update_action =
                    update_shortcut_action(&header_update_status, room_code.is_some());
                let header_update_tooltip =
                    update_shortcut_tooltip(&header_update_status, room_code.is_some())
                        .unwrap_or_else(|| "Atualização".to_owned());
                let picker_open = self.screen_picker.is_some();
                let settings_open = self.settings_open;

                let show_top_header = room_code.is_some() || settings_open;
                if show_top_header {
                    let header_height = if show_copy_confirmation && code_label.is_some() {
                        48.0
                    } else {
                        30.0
                    };
                    let header_rect = allocate_app_header_row(ui, header_height);
                    let button_size = egui::vec2(30.0, 26.0);
                    let mut code_clicked = false;

                    if let Some(code_label) = code_label {
                        let text_color = ui.visuals().text_color();
                        let code_galley = ui.painter().layout_no_wrap(
                            code_label,
                            egui::TextStyle::Monospace.resolve(ui.style()),
                            text_color,
                        );
                        let code_band = egui::Rect::from_min_max(
                            header_rect.min,
                            egui::pos2(header_rect.max.x, header_rect.min.y + 30.0),
                        );
                        let code_rect = centered_header_rect(code_band, code_galley.size());
                        let response = ui
                            .interact(
                                code_rect,
                                egui::Id::new("copy-room-code"),
                                egui::Sense::click(),
                            )
                            .on_hover_text("Clique para copiar");
                        ui.painter().galley(code_rect.min, code_galley, text_color);
                        code_clicked = response.clicked();

                        if show_copy_confirmation {
                            let confirmation_galley = ui.painter().layout_no_wrap(
                                "Código copiado".to_owned(),
                                egui::TextStyle::Body.resolve(ui.style()),
                                text_color,
                            );
                            let confirmation_band = egui::Rect::from_min_max(
                                egui::pos2(header_rect.min.x, header_rect.min.y + 30.0),
                                header_rect.max,
                            );
                            let confirmation_rect =
                                centered_header_rect(confirmation_band, confirmation_galley.size());
                            ui.painter().galley(
                                confirmation_rect.min,
                                confirmation_galley,
                                text_color,
                            );
                        }
                    }

                    let settings_rect = right_aligned_header_rect(header_rect, button_size, 0.0);
                    let mut settings_ui = ui.new_child(
                        egui::UiBuilder::new()
                            .id_salt("settings-navigation-button")
                            .max_rect(settings_rect),
                    );
                    let (icon, tooltip) = settings_navigation_button(settings_open);
                    let settings_response = settings_ui
                        .add_enabled(
                            !picker_open,
                            egui::Button::new(egui::RichText::new(icon).size(18.0))
                                .min_size(button_size),
                        )
                        .on_hover_text(tooltip);
                    let settings_clicked = settings_response.clicked();

                    let mut update_clicked = false;
                    if let Some(action) = header_update_action {
                        let update_rect = right_aligned_header_rect(header_rect, button_size, 68.0);
                        let mut update_ui = ui.new_child(
                            egui::UiBuilder::new()
                                .id_salt("update-shortcut-button")
                                .max_rect(update_rect),
                        );
                        let enabled =
                            action != UpdateShortcutAction::DisabledForRoom && !picker_open;
                        let response = update_ui
                            .add_enabled(
                                enabled,
                                egui::Button::new(egui::RichText::new("↻").size(18.0))
                                    .min_size(button_size),
                            )
                            .on_hover_text(&header_update_tooltip);
                        update_clicked = response.clicked();
                    }

                    if code_clicked && let Some(code) = room_code {
                        ui.ctx().copy_text(code);
                        self.code_copied_until = Some(Instant::now() + Duration::from_secs(2));
                        ui.ctx().request_repaint_after(Duration::from_secs(2));
                    }
                    if settings_clicked {
                        if settings_open {
                            close_settings = true;
                        } else {
                            open_settings = true;
                        }
                    }
                    if update_clicked {
                        match header_update_action {
                            Some(UpdateShortcutAction::Download) => {
                                if let UpdateStatus::Available(manifest) = &header_update_status {
                                    let manifest = manifest.clone();
                                    tracing::info!(
                                        version = %manifest.version,
                                        "Download de atualização iniciado pelo atalho"
                                    );
                                    self.updates.download(manifest.clone());
                                    self.update_status = UpdateStatus::Downloading {
                                        manifest,
                                        received: 0,
                                    };
                                }
                            }
                            Some(UpdateShortcutAction::OpenUpdates) => {
                                open_update_settings = true;
                            }
                            Some(UpdateShortcutAction::DisabledForRoom) | None => {}
                        }
                    }

                    ui.add_space(12.0);
                }

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

                if open_update_settings {
                    self.open_settings();
                    self.settings_category = SettingsCategory::Updates;
                }
                if !self.settings_open && self.room_code.is_some() {
                    ui::show_handoff_panel(self, ui, &context);
                }
                if self.settings_open {
                    ui::show_settings(self, ui);
                } else if self.room_code.is_some() {
                    ui::show_room(self, ui);
                } else {
                    ui::show_home(self, ui);
                }

                ui::show_diagnostics(self, ui, &mut open_logs_directory, &mut export_logs);
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
        if open_logs_directory {
            self.open_logs_directory();
        }

        ui::show_profile_window(self, &context);

        if self.preferences_snapshot() != preferences_before_frame {
            self.settings_dirty = true;
            self.settings_save_at = Some(Instant::now() + Duration::from_millis(400));
        }
        if self.settings_dirty
            && let Some(save_at) = self.settings_save_at
        {
            let now = Instant::now();
            if now >= save_at {
                self.save_preferences();
                context.request_repaint();
            } else {
                context.request_repaint_after(save_at - now);
            }
        }
    }

    fn open_settings(&mut self) {
        if self.room_code.is_some() && self.screen_capture.is_some() {
            self.stop_screen_capture();
        }
        self.settings_category = SettingsCategory::Audio;
        self.settings_open = true;
    }

    fn open_connection_settings(&mut self) {
        if self.room_code.is_some() && self.screen_capture.is_some() {
            self.stop_screen_capture();
        }
        if self.settings_category == SettingsCategory::Audio {
            self.stop_microphone();
        }
        self.settings_category = SettingsCategory::Connection;
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
        if self.log_performance_metrics
            && self
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
            self.clear_local_preview();
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
                    self.clear_local_preview();
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
            self.clear_local_preview();
            return;
        };

        let frame = capture.latest_frame();

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
            self.clear_local_preview();
            self.screen_status = Some(match result {
                Ok(()) => "A captura da tela foi encerrada pelo Windows.".to_owned(),
                Err(error) => {
                    if was_dxgi {
                        self.dxgi_capture_error = Some(error.clone());
                    }
                    format!("A captura da tela falhou: {error}")
                }
            });
            return;
        }
        self.refresh_local_preview(context, preview_active, frame.as_deref());
    }

    fn refresh_local_preview(
        &mut self,
        context: &egui::Context,
        preview_active: bool,
        frame: Option<&PreviewFrame>,
    ) {
        if !preview_active {
            self.clear_local_preview();
            return;
        }
        let Some(frame) = frame else {
            return;
        };
        let key = LocalPreviewFrameKey::from(frame);
        if (self.local_preview_frame_key == Some(key) && self.screen_texture.is_some())
            || self
                .local_preview_failure
                .as_ref()
                .is_some_and(|(failed_key, _)| *failed_key == key)
        {
            return;
        }
        let image = match local_preview_image(frame) {
            Ok(Some(image)) => image,
            Ok(None) => return,
            Err(error) => {
                self.clear_local_preview();
                let message = format!(
                    "Não foi possível mostrar a prévia local; a transmissão continua: {error}"
                );
                self.screen_status = Some(message.clone());
                self.local_preview_failure = Some((key, message));
                return;
            }
        };
        if let Some(texture) = self.screen_texture.as_mut() {
            texture.set(image, egui::TextureOptions::LINEAR);
        } else {
            self.screen_texture =
                Some(context.load_texture("screen-preview", image, egui::TextureOptions::LINEAR));
        }
        self.local_preview_frame_key = Some(key);
        self.clear_local_preview_failure();
    }

    fn clear_local_preview_failure(&mut self) {
        if let Some((_, message)) = self.local_preview_failure.take()
            && self.screen_status.as_deref() == Some(message.as_str())
        {
            self.screen_status = None;
        }
    }

    fn clear_local_preview(&mut self) {
        self.screen_texture = None;
        self.local_preview_frame_key = None;
        self.clear_local_preview_failure();
    }

    fn stop_screen_capture(&mut self) {
        self.stop_screen_share(true);
        let result = self
            .screen_capture
            .take()
            .map_or(Ok(()), |mut capture| capture.stop());
        self.clear_local_preview();
        self.screen_status = Some(match result {
            Ok(()) => "Captura da tela parada.".to_owned(),
            Err(error) => {
                format!("A captura parou, mas houve um erro ao liberar o recurso: {error}")
            }
        });
    }

    fn request_screen_share(&mut self, context: &egui::Context) {
        self.audio_status = None;
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
        if self.room_mode == RoomMode::InternetTest
            && let Err(error) = screen_sharing::validate_stun_uri(&self.stun_server_url)
        {
            self.screen_share_status = Some(error);
            return;
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
                    } else if self.group_sharing_upgrade_required() {
                        self.screen_share_status = Some(
                            "Este participante não anuncia correlação de sessão. Para compartilhar em grupo, todos precisam atualizar para a versão 1.1.3.".to_owned(),
                        );
                    }
                }
            }
            SignalKind::ScreenShareUnavailable => {
                if let Some(peer_id) = from_participant_id {
                    self.group_available_shares.remove(&peer_id);
                    self.group_watched_shares.remove(&peer_id);
                    self.group_auto_focus_pending.remove(&peer_id);
                    if let Some(session) = self.group_inbound_sessions.remove(&peer_id) {
                        group_sharing::log_final_group_session_diagnostics(
                            &session,
                            "receiver",
                            "remote_share_unavailable",
                        );
                    }
                    if let Some(generation) = self.group_inbound_generations.remove(&peer_id) {
                        self.mark_group_generation_closed(&peer_id, generation);
                    }
                    self.group_audio_status.remove(&peer_id);
                    self.group_inbound_ports.remove(&peer_id);
                    self.group_remote_textures.remove(&peer_id);
                    self.group_remote_sequences.remove(&peer_id);
                    if self.focused_group_screen.as_deref() == Some(peer_id.as_str()) {
                        self.focused_group_screen = None;
                    }
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
                    let generation = self.group_outbound_generations.remove(&viewer_id);
                    if let Some(generation) = generation.as_ref() {
                        let _ = self.send_screen_share_signal_to_stream(
                            &viewer_id,
                            generation.clone(),
                            SignalKind::ScreenShareStopped,
                            String::new(),
                        );
                    }
                    if let Some(session) = self.group_outbound_sessions.remove(&viewer_id) {
                        group_sharing::log_final_group_session_diagnostics(
                            &session,
                            "sender",
                            "viewer_unwatched",
                        );
                    }
                    if let Some(generation) = generation {
                        self.mark_group_generation_closed(&viewer_id, generation);
                    }
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

                match ScreenShareSession::new_with_audio_volume(
                    context.clone(),
                    bind_ipv4,
                    self.stun_server_for_room(),
                    self.turn_credentials_for_room(),
                    self.video_decoder_preference,
                    self.remote_audio_default_volume_percent,
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
                match ScreenShareSession::new_with_audio_volume(
                    context.clone(),
                    bind_ipv4,
                    self.stun_server_for_room(),
                    self.turn_credentials_for_room(),
                    self.video_decoder_preference,
                    self.remote_audio_default_volume_percent,
                ) {
                    Ok(session) => {
                        if let Some(capture) = self.screen_capture.as_ref() {
                            let _ = capture.take_performance_snapshot();
                        }
                        if let Err(error) = session.start_sending_with_audio_exclusion(
                            source,
                            self.include_system_audio,
                            self.excluded_audio_application_path.clone(),
                        ) {
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
                        && stream_id.as_deref()
                            == self.group_inbound_generations.get(*id).map(String::as_str)
                }) {
                    self.group_auto_focus_pending.insert(peer_id.to_owned());
                    if let Some(session) = self.group_inbound_sessions.remove(peer_id) {
                        group_sharing::log_final_group_session_diagnostics(
                            &session,
                            "receiver",
                            "remote_share_reconfigured",
                        );
                    }
                    if let Some(generation) = self.group_inbound_generations.remove(peer_id) {
                        self.mark_group_generation_closed(peer_id, generation);
                    }
                    self.group_inbound_ports.remove(peer_id);
                    self.group_remote_textures.remove(peer_id);
                    self.group_remote_sequences.remove(peer_id);
                    self.group_peer_status.insert(
                        peer_id.to_owned(),
                        "A transmissão está sendo reconfigurada…".to_owned(),
                    );
                    self.group_audio_status.remove(peer_id);
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
                } else if let Some(session) = &self.screen_share_session
                    && let Err(error) = session.handle_signal(kind, payload)
                {
                    self.stop_screen_share(true);
                    self.screen_share_status = Some(error);
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
            let should_log_metrics = self.log_performance_metrics
                && self
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
                    track_ssrc = ?metrics.track_ssrc,
                    outbound_video_ssrc = ?metrics.outbound_video_ssrc,
                    inbound_video_ssrc = ?metrics.inbound_video_ssrc,
                    nack_sent_video_ssrc = ?metrics.inbound_video_ssrc,
                    nack_received_video_ssrc = ?metrics.outbound_video_ssrc,
                    assembled_idr_with_parameters_total = metrics.assembled_idr_with_parameters,
                    assembled_idr_without_parameters_total = metrics.assembled_idr_without_parameters,
                    assembled_delta_units_total = metrics.assembled_delta_units,
                    assembled_no_frame_units_total = metrics.assembled_no_frame_units,
                    decoder_queue_accepted_total = metrics.decoder_queue_accepted,
                    capture_width = metrics.capture_width,
                    capture_height = metrics.capture_height,
                    encoder_width = metrics.encoder_width,
                    encoder_height = metrics.encoder_height,
                    decoder_width = metrics.decoder_width,
                    decoder_height = metrics.decoder_height,
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
                    h264_pipeline_interval = %crate::screen_sharing::h264_pipeline_interval_label(&performance),
                    rtp_recovery_stats = %crate::screen_sharing::rtp_recovery_interval_label(metrics, &performance),
                    pli_queue_overflow = metrics.pli_queue_overflow,
                    rtc_rtp_out_packets = metrics.outbound_rtp_packets,
                    rtc_rtp_out_bytes = metrics.outbound_rtp_bytes,
                    rtc_rtp_in_packets = metrics.inbound_rtp_packets,
                    rtc_rtp_in_bytes = metrics.inbound_rtp_bytes,
                    rtc_rtp_in_lost = metrics.inbound_rtp_lost,
                    rtc_rtp_in_jitter_ms = metrics.inbound_rtp_jitter_ms,
                    rtp_reorder_window_ms = metrics.rtp_reorder_window_ms,
                    rtp_reorder_samples = metrics.rtp_reorder_samples,
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
                ScreenShareEvent::AudioState(status) => {
                    tracing::info!(screen_share_session, stage = "audio_pipeline_state", state = %status, "Estado agregado de áudio da sessão");
                    self.audio_status = Some(status);
                }
                ScreenShareEvent::Error(error) => {
                    tracing::error!(screen_share_session, error = %error, "Erro na sessão WebRTC de compartilhamento");
                    self.screen_share_status = Some(error);
                    stop_session = true;
                }
                ScreenShareEvent::AudioError(error) => {
                    tracing::error!(screen_share_session, stage = "audio_pipeline", error = %error, "Falha de áudio isolada; vídeo continua ativo");
                    self.screen_share_status = Some(error);
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
        if let Some(frame) = remote_frame
            && self.remote_screen_sequence != frame.sequence
        {
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
        if announce && let Some(request_id) = request_id {
            let _ = self.send_screen_share_signal(SignalKind::ScreenShareStopped, request_id);
        }
        if let Some(session) = self.screen_share_session.take() {
            let metrics = session.metrics();
            tracing::info!(
                screen_share_session = metrics.session_id,
                track_ssrc = ?metrics.track_ssrc,
                outbound_video_ssrc = ?metrics.outbound_video_ssrc,
                inbound_video_ssrc = ?metrics.inbound_video_ssrc,
                nack_sent_video_ssrc = ?metrics.inbound_video_ssrc,
                nack_received_video_ssrc = ?metrics.outbound_video_ssrc,
                nack_packets_received_observed_total = metrics.nack_packets_received_observed,
                nack_received_media_ssrc = ?metrics.nack_received_media_ssrc,
                nack_rtcp_packets_sent_total = %crate::screen_sharing::optional_count_label(metrics.nack_packets_sent),
                nack_rtcp_packets_received_total = %crate::screen_sharing::optional_count_label(metrics.nack_packets_received),
                retransmitted_packets_sent_total = %crate::screen_sharing::optional_count_label(metrics.retransmitted_packets_sent),
                retransmitted_bytes_sent_total = %crate::screen_sharing::optional_count_label(metrics.retransmitted_bytes_sent),
                retransmitted_packets_received_total = %crate::screen_sharing::optional_count_label(metrics.retransmitted_packets_received),
                retransmitted_bytes_received_total = %crate::screen_sharing::optional_count_label(metrics.retransmitted_bytes_received),
                role = match &self.screen_share_role {
                    ScreenShareRole::Sending { .. } => "sender",
                    ScreenShareRole::Receiving { .. } => "receiver",
                    ScreenShareRole::Requesting { .. } => "requesting",
                    ScreenShareRole::Idle => "idle",
                },
                phase = "final",
                termination_reason = "local_stop",
                video_track_seen = session.remote_video_track_seen(),
                encoded_frames = metrics.encoded_frames,
                sent_frames = metrics.sent_frames,
                received_rtp_packets = metrics.received_packets,
                inbound_rtp_packets = metrics.inbound_rtp_packets,
                decoded_frames = metrics.decoded_frames,
                published_frames = metrics.published_frames,
                decode_errors = metrics.decode_errors,
                rtp_reorder_window_ms = metrics.rtp_reorder_window_ms,
                rtp_reorder_samples = metrics.rtp_reorder_samples,
                selected_ice_pair = %metrics.selected_ice_pair,
                rtc_outbound = %metrics.rtc_outbound_summary,
                rtc_inbound = %metrics.rtc_inbound_summary,
                h264 = %metrics.h264_diagnostics,
                "Resumo final do compartilhamento antes de liberar a sessão"
            );
            session.stop();
        }
        self.last_screen_metrics_log_at = None;
        self.audio_status = None;
        self.screen_share_role = ScreenShareRole::Idle;
        self.remote_screen_texture = None;
        self.remote_screen_sequence = 0;
        if was_sending {
            self.clear_local_preview();
            self.capture_preview_enabled.store(false, Ordering::Relaxed);
        }
        if was_active {
            self.screen_share_status = Some("Compartilhamento de tela encerrado.".to_owned());
        }
    }

    fn stun_server_for_room(&self) -> Option<String> {
        (self.room_mode == RoomMode::InternetTest).then(|| self.stun_server_url.trim().to_owned())
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
            logging_status: self.logging.status_label(),
            room_mode: match self.room_mode {
                RoomMode::Local => "local/Radmin",
                RoomMode::InternetTest => "internet (teste)",
            }
            .to_owned(),
            connected_to_signaling: self.signaling.is_some() && !self.connecting,
            participant_count: self.participants.len(),
            signaling_address: safe_signaling_endpoint(
                if self.hosting_locally && self.room_mode == RoomMode::InternetTest {
                    &self.public_server_url
                } else {
                    self.join_address()
                },
            ),
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
                format_args!(
                    "janela adaptativa RTP={} ms/{} amostras; {}",
                    metrics.rtp_reorder_window_ms,
                    metrics.rtp_reorder_samples,
                    metrics.h264_diagnostics
                )
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

fn validate_saved_host_profile(profile: &SavedHostProfile) -> Result<(), String> {
    validate_host_name(&profile.name)?;
    let address = profile.address.trim();
    if address.len() > 512 {
        return Err("O endereço excede 512 caracteres.".to_owned());
    }
    signaling_ws_url(address).map(|_| ())
}

fn validate_new_host_profile(
    name: &str,
    address: &str,
    current_count: usize,
) -> Result<(), String> {
    if current_count >= MAX_SAVED_HOSTS {
        return Err(format!(
            "A lista já atingiu o limite de {MAX_SAVED_HOSTS} anfitriões."
        ));
    }
    validate_host_name(name)?;
    validate_saved_host_profile(&SavedHostProfile {
        name: name.trim().to_owned(),
        address: address.trim().to_owned(),
    })
}

fn validate_host_name(name: &str) -> Result<(), String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("Informe um apelido para este anfitrião.".to_owned());
    }
    if name.chars().count() > 64 {
        return Err("O apelido pode ter no máximo 64 caracteres.".to_owned());
    }
    Ok(())
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

pub(super) fn run() -> eframe::Result {
    let arguments: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    if let Some(exit_code) = update::helper_arguments(&arguments) {
        std::process::exit(exit_code);
    }
    let mut app = ClientUi::default();
    app.microphone_level_dbfs = -60.0;
    let (preferences, settings_warning) = settings::load();
    app.apply_preferences(preferences);
    app.logging = LoggingState::initialize(app.logging_level);
    app.logging.set_errors_enabled(app.log_errors_file);
    logging::install_panic_hook();
    app.updates.check();
    app.settings_error = settings_warning;
    match profile::load_avatar() {
        Ok(avatar) => app.profile_avatar_jpeg = avatar,
        Err(error) => app.profile_avatar_error = Some(error),
    }
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
        allocate_app_header_row, centered_header_rect, copy_confirmation_visible, egui,
        is_public_ipv4_candidate, right_aligned_header_rect, room_header_code_label,
        settings_navigation_button, signaling_ws_url, validate_new_host_profile,
        validate_saved_host_profile,
    };
    use crate::settings::{MAX_SAVED_HOSTS, SavedHostProfile};
    use std::net::Ipv4Addr;

    fn local_preview_frame(
        sequence: u64,
        width: u32,
        height: u32,
    ) -> crate::screen_capture::PreviewFrame {
        crate::screen_capture::PreviewFrame {
            sequence,
            width,
            height,
            rgba: vec![96; (width * height * 4) as usize],
            #[cfg(windows)]
            gpu_nv12: None,
            #[cfg(windows)]
            cpu_nv12: None,
        }
    }

    fn render_local_preview(
        app: &mut super::ClientUi,
        context: &egui::Context,
        active: bool,
        frame: Option<&crate::screen_capture::PreviewFrame>,
    ) -> usize {
        let previous_id = app.screen_texture.as_ref().map(egui::TextureHandle::id);
        let mut output = context.run_ui(egui::RawInput::default(), |_| {
            app.refresh_local_preview(context, active, frame);
        });
        let texture_id = app
            .screen_texture
            .as_ref()
            .map(egui::TextureHandle::id)
            .or(previous_id);
        let uploads = texture_id
            .and_then(|id| output.textures_delta.set.get(&id))
            .map_or(0, |deltas| deltas.len());
        // The tests inspect upload requests without a GPU renderer.
        output.textures_delta.clear();
        uploads
    }

    #[cfg(windows)]
    fn local_preview_nv12_frame(sequence: u64) -> crate::screen_capture::PreviewFrame {
        let mut frame = local_preview_frame(sequence, 2, 2);
        frame.rgba.clear();
        frame.cpu_nv12 = Some(std::sync::Arc::new(crate::screen_capture::CpuNv12Frame {
            bytes: std::sync::Arc::new(vec![16, 16, 16, 16, 128, 128]),
            stride: 2,
        }));
        frame
    }

    #[test]
    fn local_preview_repeated_frame_uploads_only_once() {
        let context = egui::Context::default();
        let mut app = super::ClientUi::default();
        let frame = local_preview_frame(1, 2, 2);
        let mut uploads = 0;
        for _ in 0..10 {
            uploads += render_local_preview(&mut app, &context, true, Some(&frame));
        }
        assert_eq!(
            uploads, 1,
            "the same captured frame must not be uploaded on every UI cycle"
        );
    }

    #[test]
    fn local_preview_updates_for_new_sequence_dimensions_and_sequence_wrap() {
        let context = egui::Context::default();
        let mut app = super::ClientUi::default();
        let frames = [
            local_preview_frame(u64::MAX, 2, 2),
            local_preview_frame(0, 2, 2),
            local_preview_frame(0, 4, 2),
            local_preview_frame(0, 4, 4),
        ];
        let mut texture_id = None;
        for frame in &frames {
            assert_eq!(
                render_local_preview(&mut app, &context, true, Some(frame)),
                1
            );
            let texture = app.screen_texture.as_ref().unwrap();
            assert_eq!(
                texture.size(),
                [frame.width as usize, frame.height as usize]
            );
            assert_eq!(*texture_id.get_or_insert(texture.id()), texture.id());
            assert_eq!(
                app.local_preview_frame_key,
                Some(super::LocalPreviewFrameKey::from(frame))
            );
            assert_eq!(
                render_local_preview(&mut app, &context, true, Some(frame)),
                0
            );
        }
    }

    #[test]
    fn local_preview_recreates_missing_texture_for_the_same_frame() {
        let context = egui::Context::default();
        let mut app = super::ClientUi::default();
        let frame = local_preview_frame(1, 2, 2);
        assert_eq!(
            render_local_preview(&mut app, &context, true, Some(&frame)),
            1
        );
        let previous_id = app.screen_texture.take().unwrap().id();
        assert!(app.local_preview_frame_key.is_some());
        assert_eq!(
            render_local_preview(&mut app, &context, true, Some(&frame)),
            1
        );
        assert_ne!(app.screen_texture.as_ref().unwrap().id(), previous_id);
        assert_eq!(
            render_local_preview(&mut app, &context, true, Some(&frame)),
            0
        );
    }

    #[test]
    fn local_preview_hide_and_reopen_restores_a_static_frame_once() {
        let context = egui::Context::default();
        let mut app = super::ClientUi::default();
        let frame = local_preview_frame(1, 2, 2);
        assert_eq!(
            render_local_preview(&mut app, &context, true, Some(&frame)),
            1
        );
        assert_eq!(
            render_local_preview(&mut app, &context, false, Some(&frame)),
            0
        );
        assert!(app.screen_texture.is_none());
        assert!(app.local_preview_frame_key.is_none());
        assert_eq!(
            render_local_preview(&mut app, &context, true, Some(&frame)),
            1
        );
        assert_eq!(
            render_local_preview(&mut app, &context, true, Some(&frame)),
            0
        );
    }

    #[test]
    fn local_preview_absent_data_does_not_advance_the_presented_key() {
        let context = egui::Context::default();
        let mut app = super::ClientUi::default();
        let mut frame = local_preview_frame(1, 2, 2);
        frame.rgba.clear();
        assert_eq!(render_local_preview(&mut app, &context, true, None), 0);
        assert_eq!(
            render_local_preview(&mut app, &context, true, Some(&frame)),
            0
        );
        assert!(app.screen_texture.is_none());
        assert!(app.local_preview_frame_key.is_none());
        assert!(app.local_preview_failure.is_none());
        assert!(app.screen_status.is_none());
        frame.rgba = vec![96; 16];
        assert_eq!(
            render_local_preview(&mut app, &context, true, Some(&frame)),
            1
        );
        let last_key = app.local_preview_frame_key;
        frame.sequence += 1;
        frame.rgba.clear();
        assert_eq!(
            render_local_preview(&mut app, &context, true, Some(&frame)),
            0
        );
        assert_eq!(app.local_preview_frame_key, last_key);
        assert!(app.screen_texture.is_some());
    }

    #[test]
    fn local_preview_invalid_rgba_only_stops_preview_and_caches_failure() {
        let context = egui::Context::default();
        let mut app = super::ClientUi::default();
        app.screen_share_role = super::ScreenShareRole::Sending {
            request_id: "test".to_owned(),
        };
        app.group_local_sharing = true;
        let mut frame = local_preview_frame(1, 2, 2);
        assert_eq!(
            render_local_preview(&mut app, &context, true, Some(&frame)),
            1
        );
        frame.sequence += 1;
        frame.rgba.truncate(3);
        assert_eq!(
            render_local_preview(&mut app, &context, true, Some(&frame)),
            0
        );
        assert!(app.screen_texture.is_none());
        assert!(app.local_preview_frame_key.is_none());
        assert!(app.screen_status.as_ref().unwrap().contains("prévia local"));
        assert!(app.local_preview_failure.is_some());
        assert!(matches!(
            app.screen_share_role,
            super::ScreenShareRole::Sending { .. }
        ));
        assert!(app.group_local_sharing);
        // A retry would replace this status with the conversion error again.
        app.screen_status = Some("Outro aviso de captura".to_owned());
        assert_eq!(
            render_local_preview(&mut app, &context, true, Some(&frame)),
            0
        );
        assert_eq!(app.screen_status.as_deref(), Some("Outro aviso de captura"));
        let valid = local_preview_frame(3, 2, 2);
        assert_eq!(
            render_local_preview(&mut app, &context, true, Some(&valid)),
            1
        );
        assert!(app.local_preview_failure.is_none());
        assert_eq!(app.screen_status.as_deref(), Some("Outro aviso de captura"));
    }

    #[cfg(windows)]
    #[test]
    fn local_preview_reopens_static_nv12_without_another_capture_frame() {
        let context = egui::Context::default();
        let mut app = super::ClientUi::default();
        let rgba = local_preview_frame(1, 2, 2);
        assert_eq!(
            render_local_preview(&mut app, &context, true, Some(&rgba)),
            1
        );
        assert_eq!(render_local_preview(&mut app, &context, false, None), 0);
        let nv12 = local_preview_nv12_frame(1);
        let image = super::local_preview_image(&nv12).unwrap().unwrap();
        assert_eq!(image.size, [2, 2]);
        assert_eq!(image.pixels, vec![egui::Color32::BLACK; 4]);
        assert_eq!(
            render_local_preview(&mut app, &context, true, Some(&nv12)),
            1
        );
        assert_eq!(
            render_local_preview(&mut app, &context, true, Some(&nv12)),
            0
        );
    }

    #[cfg(windows)]
    #[test]
    fn local_preview_nv12_failure_retries_only_after_new_frame_or_reactivation() {
        let context = egui::Context::default();
        let mut app = super::ClientUi::default();
        app.group_local_sharing = true;
        let mut frame = local_preview_nv12_frame(1);
        frame
            .cpu_nv12
            .as_mut()
            .unwrap()
            .clone_from(&std::sync::Arc::new(crate::screen_capture::CpuNv12Frame {
                bytes: std::sync::Arc::new(vec![16]),
                stride: 2,
            }));
        assert_eq!(
            render_local_preview(&mut app, &context, true, Some(&frame)),
            0
        );
        assert!(app.screen_status.is_some());
        assert!(app.local_preview_frame_key.is_none());
        app.screen_status = None;
        for _ in 0..10 {
            assert_eq!(
                render_local_preview(&mut app, &context, true, Some(&frame)),
                0
            );
        }
        assert!(
            app.screen_status.is_none(),
            "unchanged failed frames must not retry conversion"
        );
        frame.sequence += 1;
        assert_eq!(
            render_local_preview(&mut app, &context, true, Some(&frame)),
            0
        );
        assert!(
            app.screen_status.is_some(),
            "a new frame permits another attempt"
        );
        assert_eq!(render_local_preview(&mut app, &context, false, None), 0);
        assert!(app.local_preview_failure.is_none());
        assert!(app.screen_status.is_none());
        assert_eq!(
            render_local_preview(&mut app, &context, true, Some(&frame)),
            0
        );
        assert!(
            app.screen_status.is_some(),
            "reopening also permits another attempt"
        );
        assert_eq!(
            render_local_preview(&mut app, &context, true, Some(&local_preview_nv12_frame(3))),
            1
        );
        assert!(
            app.screen_status.is_none(),
            "success clears the preview's own error"
        );
        assert!(app.group_local_sharing);
    }

    #[cfg(windows)]
    #[test]
    fn local_preview_rejects_odd_nv12_dimensions_without_panicking() {
        let mut frame = local_preview_nv12_frame(1);
        frame.width = 1;
        assert!(
            super::local_preview_image(&frame)
                .unwrap_err()
                .contains("dimensões pares")
        );
        frame.width = 2;
        frame.height = 1;
        assert!(
            super::local_preview_image(&frame)
                .unwrap_err()
                .contains("dimensões pares")
        );
    }

    #[test]
    fn local_preview_lifecycle_reset_allows_a_new_capture_to_reuse_sequence() {
        let context = egui::Context::default();
        let frame = local_preview_frame(1, 2, 2);
        let stop_actions: [fn(&mut super::ClientUi); 4] = [
            |app| app.stop_screen_share(false),
            super::ClientUi::stop_screen_capture,
            super::ClientUi::leave_room,
            |app| app.stop_group_media(false),
        ];
        for stop in stop_actions {
            let mut app = super::ClientUi::default();
            app.screen_share_role = super::ScreenShareRole::Sending {
                request_id: "test".to_owned(),
            };
            assert_eq!(
                render_local_preview(&mut app, &context, true, Some(&frame)),
                1
            );
            stop(&mut app);
            assert!(app.screen_texture.is_none());
            assert!(app.local_preview_frame_key.is_none());
            assert!(app.local_preview_failure.is_none());
            assert_eq!(
                render_local_preview(&mut app, &context, true, Some(&frame)),
                1
            );
        }
    }

    #[test]
    fn local_preview_missing_capture_clears_a_stale_texture() {
        let context = egui::Context::default();
        let mut app = super::ClientUi::default();
        app.group_local_sharing = true;
        app.show_local_preview = true;
        let frame = local_preview_frame(1, 2, 2);
        assert_eq!(
            render_local_preview(&mut app, &context, true, Some(&frame)),
            1
        );
        app.refresh_screen(&context);
        assert!(app.screen_texture.is_none());
        assert!(app.local_preview_frame_key.is_none());
    }

    #[test]
    fn settings_navigation_uses_icon_only_with_accessible_tooltip() {
        assert_eq!(settings_navigation_button(false), ("⚙", "Configurações"));
        assert_eq!(settings_navigation_button(true), ("←", "Voltar"));
    }

    #[test]
    fn room_code_is_available_for_the_header_only_when_in_a_room() {
        assert_eq!(room_header_code_label(None), None);
        assert_eq!(
            room_header_code_label(Some("033918DD")),
            Some("Código: 033918DD".to_owned())
        );
    }

    #[test]
    fn room_code_copy_confirmation_expires_after_two_seconds() {
        let now = std::time::Instant::now();
        let expires_at = now + std::time::Duration::from_secs(2);

        assert!(copy_confirmation_visible(Some(expires_at), now));
        assert!(copy_confirmation_visible(
            Some(expires_at),
            now + std::time::Duration::from_millis(1999)
        ));
        assert!(!copy_confirmation_visible(Some(expires_at), expires_at));
        assert!(!copy_confirmation_visible(None, now));
    }

    #[test]
    fn header_row_stays_compact_and_centers_code_while_right_aligning_controls() {
        let context = egui::Context::default();
        let mut header_rect = None;
        let mut center_rect = None;
        let mut settings_rect = None;
        let mut following_content_rect = None;

        let mut output = context.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1200.0, 800.0),
                )),
                ..Default::default()
            },
            |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    let row = allocate_app_header_row(ui, 30.0);
                    header_rect = Some(row);
                    center_rect = Some(centered_header_rect(row, egui::vec2(140.0, 20.0)));
                    settings_rect =
                        Some(right_aligned_header_rect(row, egui::vec2(30.0, 26.0), 0.0));
                    ui.add_space(12.0);
                    following_content_rect = Some(ui.allocate_space(egui::vec2(10.0, 10.0)).1);
                });
            },
        );
        output.textures_delta.clear();

        let header_rect = header_rect.expect("header row should be laid out");
        let center_rect = center_rect.expect("center column should be laid out");
        let settings_rect = settings_rect.expect("settings control should be laid out");
        let following_content_rect =
            following_content_rect.expect("following content should be laid out");

        assert!((header_rect.height() - 30.0).abs() < 0.1);
        assert!((center_rect.center().x - header_rect.center().x).abs() < 0.1);
        assert!((settings_rect.right() - header_rect.right()).abs() < 0.1);
        assert!(following_content_rect.min.y - header_rect.max.y < 50.0);
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
    fn saved_host_profiles_validate_name_address_and_limit() {
        assert!(validate_new_host_profile("Friend", "192.168.1.20", 0).is_ok());
        assert!(validate_new_host_profile("  ", "192.168.1.20", 0).is_err());
        assert!(validate_new_host_profile("Friend", "not an address", 0).is_err());
        assert!(validate_new_host_profile("Friend", "192.168.1.20", MAX_SAVED_HOSTS).is_err());
        assert!(validate_new_host_profile("Friend", "192.168.1.20", MAX_SAVED_HOSTS - 1).is_ok());
        assert!(
            validate_saved_host_profile(&SavedHostProfile {
                name: "Friend".to_owned(),
                address: "ws://friend.example.net:9000".to_owned(),
            })
            .is_ok()
        );
    }

    #[test]
    fn selected_saved_host_is_used_for_join_without_changing_own_invite_address() {
        let mut ui = super::ClientUi::default();
        ui.server_url = "manual.example.net".to_owned();
        ui.public_server_url = "my-room.example.net".to_owned();
        ui.saved_hosts = vec![SavedHostProfile {
            name: "Friend".to_owned(),
            address: "friend.example.net".to_owned(),
        }];
        ui.selected_host_index = Some(0);

        assert_eq!(ui.join_address(), "friend.example.net");
        assert_eq!(ui.public_server_url, "my-room.example.net");
        ui.selected_host_index = None;
        assert_eq!(ui.join_address(), "manual.example.net");
    }

    #[test]
    fn turn_host_address_requires_a_public_ipv4_candidate() {
        assert!(is_public_ipv4_candidate(Ipv4Addr::new(8, 8, 8, 8)));
        assert!(!is_public_ipv4_candidate(Ipv4Addr::new(192, 168, 1, 5)));
        assert!(!is_public_ipv4_candidate(Ipv4Addr::new(100, 80, 2, 3)));
        assert!(!is_public_ipv4_candidate(Ipv4Addr::new(203, 0, 113, 5)));
    }
}
