#![windows_subsystem = "windows"]

mod audio_capture;
mod screen_capture;

use std::time::Duration;

use audio_capture::MicrophoneTest;
use eframe::egui;
use screen_capture::ScreenCapture;

const DEMO_ROOM_CODE: &str = "DEMO-0001";

#[derive(Default)]
struct ClientUi {
    room_code: Option<String>,
    join_code: String,
    code_copied: bool,
    microphone: Option<MicrophoneTest>,
    microphone_level: f32,
    microphone_error: Option<String>,
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
            ui.vertical_centered(|ui| {
                ui.add_space(16.0);
                ui.heading("P2P - Voz e tela");
                ui.label("Demonstração local — sem conexão ou transmissão");
            });

            ui.add_space(20.0);

            if self.room_code.is_some() {
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
            ui.heading("Teste local do microfone");
            ui.label("As amostras são usadas só para medir o nível e descartadas; não há gravação nem reprodução.");

            if let Some(error) = &self.microphone_error {
                ui.colored_label(egui::Color32::from_rgb(190, 55, 55), error);
            }

            if self.microphone.is_some() {
                ui.label("Captura do microfone ativa.");
                ui.add(
                    egui::ProgressBar::new(self.microphone_level)
                        .text(format!("Nível: {:.0}%", self.microphone_level * 100.0)),
                );
                if ui.button("Parar teste do microfone").clicked() {
                    self.stop_microphone();
                }
            } else if ui.button("Testar microfone").clicked() {
                self.start_microphone();
            }

            ui.small("Se o acesso estiver bloqueado: Configurações > Privacidade e segurança > Microfone (no Windows 10, Privacidade > Microfone) > permitir acesso a aplicativos de área de trabalho.");
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

    fn start_microphone(&mut self) {
        self.microphone_error = None;
        self.microphone_level = 0.0;
        match MicrophoneTest::start() {
            Ok(test) => self.microphone = Some(test),
            Err(error) => self.microphone_error = Some(error),
        }
    }

    fn stop_microphone(&mut self) {
        self.microphone = None;
        self.microphone_level = 0.0;
    }

    fn refresh_microphone(&mut self) {
        let Some(microphone) = self.microphone.as_ref() else {
            return;
        };

        self.microphone_level = microphone.level();
        if let Some(error) = microphone.take_error() {
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

    eframe::run_ui_native(
        "P2P - Voz e tela",
        eframe::NativeOptions::default(),
        move |ui, _frame| {
            egui::CentralPanel::default().show(ui, |ui| app.show(ui));
        },
    )
}
