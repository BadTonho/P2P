#![windows_subsystem = "windows"]

use eframe::egui;

const DEMO_ROOM_CODE: &str = "DEMO-0001";

#[derive(Default)]
struct ClientUi {
    room_code: Option<String>,
    join_code: String,
    call_active: bool,
    screen_sharing: bool,
    code_copied: bool,
}

impl ClientUi {
    fn show(&mut self, ui: &mut egui::Ui) {
        ui.vertical_centered(|ui| {
            ui.add_space(16.0);
            ui.heading("P2P - Voz e tela");
            ui.label("Demonstração local — sem conexão real");
        });

        ui.add_space(20.0);

        if self.room_code.is_some() {
            self.show_room(ui);
        } else {
            self.show_home(ui);
        }
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
                    self.join_code.clear();
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
            ui.heading("Chamada e compartilhamento");

            let call_status = if self.call_active {
                "Chamada de demonstração ativa — sem áudio real."
            } else {
                "Chamada desligada."
            };
            ui.label(call_status);

            if ui
                .button(if self.call_active {
                    "Encerrar chamada"
                } else {
                    "Iniciar chamada"
                })
                .clicked()
            {
                self.call_active = !self.call_active;
                if !self.call_active {
                    self.screen_sharing = false;
                }
            }

            ui.add_space(8.0);

            let share_status = if self.screen_sharing {
                "Compartilhamento de demonstração ativo — sem captura da tela."
            } else {
                "Compartilhamento de tela desligado."
            };
            ui.label(share_status);

            let share_label = if self.screen_sharing {
                "Parar compartilhamento"
            } else {
                "Compartilhar tela"
            };
            if ui
                .add_enabled(self.call_active, egui::Button::new(share_label))
                .clicked()
            {
                self.screen_sharing = !self.screen_sharing;
            }
        });

        ui.add_space(12.0);

        if ui.button("Sair da sala").clicked() {
            self.leave_room();
        }
    }

    fn enter_room(&mut self, code: String) {
        self.room_code = Some(code);
        self.call_active = false;
        self.screen_sharing = false;
        self.code_copied = false;
    }

    fn leave_room(&mut self) {
        self.room_code = None;
        self.join_code.clear();
        self.call_active = false;
        self.screen_sharing = false;
        self.code_copied = false;
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
