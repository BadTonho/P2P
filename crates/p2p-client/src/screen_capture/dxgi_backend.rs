use super::*;
use windows::Win32::Foundation::E_ACCESSDENIED;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_RENDER_TARGET, D3D11_BOX, D3D11_CPU_ACCESS_READ, D3D11_MAP_READ,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, D3D11_USAGE_STAGING, ID3D11Device,
    ID3D11DeviceContext, ID3D11Resource, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{
    DXGI_ERROR_ACCESS_DENIED, DXGI_ERROR_ACCESS_LOST, DXGI_ERROR_SESSION_DISCONNECTED,
    DXGI_ERROR_UNSUPPORTED, DXGI_OUTDUPL_POINTER_SHAPE_INFO, DXGI_OUTDUPL_POINTER_SHAPE_TYPE_COLOR,
    DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MASKED_COLOR, DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MONOCHROME,
};
use windows::core::Interface;
use windows_capture::dxgi_duplication_api::Error as DxgiCaptureError;

const DXGI_RECOVERY_RETRY_INTERVAL: Duration = Duration::from_millis(250);

fn is_transient_desktop_error(error: &DxgiCaptureError) -> bool {
    let code = match error {
        DxgiCaptureError::AccessLost => return true,
        DxgiCaptureError::WindowsError(error)
        | DxgiCaptureError::DirectXError(windows_capture::d3d11::Error::WindowsError(error)) => {
            error.code()
        }
        _ => return false,
    };
    matches!(
        code,
        E_ACCESSDENIED
            | DXGI_ERROR_ACCESS_DENIED
            | DXGI_ERROR_ACCESS_LOST
            | DXGI_ERROR_SESSION_DISCONNECTED
            | DXGI_ERROR_UNSUPPORTED
    )
}

fn retry_dxgi_recreation<T>(
    stop: &AtomicBool,
    mut recreate: impl FnMut() -> Result<T, DxgiCaptureError>,
    mut wait: impl FnMut(),
) -> Result<Option<T>, DxgiCaptureError> {
    // UAC can keep the secure desktop active for an arbitrary amount of time.
    // A transient denial must not finish the capture worker and tear down WebRTC.
    while !stop.load(Ordering::Relaxed) {
        match recreate() {
            Ok(duplication) => return Ok(Some(duplication)),
            Err(error) if is_transient_desktop_error(&error) => wait(),
            Err(error) => return Err(error),
        }
    }
    Ok(None)
}

fn wait_for_dxgi_recovery(stop: &AtomicBool) {
    let deadline = Instant::now() + DXGI_RECOVERY_RETRY_INTERVAL;
    while !stop.load(Ordering::Relaxed) {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        thread::sleep(remaining.min(Duration::from_millis(25)));
    }
}

