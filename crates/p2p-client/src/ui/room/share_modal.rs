use eframe::egui::{self, Color32, CornerRadius, Frame, Margin, Stroke, Vec2};

use crate::app::ClientUi;
use crate::screen_capture::{MonitorOption, WindowOption};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ScreenShareModalTab {
    #[default]
    Screens,
    Windows,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShareSourceSelection {
    Monitor(String),
    Window(isize),
}

const DISCORD_BLURPLE: Color32 = Color32::from_rgb(88, 101, 242);
const DISCORD_CARD_BG: Color32 = Color32::from_rgb(43, 45, 49);
const DISCORD_CARD_BG_SELECTED: Color32 = Color32::from_rgb(30, 31, 34);
const DISCORD_BORDER_NORMAL: Color32 = Color32::from_rgb(60, 64, 72);

impl ClientUi {
    pub(in crate::app::ui) fn show_screen_share_modal(&mut self, ctx: &egui::Context) {
        if !self.screen_share_modal_open {
            return;
        }

        self.poll_windows_refresh();

        let mut should_start_share = false;
        let mut should_open_picker = false;
        let mut cancel_clicked = false;

        let modal = egui::Modal::new(egui::Id::new("screen_share_modal_dialog"))
            .backdrop_color(Color32::from_black_alpha(175));

        let modal_response = modal.show(ctx, |ui| {
            ui.set_width(620.0);
            ui.vertical(|ui| {
                ui.add_space(4.0);
                ui.heading("Transmitir sua tela");
                ui.label(
                    "Escolha uma tela inteira ou janela de aplicativo para transmitir na chamada.",
                );
                ui.add_space(8.0);

                // Abas superiores estilo Discord: Telas / Janelas
                ui.horizontal(|ui| {
                    let screens_active =
                        self.screen_share_modal_tab == ScreenShareModalTab::Screens;
                    let windows_active =
                        self.screen_share_modal_tab == ScreenShareModalTab::Windows;

                    let screens_btn =
                        egui::Button::new(egui::RichText::new("🖥  Telas").size(14.0).strong())
                            .fill(if screens_active {
                                DISCORD_BLURPLE
                            } else {
                                Color32::from_rgb(45, 47, 52)
                            })
                            .corner_radius(CornerRadius::same(6));
                    if ui.add(screens_btn).clicked() {
                        self.screen_share_modal_tab = ScreenShareModalTab::Screens;
                        if !self.monitors_loaded {
                            self.refresh_monitors(true);
                        }
                    }

                    let windows_btn =
                        egui::Button::new(egui::RichText::new("🪟  Janelas").size(14.0).strong())
                            .fill(if windows_active {
                                DISCORD_BLURPLE
                            } else {
                                Color32::from_rgb(45, 47, 52)
                            })
                            .corner_radius(CornerRadius::same(6));
                    if ui.add(windows_btn).clicked() {
                        self.screen_share_modal_tab = ScreenShareModalTab::Windows;
                        if !self.windows_loaded && !self.windows_loading {
                            self.refresh_windows();
                        }
                    }
                });

                ui.add_space(10.0);
                ui.separator();
                ui.add_space(6.0);

                // Conteúdo da Aba
                match self.screen_share_modal_tab {
                    ScreenShareModalTab::Screens => {
                        self.render_screens_tab(ui);
                    }
                    ScreenShareModalTab::Windows => {
                        should_open_picker = self.render_windows_tab(ui);
                    }
                }

                ui.add_space(10.0);
                ui.separator();
                ui.add_space(6.0);

                // Opção de Áudio Integrada no Modal
                ui.horizontal(|ui| {
                    let audio_changed = ui
                        .checkbox(
                            &mut self.include_system_audio,
                            "Compartilhar áudio do computador",
                        )
                        .changed();
                    if audio_changed {
                        self.save_preferences();
                        tracing::info!(
                            include_system_audio = self.include_system_audio,
                            "Preferência de áudio do sistema alterada no modal"
                        );
                    }
                    ui.small("Transmite os sons de jogos, vídeos e músicas reproduzidos no PC.");
                });

                ui.add_space(8.0);

                // Rodapé de Ações
                ui.horizontal(|ui| {
                    if ui.button("Cancelar").clicked() {
                        cancel_clicked = true;
                    }

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let can_share = self.selected_share_source.is_some()
                            && (self.peer_connected || self.hosting_locally);

                        let share_btn = egui::Button::new(
                            egui::RichText::new("Transmitir tela")
                                .size(14.0)
                                .color(Color32::WHITE)
                                .strong(),
                        )
                        .fill(if can_share {
                            DISCORD_BLURPLE
                        } else {
                            Color32::from_rgb(60, 64, 72)
                        })
                        .corner_radius(CornerRadius::same(6));

                        if ui.add_enabled(can_share, share_btn).clicked() {
                            should_start_share = true;
                        }
                    });
                });
            });
        });

        if cancel_clicked || modal_response.should_close() {
            self.screen_share_modal_open = false;
        }

        if should_open_picker {
            self.screen_share_modal_open = false;
            self.auto_start_share_after_picker = true;
            self.select_screen(ctx);
        } else if should_start_share {
            self.start_selected_share(ctx);
        }
    }

    fn render_screens_tab(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(format!(
                "Monitores disponíveis: {}",
                self.available_monitors.len()
            ));
            if ui.button("Atualizar").clicked() {
                self.refresh_monitors(true);
            }
        });
        ui.add_space(6.0);

        if self.available_monitors.is_empty() {
            ui.vertical_centered(|ui| {
                ui.add_space(40.0);
                ui.label("Nenhum monitor encontrado.");
                ui.small("Confira se o Windows está configurado para Estender as telas.");
            });
            return;
        }

        // Se nenhuma fonte estiver selecionada, seleciona o primeiro monitor por padrão
        if self.selected_share_source.is_none()
            && let Some(first) = self.available_monitors.first()
        {
            self.selected_share_source =
                Some(ShareSourceSelection::Monitor(first.device_id.clone()));
        }

        let monitors = self.available_monitors.clone();
        egui::ScrollArea::vertical()
            .id_salt("screens_scroll")
            .max_height(260.0)
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    for (index, monitor) in monitors.iter().enumerate() {
                        let is_selected = matches!(
                            &self.selected_share_source,
                            Some(ShareSourceSelection::Monitor(id)) if id == &monitor.device_id
                        );

                        if render_monitor_card(ui, index + 1, monitor, is_selected) {
                            self.selected_share_source =
                                Some(ShareSourceSelection::Monitor(monitor.device_id.clone()));
                        }
                        ui.add_space(8.0);
                    }
                });
            });
    }

    fn render_windows_tab(&mut self, ui: &mut egui::Ui) -> bool {
        let mut open_picker = false;

        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.window_search_query)
                    .hint_text("Buscar janela ou aplicativo…")
                    .desired_width(260.0),
            );
            if ui.button("Atualizar janelas").clicked() {
                self.refresh_windows();
            }
        });
        ui.add_space(6.0);

        if self.windows_loading {
            ui.vertical_centered(|ui| {
                ui.add_space(40.0);
                ui.spinner();
                ui.label("Consultando janelas abertas no Windows…");
            });
            return false;
        }

        let query = self.window_search_query.trim().to_lowercase();
        let filtered_windows = self
            .available_windows
            .iter()
            .filter(|w| {
                query.is_empty()
                    || w.title.to_lowercase().contains(&query)
                    || w.process_name.to_lowercase().contains(&query)
            })
            .cloned()
            .collect::<Vec<_>>();

        if filtered_windows.is_empty() {
            ui.vertical_centered(|ui| {
                ui.add_space(30.0);
                ui.label("Nenhuma janela compatível encontrada.");
                ui.small("Abra ou restaure o aplicativo desejado no Windows.");
                ui.add_space(8.0);
                if ui
                    .button("Selecionar pelo seletor padrão do Windows")
                    .clicked()
                {
                    open_picker = true;
                }
            });
            return open_picker;
        }

        egui::ScrollArea::vertical()
            .id_salt("windows_scroll")
            .max_height(240.0)
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    for win in &filtered_windows {
                        let is_selected = matches!(
                            &self.selected_share_source,
                            Some(ShareSourceSelection::Window(id)) if *id == win.id
                        );

                        if render_window_card(ui, win, is_selected) {
                            self.selected_share_source = Some(ShareSourceSelection::Window(win.id));
                        }
                        ui.add_space(8.0);
                    }
                });
            });

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.small("Não encontrou o que procurava?");
            if ui.small_button("Usar seletor do Windows").clicked() {
                open_picker = true;
            }
        });

        open_picker
    }
}

