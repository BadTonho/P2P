use super::super::{
    ClientUi, RoomMode, UpdateShortcutAction, UpdateStatus, signaling_ws_url,
    update_shortcut_action, update_shortcut_tooltip,
};
use eframe::egui;

const PROFILE_PILL_AVATAR_SIZE: f32 = 34.0;
const PROFILE_WINDOW_AVATAR_SIZE: f32 = 64.0;
const PRIMARY_ACTION_HEIGHT: f32 = 30.0;
const PRIMARY_ACTION_WIDTH: f32 = 120.0;

#[derive(Default)]
struct ProfileEditorActions {
    choose_avatar: bool,
    remove_avatar: bool,
    close_window: bool,
}

fn show_room_mode_selector(
    ui: &mut egui::Ui,
    mode: &mut RoomMode,
    open_network_window: &mut bool,
) -> (egui::Response, egui::Response) {
    let mut network_button = None;
    let row = ui.horizontal_wrapped(|ui| {
        ui.label("Modo:");
        ui.selectable_value(mode, RoomMode::Local, "Rede local / Radmin");
        ui.selectable_value(mode, RoomMode::InternetTest, "Internet (teste)");
        let response = ui.small_button("Configurar rede…");
        if response.clicked() {
            *open_network_window = true;
        }
        network_button = Some(response);
    });

    (
        row.response,
        network_button.expect("network settings button should be laid out"),
    )
}

fn show_network_window(
    context: &egui::Context,
    open: &mut bool,
    add_contents: impl FnOnce(&mut egui::Ui),
) -> bool {
    if !*open {
        return false;
    }

    egui::Window::new("Rede de controle da sala")
        .id(egui::Id::new("control-network-settings-window"))
        .open(open)
        .collapsible(false)
        .resizable(false)
        .default_width(480.0)
        .show(context, add_contents)
        .is_some()
}

fn show_control_network_window(ui: &mut egui::Ui, app: &mut ClientUi) {
    let mut open = app.control_network_window_open;
    show_network_window(ui.ctx(), &mut open, |window| {
        window.label("Escolha o endereço que seu amigo consegue alcançar.");
        app.show_control_address_picker(window);
        window.checkbox(
            &mut app.may_host,
            "Permitir que este computador seja escolhido para hospedar futuramente",
        );
        window.collapsing("Requisitos de rede", |window| {
                window.label("A interface selecionada será usada para controle e vídeo.");
                window.label("Libere TCP 9001 e UDP 9002–9009 no firewall do Windows.");
                window.label("Use Radmin VPN quando o amigo entrar pelo endereço Radmin; use Ethernet/Wi-Fi na rede local.");
                window.label("A permissão para hospedar só permite assumir a sala se o anfitrião sair.");
            });
    });
    app.control_network_window_open = open;
}

fn primary_action_button(
    ui: &mut egui::Ui,
    label: &str,
    width: f32,
    enabled: bool,
) -> egui::Response {
    ui.scope(|ui| {
        configure_action_button_visuals(ui.visuals_mut());
        ui.add_enabled(
            enabled,
            egui::Button::new(egui::RichText::new(label).strong())
                .min_size(egui::vec2(width, PRIMARY_ACTION_HEIGHT)),
        )
    })
    .inner
}

fn configure_action_button_visuals(visuals: &mut egui::Visuals) {
    let (normal, hovered, active, disabled) = if visuals.dark_mode {
        (
            (43, 105, 225),
            (58, 145, 245),
            (35, 165, 245),
            (35, 72, 120),
        )
    } else {
        (
            (238, 125, 35),
            (224, 78, 25),
            (210, 58, 20),
            (238, 195, 150),
        )
    };

    set_action_button_visual(&mut visuals.widgets.inactive, normal);
    set_action_button_visual(&mut visuals.widgets.hovered, hovered);
    set_action_button_visual(&mut visuals.widgets.active, active);
    set_action_button_visual(&mut visuals.widgets.open, hovered);
    set_action_button_visual(&mut visuals.widgets.noninteractive, disabled);
}

fn set_action_button_visual(
    widget: &mut egui::style::WidgetVisuals,
    (fill, border, text): (u8, u8, u8),
) {
    widget.bg_fill = egui::Color32::from_gray(fill);
    widget.weak_bg_fill = egui::Color32::from_gray(fill);
    widget.bg_stroke = egui::Stroke::new(1.0, egui::Color32::from_gray(border));
    widget.fg_stroke.color = egui::Color32::from_gray(text);
}

