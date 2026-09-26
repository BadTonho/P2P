#![windows_subsystem = "windows"]

mod audio_capture;
mod screen_capture;
mod signaling_client;

use std::net::Ipv4Addr;
use std::time::Duration;

use audio_capture::MicrophoneTest;
use eframe::egui;
use screen_capture::{PendingScreenCapture, ScreenCapture};
use signaling_client::{SignalingClient, SignalingEvent};
use signaling_protocol::SignalKind;

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum SettingsCategory {
    #[default]
    Audio,
    Connection,
}

#[derive(Default)]
struct ClientUi {
    room_code: Option<String>,
    join_code: String,
    code_copied: bool,
    settings_open: bool,
    settings_category: SettingsCategory,
    server_url: String,
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
}

#[derive(Clone)]
struct HostAddress {
    adapter: String,
    ipv4: Ipv4Addr,
}

impl ClientUi {
    fn show(&mut self, ui: &mut egui::Ui) {
        let context = ui.ctx().clone();
        if self.microphone.is_some()
            || self.screen_capture.is_some()
            || self.screen_picker.is_some()
            || self.connecting
            || self.signaling.is_some()
        {
            ui.ctx().request_repaint_after(Duration::from_millis(100));
        }
        self.refresh_microphone();
        self.refresh_screen(ui.ctx());
        self.refresh_signaling(ui.ctx());
        self.handle_window_close(ui.ctx());

        egui::ScrollArea::vertical().show(ui, |ui| {
            let mut open_settings = false;
            let mut close_settings = false;
            let settings_open = self.settings_open;

            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.add_space(16.0);
                    ui.heading("P2P - Voz e tela");
                    ui.label("Sinalização na rede local — sem áudio ou tela transmitidos");
                });

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
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

            ui.add_space(20.0);

            if open_settings {
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
        ui.group(|ui| {
            ui.heading("Criar uma sala");
            ui.label(
                "Crie uma sala hospedada neste computador e compartilhe o código com seu amigo.",
            );

            if ui
                .add_enabled(!self.connecting, egui::Button::new("Criar sala"))
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

                let has_code = !self.join_code.trim().is_empty() && !self.connecting;
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
        ui.small("Para entrar, informe em Configurações > Conexão o endereço IP que o anfitrião compartilhou.");
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
            if self.hosting_locally {
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
            ui.label("A imagem fica apenas na memória deste aplicativo. Ela não é salva nem transmitida.");

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
        });

        ui.add_space(12.0);
        if ui
            .add_enabled(
                self.screen_picker.is_none() && self.outgoing_transfer.is_none(),
                egui::Button::new(if self.hosting_locally && self.peer_connected {
                    "Transferir hospedagem e sair"
                } else {
                    "Sair da sala"
                }),
            )
            .clicked()
        {
            self.request_leave(ui.ctx());
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
            });

            ui.separator();

            ui.vertical(|ui| match self.settings_category {
                SettingsCategory::Audio => self.show_audio_settings(ui),
                SettingsCategory::Connection => self.show_connection_settings(ui),
            });
        });
    }

    fn show_connection_settings(&mut self, ui: &mut egui::Ui) {
        ui.heading("Conexão");
        ui.label("Endereço WebSocket do computador do anfitrião.");
        ui.add(
            egui::TextEdit::singleline(&mut self.server_url)
                .hint_text("ws://192.168.1.10:9000")
                .desired_width(300.0),
        );
        ui.small("O endereço fica somente na memória enquanto este aplicativo estiver aberto.");
        ui.small("Use o endereço ws://IP:9000 que o anfitrião compartilhou.");
        ui.small("No Radmin VPN, ambos precisam estar conectados à mesma rede virtual. O anfitrião precisa permitir a porta 9000 no firewall.");
        ui.small("Criar uma sala inicia o servidor neste computador; não é necessário manter um notebook separado ligado.");
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
        self.microphone_error = None;
        self.microphone_monitor_error = None;
        self.microphone_audio_warning = false;
        self.microphone_clipping_warning = false;
        self.microphone_level = 0.0;
        self.microphone_level_dbfs = -60.0;
        match MicrophoneTest::start(self.monitor_gain_db) {
            Ok(test) => self.microphone = Some(test),
            Err(error) => self.microphone_error = Some(error),
        }
    }

    fn stop_microphone(&mut self) {
        self.microphone = None;
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
        self.microphone_clipping_warning = clipping_warning;
        if audio_warning {
            self.microphone_audio_warning = true;
        }
        if let Some(error) = monitor_error {
            self.microphone_monitor_error = Some(error);
        }
        if let Some(error) = microphone_error {
            self.microphone = None;
            self.microphone_level = 0.0;
            self.microphone_error = Some(error);
        }
    }

    fn select_screen(&mut self, _context: &egui::Context) {
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
                    self.screen_status = Some(format!(
                        "Não foi possível iniciar a captura da tela: {error}"
                    ));
                }
            }
        }

        let Some(capture) = self.screen_capture.as_mut() else {
            return;
        };

        if let Some(frame) = capture.take_latest_frame() {
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
            self.stop_screen_capture();
            self.screen_status =
                Some("A tela ou janela escolhida foi fechada; a captura terminou.".to_owned());
            return;
        }

        if let Some(result) = capture.poll_finished() {
            self.screen_capture = None;
            self.screen_texture = None;
            self.screen_status = Some(match result {
                Ok(()) => "A captura da tela foi encerrada pelo Windows.".to_owned(),
                Err(error) => format!("A captura da tela falhou: {error}"),
            });
        }
    }

    fn stop_screen_capture(&mut self) {
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

    fn enter_room(&mut self, code: String) {
        self.room_code = Some(code);
        self.code_copied = false;
        self.microphone_error = None;
        self.connection_error = None;
        self.screen_status = None;
    }

    fn start_hosting(&mut self) {
        if self.signaling.is_some() || self.connecting {
            return;
        }
        self.refresh_host_addresses();
        self.connection_error = None;
        self.connection_status = Some("Iniciando servidor de sala neste computador…".to_owned());
        self.connecting = true;
        self.starting_host = true;
        self.peer_connected = false;
        self.diagnostic_status = None;
        self.room_code = None;
        self.hosting_locally = false;

        match SignalingClient::start_host() {
            Ok(client) => self.signaling = Some(client),
            Err(error) => {
                self.connecting = false;
                self.starting_host = false;
                self.connection_status = Some("Desconectado.".to_owned());
                self.connection_error = Some(error);
            }
        }
    }

    fn start_join(&mut self, code: String) {
        if self.signaling.is_some() || self.connecting {
            return;
        }
        self.connection_error = None;
        self.connection_status = Some("Conectando ao anfitrião…".to_owned());
        self.connecting = true;
        self.starting_host = false;
        self.hosting_locally = false;
        self.peer_connected = false;
        self.diagnostic_status = None;
        self.room_code = None;

        match SignalingClient::join(self.server_url.trim().to_owned(), code) {
            Ok(client) => self.signaling = Some(client),
            Err(error) => {
                self.connecting = false;
                self.connection_status = Some("Desconectado.".to_owned());
                self.connection_error = Some(error);
            }
        }
    }

    fn refresh_host_addresses(&mut self) {
        match enumerate_host_addresses() {
            Ok(addresses) => {
                self.host_addresses = addresses;
                self.host_addresses_error = None;
                if self.selected_host_address >= self.host_addresses.len() {
                    self.selected_host_address = 0;
                }
            }
            Err(error) => {
                self.host_addresses.clear();
                self.host_addresses_error = Some(error);
            }
        }
    }

    fn show_host_address_picker(&mut self, ui: &mut egui::Ui) {
        if !self.host_addresses.is_empty() {
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

    fn refresh_signaling(&mut self, context: &egui::Context) {
        let pending_events = self
            .pending_signaling
            .as_ref()
            .map(|client| std::iter::from_fn(|| client.try_recv()).collect::<Vec<_>>())
            .unwrap_or_default();
        for event in pending_events {
            match event {
                SignalingEvent::RoomAdopted(code) => {
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
                SignalingEvent::Error(error) | SignalingEvent::ServerError(error) => {
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
                    self.connecting = false;
                    self.starting_host = false;
                    self.hosting_locally = true;
                    self.connection_status =
                        Some("Sala hospedada neste computador; aguardando seu amigo.".to_owned());
                    self.enter_room(code);
                }
                SignalingEvent::RoomJoined(code) => {
                    self.connecting = false;
                    self.peer_connected = true;
                    self.connection_status = Some("Você entrou na sala do anfitrião.".to_owned());
                    self.enter_room(code);
                }
                SignalingEvent::RoomAdopted(_) => {}
                SignalingEvent::PeerJoined => {
                    self.connecting = false;
                    self.peer_connected = true;
                    self.connection_status = Some("Seu amigo entrou na sala.".to_owned());
                }
                SignalingEvent::PeerLeft => {
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
                SignalingEvent::HostTransferPending { code, token } => {
                    self.outgoing_transfer = Some((code, token));
                    self.handoff_error = None;
                }
                SignalingEvent::HostTransferRequested { code, token } => {
                    self.incoming_transfer = Some((code, token));
                    self.handoff_error = None;
                }
                SignalingEvent::HostTransferComplete(code) => {
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
                    } else {
                        if self
                            .outgoing_transfer
                            .as_ref()
                            .is_some_and(|(_, token)| token.is_empty())
                        {
                            self.outgoing_transfer = None;
                        }
                        self.handoff_error = Some(error);
                    }
                }
                SignalingEvent::Signal {
                    kind: SignalKind::Diagnostic,
                    payload,
                } => match payload.as_str() {
                    "diagnostic-ping-v1" => {
                        self.diagnostic_status =
                            Some("Sinal recebido; enviando confirmação ao seu amigo.".to_owned());
                        acknowledge_diagnostic = true;
                    }
                    "diagnostic-pong-v1" => {
                        self.diagnostic_status = Some(
                            "Seu amigo confirmou o recebimento do sinal de diagnóstico.".to_owned(),
                        );
                    }
                    _ => {}
                },
                SignalingEvent::Signal { .. } => {
                    self.connection_status = Some("Sinal de conexão recebido.".to_owned());
                }
                SignalingEvent::Error(error) => {
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
        if self.hosting_locally && self.peer_connected {
            self.request_host_transfer();
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
                self.request_host_transfer();
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
                        self.leave_room();
                        if self.close_after_transfer {
                            self.allow_window_close = true;
                            context.send_viewport_cmd(egui::ViewportCommand::Close);
                        }
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
                        self.leave_room();
                        if self.close_after_transfer {
                            self.allow_window_close = true;
                            context.send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                    }
                });
            });
        }
    }

    fn leave_room(&mut self) {
        self.stop_microphone();
        self.screen_picker = None;
        if let Some(mut capture) = self.screen_capture.take() {
            let _ = capture.stop();
        }
        self.screen_texture = None;
        self.room_code = None;
        self.join_code.clear();
        self.code_copied = false;
        self.microphone_error = None;
        self.screen_status = None;
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

impl Drop for ClientUi {
    fn drop(&mut self) {
        self.stop_microphone();
        self.screen_picker = None;
        if let Some(mut capture) = self.screen_capture.take() {
            let _ = capture.stop();
        }
        self.signaling = None;
    }
}

fn main() -> eframe::Result {
    let mut app = ClientUi::default();
    app.monitor_gain_db = 6.0;
    app.microphone_level_dbfs = -60.0;

    eframe::run_ui_native(
        "P2P - Voz e tela",
        eframe::NativeOptions::default(),
        move |ui, _frame| {
            egui::CentralPanel::default().show(ui, |ui| app.show(ui));
        },
    )
}
