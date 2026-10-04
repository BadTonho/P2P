use crate::app::ClientUi;
use eframe::egui;

impl ClientUi {
    pub(in crate::app::ui) fn show_handoff_panel(
        &mut self,
        ui: &mut egui::Ui,
        context: &egui::Context,
    ) {
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
                    if ui.add_enabled(can_cancel, egui::Button::new("Cancelar transferência")).clicked()
                        && let (Some(signaling), Some((_, token))) = (&self.signaling, &self.outgoing_transfer)
                            && let Err(error) = signaling.cancel_host_transfer(token.clone()) {
                                self.handoff_error = Some(error);
                            }
                    if ui.button("Encerrar sala").clicked() {
                        self.end_room_explicitly(context);
                    }
                });
            });
        }

        if let Some(error) = self.handoff_error.clone() {
            ui.add_space(10.0);
            ui.group(|ui| {
                Self::show_notice(ui, "Transferência:", &error);
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
                        self.end_room_explicitly(context);
                    }
                });
            });
        }
    }
}