#[derive(Clone, Debug)]
struct DxgiCursorShape {
    width: u32,
    height: u32,
    pitch: u32,
    kind: i32,
    bytes: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CursorPatchRect {
    left: u32,
    top: u32,
    width: u32,
    height: u32,
    local_cursor_x: i32,
    local_cursor_y: i32,
}

#[derive(Default)]
struct DxgiCursorState {
    position: Option<(i32, i32)>,
    visible: bool,
    shape: Option<DxgiCursorShape>,
    shape_error_logged: bool,
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

fn validate_cursor_shape(shape: &DxgiCursorShape) -> Option<(bool, u32)> {
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
        return None;
    }
    if !monochrome
        && kind != DXGI_OUTDUPL_POINTER_SHAPE_TYPE_COLOR.0
        && kind != DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MASKED_COLOR.0
    {
        return None;
    }
    Some((monochrome, visual_height))
}

fn cursor_patch_rect(
    width: u32,
    height: u32,
    cursor_x: i32,
    cursor_y: i32,
    shape: &DxgiCursorShape,
) -> Option<CursorPatchRect> {
    if width == 0 || height == 0 {
        return None;
    }
    let (_, visual_height) = validate_cursor_shape(shape)?;
    let frame_w = i64::from(width);
    let frame_h = i64::from(height);
    let cx = i64::from(cursor_x);
    let cy = i64::from(cursor_y);
    let left = cx.clamp(0, frame_w);
    let top = cy.clamp(0, frame_h);
    let right = (cx + i64::from(shape.width)).clamp(0, frame_w);
    let bottom = (cy + i64::from(visual_height)).clamp(0, frame_h);
    if right <= left || bottom <= top {
        return None;
    }
    Some(CursorPatchRect {
        left: left as u32,
        top: top as u32,
        width: (right - left) as u32,
        height: (bottom - top) as u32,
        local_cursor_x: (cx - left) as i32,
        local_cursor_y: (cy - top) as i32,
    })
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
    let Some((monochrome, visual_height)) = validate_cursor_shape(shape) else {
        return false;
    };
    let kind = shape.kind;

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

#[allow(clippy::too_many_arguments)] // Keeps reusable GPU canvas and staging textures explicit.
fn composite_cursor_on_gpu_texture<'a>(
    device: &ID3D11Device,
    context: &ID3D11DeviceContext,
    source_texture: &'a ID3D11Texture2D,
    width: u32,
    height: u32,
    cursor: &DxgiCursorState,
    gpu_canvas: &'a mut Option<ID3D11Texture2D>,
    patch_staging: &mut Option<ID3D11Texture2D>,
) -> Result<&'a ID3D11Texture2D, String> {
    if !cursor.needs_cpu_composition() {
        return Ok(source_texture);
    }
    let (Some((cursor_x, cursor_y)), Some(shape)) = (cursor.position, cursor.shape.as_ref()) else {
        return Ok(source_texture);
    };
    let Some(patch) = cursor_patch_rect(width, height, cursor_x, cursor_y, shape) else {
        return Ok(source_texture);
    };

    let needs_new_canvas = match gpu_canvas.as_ref() {
        Some(existing) => {
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            unsafe { existing.GetDesc(&mut desc) };
            desc.Width != width
                || desc.Height != height
                || desc.Format != DXGI_FORMAT_B8G8R8A8_UNORM
        }
        None => true,
    };
    if needs_new_canvas {
        let canvas_desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let mut created = None;
        unsafe {
            device
                .CreateTexture2D(&canvas_desc, None, Some(&mut created))
                .map_err(|error| {
                    format!("Não foi possível criar textura GPU para o cursor DXGI: {error}")
                })?;
        }
        *gpu_canvas = created;
    }

    let needs_new_staging = match patch_staging.as_ref() {
        Some(existing) => {
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            unsafe { existing.GetDesc(&mut desc) };
            desc.Width < patch.width
                || desc.Height < patch.height
                || desc.Format != DXGI_FORMAT_B8G8R8A8_UNORM
        }
        None => true,
    };
    if needs_new_staging {
        let staging_desc = D3D11_TEXTURE2D_DESC {
            Width: patch.width.max(64),
            Height: patch.height.max(64),
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_STAGING,
            BindFlags: 0,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            MiscFlags: 0,
        };
        let mut created = None;
        unsafe {
            device
                .CreateTexture2D(&staging_desc, None, Some(&mut created))
                .map_err(|error| {
                    format!("Não foi possível criar staging do cursor DXGI: {error}")
                })?;
        }
        *patch_staging = created;
    }

    let canvas_ref = gpu_canvas
        .as_ref()
        .ok_or_else(|| "Textura GPU do cursor DXGI ausente.".to_owned())?;
    let staging_ref = patch_staging
        .as_ref()
        .ok_or_else(|| "Textura staging do cursor DXGI ausente.".to_owned())?;
    let source_res: ID3D11Resource = source_texture
        .cast()
        .map_err(|error| format!("Não foi possível acessar textura de origem DXGI: {error}"))?;
    let canvas_res: ID3D11Resource = canvas_ref
        .cast()
        .map_err(|error| format!("Não foi possível acessar textura destino do cursor: {error}"))?;
    let staging_res: ID3D11Resource = staging_ref
        .cast()
        .map_err(|error| format!("Não foi possível acessar staging do cursor: {error}"))?;

    let region = D3D11_BOX {
        left: patch.left,
        top: patch.top,
        front: 0,
        right: patch.left + patch.width,
        bottom: patch.top + patch.height,
        back: 1,
    };

