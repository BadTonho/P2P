use super::*;

pub(crate) fn compute_microphone_level(rms: f32) -> (f32, f32) {
    let level_dbfs = if rms > 0.0 { 20.0 * rms.log10() } else { -60.0 };
    let level_dbfs = level_dbfs.clamp(-60.0, 0.0);
    let normalized = (level_dbfs + 60.0) / 60.0;
    (level_dbfs, normalized)
}

impl ClientUi {
    pub(crate) fn start_microphone(&mut self) {
        tracing::info!("Iniciando teste local de microfone e retorno de áudio");
        self.last_audio_metrics_log_at = None;
        self.microphone_error = None;
        self.microphone_monitor_error = None;
        self.microphone_audio_warning = false;
        self.microphone_clipping_warning = false;
        self.microphone_level = 0.0;
        self.microphone_level_dbfs = -60.0;
        match MicrophoneTest::start(self.monitor_gain_db) {
            Ok(test) => {
                self.microphone = Some(test);
                tracing::info!(
                    gain_db = self.monitor_gain_db,
                    "Teste do microfone iniciado"
                );
            }
            Err(error) => {
                tracing::error!(error = %error, "Falha ao iniciar teste do microfone");
                self.microphone_error = Some(error);
            }
        }
    }

    pub(crate) fn stop_microphone(&mut self) {
        let was_active = self.microphone.is_some();
        self.microphone = None;
        if was_active {
            tracing::info!("Teste local do microfone encerrado");
        }
        self.microphone_level = 0.0;
        self.microphone_level_dbfs = -60.0;
        self.microphone_clipping_warning = false;
    }

    pub(crate) fn refresh_microphone(&mut self) {
        let Some(microphone) = self.microphone.as_mut() else {
            return;
        };

        let rms = microphone.level();
        let (level_dbfs, level) = compute_microphone_level(rms);
        if self.log_performance_metrics
            && self
                .last_audio_metrics_log_at
                .is_none_or(|last| last.elapsed() >= Duration::from_secs(5))
        {
            tracing::info!(
                level_dbfs,
                gain_db = self.monitor_gain_db,
                "Resumo periódico do teste de microfone"
            );
            self.last_audio_metrics_log_at = Some(Instant::now());
        }
        let microphone_error = microphone.take_microphone_error();
        let monitor_error = microphone.take_monitor_error();
        let audio_warning = microphone.take_audio_warning();
        let clipping_warning = microphone.take_clipping_warning();
        if monitor_error.is_some() {
            microphone.stop_monitoring();
        }

        self.microphone_level = level;
        self.microphone_level_dbfs = level_dbfs;
        if clipping_warning && !self.microphone_clipping_warning {
            tracing::warn!(
                gain_db = self.monitor_gain_db,
                "Retorno local de microfone atingiu limitação digital"
            );
        }
        self.microphone_clipping_warning = clipping_warning;
        if audio_warning {
            if !self.microphone_audio_warning {
                tracing::warn!("Fila de áudio local reportou excesso ou falta de amostras");
            }
            self.microphone_audio_warning = true;
        }
        if let Some(error) = monitor_error {
            tracing::error!(error = %error, "Falha no retorno local de áudio; medidor continua ativo");
            self.microphone_monitor_error = Some(error);
        }
        if let Some(error) = microphone_error {
            tracing::error!(error = %error, "Falha na captura do microfone; teste encerrado");
            self.microphone = None;
            self.microphone_level = 0.0;
            self.microphone_error = Some(error);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compute_microphone_level_zero_rms_is_floor_dbfs() {
        let (dbfs, normalized) = compute_microphone_level(0.0);
        assert_eq!(dbfs, -60.0);
        assert_eq!(normalized, 0.0);
    }

    #[test]
    fn compute_microphone_level_full_scale_rms_is_zero_dbfs() {
        let (dbfs, normalized) = compute_microphone_level(1.0);
        assert_eq!(dbfs, 0.0);
        assert_eq!(normalized, 1.0);
    }

    #[test]
    fn compute_microphone_level_clamps_out_of_range_values() {
        let (dbfs_over, norm_over) = compute_microphone_level(2.0);
        assert_eq!(dbfs_over, 0.0);
        assert_eq!(norm_over, 1.0);

        let (dbfs_under, norm_under) = compute_microphone_level(-0.5);
        assert_eq!(dbfs_under, -60.0);
        assert_eq!(norm_under, 0.0);
    }
}
