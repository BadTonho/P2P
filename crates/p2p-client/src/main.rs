#![windows_subsystem = "windows"]

mod audio_capture;
mod screen_capture;

use std::time::Duration;

use audio_capture::MicrophoneTest;
use eframe::egui;
use screen_capture::ScreenCapture;

const DEMO_ROOM_CODE: &str = "DEMO-0001";

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum SettingsCategory {
    #[default]
    Audio,
}

#[derive(Default)]
struct ClientUi {
    room_code: Option<String>,
    join_code: String,
    code_copied: bool,
    settings_open: bool,
    settings_category: SettingsCategory,
    microphone: Option<MicrophoneTest>,
    microphone_level: f32,
    microphone_level_dbfs: f32,
    monitor_gain_db: f32,
    microphone_error: Option<String>,
    microphone_monitor_error: Option<String>,
    microphone_audio_warning: bool,
    microphone_clipping_warning: bool,
    screen_capture: Option<ScreenCapture>,
    screen_texture: Option<egui::TextureHandle>,
    screen_status: Option<String>,
}

impl ClientUi {
    fn show(&mut self, ui: &mut egui::Ui) {
        if self.microphone.is_some() || self.screen_capture.is_some() {
            ui.ctx().request_repaint_after(Duration::from_millis(100));
        }
        self.refresh_microphone();
        self.refresh_screen(ui.ctx());

        egui::ScrollArea::vertical().show(ui, |ui| {
            let mut open_settings = false;
            let mut close_settings = false;
            let settings_open = self.settings_open;

            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.add_space(16.0);
                    ui.heading("P2P - Voz e tela");
                    ui.label("Demonstração local — sem conexão ou transmissão");
                });

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if settings_open {
                        close_settings = ui.button("Voltar").clicked();
                    } else {
                        open_settings = ui.button("Configurações").clicked();
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
        });
    }

    fn show_home(&mut self, ui: &mut egui::Ui) {
        ui.group(|ui| {
            ui.heading("Criar uma sala");
            ui.label("Crie uma sala de demonstração e compartilhe o código com seu amigo.");

            if ui.button("Criar sala").clicked() {
                self.enter_room(DEMO_ROOM_CODE.to_owned());
            }
        });

        ui.add_space(12.0);

        ui.group(|ui| {
            ui.heading("Entrar em uma sala");
            ui.label("Digite o código da sala:");

            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.join_code)
                        .hint_text("Ex.: DEMO-0001")
                        .desired_width(220.0),
                );

                let has_code = !self.join_code.trim().is_empty();
                if ui
                    .add_enabled(has_code, egui::Button::new("Entrar"))
                    .clicked()
                {
                    let code = self.join_code.trim().to_owned();
                    self.enter_room(code);
                }
            });
        });
    }

    fn show_room(&mut self, ui: &mut egui::Ui) {
        let code = self.room_code.clone().unwrap_or_default();

        ui.group(|ui| {
            ui.heading("Sala de demonstração");
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
            } else if ui.button("Selecionar tela ou janela").clicked() {
                self.select_screen(ui.ctx());
            }

            if let Some(status) = &self.screen_status {
                ui.label(status);
            }
        });

        ui.add_space(12.0);
        if ui.button("Sair da sala").clicked() {
            self.leave_room();
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
            });

            ui.separator();

            ui.vertical(|ui| match self.settings_category {
                SettingsCategory::Audio => self.show_audio_settings(ui),
            });
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

    fn select_screen(&mut self, context: &egui::Context) {
        self.screen_status = None;
        match ScreenCapture::pick_and_start(context.clone()) {
            Ok(Some(capture)) => {
                self.screen_capture = Some(capture);
                self.screen_status =
                    Some("A prévia será atualizada enquanto a captura estiver ativa.".to_owned());
            }
            Ok(None) => {
                self.screen_status =
                    Some("Seleção cancelada; a captura permaneceu desligada.".to_owned());
            }
            Err(error) => {
                self.screen_status = Some(format!("Não foi possível iniciar a captura: {error}"));
            }
        }
    }

    fn refresh_screen(&mut self, context: &egui::Context) {
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
        self.screen_status = None;
    }

    fn leave_room(&mut self) {
        self.stop_microphone();
        if let Some(mut capture) = self.screen_capture.take() {
            let _ = capture.stop();
        }
        self.screen_texture = None;
        self.room_code = None;
        self.join_code.clear();
        self.code_copied = false;
        self.microphone_error = None;
        self.screen_status = None;
    }
}

impl Drop for ClientUi {
    fn drop(&mut self) {
        self.stop_microphone();
        if let Some(mut capture) = self.screen_capture.take() {
            let _ = capture.stop();
        }
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
