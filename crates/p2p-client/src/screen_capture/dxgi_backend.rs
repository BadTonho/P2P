use super::*;

pub(super) fn capture_dxgi_monitor(
    monitor: Monitor,
    context: egui::Context,
    latest_frame: LatestFrame,
    performance: Arc<CapturePerformanceCounters>,
    fallback_reason: Arc<Mutex<Option<String>>>,
    stop: Arc<AtomicBool>,
    preview_enabled: Arc<AtomicBool>,
) -> Result<(), String> {
    let mut duplication = DxgiDuplicationApi::new_options(monitor, &[DxgiDuplicationFormat::Bgra8])
        .map_err(|error| format!("DXGI Desktop Duplication nÃ£o iniciou: {error}"))?;
    let mut sequence = 0u64;
    let mut limiter = FrameRateLimiter::default();
    let mut scratch = Vec::new();
    let mut gpu_processor: Option<(crate::mf_video::GpuNv12Processor, (u32, u32, u32, u32))> = None;
    let mut gpu_processor_attempted: Option<(u32, u32, u32, u32)> = None;
    let mut gpu_fallback_logged = false;
    let mut gpu_preview_fallback_logged = false;
    let mut staging_texture = None;

    while !stop.load(Ordering::Relaxed) {
        let mut frame = match duplication.acquire_next_frame(100) {
            Ok(frame) => frame,
            Err(windows_capture::dxgi_duplication_api::Error::Timeout) => continue,
            Err(windows_capture::dxgi_duplication_api::Error::AccessLost) => {
                tracing::warn!("DXGI perdeu acesso ao monitor; recriando a duplicação");
                gpu_processor = None;
                gpu_processor_attempted = None;
                staging_texture = None;
                *fallback_reason
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
                duplication = duplication
                    .recreate_options(&[DxgiDuplicationFormat::Bgra8])
                    .map_err(|error| {
                        format!("DXGI nÃ£o recriou a captura apÃ³s mudanÃ§a de tela: {error}")
                    })?;
                limiter = FrameRateLimiter::default();
                continue;
            }
            Err(error) => return Err(format!("Falha ao capturar monitor via DXGI: {error}")),
        };
        performance.received_frames.fetch_add(1, Ordering::Relaxed);
        if !limiter.should_process(Instant::now()) {
            performance.skipped_frames.fetch_add(1, Ordering::Relaxed);
            continue;
        }
        let width = frame.width();
        let height = frame.height();
        if width == 0 || height == 0 {
            continue;
        }
        let (out_width, out_height) = scaled_dimensions(width, height);
        let dimensions = (width, height, out_width, out_height);
        if gpu_processor_attempted != Some(dimensions) {
            gpu_processor_attempted = Some(dimensions);
            match crate::mf_video::GpuNv12Processor::new(
                frame.device(),
                frame.device_context(),
                width,
                height,
                out_width,
                out_height,
            ) {
                Ok(processor) => {
                    tracing::info!(
                        width,
                        height,
                        output_width = out_width,
                        output_height = out_height,
                        "Conversor DXGI GPU para NV12 ativado"
                    );
                    gpu_processor = Some((processor, dimensions));
                    gpu_fallback_logged = false;
                    *fallback_reason
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
                }
                Err(error) => {
                    tracing::warn!(error = %error, "Conversão DXGI para NV12 na GPU indisponível; encoder usará o caminho atual por CPU");
                    gpu_processor = None;
                    gpu_fallback_logged = true;
                    *fallback_reason
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(error);
                }
            }
        }
        let gpu_convert_started_at = Instant::now();
        let gpu_result = gpu_processor
            .as_mut()
            .map(|(processor, _)| processor.process(frame.texture()));
        performance.gpu_convert_nanos.fetch_add(
            gpu_convert_started_at.elapsed().as_nanos() as u64,
            Ordering::Relaxed,
        );
        let gpu_surface = match gpu_result {
            Some(Ok(surface)) => Some(Arc::new(surface)),
            Some(Err(error)) => {
                if !gpu_fallback_logged {
                    tracing::warn!(error = %error, "Falha no processamento GPU do quadro DXGI; voltando ao caminho de CPU");
                    gpu_fallback_logged = true;
                }
                gpu_processor = None;
                *fallback_reason
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(error);
                None
            }
            None => None,
        };
        let next_sequence = sequence.wrapping_add(1);
        let wants_preview = preview_enabled.load(Ordering::Relaxed);
        let mut readback_nanos = 0;
        let mut resize_nanos = 0;
        let (preview_rgba, cpu_nv12) = if let Some(surface) = gpu_surface.as_ref() {
            if !wants_preview {
                gpu_preview_fallback_logged = false;
                let mut reason = fallback_reason
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if reason
                    .as_deref()
                    .is_some_and(|value| value.starts_with("Prévia GPU:"))
                {
                    *reason = None;
                }
                (None, None)
            } else {
                let started_at = Instant::now();
                match surface.readback_nv12_into(&mut staging_texture) {
                    Ok((nv12, stride)) => {
                        readback_nanos += started_at.elapsed().as_nanos() as u64;
                        let cpu_nv12 = Some(Arc::new(CpuNv12Frame {
                            bytes: Arc::new(nv12),
                            stride,
                        }));
                        let started_at = Instant::now();
                        match surface.to_rgba(
                            &cpu_nv12.as_ref().expect("NV12 frame was created").bytes,
                            stride,
                        ) {
                            Ok(rgba) => {
                                resize_nanos += started_at.elapsed().as_nanos() as u64;
                                gpu_preview_fallback_logged = false;
                                let mut reason = fallback_reason
                                    .lock()
                                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                                if reason
                                    .as_deref()
                                    .is_some_and(|value| value.starts_with("Prévia GPU:"))
                                {
                                    *reason = None;
                                }
                                (Some(rgba), cpu_nv12)
                            }
                            Err(error) => {
                                resize_nanos += started_at.elapsed().as_nanos() as u64;
                                if !gpu_preview_fallback_logged {
                                    tracing::warn!(error = %error, "Falha ao converter a prévia NV12; usando cópia BGRA do DXGI");
                                    gpu_preview_fallback_logged = true;
                                }
                                *fallback_reason
                                    .lock()
                                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(
                                    format!(
                                        "Prévia GPU: falha na conversão para a imagem local: {error}"
                                    ),
                                );
                                (None, cpu_nv12)
                            }
                        }
                    }
                    Err(error) => {
                        readback_nanos += started_at.elapsed().as_nanos() as u64;
                        if !gpu_preview_fallback_logged {
                            tracing::warn!(error = %error, "Falha ao ler a superfície NV12 reduzida; usando cópia BGRA do DXGI");
                            gpu_preview_fallback_logged = true;
                        }
                        *fallback_reason
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(format!(
                            "Prévia GPU: falha na leitura da imagem reduzida: {error}"
                        ));
                        (None, None)
                    }
                }
            }
        } else {
            (None, None)
        };
        let rgba = match preview_rgba {
            Some(rgba) => rgba,
            None if !wants_preview && (cpu_nv12.is_some() || gpu_surface.is_some()) => Vec::new(),
            None => {
                let started_at = Instant::now();
                let buffer = frame.buffer().map_err(|error| {
                    format!("DXGI nÃ£o conseguiu ler o quadro do monitor: {error}")
                })?;
                let bytes = buffer.as_nopadding_buffer(&mut scratch);
                readback_nanos += started_at.elapsed().as_nanos() as u64;
                let started_at = Instant::now();
                let preview = downsample_bgra(bytes, width, height, next_sequence);
                resize_nanos += started_at.elapsed().as_nanos() as u64;
                preview.rgba
            }
        };
        let mut preview = PreviewFrame {
            sequence: next_sequence,
            width: out_width,
            height: out_height,
            rgba,
            #[cfg(windows)]
            gpu_nv12: None,
            #[cfg(windows)]
            cpu_nv12,
        };
        preview.gpu_nv12 = gpu_surface;
        // DXGI surfaces are copied to NV12 CPU memory already to make the local
        // preview. Keep that readback alongside the latest frame so MF can use
        // it if a D3D11 surface input is rejected, without converting RGBA again.
        performance
            .readback_nanos
            .fetch_add(readback_nanos, Ordering::Relaxed);
        performance
            .resize_nanos
            .fetch_add(resize_nanos, Ordering::Relaxed);
        if same_preview_pixels(latest_frame.latest().as_deref(), &preview) {
            performance.unchanged_frames.fetch_add(1, Ordering::Relaxed);
            continue;
        }
        sequence = next_sequence;
        latest_frame.publish(preview);
        performance.processed_frames.fetch_add(1, Ordering::Relaxed);
        context.request_repaint();
    }
    Ok(())
}

fn downsample_bgra(bytes: &[u8], width: u32, height: u32, sequence: u64) -> PreviewFrame {
    let (out_width, out_height) = scaled_dimensions(width, height);
    let mut rgba = Vec::with_capacity((out_width * out_height * 4) as usize);
    for y in 0..out_height {
        let source_y = (y * height / out_height).min(height - 1);
        for x in 0..out_width {
            let source_x = (x * width / out_width).min(width - 1);
            let offset = ((source_y * width + source_x) * 4) as usize;
            if let Some(pixel) = bytes.get(offset..offset + 4) {
                rgba.extend_from_slice(&[pixel[2], pixel[1], pixel[0], pixel[3]]);
            } else {
                rgba.extend_from_slice(&[0, 0, 0, 255]);
            }
        }
    }
    PreviewFrame {
        sequence,
        width: out_width,
        height: out_height,
        rgba,
        #[cfg(windows)]
        gpu_nv12: None,
        #[cfg(windows)]
        cpu_nv12: None,
    }
}