    unsafe {
        context.CopyResource(&canvas_res, &source_res);
        context.CopySubresourceRegion(&staging_res, 0, 0, 0, 0, &source_res, 0, Some(&region));
    }

    let row_bytes = patch.width as usize * 4;
    let mut patch_bgra = vec![0u8; row_bytes * patch.height as usize];
    let mut mapped = Default::default();
    unsafe {
        context
            .Map(staging_ref, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
            .map_err(|error| format!("Não foi possível mapear região do cursor DXGI: {error}"))?;
        let copy_result = (|| {
            let pitch = mapped.RowPitch as usize;
            if mapped.pData.is_null() || pitch < row_bytes {
                return Err("Leitura inválida da região do cursor DXGI.".to_owned());
            }
            let base = mapped.pData.cast::<u8>();
            for row in 0..patch.height as usize {
                let src_row = std::slice::from_raw_parts(base.add(row * pitch), row_bytes);
                let dst_start = row * row_bytes;
                patch_bgra[dst_start..dst_start + row_bytes].copy_from_slice(src_row);
            }
            Ok(())
        })();
        context.Unmap(staging_ref, 0);
        copy_result?;
    }

    if !composite_cursor_bgra(
        &mut patch_bgra,
        patch.width,
        patch.height,
        patch.local_cursor_x,
        patch.local_cursor_y,
        shape,
    ) {
        return Ok(source_texture);
    }

    unsafe {
        context.UpdateSubresource(
            &canvas_res,
            0,
            Some(&region),
            patch_bgra.as_ptr().cast(),
            patch.width * 4,
            0,
        );
    }
    Ok(canvas_ref)
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
    let Some(mut duplication) = retry_dxgi_recreation(
        &stop,
        || DxgiDuplicationApi::new_options(monitor, &[DxgiDuplicationFormat::Bgra8]),
        || wait_for_dxgi_recovery(&stop),
    )
    .map_err(|error| format!("DXGI Desktop Duplication não iniciou: {error}"))?
    else {
        return Ok(());
    };
    let mut sequence = 0u64;
    let mut limiter = FrameRateLimiter::new(quality);
    let mut local_preview_limiter = LocalPreviewLimiter::default();
    let mut scratch = Vec::new();
    let mut gpu_processor: Option<(crate::mf_video::GpuNv12Processor, (u32, u32, u32, u32))> = None;
    let mut gpu_processor_attempted: Option<(u32, u32, u32, u32)> = None;
    let mut gpu_preview_fallback_logged = false;
    let mut staging_texture = None;
    let mut cursor_gpu_canvas = None;
    let mut cursor_patch_staging = None;
    let mut cursor = DxgiCursorState::default();
    let mut last_color_path = None;

    while !stop.load(Ordering::Relaxed) {
        let mut frame = match duplication.acquire_next_frame(100) {
            Ok(frame) => frame,
            Err(windows_capture::dxgi_duplication_api::Error::Timeout) => continue,
            Err(error) if is_transient_desktop_error(&error) => {
                tracing::warn!(error = %error, "Captura DXGI pausada pelo Windows; aguardando acesso à área de trabalho sem encerrar o compartilhamento");
                gpu_processor = None;
                gpu_processor_attempted = None;
                staging_texture = None;
                cursor_gpu_canvas = None;
                cursor_patch_staging = None;
                cursor = DxgiCursorState::default();
                *fallback_reason
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
                // recreate_options consumes the old interface even on failure.
                // Keep its device when the first attempt succeeds; otherwise a
                // fresh factory allows retrying after the protected desktop closes.
                let mut previous = Some(duplication);
                let replacement = retry_dxgi_recreation(
                    &stop,
                    || match previous.take() {
                        Some(previous) => {
                            previous.recreate_options(&[DxgiDuplicationFormat::Bgra8])
                        }
                        None => DxgiDuplicationApi::new_options(
                            monitor,
                            &[DxgiDuplicationFormat::Bgra8],
                        ),
                    },
                    || wait_for_dxgi_recovery(&stop),
                )
                .map_err(|error| {
                    format!("DXGI não recriou a captura após mudança de tela: {error}")
                })?;
                let Some(replacement) = replacement else {
                    return Ok(());
                };
                duplication = replacement;
                limiter = FrameRateLimiter::new(quality);
                tracing::info!("Captura DXGI retomada após mudança da área de trabalho");
                context.request_repaint();
                continue;
            }
            Err(error) => return Err(format!("Falha ao capturar monitor via DXGI: {error}")),
        };
        let frame_info = *frame.frame_info();
        cursor.update(&frame);
        let compose_cursor = cursor.needs_cpu_composition();
        performance.received_frames.fetch_add(1, Ordering::Relaxed);
        // Desktop Duplication can return a frame at the monitor refresh rate
        // even when neither desktop pixels nor cursor changed. Avoid GPU
        // conversion, texture allocation, readback, and encoding in that case.
        if sequence != 0 && frame_info.LastPresentTime == 0 && frame_info.LastMouseUpdateTime == 0 {
            performance.unchanged_frames.fetch_add(1, Ordering::Relaxed);
            continue;
        }
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
        if gpu_processor_attempted != Some(dimensions) {
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
                    *fallback_reason
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
                }
                Err(error) => {
                    *fallback_reason
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(error.clone());
                    return Err(format!(
                        "Inicialização do conversor DXGI/NV12 na GPU falhou; fallback por CPU está desativado: {error}"
                    ));
                }
            }
        }
        let gpu_convert_started_at = Instant::now();
        let gpu_result = gpu_processor.as_mut().map(|(processor, _)| {
            performance
                .pipeline
                .record_path(crate::video_performance::FramePath::CaptureGpuNv12);
            let _timer = performance
                .pipeline
                .timer(crate::video_performance::Stage::CaptureGpuConvert);
            let source_texture = composite_cursor_on_gpu_texture(
                frame.device(),
                frame.device_context(),
                frame.texture(),
                width,
                height,
                &cursor,
                &mut cursor_gpu_canvas,
                &mut cursor_patch_staging,
            )?;
            processor.process(source_texture)
        });
        performance.gpu_convert_nanos.fetch_add(
            gpu_convert_started_at.elapsed().as_nanos() as u64,
            Ordering::Relaxed,
        );
        let gpu_surface = match gpu_result {
            Some(Ok(surface)) => Some(Arc::new(surface)),
            Some(Err(error)) => {
                if compose_cursor {
                    tracing::warn!(
                        error = %error,
                        "A composição GPU do cursor falhou; a captura GPU será interrompida"
                    );
                } else {
                    tracing::warn!(error = %error, "O processamento GPU do quadro DXGI falhou; a captura será interrompida");
                }
                *fallback_reason
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(error.clone());
                return Err(format!(
                    "Conversão DXGI/NV12 na GPU falhou; fallback de captura por CPU está desativado: {error}"
                ));
            }
            None => None,
        };
        let next_sequence = sequence.wrapping_add(1);
        let wants_preview = local_preview_limiter
            .should_update(preview_enabled.load(Ordering::Relaxed), Instant::now());
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
#[path = "dxgi_recovery_tests.rs"]
mod recovery_tests;

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

