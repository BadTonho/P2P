use super::super::{ClientUi, RoomMode, signaling_ws_url};
use eframe::egui;

const PROFILE_AVATAR_SIZE: f32 = 48.0;
const PROFILE_NAME_INLINE_MIN_WIDTH: f32 = 280.0;
const PRIMARY_ACTION_HEIGHT: f32 = 30.0;
const JOIN_BUTTON_WIDTH: f32 = 96.0;

#[derive(Default)]
struct ProfileCardActions {
    choose_avatar: bool,
    remove_avatar: bool,
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
    let (fill, text_color) = if enabled {
        (egui::Color32::from_gray(215), egui::Color32::BLACK)
    } else {
        let visuals = &ui.visuals().widgets.noninteractive;
        (visuals.bg_fill, visuals.fg_stroke.color)
    };

    ui.add_enabled(
        enabled,
        egui::Button::new(egui::RichText::new(label).strong().color(text_color))
            .fill(fill)
            .min_size(egui::vec2(width, PRIMARY_ACTION_HEIGHT)),
    )
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

fn show_profile_card(
    ui: &mut egui::Ui,
    display_name: &mut String,
    avatar: Option<&egui::TextureHandle>,
    has_saved_avatar: bool,
) -> (egui::Response, ProfileCardActions) {
    let mut actions = ProfileCardActions::default();
    let response = ui.group(|ui| {
        ui.heading("Seu perfil");
        ui.horizontal(|ui| {
            if let Some(texture) = avatar {
                ui.add(
                    egui::Image::new((
                        texture.id(),
                        egui::vec2(PROFILE_AVATAR_SIZE, PROFILE_AVATAR_SIZE),
                    ))
                    .fit_to_exact_size(egui::vec2(
                        PROFILE_AVATAR_SIZE,
                        PROFILE_AVATAR_SIZE,
                    )),
                );
            }

            ui.vertical(|ui| {
                if ui.available_width() >= PROFILE_NAME_INLINE_MIN_WIDTH {
                    ui.horizontal(|ui| {
                        ui.label("Nome na sala");
                        ui.add(
                            egui::TextEdit::singleline(display_name)
                                .hint_text("Se ficar vazio, aparecerá Participante N")
                                .char_limit(32)
                                .desired_width(ui.available_width().min(360.0)),
                        );
                    });
                } else {
                    ui.label("Nome na sala (opcional)");
                    ui.add(
                        egui::TextEdit::singleline(display_name)
                            .hint_text("Se ficar vazio, aparecerá Participante N")
                            .char_limit(32)
                            .desired_width(ui.available_width().min(360.0)),
                    );
                }

                ui.horizontal_wrapped(|ui| {
                    if ui
                        .small_button(if has_saved_avatar {
                            "Alterar foto"
                        } else {
                            "Escolher foto"
                        })
                        .clicked()
                    {
                        actions.choose_avatar = true;
                    }
                    if has_saved_avatar && ui.small_button("Remover foto").clicked() {
                        actions.remove_avatar = true;
                    }
                });
            });
        });
        ui.small("O nome aparece em qualquer sala. A foto é compartilhada apenas em salas Rede local / Radmin.");
    });

    (response.response, actions)
}

impl ClientUi {
    pub(super) fn show_home(&mut self, ui: &mut egui::Ui) {
        if !self.addresses_loaded {
            self.refresh_host_addresses();
        }

        self.show_profile_card(ui);
        ui.add_space(4.0);

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

    fn show_profile_card(&mut self, ui: &mut egui::Ui) {
        let avatar = self.local_profile_avatar_texture(ui.ctx());
        let has_saved_avatar = self.profile_avatar_jpeg.is_some();
        let (_, actions) = show_profile_card(
            ui,
            &mut self.profile_display_name,
            avatar.as_ref(),
            has_saved_avatar,
        );

        if actions.choose_avatar {
            self.choose_profile_avatar();
        }
        if actions.remove_avatar {
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
            let enabled = room_actions_enabled(self.connecting, self.update_blocks_room_actions());
            let response = primary_action_button(ui, "Criar sala", ui.available_width(), enabled);
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
                    (ui.available_width() - JOIN_BUTTON_WIDTH - ui.spacing().item_spacing.x)
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
                let response = primary_action_button(ui, "Entrar", JOIN_BUTTON_WIDTH, enabled);
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
mod tests {
    use super::{
        JOIN_BUTTON_WIDTH, PRIMARY_ACTION_HEIGHT, RoomMode, join_action_enabled,
        primary_action_button, room_actions_enabled, show_host_picker_and_manage,
        show_network_window, show_profile_card, show_room_mode_selector,
    };
    use eframe::egui;

    struct HomeActionLayout {
        create_card: egui::Rect,
        create_button: egui::Rect,
        join_card: egui::Rect,
        host_row: egui::Rect,
        manage_button: egui::Rect,
        code_field: egui::Rect,
        join_button: egui::Rect,
        create_enabled: bool,
        join_enabled: bool,
    }

    fn render_home_action_layout(width: f32) -> HomeActionLayout {
        let context = egui::Context::default();
        let mut layout = None;
        let mut selected_host = None;
        let mut output = context.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(width, 260.0),
                )),
                ..Default::default()
            },
            |ui| {
                let create = ui.group(|ui| {
                    ui.heading("Criar sala");
                    let button =
                        primary_action_button(ui, "Criar sala", ui.available_width(), true);
                    (button.rect, button.enabled())
                });

                let join = ui.group(|ui| {
                    let (host_row, manage_button) = show_host_picker_and_manage(
                        ui,
                        "Endereço manual",
                        &mut selected_host,
                        std::iter::empty(),
                    );
                    ui.label("Digite o código da sala:");
                    let mut code = String::new();
                    let mut field_rect = None;
                    let mut button_rect = None;
                    let mut button_enabled = false;
                    ui.horizontal_wrapped(|ui| {
                        let field_width = (ui.available_width()
                            - JOIN_BUTTON_WIDTH
                            - ui.spacing().item_spacing.x)
                            .max(80.0);
                        field_rect = Some(
                            ui.add_sized(
                                egui::vec2(field_width, PRIMARY_ACTION_HEIGHT),
                                egui::TextEdit::singleline(&mut code),
                            )
                            .rect,
                        );
                        let response = primary_action_button(ui, "Entrar", JOIN_BUTTON_WIDTH, true);
                        button_rect = Some(response.rect);
                        button_enabled = response.enabled();
                    });
                    (
                        host_row.rect,
                        manage_button.rect,
                        field_rect.expect("code field should be laid out"),
                        button_rect.expect("join button should be laid out"),
                        button_enabled,
                    )
                });

                layout = Some(HomeActionLayout {
                    create_card: create.response.rect,
                    create_button: create.inner.0,
                    join_card: join.response.rect,
                    host_row: join.inner.0,
                    manage_button: join.inner.1,
                    code_field: join.inner.2,
                    join_button: join.inner.3,
                    create_enabled: create.inner.1,
                    join_enabled: join.inner.4,
                });
            },
        );
        output.textures_delta.clear();
        layout.expect("home action cards should be laid out")
    }