fn room_actions_enabled(connecting: bool, update_blocks_actions: bool) -> bool {
    !connecting && !update_blocks_actions
}

fn join_action_enabled(
    has_code: bool,
    valid_address: bool,
    connecting: bool,
    update_blocks_actions: bool,
) -> bool {
    has_code && valid_address && room_actions_enabled(connecting, update_blocks_actions)
}

fn show_host_picker_and_manage<'a>(
    ui: &mut egui::Ui,
    selected_label: &str,
    selected_host_index: &mut Option<usize>,
    saved_hosts: impl Iterator<Item = (usize, &'a str)>,
) -> (egui::Response, egui::Response) {
    let mut manage_response = None;
    let row = ui.horizontal_wrapped(|ui| {
        egui::ComboBox::from_id_salt("join-host-profile")
            .selected_text(selected_label)
            .show_ui(ui, |ui| {
                ui.selectable_value(selected_host_index, None, "Endereço manual");
                for (index, name) in saved_hosts {
                    ui.selectable_value(selected_host_index, Some(index), name);
                }
            });

        manage_response = Some(ui.small_button("Gerenciar anfitriões"));
    });

    (
        row.response,
        manage_response.expect("host management button should be laid out"),
    )
}

fn show_profile_pill(
    ui: &mut egui::Ui,
    display_name: &str,
    avatar: Option<&egui::TextureHandle>,
) -> egui::Response {
    let name_display = if display_name.trim().is_empty() {
        "Definir nome…".to_string()
    } else {
        display_name.trim().to_string()
    };

    let frame = egui::Frame::new()
        .corner_radius(egui::CornerRadius::same(6))
        .inner_margin(egui::Margin::symmetric(8, 4))
        .fill(egui::Color32::from_rgb(26, 29, 36))
        .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(44, 49, 62)));

    let inner = frame.show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;

            if let Some(texture) = avatar {
                ui.add(
                    egui::Image::new((
                        texture.id(),
                        egui::vec2(PROFILE_PILL_AVATAR_SIZE, PROFILE_PILL_AVATAR_SIZE),
                    ))
                    .fit_to_exact_size(egui::vec2(
                        PROFILE_PILL_AVATAR_SIZE,
                        PROFILE_PILL_AVATAR_SIZE,
                    ))
                    .corner_radius(egui::CornerRadius::same(4)),
                );
            } else {
                let (rect, _) = ui.allocate_exact_size(
                    egui::vec2(PROFILE_PILL_AVATAR_SIZE, PROFILE_PILL_AVATAR_SIZE),
                    egui::Sense::hover(),
                );
                ui.painter().circle_filled(
                    rect.center(),
                    PROFILE_PILL_AVATAR_SIZE / 2.0,
                    egui::Color32::from_rgb(45, 52, 68),
                );
                let initial = name_display
                    .chars()
                    .next()
                    .unwrap_or('?')
                    .to_uppercase()
                    .to_string();
                let text_color = egui::Color32::from_rgb(180, 195, 220);
                let galley = ui.painter().layout_no_wrap(
                    initial,
                    egui::TextStyle::Body.resolve(ui.style()),
                    text_color,
                );
                let text_rect = egui::Rect::from_center_size(rect.center(), galley.size());
                ui.painter().galley(text_rect.min, galley, text_color);
            }

            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 1.0;
                ui.label(egui::RichText::new(&name_display).strong());
                ui.label(
                    egui::RichText::new("Editar perfil")
                        .size(11.0)
                        .color(egui::Color32::from_rgb(130, 140, 160)),
                );
            });
        });
    });

    ui.interact(inner.response.rect, inner.response.id, egui::Sense::click())
        .on_hover_text("Clique para editar o perfil")
        .on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn show_profile_window_contents(
    ui: &mut egui::Ui,
    display_name: &mut String,
    avatar: Option<&egui::TextureHandle>,
    has_saved_avatar: bool,
    avatar_error: Option<&str>,
) -> ProfileEditorActions {
    let mut actions = ProfileEditorActions::default();

    ui.vertical(|ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 12.0;

            if let Some(texture) = avatar {
                ui.add(
                    egui::Image::new((
                        texture.id(),
                        egui::vec2(PROFILE_WINDOW_AVATAR_SIZE, PROFILE_WINDOW_AVATAR_SIZE),
                    ))
                    .fit_to_exact_size(egui::vec2(
                        PROFILE_WINDOW_AVATAR_SIZE,
                        PROFILE_WINDOW_AVATAR_SIZE,
                    ))
                    .corner_radius(egui::CornerRadius::same(8)),
                );
            } else {
                let (rect, _) = ui.allocate_exact_size(
                    egui::vec2(PROFILE_WINDOW_AVATAR_SIZE, PROFILE_WINDOW_AVATAR_SIZE),
                    egui::Sense::hover(),
                );
                ui.painter().rect_filled(
                    rect,
                    egui::CornerRadius::same(8),
                    egui::Color32::from_rgb(38, 42, 54),
                );
                let initial = display_name
                    .trim()
                    .chars()
                    .next()
                    .unwrap_or('?')
                    .to_uppercase()
                    .to_string();
                let text_color = egui::Color32::from_rgb(180, 195, 220);
                let galley = ui.painter().layout_no_wrap(
                    initial,
                    egui::TextStyle::Heading.resolve(ui.style()),
                    text_color,
                );
                let text_rect = egui::Rect::from_center_size(rect.center(), galley.size());
                ui.painter().galley(text_rect.min, galley, text_color);
            }

            ui.vertical(|ui| {
                ui.label(egui::RichText::new("Foto de perfil").strong());
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    if ui
                        .button(if has_saved_avatar {
                            "Alterar foto"
                        } else {
                            "Escolher foto"
                        })
                        .clicked()
                    {
                        actions.choose_avatar = true;
                    }
                    if has_saved_avatar && ui.button("Remover foto").clicked() {
                        actions.remove_avatar = true;
                    }
                });
                ui.add_space(2.0);
                ui.small("PNG ou JPEG (96x96).");
            });
        });

        if let Some(error) = avatar_error {
            ui.add_space(4.0);
            ClientUi::show_notice(ui, "Foto do perfil:", error);
        }

        ui.add_space(8.0);
        ui.separator();
        ui.add_space(8.0);

        ui.label(egui::RichText::new("Nome na sala").strong());
        ui.add(
            egui::TextEdit::singleline(display_name)
                .hint_text("Se ficar vazio, aparecerá Participante N")
                .char_limit(32)
                .desired_width(ui.available_width()),
        );
        ui.add_space(4.0);
        ui.small("O nome aparece em qualquer sala. A foto é compartilhada apenas em salas Rede local / Radmin.");

        ui.add_space(12.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button("Concluir").clicked() {
                actions.close_window = true;
            }
        });
    });

    actions
}

