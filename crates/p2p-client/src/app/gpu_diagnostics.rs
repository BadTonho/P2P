use super::*;

impl ClientUi {
    pub(super) fn refresh_gpu_telemetry(&mut self, context: &egui::Context) {
        let active = self.screen_share_session.is_some()
            || !self.group_share.outbound_sessions.is_empty()
            || !self.group_share.inbound_sessions.is_empty();
        let enabled =
            self.logging_level == LoggingLevel::Detailed && self.log_performance_metrics && active;
        if !enabled {
            self.gpu_telemetry = None;
            self.last_gpu_metrics_log_at = None;
            return;
        }
        if self.gpu_telemetry.is_none() {
            self.gpu_telemetry = Some(crate::gpu_telemetry::GpuTelemetry::start());
        }
        context.request_repaint_after(Duration::from_secs(1));
        if self
            .last_gpu_metrics_log_at
            .is_none_or(|last| last.elapsed() >= Duration::from_secs(5))
        {
            tracing::info!(gpu_process_engines = %self.gpu_diagnostic_summary(), "Utilização dos motores GPU do processo; percentuais independentes, sem soma");
            self.last_gpu_metrics_log_at = Some(Instant::now());
        }
    }

    pub(super) fn gpu_diagnostic_summary(&self) -> String {
        self.gpu_telemetry.as_ref().map_or_else(
            || "inactive; ative logs Detalhados e métricas de desempenho durante o compartilhamento".to_owned(),
            crate::gpu_telemetry::GpuTelemetry::label,
        )
    }

    pub(super) fn video_performance_summary(&self) -> String {
        let mut sessions = Vec::new();
        if let Some(session) = &self.screen_share_session {
            sessions.push(session.metrics());
        }
        sessions.extend(
            self.group_share
                .outbound_sessions
                .values()
                .map(ScreenShareSession::metrics),
        );
        sessions.extend(
            self.group_share
                .inbound_sessions
                .values()
                .map(ScreenShareSession::metrics),
        );
        let mut labels = sessions
            .iter()
            .map(|metrics| {
                format!(
                    "session={} encoder={} decoder={} [{}]",
                    metrics.session_id,
                    metrics.encoder_backend,
                    metrics.decoder_backend,
                    metrics.video_performance.label()
                )
            })
            .collect::<Vec<_>>();
        if let Some(capture) = &self.screen_capture {
            labels.push(format!("capture [{}]", capture.performance_label()));
        }
        if labels.is_empty() {
            "nenhuma sessão com medições".to_owned()
        } else {
            labels.join("\n")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::ManuallyDrop;

    #[test]
    fn gpu_sampler_stays_inactive_without_detailed_logs_or_active_media() {
        let mut app = ManuallyDrop::new(ClientUi::default());
        app.logging_level = LoggingLevel::Detailed;
        app.log_performance_metrics = true;
        app.refresh_gpu_telemetry(&egui::Context::default());
        assert!(app.gpu_telemetry.is_none());
        assert!(app.gpu_diagnostic_summary().contains("inactive"));
        app.screen_share_session = Some(
            ScreenShareSession::new_with_port(
                egui::Context::default(),
                std::net::Ipv4Addr::LOCALHOST,
                0,
                None,
                None,
                VideoDecoderPreference::Cpu,
            )
            .unwrap(),
        );
        app.logging_level = LoggingLevel::WarningsAndErrors;
        app.refresh_gpu_telemetry(&egui::Context::default());
        assert!(app.gpu_telemetry.is_none());
        app.logging_level = LoggingLevel::Detailed;
        app.log_performance_metrics = false;
        app.refresh_gpu_telemetry(&egui::Context::default());
        assert!(app.gpu_telemetry.is_none());
        app.screen_share_session = None;
    }
}
