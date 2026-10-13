use super::*;
use windows::Win32::Graphics::Dxgi::{
    DXGI_OUTDUPL_POINTER_SHAPE_INFO, DXGI_OUTDUPL_POINTER_SHAPE_TYPE_COLOR,
    DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MASKED_COLOR, DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MONOCHROME,
};

#[derive(Clone, Debug)]
struct DxgiCursorShape {
    width: u32,
    height: u32,
    pitch: u32,
    kind: i32,
    bytes: Vec<u8>,
}

#[derive(Default)]
struct DxgiCursorState {
    position: Option<(i32, i32)>,
    visible: bool,
    shape: Option<DxgiCursorShape>,
    shape_error_logged: bool,
    cpu_fallback_logged: bool,
}

impl DxgiCursorState {
    fn update(&mut self, frame: &windows_capture::dxgi_duplication_api::DxgiDuplicationFrame<'_>) {
        let frame_info = *frame.frame_info();
        if frame_info.PointerShapeBufferSize > 0 {
            let mut bytes = vec![0; frame_info.PointerShapeBufferSize as usize];
            let mut required = 0;
            let mut shape_info = DXGI_OUTDUPL_POINTER_SHAPE_INFO::default();
            let result = unsafe {
                frame.duplication().GetFramePointerShape(
                    bytes.len() as u32,
                    bytes.as_mut_ptr().cast(),
                    &mut required,
                    &mut shape_info,
                )
            };
            match result {
                Ok(()) if required as usize <= bytes.len() => {
                    bytes.truncate(required as usize);
                    self.shape = Some(DxgiCursorShape {
                        width: shape_info.Width,
                        height: shape_info.Height,
                        pitch: shape_info.Pitch,
                        kind: shape_info.Type as i32,
                        bytes,
                    });
                    self.shape_error_logged = false;
                }
                Ok(()) => {
                    self.log_shape_error(
                        "O Windows retornou uma forma de cursor maior que o buffer informado",
                    );
                }
                Err(error) => self.log_shape_error(&format!(
                    "Não foi possível obter a forma do cursor DXGI: {error}"
                )),
            }
        }
        let initial_position = self.position.is_none();
        self.update_position(
            frame_info.LastMouseUpdateTime != 0 || initial_position,
            (
                frame_info.PointerPosition.Position.x,
                frame_info.PointerPosition.Position.y,
            ),
            frame_info.PointerPosition.Visible.as_bool(),
        );
    }

    fn update_position(&mut self, changed: bool, position: (i32, i32), visible: bool) {
        if changed {
            self.position = Some(position);
            self.visible = visible;
        }
    }

    fn log_shape_error(&mut self, message: &str) {
        if !self.shape_error_logged {
            tracing::warn!(
                error = message,
                "A composição do cursor DXGI está indisponível neste quadro"
            );
            self.shape_error_logged = true;
        }
    }

    fn needs_cpu_composition(&self) -> bool {
        self.visible && self.position.is_some() && self.shape.is_some()
    }

    fn composite(&self, bgra: &mut [u8], width: u32, height: u32) -> bool {
        let (Some((x, y)), Some(shape)) = (self.position, self.shape.as_ref()) else {
            return false;
        };
        if !self.visible {
            return false;
        }
        composite_cursor_bgra(bgra, width, height, x, y, shape)
    }
}

