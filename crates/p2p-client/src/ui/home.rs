use super::super::{ClientUi, RoomMode, signaling_ws_url};
use eframe::egui;

impl ClientUi {
    pub(super) fn show_home(&mut self, ui: &mut egui::Ui) {
        if !self.addresses_loaded {
            self.refresh_host_addresses();
        }

        self.show_profile_card(ui);
        ui.add_space(8.0);

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
            ui.label("Escolha um anfitrião salvo na tela inicial; use Gerenciar anfitriões para adicionar ou editar endereços.");
        });
    }

    fn show_profile_card(&mut self, ui: &mut egui::Ui) {
        let mut choose_avatar = false;
        let mut remove_avatar = false;
        ui.group(|ui| {
            ui.heading("Seu perfil");
            ui.horizontal(|ui| {
                if let Some(texture) = self.local_profile_avatar_texture(ui.ctx()) {
                    ui.add(
                        egui::Image::new((texture.id(), egui::vec2(64.0, 64.0)))
                            .fit_to_exact_size(egui::vec2(64.0, 64.0)),
                    );
                }
                ui.vertical(|ui| {
                    ui.label("Nome na sala (opcional)");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.profile_display_name)
                            .hint_text("Se ficar vazio, aparecerá Participante N")
                            .char_limit(32)
                            .desired_width(ui.available_width().min(360.0)),
                    );
                    ui.horizontal(|ui| {
                        if ui
                            .button(if self.profile_avatar_jpeg.is_some() {
                                "Alterar foto"
                            } else {
                                "Escolher foto"
                            })
                            .clicked()
                        {
                            choose_avatar = true;
                        }
                        if self.profile_avatar_jpeg.is_some()
                            && ui.button("Remover foto").clicked()
                        {
                            remove_avatar = true;
                        }
                    });
                });
            });
            ui.small("O nome aparece em qualquer sala. A foto é compartilhada apenas em salas Rede local / Radmin.");
        });

        if choose_avatar {
            self.choose_profile_avatar();
        }
        if remove_avatar {
            self.remove_profile_avatar();
        }
        if let Some(error) = self.profile_avatar_error.clone() {
            ClientUi::show_notice(ui, "Foto do perfil:", &error);
        }
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
        let mut manage_hosts = false;
        ui.group(|ui| {
            ui.heading("Entrar em uma sala");
            let selected_label = self
                .selected_host_index
                .and_then(|index| self.saved_hosts.get(index))
                .map(|profile| profile.name.as_str())
                .unwrap_or("Endereço manual");
            egui::ComboBox::from_id_salt("join-host-profile")
                .selected_text(selected_label)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.selected_host_index, None, "Endereço manual");
                    for (index, profile) in self.saved_hosts.iter().enumerate() {
                        ui.selectable_value(
                            &mut self.selected_host_index,
                            Some(index),
                            &profile.name,
                        );
                    }
                });
            if let Some(index) = self.selected_host_index {
                if let Some(profile) = self.saved_hosts.get(index) {
                    ui.small(format!("Endereço: {}", profile.address));
                    if let Err(error) = signaling_ws_url(&profile.address) {
                        Self::show_notice(ui, "Perfil inválido:", &error);
                    }
                }
            } else {
                ui.add(
                    egui::TextEdit::singleline(&mut self.server_url)
                        .hint_text("IP ou nome DDNS do anfitrião")
                        .desired_width(ui.available_width().min(420.0)),
                );
            }
            if ui.small_button("Gerenciar anfitriões").clicked() {
                manage_hosts = true;
            }
            ui.label("Digite o código da sala:");
            ui.horizontal(|ui| {
                let field_width = (ui.available_width() - 72.0).max(100.0);
                ui.add(
                    egui::TextEdit::singleline(&mut self.join_code)
                        .hint_text("Código da sala")
                        .desired_width(field_width),
                );

                let address = self.join_address();
                let valid_address = signaling_ws_url(address).is_ok();
                let has_code = !self.join_code.trim().is_empty()
                    && valid_address
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
        if manage_hosts {
            self.open_connection_settings();
        }
    }
}
