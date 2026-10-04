use super::{
    ClientUi, PRIMARY_ACTION_HEIGHT, PRIMARY_ACTION_WIDTH, RoomMode,
    configure_action_button_visuals, join_action_enabled, primary_action_button,
    room_actions_enabled, show_host_picker_and_manage, show_network_window, show_profile_pill,
    show_profile_window_contents, show_room_mode_selector,
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

fn render_home_action_layout(width: f32, code_value: &str) -> HomeActionLayout {
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
                let button = primary_action_button(
                    ui,
                    "Criar sala",
                    PRIMARY_ACTION_WIDTH,
                    room_actions_enabled(false, false),
                );
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
                let mut code = code_value.to_owned();
                let mut field_rect = None;
                let mut button_rect = None;
                let mut button_enabled = false;
                ui.horizontal_wrapped(|ui| {
                    let field_width =
                        (ui.available_width() - PRIMARY_ACTION_WIDTH - ui.spacing().item_spacing.x)
                            .max(80.0);
                    field_rect = Some(
                        ui.add_sized(
                            egui::vec2(field_width, PRIMARY_ACTION_HEIGHT),
                            egui::TextEdit::singleline(&mut code),
                        )
                        .rect,
                    );
                    let response = primary_action_button(
                        ui,
                        "Entrar",
                        PRIMARY_ACTION_WIDTH,
                        join_action_enabled(!code.trim().is_empty(), true, false, false),
                    );
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

#[test]
fn profile_pill_renders_and_responds_to_click() {
    let context = egui::Context::default();
    let display_name = "Tonho".to_owned();
    let mut pill_rect = None;

    let mut first_frame = context.run_ui(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(640.0, 100.0),
            )),
            ..Default::default()
        },
        |ui| {
            let avatar = ui.ctx().load_texture(
                "profile-pill-test-avatar",
                egui::ColorImage::filled([1, 1], egui::Color32::WHITE),
                egui::TextureOptions::default(),
            );
            let response = show_profile_pill(ui, &display_name, Some(&avatar));
            pill_rect = Some(response.rect);
        },
    );
    first_frame.textures_delta.clear();

    let rect = pill_rect.expect("profile pill should be laid out");
    assert!(rect.width() > 50.0);
    assert!(rect.height() >= 34.0);

    let click_pos = rect.center();
    let mut clicked = false;
    let mut second_frame = context.run_ui(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(640.0, 100.0),
            )),
            events: vec![
                egui::Event::PointerMoved(click_pos),
                egui::Event::PointerButton {
                    pos: click_pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::default(),
                },
                egui::Event::PointerButton {
                    pos: click_pos,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::default(),
                },
            ],
            ..Default::default()
        },
        |ui| {
            let response = show_profile_pill(ui, &display_name, None);
            clicked = response.clicked();
        },
    );
    second_frame.textures_delta.clear();
    assert!(clicked);
}

#[test]
fn profile_window_contents_render_and_handle_actions() {
    let context = egui::Context::default();
    let mut display_name = "Tonho".to_owned();

    let mut output = context.run_ui(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(400.0, 400.0),
            )),
            ..Default::default()
        },
        |ui| {
            let actions = show_profile_window_contents(
                ui,
                &mut display_name,
                None,
                true,
                Some("Aviso de teste"),
            );
            assert!(!actions.choose_avatar);
            assert!(!actions.remove_avatar);
            assert!(!actions.close_window);
        },
    );
    output.textures_delta.clear();
}

#[test]
fn home_bottom_bar_renders_profile_and_right_aligned_buttons() {
    let context = egui::Context::default();
    let mut app = ClientUi::default();
    let mut open_settings = false;
    let mut open_update_settings = false;

    let mut output = context.run_ui(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(640.0, 60.0),
            )),
            ..Default::default()
        },
        |ui| {
            app.show_home_bottom_bar(ui, &mut open_settings, &mut open_update_settings);
        },
    );
    output.textures_delta.clear();
    assert!(!open_settings);
    assert!(!open_update_settings);
}