    fn render_profile_and_mode(width: f32) -> (egui::Rect, egui::Rect, RoomMode) {
        let context = egui::Context::default();
        let mut profile_rect = None;
        let mut mode_rect = None;
        let mut display_name = "Tonho".to_owned();
        let mut mode = RoomMode::Local;
        let mut popup_open = false;

        let mut output = context.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(width, 480.0),
                )),
                ..Default::default()
            },
            |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    let avatar = ui.ctx().load_texture(
                        "profile-layout-test-avatar",
                        egui::ColorImage::filled([1, 1], egui::Color32::WHITE),
                        egui::TextureOptions::default(),
                    );
                    let (profile, _) =
                        show_profile_card(ui, &mut display_name, Some(&avatar), true);
                    profile_rect = Some(profile.rect);
                    ui.add_space(4.0);
                    let (mode_row, _) = show_room_mode_selector(ui, &mut mode, &mut popup_open);
                    mode_rect = Some(mode_row.rect);
                });
            },
        );
        output.textures_delta.clear();

        (
            profile_rect.expect("profile card should be laid out"),
            mode_rect.expect("room mode selector should be laid out"),
            mode,
        )
    }

    #[test]
    fn profile_and_room_mode_fit_wide_and_narrow_layouts() {
        let (wide_profile, wide_mode, wide_selection) = render_profile_and_mode(640.0);
        let (narrow_profile, narrow_mode, narrow_selection) = render_profile_and_mode(340.0);

        assert!(wide_profile.height() < narrow_profile.height());
        assert!(wide_profile.height() < 120.0);
        assert!(narrow_profile.right() <= 340.0);
        assert!(narrow_mode.right() <= 340.0);
        assert_eq!(wide_selection, RoomMode::Local);
        assert_eq!(narrow_selection, RoomMode::Local);
        assert!(wide_mode.min.y >= wide_profile.max.y);
        assert!(narrow_mode.min.y >= narrow_profile.max.y);
    }

    #[test]
    fn primary_actions_and_host_controls_fit_wide_and_narrow_layouts() {
        for width in [640.0, 280.0] {
            let layout = render_home_action_layout(width);

            assert!(layout.create_enabled);
            assert!(layout.join_enabled);
            assert!(layout.create_button.height() >= PRIMARY_ACTION_HEIGHT);
            assert!(layout.join_button.height() >= PRIMARY_ACTION_HEIGHT);
            assert!((layout.join_button.width() - JOIN_BUTTON_WIDTH).abs() < 1.0);
            assert!((layout.code_field.center().y - layout.join_button.center().y).abs() < 1.0);
            assert!(layout.create_button.width() >= layout.create_card.width() - 24.0);
            assert!(layout.manage_button.width() < layout.join_card.width() * 0.75);
            assert!(layout.create_button.right() <= width);
            assert!(layout.host_row.right() <= width);
            assert!(layout.manage_button.right() <= width);
            assert!(layout.code_field.right() <= width);
            assert!(layout.join_button.right() <= width);

            if width > 500.0 {
                assert!(layout.manage_button.center().y - layout.host_row.min.y < 24.0);
            } else {
                assert!(layout.host_row.height() >= layout.manage_button.height());
            }
        }
    }

    #[test]
    fn room_action_enablement_preserves_connection_and_code_requirements() {
        assert!(room_actions_enabled(false, false));
        assert!(!room_actions_enabled(true, false));
        assert!(!room_actions_enabled(false, true));

        assert!(join_action_enabled(true, true, false, false));
        assert!(!join_action_enabled(false, true, false, false));
        assert!(!join_action_enabled(true, false, false, false));
        assert!(!join_action_enabled(true, true, true, false));
        assert!(!join_action_enabled(true, true, false, true));
    }

    #[test]
    fn configure_network_button_opens_the_floating_window() {
        let context = egui::Context::default();
        let mut open = false;
        let mut mode = RoomMode::Local;
        let mut button_rect = None;

        let mut first_frame = context.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(640.0, 120.0),
                )),
                ..Default::default()
            },
            |ui| {
                let (_, button) = show_room_mode_selector(ui, &mut mode, &mut open);
                button_rect = Some(button.rect);
            },
        );
        first_frame.textures_delta.clear();
        assert!(!open);

        let position = button_rect
            .expect("network settings button should be laid out")
            .center();
        let mut second_frame = context.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(640.0, 120.0),
                )),
                events: vec![
                    egui::Event::PointerMoved(position),
                    egui::Event::PointerButton {
                        pos: position,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: egui::Modifiers::default(),
                    },
                    egui::Event::PointerButton {
                        pos: position,
                        button: egui::PointerButton::Primary,
                        pressed: false,
                        modifiers: egui::Modifiers::default(),
                    },
                ],
                ..Default::default()
            },
            |ui| {
                show_room_mode_selector(ui, &mut mode, &mut open);
            },
        );
        second_frame.textures_delta.clear();

        assert!(open);

        let mut shown = false;
        let mut third_frame = context.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(640.0, 480.0),
                )),
                ..Default::default()
            },
            |ui| {
                shown = show_network_window(ui.ctx(), &mut open, |window| {
                    window.label("Configuração de rede");
                });
            },
        );
        third_frame.textures_delta.clear();
        assert!(shown);

        open = false;
        let mut fourth_frame = context.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(640.0, 480.0),
                )),
                ..Default::default()
            },
            |ui| {
                shown = show_network_window(ui.ctx(), &mut open, |window| {
                    window.label("Configuração de rede");
                });
            },
        );
        fourth_frame.textures_delta.clear();
        assert!(!shown);
    }
}