    #[test]
    fn cursor_patch_composition_matches_full_frame_composition_across_clipping_cases() {
        let shape = DxgiCursorShape {
            width: 2,
            height: 2,
            pitch: 8,
            kind: DXGI_OUTDUPL_POINTER_SHAPE_TYPE_COLOR.0,
            bytes: vec![
                10, 20, 200, 255, 30, 40, 210, 128, 50, 60, 220, 64, 70, 80, 230, 255,
            ],
        };
        let (width, height) = (4u32, 4u32);
        let base_frame = (0..(width * height * 4))
            .map(|i| if i % 4 == 3 { 255 } else { (i * 7) as u8 })
            .collect::<Vec<_>>();

        for (cx, cy) in [(-1, -1), (0, 0), (1, 2), (3, 3), (-2, 0), (4, 4)] {
            let mut full_frame = base_frame.clone();
            assert!(composite_cursor_bgra(
                &mut full_frame,
                width,
                height,
                cx,
                cy,
                &shape
            ));

            let mut patched_frame = base_frame.clone();
            if let Some(patch) = super::cursor_patch_rect(width, height, cx, cy, &shape) {
                let row_bytes = patch.width as usize * 4;
                let mut patch_bgra = Vec::with_capacity(row_bytes * patch.height as usize);
                for py in 0..patch.height {
                    let frame_y = (patch.top + py) as usize;
                    let start = (frame_y * width as usize + patch.left as usize) * 4;
                    patch_bgra.extend_from_slice(&patched_frame[start..start + row_bytes]);
                }
                assert!(composite_cursor_bgra(
                    &mut patch_bgra,
                    patch.width,
                    patch.height,
                    patch.local_cursor_x,
                    patch.local_cursor_y,
                    &shape,
                ));
                for py in 0..patch.height {
                    let frame_y = (patch.top + py) as usize;
                    let dst_start = (frame_y * width as usize + patch.left as usize) * 4;
                    let src_start = py as usize * row_bytes;
                    patched_frame[dst_start..dst_start + row_bytes]
                        .copy_from_slice(&patch_bgra[src_start..src_start + row_bytes]);
                }
            }
            assert_eq!(
                patched_frame, full_frame,
                "cursor patch composition diverged at ({cx}, {cy})"
            );
        }
    }

