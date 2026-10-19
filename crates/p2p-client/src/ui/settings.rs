use super::super::{
    ClientUi, SettingsCategory, UpdateStatus, format_bytes, signaling_ws_url,
    validate_new_host_profile, validate_saved_host_profile,
};
use crate::audio_capture::available_audio_applications;
use crate::screen_sharing;
use crate::settings::{LoggingLevel, MAX_SAVED_HOSTS, SavedHostProfile, VideoDecoderPreference};
use crate::update::UpdateManager;
use eframe::egui;

impl ClientUi {
    pub(super) fn show_settings(&mut self, ui: &mut egui::Ui) {
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
                (SettingsCategory::General, "Geral"),
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
            SettingsCategory::General => self.show_general_settings(ui),
            SettingsCategory::Audio => self.show_audio_settings(ui),
            SettingsCategory::Connection => self.show_connection_settings(ui),
            SettingsCategory::Video => self.show_video_settings(ui),
            SettingsCategory::Updates => self.show_update_settings(ui),
        }
    }

    fn show_general_settings(&mut self, ui: &mut egui::Ui) {
        ui.heading("Geral");
        ui.group(|ui| {
            ui.heading("Registro de logs");
            ui.label("Escolha quantos eventos o aplicativo grava neste computador.");

            let mut selected_level = self.logging_level;
            egui::ComboBox::from_id_salt("logging-level")
                .selected_text(selected_level.label())
                .show_ui(ui, |ui| {
                    for level in [
                        LoggingLevel::Disabled,
                        LoggingLevel::WarningsAndErrors,
                        LoggingLevel::Detailed,
                    ] {
                        ui.selectable_value(&mut selected_level, level, level.label());
                    }
                });

            if selected_level != self.logging_level {
                self.logging_level = selected_level;
                self.logging.set_level(selected_level);
            }

            ui.small(
                "Padrão: somente avisos e erros. Logs detalhados incluem informações e depuração.",
            );
            ui.small("Desativados não cria arquivos de sessão; logs antigos são preservados.");
            ui.add_space(4.0);
            ui.checkbox(
                &mut self.log_performance_metrics,
                "Registrar métricas periódicas de desempenho no log",
            );
            ui.small(
                "Desmarcado por padrão para economizar espaço e evitar sobrecarga de I/O em disco.",
            );
            ui.small("Com logs Detalhados, mede também os motores da GPU durante o compartilhamento de tela.");
            ui.add_space(4.0);
            if ui
                .checkbox(
                    &mut self.log_errors_file,
                    "Gravar log dedicado de erros (p2p-errors.log)",
                )
                .changed()
            {
                self.logging.set_errors_enabled(self.log_errors_file);
            }
            ui.small("Ativado por padrão para facilitar o diagnóstico rápido de falhas.");
        });
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
                    "A versão {} foi baixada e validada. Ela será instalada automaticamente ao fechar o aplicativo.",
                    manifest.version
                ));
                if self.room_code.is_some() {
                    ui.small("Saia da sala antes de reiniciar para aplicar a atualização agora.");
                }
                if ui
                    .add_enabled(
                        self.room_code.is_none(),
                        egui::Button::new("Reiniciar agora para atualizar"),
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
            ui.small("O endereço escolhido vale para descoberta, controle e mídia.");
            ui.checkbox(&mut self.announce_local_room, "Anunciar minhas salas locais nesta rede");
            ui.small("Com o anúncio ativo, outras pessoas nesta rede podem entrar sem código.");
            ui.small("Descoberta: UDP 8999. Sinalização e controle: TCP 9000–9001. Mídia: UDP 9002–9009.");
        });

        ui.add_space(8.0);
        ui.group(|ui| {
            ui.heading("Anfitriões salvos");
            ui.label("Salve os endereços das pessoas com quem você costuma entrar em salas.");
            let mut remove_index = None;
            let mut select_index = None;
            for (index, profile) in self.saved_hosts.iter_mut().enumerate() {
                ui.separator();
                ui.horizontal_wrapped(|ui| {
                    ui.label(format!("{}.", index + 1));
                    ui.add(
                        egui::TextEdit::singleline(&mut profile.name)
                            .hint_text("Apelido")
                            .desired_width(120.0),
                    );
                    ui.add(
                        egui::TextEdit::singleline(&mut profile.address)
                            .hint_text("IPv4 ou nome DDNS")
                            .desired_width(180.0),
                    );
                });
                let validation = validate_saved_host_profile(profile);
                ui.horizontal_wrapped(|ui| {
                    let is_selected = self.selected_host_index == Some(index);
                    if ui
                        .add_enabled(
                            validation.is_ok(),
                            egui::Button::new(if is_selected {
                                "Selecionado"
                            } else {
                                "Usar ao entrar"
                            }),
                        )
                        .clicked()
                    {
                        select_index = Some(index);
                    }
                    if ui.small_button("Remover").clicked() {
                        remove_index = Some(index);
                    }
                    if let Err(error) = &validation {
                        ui.small(egui::RichText::new(error).strong());
                    }
                });
            }
            if let Some(index) = remove_index {
                self.saved_hosts.remove(index);
                self.selected_host_index = match self.selected_host_index {
                    Some(selected) if selected == index => None,
                    Some(selected) if selected > index => Some(selected - 1),
                    selected => selected,
                };
                self.saved_host_error = None;
            }
            if let Some(index) = select_index {
                self.selected_host_index = Some(index);
            }

            ui.separator();
            ui.label(format!(
                "Adicionar anfitrião ({}/{MAX_SAVED_HOSTS})",
                self.saved_hosts.len()
            ));
            ui.horizontal_wrapped(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.new_host_name)
                        .hint_text("Apelido, por exemplo: Ana")
                        .desired_width(130.0),
                );
                ui.add(
                    egui::TextEdit::singleline(&mut self.new_host_address)
                        .hint_text("IPv4 ou nome DDNS")
                        .desired_width(180.0),
                );
                if ui
                    .add_enabled(
                        self.saved_hosts.len() < MAX_SAVED_HOSTS,
                        egui::Button::new("Adicionar"),
                    )
                    .clicked()
                {
                    match validate_new_host_profile(
                        &self.new_host_name,
                        &self.new_host_address,
                        self.saved_hosts.len(),
                    ) {
                        Ok(()) => {
                            self.saved_hosts.push(SavedHostProfile {
                                name: self.new_host_name.trim().to_owned(),
                                address: self.new_host_address.trim().to_owned(),
                            });
                            self.selected_host_index = Some(self.saved_hosts.len() - 1);
                            self.new_host_name.clear();
                            self.new_host_address.clear();
                            self.saved_host_error = None;
                        }
                        Err(error) => self.saved_host_error = Some(error),
                    }
                }
            });
            if self.saved_hosts.len() >= MAX_SAVED_HOSTS {
                ui.small("Limite de 50 anfitriões atingido.");
            }
            if let Some(error) = &self.saved_host_error {
                Self::show_notice(ui, "Não foi possível adicionar:", error);
            }
            ui.small("Endereços inválidos continuam visíveis para você corrigir, mas não podem ser selecionados.");
        });

        ui.add_space(8.0);
        ui.group(|ui| {
            ui.label("Endereço manual para entrar · porta 9000");
            ui.add(
                egui::TextEdit::singleline(&mut self.server_url)
                    .hint_text("IP ou nome DDNS do anfitrião")
                    .desired_width(ui.available_width().min(420.0)),
            );
            match signaling_ws_url(&self.server_url) {
                Ok(url) => {
                    ui.monospace(url);
                }
                Err(error) if !self.server_url.trim().is_empty() => {
                    Self::show_notice(ui, "Endereço manual inválido:", &error);
                }
                Err(_) => {}
            }
            ui.small("Usado quando “Endereço manual” estiver selecionado na tela inicial.");
        });

        ui.add_space(8.0);
        ui.group(|ui| {
            ui.label("Meu endereço público para convites · porta 9000");
            ui.add(
                egui::TextEdit::singleline(&mut self.public_server_url)
                    .hint_text("Meu IPv4 público ou minha-sala.ddns.net")
                    .desired_width(ui.available_width().min(420.0)),
            );
            match signaling_ws_url(&self.public_server_url) {
                Ok(url) => {
                    ui.horizontal_wrapped(|ui| {
                        ui.monospace(&url);
                        if ui.button("Copiar endereço").clicked() {
                            ui.ctx().copy_text(url);
                        }
                    });
                }
                Err(error) if !self.public_server_url.trim().is_empty() => {
                    Self::show_notice(ui, "Endereço de convite inválido:", &error);
                }
                Err(_) => {}
            }
            ui.small("Usado somente nos convites das suas salas pela Internet. Escolher outro anfitrião para entrar não altera este endereço.");
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
            ui.label("Salve um perfil por pessoa em Anfitriões salvos e escolha-o na tela inicial ao entrar.");
            ui.label("Sem perfil selecionado, o campo Endereço manual da tela inicial será usado.");
            ui.label("O endereço de convite é o IPv4 público ou DDNS deste computador; o app não detecta o IP público.");
            ui.label("Para hospedar na Internet, encaminhe TCP 9000 e libere a porta no firewall. TURN também requer UDP 3478 e UDP 50000–50100; CGNAT pode impedir conexões de entrada.");
            ui.label("TURN é configurado pelo anfitrião. Não é necessário manter um notebook separado ligado.");
            ui.label("ws:// não criptografa a sinalização. Utilize wss:// para criptografia com TLS; em ws://, use apenas testes controlados com pessoas conhecidas.");
            ui.label("Perfis, endereço manual e endereço de convite ficam salvos nas preferências locais deste aplicativo.");
        });
    }

    fn show_audio_settings(&mut self, ui: &mut egui::Ui) {
        self.poll_audio_applications_query();
        if !self.audio_applications_loaded && !self.audio_applications_loading {
            self.refresh_audio_applications(ui.ctx().clone());
        }
        ui.heading("Áudio");
        ui.group(|ui| {
            ui.heading("Som da tela compartilhada");
            ui.label("Ao compartilhar a tela, marque ‘Compartilhar áudio do computador’ na janela para transmitir o áudio reproduzido no Windows.");
            ui.small("A opção fica desligada por padrão. Ela captura o áudio do computador, não o microfone; a faixa Opus segue diretamente aos participantes que assistem à tela.");
            ui.separator();

            let mut exclude_self = self.exclude_self_audio;
            if ui
                .checkbox(
                    &mut exclude_self,
                    "Ignorar áudio do próprio aplicativo (evita eco ao transmitir e assistir)",
                )
                .changed()
            {
                self.exclude_self_audio = exclude_self;
                self.save_preferences();
            }
            ui.small("Ativado por padrão para impedir retorno de áudio em loop quando você e outros participantes transmitem e assistem simultaneamente.");

            ui.separator();
            ui.label("Não compartilhar o áudio de aplicativos específicos:");

            if self.audio_applications_loading && self.available_audio_applications.is_empty() {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Consultando aplicativos com áudio em execução…");
                });
            } else if self.available_audio_applications.is_empty() {
                ui.small("Nenhum outro aplicativo reproduzindo áudio detectado no momento.");
            } else {
                let mut toggled = None;
                for app in &self.available_audio_applications {
                    let is_excluded = self
                        .excluded_audio_applications
                        .iter()
                        .any(|p| p.eq_ignore_ascii_case(&app.executable_path));
                    let mut checked = is_excluded;
                    if ui.checkbox(&mut checked, &app.display_name).changed() {
                        toggled = Some((app.executable_path.clone(), checked));
                    }
                }
                if let Some((path, checked)) = toggled {
                    if checked {
                        if !self
                            .excluded_audio_applications
                            .iter()
                            .any(|p| p.eq_ignore_ascii_case(&path))
                        {
                            self.excluded_audio_applications.push(path);
                        }
                    } else {
                        self.excluded_audio_applications
                            .retain(|p| !p.eq_ignore_ascii_case(&path));
                    }
                    self.excluded_audio_application_path =
                        self.excluded_audio_applications.first().cloned();
                    self.save_preferences();
                }
            }

            let not_running: Vec<String> = self
                .excluded_audio_applications
                .iter()
                .filter(|saved| {
                    !self
                        .available_audio_applications
                        .iter()
                        .any(|app| app.executable_path.eq_ignore_ascii_case(saved))
                })
                .cloned()
                .collect();

            if !not_running.is_empty() {
                ui.add_space(4.0);
                ui.label(egui::RichText::new("Aplicativos selecionados que não estão em execução:").italics());
                let mut remove_path = None;
                for path in &not_running {
                    ui.horizontal(|ui| {
                        let name = std::path::Path::new(path)
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or(path);
                        ui.label(format!("• {name}"));
                        if ui.small_button("Remover").clicked() {
                            remove_path = Some(path.clone());
                        }
                    });
                }
                if let Some(removed) = remove_path {
                    self.excluded_audio_applications
                        .retain(|p| !p.eq_ignore_ascii_case(&removed));
                    self.excluded_audio_application_path =
                        self.excluded_audio_applications.first().cloned();
                    self.save_preferences();
                }
            }

            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let refresh_label = if self.audio_applications_loading {
                    "Consultando…"
                } else {
                    "Atualizar lista de aplicativos"
                };
                if ui
                    .add_enabled(
                        !self.audio_applications_loading,
                        egui::Button::new(refresh_label),
                    )
                    .clicked()
                {
                    self.refresh_audio_applications(ui.ctx().clone());
                }
                if !self.excluded_audio_applications.is_empty()
                    && ui.button("Limpar exclusões").clicked()
                {
                    self.excluded_audio_applications.clear();
                    self.excluded_audio_application_path = None;
                    self.save_preferences();
                }
            });

            if let Some(error) = &self.audio_applications_error {
                Self::show_notice(ui, "Lista de aplicativos:", error);
            }
            ui.small("Os aplicativos marcados serão ignorados durante o compartilhamento. A escolha é salva neste computador.");
            ui.small("A exclusão exige Windows build 20348 ou posterior; sem suporte, o vídeo continua e o áudio completo é enviado.");
            ui.small("Falhas de captura, codificação, envio, recepção e reprodução ficam registradas por sessão nos logs. Uma falha de áudio não encerra a transmissão de vídeo.");
        });
        ui.add_space(8.0);
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
            if gain_changed
                && let Some(microphone) = &self.microphone {
                    microphone.set_monitor_gain_db(self.monitor_gain_db);
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

    fn poll_audio_applications_query(&mut self) {
        if let Some(rx) = &self.audio_applications_rx {
            match rx.try_recv() {
                Ok(Ok(applications)) => {
                    self.available_audio_applications = applications;
                    self.audio_applications_error = None;
                    self.audio_applications_loaded = true;
                    self.audio_applications_loading = false;
                    self.audio_applications_rx = None;
                }
                Ok(Err(error)) => {
                    self.available_audio_applications.clear();
                    self.audio_applications_error = Some(error);
                    self.audio_applications_loaded = true;
                    self.audio_applications_loading = false;
                    self.audio_applications_rx = None;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.audio_applications_loading = false;
                    self.audio_applications_rx = None;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
    }

    fn refresh_audio_applications(&mut self, context: egui::Context) {
        if self.audio_applications_loading {
            return;
        }
        self.audio_applications_loading = true;
        let (tx, rx) = std::sync::mpsc::channel();
        self.audio_applications_rx = Some(rx);
        std::thread::Builder::new()
            .name("p2p-audio-apps-query".to_owned())
            .spawn(move || {
                let result = available_audio_applications();
                let _ = tx.send(result);
                context.request_repaint();
            })
            .ok();
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
}
