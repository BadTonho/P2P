use super::super::{ClientUi, RoomMode, ScreenShareRole};
use eframe::egui;

impl ClientUi {
    pub(super) fn show_diagnostics(
        &self,
        ui: &mut egui::Ui,
        open_logs_directory: &mut bool,
        export_logs: &mut bool,
    ) {
        egui::CollapsingHeader::new("Diagnóstico")
            .default_open(false)
            .show(ui, |ui| {
                if ui.button("Exportar logs").clicked() {
                    *export_logs = true;
                }

                ui.label("Log desta execução");
                ui.horizontal_wrapped(|ui| {
                    ui.label(self.logging.current_log_file_label());
                    if let Some(path) = self.logging.current_log_file()
                        && ui.small_button("Copiar caminho").clicked()
                    {
                        ui.ctx().copy_text(path.display().to_string());
                    }
                    if ui
                        .add_enabled(
                            self.logging.log_directory().is_some(),
                            egui::Button::new("Abrir pasta de logs").small(),
                        )
                        .clicked()
                    {
                        *open_logs_directory = true;
                    }
                });

                if let Some(error_path) = self.logging.errors_log_file() {
                    ui.label("Log de erros (apenas erros):");
                    ui.horizontal_wrapped(|ui| {
                        ui.label(error_path.display().to_string());
                        if ui.small_button("Copiar caminho").clicked() {
                            ui.ctx().copy_text(error_path.display().to_string());
                        }
                    });
                }

                if self.room_code.is_some() {
                    ui.separator();
                    self.show_screen_diagnostics(ui);
                }
            });
    }