/// Renderiza o cartão visual de um Monitor (estilo Discord)
fn render_monitor_card(
    ui: &mut egui::Ui,
    ordinal: usize,
    monitor: &MonitorOption,
    is_selected: bool,
) -> bool {
    let card_width = 175.0;
    let card_height = 145.0;

    let frame = Frame::new()
        .fill(if is_selected {
            DISCORD_CARD_BG_SELECTED
        } else {
            DISCORD_CARD_BG
        })
        .stroke(Stroke::new(
            if is_selected { 2.5 } else { 1.0 },
            if is_selected {
                DISCORD_BLURPLE
            } else {
                DISCORD_BORDER_NORMAL
            },
        ))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::same(8));

    let response = frame.show(ui, |ui| {
        ui.set_width(card_width);
        ui.set_height(card_height);
        ui.vertical_centered(|ui| {
            // Miniatura visual do monitor (Aspect Ratio 16:9 estilizado)
            let (rect, _) = ui.allocate_exact_size(Vec2::new(140.0, 78.0), egui::Sense::hover());
            let painter = ui.painter();

            // Moldura externa da tela
            painter.rect_filled(rect, 4.0, Color32::from_rgb(20, 22, 25));
            painter.rect_stroke(
                rect,
                4.0,
                Stroke::new(1.0, Color32::from_rgb(70, 74, 82)),
                egui::StrokeKind::Inside,
            );

            // Conteúdo interno (Wallpaper degradê e barra de tarefas)
            let inner_rect = rect.shrink(3.0);
            painter.rect_filled(inner_rect, 2.0, Color32::from_rgb(32, 42, 68));

            // Barra de tarefas na base
            let taskbar_rect = egui::Rect::from_min_max(
                egui::pos2(inner_rect.min.x, inner_rect.max.y - 6.0),
                inner_rect.max,
            );
            painter.rect_filled(taskbar_rect, 0.0, Color32::from_rgb(15, 17, 22));

            // Número do monitor no centro
            painter.text(
                inner_rect.center(),
                egui::Align2::CENTER_CENTER,
                format!("{ordinal}"),
                egui::FontId::proportional(22.0),
                Color32::from_rgb(200, 215, 255),
            );

            // Indicador de seleção no canto
            if is_selected {
                let badge_pos = egui::pos2(rect.max.x - 10.0, rect.min.y + 10.0);
                painter.circle_filled(badge_pos, 7.0, DISCORD_BLURPLE);
                painter.text(
                    badge_pos,
                    egui::Align2::CENTER_CENTER,
                    "✔",
                    egui::FontId::proportional(9.0),
                    Color32::WHITE,
                );
            }

            ui.add_space(6.0);
            let title = if monitor.is_primary {
                format!("Tela {ordinal} (Principal)")
            } else {
                format!("Tela {ordinal}")
            };
            ui.label(egui::RichText::new(title).size(13.0).strong());
            ui.label(
                egui::RichText::new(&monitor.name)
                    .size(11.0)
                    .color(Color32::from_rgb(180, 190, 205)),
            );
            ui.small(format!("{} × {}", monitor.width, monitor.height));
        });
    });

    let card_response = ui.interact(
        response.response.rect,
        ui.id().with(("monitor_click", ordinal)),
        egui::Sense::click(),
    );

    card_response.clicked()
}