impl ClientUi {
    pub(super) fn show_home(&mut self, ui: &mut egui::Ui) {
        if !self.addresses_loaded {
            self.refresh_host_addresses();
        }

        show_room_mode_selector(
            ui,
            &mut self.create_room_mode,
            &mut self.control_network_window_open,
        );
        show_control_network_window(ui, self);
        ui.add_space(2.0);

        if self.create_room_mode == RoomMode::InternetTest {
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

    pub(super) fn show_home_bottom_bar(
        &mut self,
        ui: &mut egui::Ui,
        open_settings: &mut bool,
        open_update_settings: &mut bool,
    ) {
        let avatar = self.local_profile_avatar_texture(ui.ctx());
        let picker_open = self.screen_picker.is_some();
        let update_status = self.update_status.clone();
        let update_action = update_shortcut_action(&update_status, false);
        let update_tooltip = update_shortcut_tooltip(&update_status, false)
            .unwrap_or_else(|| "Atualização".to_owned());

        let mut update_clicked = false;

        ui.horizontal(|ui| {
            let response = show_profile_pill(ui, &self.profile_display_name, avatar.as_ref());
            if response.clicked() {
                self.profile_window_open = true;
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let button_size = egui::vec2(30.0, 26.0);

                let settings_response = ui
                    .add_enabled(
                        !picker_open,
                        egui::Button::new(egui::RichText::new("⚙").size(18.0))
                            .min_size(button_size),
                    )
                    .on_hover_text("Configurações");
                if settings_response.clicked() {
                    *open_settings = true;
                }

                if let Some(action) = update_action {
                    let enabled = action != UpdateShortcutAction::DisabledForRoom && !picker_open;
                    let response = ui
                        .add_enabled(
                            enabled,
                            egui::Button::new(egui::RichText::new("↻").size(18.0))
                                .min_size(button_size),
                        )
                        .on_hover_text(&update_tooltip);
                    if response.clicked() {
                        update_clicked = true;
                    }
                }
            });
        });

        if update_clicked {
            match update_action {
                Some(UpdateShortcutAction::Download) => {
                    if let UpdateStatus::Available(manifest) = &update_status {
                        let manifest = manifest.clone();
                        tracing::info!(
                            version = %manifest.version,
                            "Download de atualização iniciado pelo atalho na barra inferior"
                        );
                        self.updates.download(manifest.clone());
                        self.update_status = UpdateStatus::Downloading {
                            manifest,
                            received: 0,
                        };
                    }
                }
                Some(UpdateShortcutAction::OpenUpdates) => {
                    *open_update_settings = true;
                }
                Some(UpdateShortcutAction::DisabledForRoom) | None => {}
            }
        }
    }

    pub(super) fn show_profile_window(&mut self, context: &egui::Context) {
        if !self.profile_window_open {
            return;
        }
        let mut open = self.profile_window_open;
        let mut choose_avatar = false;
        let mut remove_avatar = false;
        let mut close_window = false;
        let has_saved_avatar = self.profile_avatar_jpeg.is_some();
        let avatar = self.local_profile_avatar_texture(context);
        let avatar_error = self.profile_avatar_error.clone();

        egui::Window::new("Editar perfil")
            .id(egui::Id::new("profile-editor-window"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(380.0)
            .show(context, |ui| {
                Self::apply_monochrome_style(ui);
                let actions = show_profile_window_contents(
                    ui,
                    &mut self.profile_display_name,
                    avatar.as_ref(),
                    has_saved_avatar,
                    avatar_error.as_deref(),
                );
                if actions.choose_avatar {
                    choose_avatar = true;
                }
                if actions.remove_avatar {
                    remove_avatar = true;
                }
                if actions.close_window {
                    close_window = true;
                }
            });

        if close_window {
            open = false;
        }
        self.profile_window_open = open;
        if choose_avatar {
            self.choose_profile_avatar();
        }
        if remove_avatar {
            self.remove_profile_avatar();
        }
    }

    fn show_create_room_card(&mut self, ui: &mut egui::Ui) {
        ui.group(|ui| {
            ui.heading("Criar sala");
            ui.label("Hospede neste computador e compartilhe o código com seu amigo.");
            let enabled = room_actions_enabled(self.connecting, self.update_blocks_room_actions());
            let response = primary_action_button(ui, "Criar sala", PRIMARY_ACTION_WIDTH, enabled);
            if response.clicked() {
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
            let (_, manage_response) = show_host_picker_and_manage(
                ui,
                selected_label,
                &mut self.selected_host_index,
                self.saved_hosts
                    .iter()
                    .enumerate()
                    .map(|(index, profile)| (index, profile.name.as_str())),
            );
            manage_hosts = manage_response.clicked();
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
            ui.label("Digite o código da sala:");
            ui.horizontal_wrapped(|ui| {
                let field_width =
                    (ui.available_width() - PRIMARY_ACTION_WIDTH - ui.spacing().item_spacing.x)
                        .max(80.0);
                ui.add_sized(
                    egui::vec2(field_width, PRIMARY_ACTION_HEIGHT),
                    egui::TextEdit::singleline(&mut self.join_code)
                        .hint_text("Código da sala")
                        .desired_width(field_width),
                );

                let address = self.join_address();
                let valid_address = signaling_ws_url(address).is_ok();
                let enabled = join_action_enabled(
                    !self.join_code.trim().is_empty(),
                    valid_address,
                    self.connecting,
                    self.update_blocks_room_actions(),
                );
                let response = primary_action_button(ui, "Entrar", PRIMARY_ACTION_WIDTH, enabled);
                let clicked = response.clicked();
                if self.join_code.trim().is_empty() {
                    response.on_hover_text("Digite o código da sala para habilitar Entrar.");
                }
                if clicked {
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

#[cfg(test)]
#[path = "home_tests.rs"]
mod tests;