    #[test]
    fn gpu_cursor_composition_updates_d3d11_texture_and_preserves_gpu_nv12_path() {
        use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL};
        use windows::Win32::Graphics::Direct3D11::{
            D3D11_BIND_RENDER_TARGET, D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_SDK_VERSION,
            D3D11_SUBRESOURCE_DATA, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, D3D11CreateDevice,
        };
        use windows::Win32::Graphics::Dxgi::Common::{
            DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC,
        };

        let mut device = None;
        let mut context = None;
        let mut level = D3D_FEATURE_LEVEL(0);
        let created = unsafe {
            D3D11CreateDevice(
                None::<&windows::Win32::Graphics::Dxgi::IDXGIAdapter>,
                D3D_DRIVER_TYPE_HARDWARE,
                Default::default(),
                D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                Some(&mut level),
                Some(&mut context),
            )
        };
        let (Ok(()), Some(device), Some(context)) = (created, device, context) else {
            return;
        };

        let (width, height) = (64u32, 64u32);
        let bgra = [30u8, 60, 90, 255].repeat((width * height) as usize);
        let desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
            ..Default::default()
        };
        let init = D3D11_SUBRESOURCE_DATA {
            pSysMem: bgra.as_ptr().cast(),
            SysMemPitch: width * 4,
            SysMemSlicePitch: 0,
        };
        let mut source_texture = None;
        unsafe {
            device
                .CreateTexture2D(&desc, Some(&init), Some(&mut source_texture))
                .unwrap();
        }
        let source_texture = source_texture.unwrap();
        let cursor = DxgiCursorState {
            position: Some((8, 8)),
            visible: true,
            shape: Some(DxgiCursorShape {
                width: 8,
                height: 8,
                pitch: 32,
                kind: DXGI_OUTDUPL_POINTER_SHAPE_TYPE_COLOR.0,
                bytes: [0u8, 0, 255, 255].repeat(64),
            }),
            ..Default::default()
        };
        let mut gpu_canvas = None;
        let mut patch_staging = None;
        let composited_tex = super::composite_cursor_on_gpu_texture(
            &device,
            &context,
            &source_texture,
            width,
            height,
            &cursor,
            &mut gpu_canvas,
            &mut patch_staging,
        )
        .unwrap();
        if let Ok(mut processor) = crate::mf_video::GpuNv12Processor::new(
            &device, &context, width, height, width, height, 30,
        ) {
            let surface = processor.process(composited_tex).unwrap();
            let (nv12, stride) = surface.readback_nv12().unwrap();
            let rgba = surface.to_rgba(&nv12, stride).unwrap();
            let cursor_pixel =
                &rgba[(10 * width as usize + 10) * 4..(10 * width as usize + 10) * 4 + 4];
            let bg_pixel =
                &rgba[(40 * width as usize + 40) * 4..(40 * width as usize + 40) * 4 + 4];
            assert!(
                cursor_pixel[0] > bg_pixel[0] + 80,
                "expected red cursor in GPU NV12 output, got cursor={cursor_pixel:?} bg={bg_pixel:?}"
            );
        }
        assert!(gpu_canvas.is_some());
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