    fn show_screen_diagnostics(&self, ui: &mut egui::Ui) {
        ui.heading("Transmissão de tela");
        if let Some(address) = self.host_addresses.get(self.selected_host_address) {
            ui.label(format!(
                "Adaptador de mídia UDP: {} ({})",
                address.ipv4, address.adapter
            ));
        }
        if self.room_mode == RoomMode::InternetTest {
            ui.label("Rede: TCP 9000 para sinalização; UDP 9002 para mídia direta.");
            ui.label(format!("STUN: {}", self.stun_server_url));
            if self.hosting_locally {
                ui.label(if self.turn_server.is_some() {
                    "TURN ativo neste PC; encaminhe UDP 3478 e UDP 50000–50100."
                } else {
                    "TURN desativado pelo anfitrião; a tela depende de P2P direto."
                });
            } else if self.turn_config_received {
                ui.label(
                    if self
                        .turn_room_config
                        .as_ref()
                        .is_some_and(|config| config.turn.is_some())
                    {
                        "O anfitrião habilitou TURN como alternativa."
                    } else {
                        "O anfitrião desativou TURN; a tela depende de P2P direto."
                    },
                );
            } else {
                ui.label("Aguardando a configuração de rede enviada pelo anfitrião.");
            }
            ui.label("Teste controlado: ws:// não criptografa a sinalização ou as credenciais temporárias do TURN.");
        } else {
            ui.label(
                "Rede local/Radmin: libere TCP 9001 e UDP 9002–9009 no firewall dos participantes.",
            );
        }

        if !self.screen_pipeline_summary.is_empty() {
            ui.label(&self.screen_pipeline_summary);
        }
        if !self.screen_share_metrics.encoder_backend.is_empty()
            || !self.screen_share_metrics.decoder_backend.is_empty()
        {
            ui.label(format!(
                "Encoder: {}. Decoder: {}. Preferência: {}.",
                self.screen_share_metrics.encoder_backend,
                self.screen_share_metrics.decoder_backend,
                self.screen_share_metrics.decoder_preference
            ));
            if let Some(reason) = &self.screen_share_metrics.encoder_fallback_reason {
                ui.label(format!("Fallback do encoder: {reason}"));
            }
            if let Some(reason) = &self.screen_share_metrics.decoder_fallback_reason {
                ui.label(format!("Fallback do decoder: {reason}"));
            }
        }

        match &self.screen_share_role {
            ScreenShareRole::Sending { .. } => {
                ui.label(format!(
                    "Sessão {} · SSRC {} · H.264: {} entradas, {} produzidos, {} enviados (IDR {}, P {}), {} descartados antes do IDR.",
                    self.screen_share_metrics.session_id,
                    self.screen_share_metrics.track_ssrc.unwrap_or_default(),
                    self.screen_share_metrics.encoder_input_frames,
                    self.screen_share_metrics.encoded_frames,
                    self.screen_share_metrics.sent_frames,
                    self.screen_share_metrics.sent_idr_frames,
                    self.screen_share_metrics.sent_delta_frames,
                    self.screen_share_metrics.dropped_before_initial_idr
                ));
                ui.label(&self.screen_share_metrics.selected_ice_pair);
                ui.label(format!(
                    "RTP enviado: {} pacotes, {} bytes.",
                    self.screen_share_metrics.outbound_rtp_packets,
                    self.screen_share_metrics.outbound_rtp_bytes
                ));
                ui.label(&self.screen_share_metrics.rtc_outbound_summary);
                ui.label(&self.screen_share_metrics.h264_diagnostics);
            }
            ScreenShareRole::Receiving { .. } => {
                ui.label(format!(
                    "Sessão {} · SSRC {} · recebidos {}, montados {}, entradas no decoder {}, decodificados {}, publicados {}, atualizações da prévia {}, erros H.264 {}.",
                    self.screen_share_metrics.session_id,
                    self.screen_share_metrics.track_ssrc.unwrap_or_default(),
                    self.screen_share_metrics.received_packets,
                    self.screen_share_metrics.received_delta_frames,
                    self.screen_share_metrics.decoder_input_frames,
                    self.screen_share_metrics.decoded_frames,
                    self.screen_share_metrics.published_frames,
                    self.screen_share_metrics.ui_texture_updates,
                    self.screen_share_metrics.decode_errors
                ));
                ui.label(&self.screen_share_metrics.selected_ice_pair);
                ui.label(format!(
                    "RTP recebido: {} pacotes, {} bytes; perda {}, jitter {:.1} ms.",
                    self.screen_share_metrics.inbound_rtp_packets,
                    self.screen_share_metrics.inbound_rtp_bytes,
                    self.screen_share_metrics.inbound_rtp_lost,
                    self.screen_share_metrics.inbound_rtp_jitter_ms
                ));
                ui.label(&self.screen_share_metrics.rtc_inbound_summary);
                ui.label(format!(
                    "Janela adaptativa de reordenação RTP: {} ms, com {} amostras recentes.",
                    self.screen_share_metrics.rtp_reorder_window_ms,
                    self.screen_share_metrics.rtp_reorder_samples
                ));
                if let Some(error) = &self.screen_share_metrics.last_decode_error {
                    ui.label(format!("Último erro H.264: {error}"));
                }
                ui.label(&self.screen_share_metrics.h264_diagnostics);
                let recovery_time = self
                    .screen_share_metrics
                    .last_recovery_time_millis
                    .map_or_else(
                        || "ainda não disponível".to_owned(),
                        |ms| format!("{ms} ms"),
                    );
                ui.label(format!(
                    "Recuperação: PLI enviado/recebido {}/{}, fila cheia {}, ressincronizações {}, IDRs decodificados {}, último tempo até IDR {}.",
                    self.screen_share_metrics.pli_requests_sent,
                    self.screen_share_metrics.pli_requests_received,
                    self.screen_share_metrics.pli_queue_overflow,
                    self.screen_share_metrics.keyframe_resyncs,
                    self.screen_share_metrics.decoded_idr_frames,
                    recovery_time
                ));
            }
            ScreenShareRole::Idle | ScreenShareRole::Requesting { .. } => {}
        }

        if !matches!(&self.screen_share_role, ScreenShareRole::Idle)
            || self.screen_share_metrics.local_ice_candidates > 0
            || self.screen_share_metrics.remote_ice_candidates > 0
            || self.screen_share_status.is_some()
        {
            ui.label(format!(
                "ICE: {} candidatos locais ({} STUN, {} TURN), {} do amigo ({} STUN, {} TURN).",
                self.screen_share_metrics.local_ice_candidates,
                self.screen_share_metrics.local_srflx_candidates,
                self.screen_share_metrics.local_relay_candidates,
                self.screen_share_metrics.remote_ice_candidates,
                self.screen_share_metrics.remote_srflx_candidates,
                self.screen_share_metrics.remote_relay_candidates
            ));
        }

        if self.room_mode == RoomMode::Local && !self.control_queue.is_empty() {
            ui.separator();
            ui.label("Fila de sucessão · métricas dos enlaces");
            let mut queue = self.control_queue.clone();
            queue.sort_by(|left, right| {
                right
                    .eligible
                    .cmp(&left.eligible)
                    .then_with(|| left.loss_percent.total_cmp(&right.loss_percent))
                    .then_with(|| left.jitter_ms.total_cmp(&right.jitter_ms))
                    .then_with(|| left.latency_ms.total_cmp(&right.latency_ms))
                    .then_with(|| left.participant.order.cmp(&right.participant.order))
            });
            for (index, candidate) in queue.iter().enumerate() {
                ui.label(format!(
                    "{}. {} — perda {:.1}%, jitter {:.1} ms, latência {:.1} ms{}",
                    index + 1,
                    candidate.participant.display_name,
                    candidate.loss_percent,
                    candidate.jitter_ms,
                    candidate.latency_ms,
                    if candidate.eligible {
                        ""
                    } else {
                        " (inelegível)"
                    }
                ));
            }
        }
    }
}