fn composite_cursor_bgra(
    bgra: &mut [u8],
    width: u32,
    height: u32,
    cursor_x: i32,
    cursor_y: i32,
    shape: &DxgiCursorShape,
) -> bool {
    if width == 0 || height == 0 || bgra.len() < width as usize * height as usize * 4 {
        return false;
    }
    let kind = shape.kind;
    let monochrome = kind == DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MONOCHROME.0;
    let visual_height = if monochrome {
        shape.height / 2
    } else {
        shape.height
    };
    let min_pitch = if monochrome {
        (shape.width as usize).div_ceil(8)
    } else {
        shape.width as usize * 4
    };
    if shape.width == 0
        || visual_height == 0
        || (shape.pitch as usize) < min_pitch
        || shape.bytes.len() < shape.pitch as usize * shape.height as usize
    {
        return false;
    }
    if !monochrome
        && kind != DXGI_OUTDUPL_POINTER_SHAPE_TYPE_COLOR.0
        && kind != DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MASKED_COLOR.0
    {
        return false;
    }

    for source_y in 0..visual_height {
        let target_y = cursor_y + source_y as i32;
        if !(0..height as i32).contains(&target_y) {
            continue;
        }
        for source_x in 0..shape.width {
            let target_x = cursor_x + source_x as i32;
            if !(0..width as i32).contains(&target_x) {
                continue;
            }
            let target_offset = (target_y as usize * width as usize + target_x as usize) * 4;
            if monochrome {
                let byte_index = source_y as usize * shape.pitch as usize + source_x as usize / 8;
                let bit = 0x80 >> (source_x % 8);
                let and_mask = shape.bytes[byte_index] & bit != 0;
                let xor_offset = (source_y as usize + visual_height as usize)
                    * shape.pitch as usize
                    + source_x as usize / 8;
                let xor_mask = shape.bytes[xor_offset] & bit != 0;
                for channel in 0..3 {
                    bgra[target_offset + channel] = (bgra[target_offset + channel]
                        & if and_mask { 0xff } else { 0 })
                        ^ if xor_mask { 0xff } else { 0 };
                }
                bgra[target_offset + 3] = 0xff;
                continue;
            }

            let source_offset = source_y as usize * shape.pitch as usize + source_x as usize * 4;
            let source = &shape.bytes[source_offset..source_offset + 4];
            if kind == DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MASKED_COLOR.0 && source[3] == 0 {
                for channel in 0..3 {
                    bgra[target_offset + channel] ^= source[channel];
                }
                bgra[target_offset + 3] = 0xff;
                continue;
            }
            let alpha = source[3] as u16;
            for channel in 0..3 {
                let destination = bgra[target_offset + channel] as u16;
                bgra[target_offset + channel] = (source[channel] as u16
                    + (destination * (255 - alpha) + 127) / 255)
                    .min(255) as u8;
            }
            bgra[target_offset + 3] = 0xff;
        }
    }
    true
}