/// Renderiza o cartão visual de uma Janela de aplicativo (estilo Discord)
fn render_window_card(ui: &mut egui::Ui, win: &WindowOption, is_selected: bool) -> bool {
    let card_width = 175.0;
    let card_height = 145.0;

    let frame = Frame::new()
        .fill(if is_selected {
            DISCORD_CARD_BG_SELECTED
        } else {
            DISCORD_CARD_BG
        })
        .stroke(Stroke::new(
            if is_selected { 2.5 } else { 1.0 },
            if is_selected {
                DISCORD_BLURPLE
            } else {
                DISCORD_BORDER_NORMAL
            },
        ))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::same(8));

    let response = frame.show(ui, |ui| {
        ui.set_width(card_width);
        ui.set_height(card_height);
        ui.vertical(|ui| {
            // Cabeçalho da janela simulada
            let (rect, _) = ui.allocate_exact_size(Vec2::new(155.0, 72.0), egui::Sense::hover());
            let painter = ui.painter();

            // Moldura externa da janela
            painter.rect_filled(rect, 4.0, Color32::from_rgb(26, 28, 32));
            painter.rect_stroke(
                rect,
                4.0,
                Stroke::new(1.0, Color32::from_rgb(60, 64, 72)),
                egui::StrokeKind::Inside,
            );

            // Barra de título com 3 botõezinhos (estilo janela Discord)
            let titlebar_rect =
                egui::Rect::from_min_max(rect.min, egui::pos2(rect.max.x, rect.min.y + 14.0));
            painter.rect_filled(titlebar_rect, 4.0, Color32::from_rgb(38, 41, 47));

            // Três círculos da janela
            painter.circle_filled(
                egui::pos2(rect.min.x + 8.0, rect.min.y + 7.0),
                2.5,
                Color32::from_rgb(235, 100, 100),
            );
            painter.circle_filled(
                egui::pos2(rect.min.x + 15.0, rect.min.y + 7.0),
                2.5,
                Color32::from_rgb(240, 190, 80),
            );
            painter.circle_filled(
                egui::pos2(rect.min.x + 22.0, rect.min.y + 7.0),
                2.5,
                Color32::from_rgb(100, 200, 100),
            );

            // Ícone do executável no centro do preview
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                &win.process_name,
                egui::FontId::proportional(12.0),
                Color32::from_rgb(180, 190, 210),
            );

            // Indicador de seleção
            if is_selected {
                let badge_pos = egui::pos2(rect.max.x - 10.0, rect.min.y + 7.0);
                painter.circle_filled(badge_pos, 6.0, DISCORD_BLURPLE);
                painter.text(
                    badge_pos,
                    egui::Align2::CENTER_CENTER,
                    "✔",
                    egui::FontId::proportional(8.0),
                    Color32::WHITE,
                );
            }

            ui.add_space(4.0);

            // Título da janela (truncado se necessário)
            let display_title = if win.title.chars().count() > 22 {
                format!("{}…", win.title.chars().take(20).collect::<String>())
            } else {
                win.title.clone()
            };
            ui.label(egui::RichText::new(display_title).size(12.0).strong());
            ui.small(format!("{} × {}", win.width, win.height));
        });
    });

    let card_response = ui.interact(
        response.response.rect,
        ui.id().with(("window_click", win.id)),
        egui::Sense::click(),
    );

    card_response.clicked()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_modal_initial_state() {
        let app = ClientUi::default();
        assert!(!app.screen_share_modal_open);
        assert_eq!(app.screen_share_modal_tab, ScreenShareModalTab::Screens);
        assert!(app.selected_share_source.is_none());
    }

    #[test]
    fn test_open_screen_share_modal_defaults_selection() {
        let mut app = ClientUi::default();
        app.available_monitors = vec![
            MonitorOption {
                device_id: r"\\.\DISPLAY1".to_owned(),
                name: "Monitor 1".to_owned(),
                width: 1920,
                height: 1080,
                is_primary: true,
            },
            MonitorOption {
                device_id: r"\\.\DISPLAY2".to_owned(),
                name: "Monitor 2".to_owned(),
                width: 1920,
                height: 1080,
                is_primary: false,
            },
        ];

        app.open_screen_share_modal();
        assert!(app.screen_share_modal_open);
        assert_eq!(
            app.selected_share_source,
            Some(ShareSourceSelection::Monitor(r"\\.\DISPLAY1".to_owned()))
        );
    }

    #[test]
    fn test_selection_variants() {
        let monitor_sel = ShareSourceSelection::Monitor("disp_1".into());
        let window_sel = ShareSourceSelection::Window(9876);

        assert_ne!(monitor_sel, window_sel);
        assert_eq!(monitor_sel, ShareSourceSelection::Monitor("disp_1".into()));
        assert_eq!(window_sel, ShareSourceSelection::Window(9876));
    }

    #[test]
    fn test_window_option_equality_and_filtering() {
        let win = WindowOption {
            id: 101,
            title: "VS Code — main.rs".to_owned(),
            process_name: "Code.exe".to_owned(),
            width: 1920,
            height: 1080,
        };

        assert_eq!(win.id, 101);
        assert_eq!(win.process_name, "Code.exe");
        assert!(win.title.contains("VS Code"));
    }

    #[test]
    fn test_modal_rendering_with_egui_context() {
        let context = egui::Context::default();
        let mut app = ClientUi::default();
        app.screen_share_modal_open = true;
        app.available_monitors = vec![MonitorOption {
            device_id: "disp_0".to_owned(),
            name: "Display 0".to_owned(),
            width: 1280,
            height: 720,
            is_primary: true,
        }];

        let mut output = context.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1000.0, 700.0),
                )),
                ..Default::default()
            },
            |ui| {
                app.show_screen_share_modal(ui.ctx());
            },
        );
        output.textures_delta.clear();

        assert!(app.screen_share_modal_open);
    }
}