#[test]
fn room_mode_selector_fits_wide_and_narrow_layouts() {
    for width in [640.0, 340.0] {
        let context = egui::Context::default();
        let mut mode = RoomMode::Local;
        let mut open = false;
        let mut output = context.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(width, 100.0),
                )),
                ..Default::default()
            },
            |ui| {
                let (row, _) = show_room_mode_selector(ui, &mut mode, &mut open);
                assert!(row.rect.right() <= width);
            },
        );
        output.textures_delta.clear();
        assert_eq!(mode, RoomMode::Local);
    }
}

#[test]
fn primary_actions_and_host_controls_fit_wide_and_narrow_layouts() {
    for width in [640.0, 280.0] {
        let layout = render_home_action_layout(width, "");

        assert!(layout.create_enabled);
        assert!(!layout.join_enabled);
        assert!(layout.create_button.height() >= PRIMARY_ACTION_HEIGHT);
        assert!(layout.join_button.height() >= PRIMARY_ACTION_HEIGHT);
        assert!((layout.create_button.width() - PRIMARY_ACTION_WIDTH).abs() < 1.0);
        assert!((layout.join_button.width() - PRIMARY_ACTION_WIDTH).abs() < 1.0);
        assert!((layout.code_field.center().y - layout.join_button.center().y).abs() < 1.0);
        assert!(layout.create_button.left() >= layout.create_card.left());
        assert!(layout.create_button.left() - layout.create_card.left() < 20.0);
        assert!(layout.create_button.right() < layout.create_card.right());
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

        let enabled_layout = render_home_action_layout(width, "ABCD1234");
        assert!(enabled_layout.join_enabled);
    }
}

#[test]
fn action_button_palette_is_neutral_and_distinguishes_interaction_states() {
    for mut visuals in [egui::Visuals::dark(), egui::Visuals::light()] {
        configure_action_button_visuals(&mut visuals);
        let widgets = visuals.widgets;

        for color in [
            widgets.inactive.bg_fill,
            widgets.inactive.bg_stroke.color,
            widgets.inactive.fg_stroke.color,
            widgets.hovered.bg_fill,
            widgets.active.bg_fill,
            widgets.noninteractive.bg_fill,
        ] {
            assert_eq!(color.r(), color.g());
            assert_eq!(color.g(), color.b());
        }
        assert_ne!(widgets.inactive.bg_fill, widgets.hovered.bg_fill);
        assert_ne!(widgets.hovered.bg_fill, widgets.active.bg_fill);
        assert_ne!(widgets.inactive.bg_stroke.color, egui::Color32::TRANSPARENT);
        assert_ne!(
            widgets.noninteractive.bg_stroke.color,
            egui::Color32::TRANSPARENT
        );

        if visuals.dark_mode {
            assert!(widgets.inactive.fg_stroke.color.r() > widgets.inactive.bg_fill.r());
            assert!(
                widgets.noninteractive.fg_stroke.color.r() > widgets.noninteractive.bg_fill.r()
            );
        } else {
            assert!(widgets.inactive.fg_stroke.color.r() < widgets.inactive.bg_fill.r());
            assert!(
                widgets.noninteractive.fg_stroke.color.r() < widgets.noninteractive.bg_fill.r()
            );
        }
    }
}

#[test]
fn action_button_visual_changes_are_scoped_to_the_button() {
    for theme in [egui::Visuals::dark(), egui::Visuals::light()] {
        let context = egui::Context::default();
        context.set_visuals(theme);
        let mut unchanged = false;
        let mut output = context.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(240.0, 80.0),
                )),
                ..Default::default()
            },
            |ui| {
                let before = ui.visuals().clone();
                let response = primary_action_button(ui, "Criar sala", PRIMARY_ACTION_WIDTH, false);
                unchanged = *ui.visuals() == before;
                assert!(!response.enabled());
            },
        );
        output.textures_delta.clear();
        assert!(unchanged);
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