#[allow(clippy::too_many_arguments)] // Capture owns the independent preview, shutdown and quality controls.
pub(super) fn capture_dxgi_monitor(
    monitor: Monitor,
    context: egui::Context,
    latest_frame: LatestFrame,
    performance: Arc<CapturePerformanceCounters>,
    fallback_reason: Arc<Mutex<Option<String>>>,
    stop: Arc<AtomicBool>,
    preview_enabled: Arc<AtomicBool>,
    quality: ScreenShareQuality,
) -> Result<(), String> {
    let mut duplication = DxgiDuplicationApi::new_options(monitor, &[DxgiDuplicationFormat::Bgra8])
        .map_err(|error| format!("DXGI Desktop Duplication nÃ£o iniciou: {error}"))?;
    let mut sequence = 0u64;
    let mut limiter = FrameRateLimiter::new(quality);
    let mut scratch = Vec::new();
    let mut gpu_processor: Option<(crate::mf_video::GpuNv12Processor, (u32, u32, u32, u32))> = None;
    let mut gpu_processor_attempted: Option<(u32, u32, u32, u32)> = None;
    let mut gpu_fallback_logged = false;
    let mut gpu_preview_fallback_logged = false;
    let mut staging_texture = None;
    let mut cursor = DxgiCursorState::default();
    let mut last_color_path = None;

    while !stop.load(Ordering::Relaxed) {
        let mut frame = match duplication.acquire_next_frame(100) {
            Ok(frame) => frame,
            Err(windows_capture::dxgi_duplication_api::Error::Timeout) => continue,
            Err(windows_capture::dxgi_duplication_api::Error::AccessLost) => {
                tracing::warn!("DXGI perdeu acesso ao monitor; recriando a duplicação");
                gpu_processor = None;
                gpu_processor_attempted = None;
                staging_texture = None;
                cursor = DxgiCursorState::default();
                *fallback_reason
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
                duplication = duplication
                    .recreate_options(&[DxgiDuplicationFormat::Bgra8])
                    .map_err(|error| {
                        format!("DXGI nÃ£o recriou a captura apÃ³s mudanÃ§a de tela: {error}")
                    })?;
                limiter = FrameRateLimiter::new(quality);
                continue;
            }
            Err(error) => return Err(format!("Falha ao capturar monitor via DXGI: {error}")),
        };
        cursor.update(&frame);
        let compose_cursor = cursor.needs_cpu_composition();
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
        let (out_width, out_height) = scaled_dimensions(width, height, quality);
        let dimensions = (width, height, out_width, out_height);
        if compose_cursor {
            gpu_processor = None;
            gpu_processor_attempted = None;
            if !cursor.cpu_fallback_logged {
                tracing::warn!(
                    "O DXGI informou um cursor separado; usando composição de cursor no caminho de captura CPU"
                );
                cursor.cpu_fallback_logged = true;
            }
        } else if gpu_processor_attempted != Some(dimensions) {
            gpu_processor_attempted = Some(dimensions);
            match crate::mf_video::GpuNv12Processor::new(
                frame.device(),
                frame.device_context(),
                width,
                height,
                out_width,
                out_height,
                quality.frames_per_second(),
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
        let gpu_result = (!compose_cursor)
            .then(|| gpu_processor.as_mut())
            .flatten()
            .map(|(processor, _)| {
                performance
                    .pipeline
                    .record_path(crate::video_performance::FramePath::CaptureGpuNv12);
                let _timer = performance
                    .pipeline
                    .timer(crate::video_performance::Stage::CaptureGpuConvert);
                processor.process(frame.texture())
            });
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
                let result = {
                    let _timer = performance
                        .pipeline
                        .timer(crate::video_performance::Stage::CaptureReadback);
                    surface.readback_nv12_into(&mut staging_texture)
                };
                match result {
                    Ok((nv12, stride)) => {
                        performance.pipeline.record_readback(
                            crate::video_performance::Readback::Capture,
                            nv12.len(),
                        );
                        readback_nanos += started_at.elapsed().as_nanos() as u64;
                        let cpu_nv12 = Some(Arc::new(CpuNv12Frame {
                            bytes: Arc::new(nv12),
                            stride,
                        }));
                        let started_at = Instant::now();
                        let result = {
                            let _timer = performance
                                .pipeline
                                .timer(crate::video_performance::Stage::CaptureCpuConvert);
                            surface.to_rgba(
                                &cpu_nv12.as_ref().expect("NV12 frame was created").bytes,
                                stride,
                            )
                        };
                        match result {
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
                let reading = performance
                    .pipeline
                    .timer(crate::video_performance::Stage::CaptureReadback);
                let buffer = frame.buffer().map_err(|error| {
                    format!("DXGI nÃ£o conseguiu ler o quadro do monitor: {error}")
                })?;
                let bytes = buffer.as_nopadding_buffer(&mut scratch);
                drop(reading);
                performance
                    .pipeline
                    .record_readback(crate::video_performance::Readback::Capture, bytes.len());
                performance
                    .pipeline
                    .record_path(crate::video_performance::FramePath::CaptureCpuRgba);
                readback_nanos += started_at.elapsed().as_nanos() as u64;
                let started_at = Instant::now();
                let conversion = performance
                    .pipeline
                    .timer(crate::video_performance::Stage::CaptureCpuConvert);
                let mut composited = None;
                if compose_cursor {
                    let mut owned = bytes.to_vec();
                    if cursor.composite(&mut owned, width, height) {
                        composited = Some(owned);
                    }
                }
                let pixels = composited.as_deref().unwrap_or(bytes);
                let preview = downsample_bgra(pixels, width, height, next_sequence, quality);
                drop(conversion);
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
        let color_path = if preview.gpu_nv12.is_some() {
            "gpu_nv12"
        } else {
            "cpu_rgba"
        };
        if last_color_path != Some(color_path) {
            tracing::info!(
                backend = color_path,
                cursor_composed = compose_cursor,
                color_contract = "BT.601 limited; opaque desktop",
                "Caminho de captura de vídeo alterado"
            );
            last_color_path = Some(color_path);
        }
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

fn downsample_bgra(
    bytes: &[u8],
    width: u32,
    height: u32,
    sequence: u64,
    quality: ScreenShareQuality,
) -> PreviewFrame {
    let (out_width, out_height) = scaled_dimensions(width, height, quality);
    let mut rgba = Vec::with_capacity((out_width * out_height * 4) as usize);
    for y in 0..out_height {
        let source_y = (y * height / out_height).min(height - 1);
        for x in 0..out_width {
            let source_x = (x * width / out_width).min(width - 1);
            let offset = ((source_y * width + source_x) * 4) as usize;
            if let Some(pixel) = bytes.get(offset..offset + 4) {
                rgba.extend_from_slice(&[pixel[2], pixel[1], pixel[0], 255]);
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

#[cfg(test)]
mod cursor_tests {
    use super::{DxgiCursorShape, DxgiCursorState, composite_cursor_bgra};
    use windows::Win32::Graphics::Dxgi::DXGI_OUTDUPL_POINTER_SHAPE_TYPE_COLOR;

    fn color_shape(color: [u8; 4]) -> DxgiCursorShape {
        DxgiCursorShape {
            width: 1,
            height: 1,
            pitch: 4,
            kind: DXGI_OUTDUPL_POINTER_SHAPE_TYPE_COLOR.0,
            bytes: color.to_vec(),
        }
    }

    #[test]
    fn cursor_is_composited_at_the_current_position_and_can_remain_stationary() {
        let shape = color_shape([0, 0, 255, 255]);
        let mut frame = [0, 0, 0, 255].repeat(4);
        assert!(composite_cursor_bgra(&mut frame, 2, 2, 1, 0, &shape));
        assert_eq!(&frame[4..8], &[0, 0, 255, 255]);

        let mut next_frame = [0, 0, 0, 255].repeat(4);
        assert!(composite_cursor_bgra(&mut next_frame, 2, 2, 1, 0, &shape));
        assert_eq!(&next_frame[4..8], &[0, 0, 255, 255]);
    }

    #[test]
    fn cursor_movement_updates_its_location_without_leaving_the_previous_image() {
        let shape = color_shape([0, 0, 255, 255]);
        let mut first_frame = [0, 0, 0, 255].repeat(2);
        assert!(composite_cursor_bgra(&mut first_frame, 2, 1, 0, 0, &shape));
        assert_eq!(&first_frame[..4], &[0, 0, 255, 255]);

        let mut moved_frame = [0, 0, 0, 255].repeat(2);
        assert!(composite_cursor_bgra(&mut moved_frame, 2, 1, 1, 0, &shape));
        assert_eq!(&moved_frame[..4], &[0, 0, 0, 255]);
        assert_eq!(&moved_frame[4..8], &[0, 0, 255, 255]);
    }

    #[test]
    fn cursor_shape_change_and_negative_position_are_supported() {
        let mut frame = [0, 0, 0, 255].repeat(4);
        let changed_shape = DxgiCursorShape {
            width: 2,
            height: 1,
            pitch: 8,
            kind: DXGI_OUTDUPL_POINTER_SHAPE_TYPE_COLOR.0,
            bytes: vec![255, 0, 0, 255, 255, 0, 0, 255],
        };
        assert!(composite_cursor_bgra(
            &mut frame,
            2,
            2,
            -1,
            1,
            &changed_shape
        ));
        assert_eq!(&frame[8..12], &[255, 0, 0, 255]);
        assert_eq!(&frame[12..16], &[0, 0, 0, 255]);

        let mut changed = [0, 0, 0, 255].repeat(4);
        let changed_shape = color_shape([0, 255, 0, 255]);
        assert!(composite_cursor_bgra(
            &mut changed,
            2,
            2,
            0,
            0,
            &changed_shape
        ));
        assert_eq!(&changed[..4], &[0, 255, 0, 255]);
    }

    #[test]
    fn hidden_or_out_of_bounds_cursor_does_not_modify_the_frame() {
        let shape = color_shape([0, 0, 255, 255]);
        let mut frame = vec![0, 0, 0, 255];
        assert!(composite_cursor_bgra(&mut frame, 1, 1, 1, 1, &shape));
        assert_eq!(frame, vec![0, 0, 0, 255]);

        let mut malformed = shape.clone();
        malformed.pitch = 3;
        assert!(!composite_cursor_bgra(&mut frame, 1, 1, 0, 0, &malformed));
        assert_eq!(frame, vec![0, 0, 0, 255]);
    }

    #[test]
    fn monochrome_cursor_applies_and_then_xor_masks() {
        use windows::Win32::Graphics::Dxgi::DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MONOCHROME;
        let shape = DxgiCursorShape {
            width: 2,
            height: 2,
            pitch: 1,
            kind: DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MONOCHROME.0,
            bytes: vec![0b1100_0000, 0b0100_0000],
        };
        let mut frame = vec![20, 40, 60, 255, 20, 40, 60, 255];
        assert!(composite_cursor_bgra(&mut frame, 2, 1, 0, 0, &shape));
        assert_eq!(&frame[..4], &[20, 40, 60, 255]);
        assert_eq!(&frame[4..8], &[!20, !40, !60, 255]);
    }

    #[test]
    fn masked_color_cursor_xors_destination_when_alpha_is_zero() {
        use windows::Win32::Graphics::Dxgi::DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MASKED_COLOR;
        let shape = DxgiCursorShape {
            width: 1,
            height: 1,
            pitch: 4,
            kind: DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MASKED_COLOR.0,
            bytes: vec![0x0f, 0x33, 0x55, 0],
        };
        let mut frame = vec![0xf0, 0x0f, 0xaa, 255];
        assert!(composite_cursor_bgra(&mut frame, 1, 1, 0, 0, &shape));
        assert_eq!(frame, vec![0xff, 0x3c, 0xff, 255]);
    }

    #[test]
    fn stationary_cursor_keeps_its_last_position_when_frame_has_no_mouse_update() {
        let mut cursor = DxgiCursorState::default();
        cursor.update_position(true, (12, 34), true);
        cursor.update_position(false, (90, 80), false);

        assert_eq!(cursor.position, Some((12, 34)));
        assert!(cursor.visible);
    }

    #[test]
    fn hidden_cursor_is_not_composited() {
        let mut cursor = DxgiCursorState {
            position: Some((0, 0)),
            visible: true,
            shape: Some(color_shape([0, 0, 255, 255])),
            ..Default::default()
        };
        let mut visible_frame = vec![0, 0, 0, 255];
        assert!(cursor.composite(&mut visible_frame, 1, 1));
        assert_eq!(visible_frame, vec![0, 0, 255, 255]);

        cursor.update_position(true, (0, 0), false);
        let mut hidden_frame = vec![0, 0, 0, 255];
        assert!(!cursor.composite(&mut hidden_frame, 1, 1));
        assert_eq!(hidden_frame, vec![0, 0, 0, 255]);
    }
}

#[cfg(test)]
mod brightness_tests {
    use super::*;

    #[test]
    fn cursor_and_capture_alpha_do_not_change_background_brightness() {
        let base = [40, 100, 180, 0].repeat(16);
        for alpha in [0, 96, 255] {
            let mut bytes = base.clone();
            for pixel in bytes.chunks_exact_mut(4) {
                pixel[3] = alpha;
            }
            let shape = DxgiCursorShape {
                width: 1,
                height: 1,
                pitch: 4,
                kind: DXGI_OUTDUPL_POINTER_SHAPE_TYPE_COLOR.0,
                bytes: vec![0, 0, 255, 255],
            };
            assert!(composite_cursor_bgra(&mut bytes, 4, 4, 0, 0, &shape));
            let preview = downsample_bgra(&bytes, 4, 4, 1, ScreenShareQuality::default());
            assert!(
                preview.rgba[4..]
                    .chunks_exact(4)
                    .all(|pixel| pixel == [180, 100, 40, 255])
            );
        }
    }
}
