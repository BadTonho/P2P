//! Explicit native GPU check. Runs without loading or saving user preferences.
use super::*;

pub(in crate::app) fn run() -> eframe::Result {
    let outcome = Arc::new(Mutex::new(None));
    let returned = Arc::clone(&outcome);
    struct ValidationApp {
        outcome: Arc<Mutex<Option<Result<(), String>>>>,
    }
    impl eframe::App for ValidationApp {
        fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
            let mut outcome = self.outcome.lock().unwrap();
            if outcome.is_none() {
                *outcome = Some(
                    frame
                        .gl()
                        .ok_or_else(|| "Contexto Glow ausente.".to_owned())
                        .and_then(|gl| check(gl)),
                );
            }
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
    eframe::run_native(
        "NV12 GPU validation",
        eframe::NativeOptions {
            renderer: eframe::Renderer::Glow,
            viewport: egui::ViewportBuilder::default().with_visible(false),
            ..Default::default()
        },
        Box::new(move |_| Ok(Box::new(ValidationApp { outcome }))),
    )?;
    let result = returned
        .lock()
        .unwrap()
        .take()
        .unwrap_or_else(|| Err("A validação GPU não executou.".to_owned()));
    match result {
        Ok(()) => {
            println!(
                "NV12_GPU_VALIDATION_OK: shader matches CPU; orientation, borders, resize, reuse and resource retirement passed."
            );
            Ok(())
        }
        Err(error) => {
            eprintln!("NV12_GPU_VALIDATION_FAILED: {error}");
            std::process::exit(1);
        }
    }
}

fn check(gl: &Arc<glow::Context>) -> Result<(), String> {
    let mut renderer = Renderer::default();
    let slot = Arc::new(Mutex::new(Slot {
        id: 1,
        ..Default::default()
    }));
    let output = unsafe { gl.create_texture()? };
    let framebuffer = match unsafe { gl.create_framebuffer() } {
        Ok(framebuffer) => framebuffer,
        Err(error) => {
            unsafe {
                gl.delete_texture(output);
            }
            return Err(error);
        }
    };
    let result = (|| {
        unsafe {
            gl.disable(glow::BLEND);
            gl.disable(glow::SCISSOR_TEST);
            gl.disable(glow::CULL_FACE);
            gl.disable(glow::DEPTH_TEST);
            if !gl.version().is_embedded {
                gl.disable(glow::FRAMEBUFFER_SRGB);
            }
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(framebuffer));
        }
        for (sequence, (width, height)) in [(8u32, 8u32), (16, 8), (8, 8), (8, 8)]
            .into_iter()
            .enumerate()
        {
            let y_bytes = (width * height) as usize;
            let mut bytes = vec![0; y_bytes * 3 / 2];
            for row in 0..height as usize {
                for column in 0..width as usize {
                    bytes[row * width as usize + column] =
                        (16 + (row * 37 + column * 13 + sequence * 7) % 220) as u8;
                }
            }
            for (index, value) in bytes[y_bytes..].iter_mut().enumerate() {
                *value = (80 + (index * 29 + sequence * 11) % 96) as u8;
            }
            let expected =
                crate::nv12::to_rgba(&bytes, width as usize, height as usize, width as usize)?;
            let performance = Arc::new(VideoPerformance::default());
            let frame = Arc::new(PreviewFrame {
                sequence: sequence as u64 + 1,
                width,
                height,
                rgba: Vec::new(),
                gpu_nv12: None,
                cpu_nv12: Some(Arc::new(crate::screen_capture::CpuNv12Frame {
                    bytes: Arc::new(bytes),
                    stride: width as usize,
                })),
            });
            let mut state = slot.lock().unwrap();
            state.frame = Some(frame);
            state.performance = Some(Arc::clone(&performance));
            unsafe {
                gl.active_texture(glow::TEXTURE0);
                gl.bind_texture(glow::TEXTURE_2D, Some(output));
                gl.tex_image_2d(
                    glow::TEXTURE_2D,
                    0,
                    glow::RGBA8 as i32,
                    width as i32,
                    height as i32,
                    0,
                    glow::RGBA,
                    glow::UNSIGNED_BYTE,
                    glow::PixelUnpackData::Slice(None),
                );
                gl.framebuffer_texture_2d(
                    glow::FRAMEBUFFER,
                    glow::COLOR_ATTACHMENT0,
                    glow::TEXTURE_2D,
                    Some(output),
                    0,
                );
                if gl.check_framebuffer_status(glow::FRAMEBUFFER) != glow::FRAMEBUFFER_COMPLETE {
                    return Err("Framebuffer de validação incompleto.".to_owned());
                }
                gl.viewport(0, 0, width as i32, height as i32);
            }
            renderer.paint(gl, &slot, &state)?;
            let mut actual = vec![0; y_bytes * 4];
            unsafe {
                gl.read_pixels(
                    0,
                    0,
                    width as i32,
                    height as i32,
                    glow::RGBA,
                    glow::UNSIGNED_BYTE,
                    glow::PixelPackData::Slice(Some(&mut actual)),
                );
            }
            for row in 0..height as usize {
                for column in 0..width as usize {
                    let cpu = (row * width as usize + column) * 4;
                    let gpu = ((height as usize - 1 - row) * width as usize + column) * 4;
                    for channel in 0..4 {
                        if actual[gpu + channel].abs_diff(expected[cpu + channel]) > 1 {
                            return Err(format!(
                                "Shader divergiu da CPU: sequência {sequence}, pixel {column},{row}, canal {channel}, GPU {}, CPU {}.",
                                actual[gpu + channel],
                                expected[cpu + channel]
                            ));
                        }
                    }
                }
            }
            let snapshot = performance.snapshot();
            if snapshot.path_count(FramePath::UiNv12Shader) != 1
                || snapshot.timing(Stage::UiRgbaConvert).samples != 0
            {
                return Err("Contadores da apresentação NV12 inválidos.".to_owned());
            }
            if renderer.textures.len() != 1 {
                return Err("Recursos NV12 cresceram após trocar resolução.".to_owned());
            }
            // A second draw of the same sequence must reuse both uploaded planes.
            renderer.paint(gl, &slot, &state)?;
            if performance.snapshot().timing(Stage::UiNv12Upload).samples != 1 {
                return Err("O mesmo quadro foi enviado duas vezes à GPU.".to_owned());
            }
        }
        slot.lock().unwrap().frame = None;
        renderer.collect(gl);
        if !renderer.textures.is_empty() {
            return Err("Texturas NV12 não foram liberadas ao encerrar a sessão.".to_owned());
        }
        Ok(())
    })();
    renderer.destroy(gl);
    unsafe {
        gl.bind_framebuffer(glow::FRAMEBUFFER, None);
        gl.delete_framebuffer(framebuffer);
        gl.delete_texture(output);
    }
    result?;
    check_interactions(gl)
}

fn check_interactions(gl: &Arc<glow::Context>) -> Result<(), String> {
    let mut painter = egui_glow::Painter::new(Arc::clone(gl), "", None, false)
        .map_err(|error| error.to_string())?;
    let mut video = RemoteVideoRenderer {
        available: true,
        ..Default::default()
    };
    let output = unsafe { gl.create_texture()? };
    let framebuffer = unsafe { gl.create_framebuffer()? };
    let result = (|| {
        for nv12 in [false, true] {
            for size in [
                egui::vec2(280.0, 158.0),
                egui::vec2(640.0, 360.0),
                egui::vec2(1920.0, 1080.0),
            ] {
                let context = egui::Context::default();
                let mut texture = video.texture();
                let frame = Arc::new(PreviewFrame {
                    sequence: 1,
                    width: 8,
                    height: 8,
                    rgba: if nv12 {
                        vec![]
                    } else {
                        [180, 100, 40, 0].repeat(64)
                    },
                    gpu_nv12: None,
                    cpu_nv12: nv12.then(|| {
                        Arc::new(crate::screen_capture::CpuNv12Frame {
                            stride: 8,
                            bytes: Arc::new(vec![128; 96]),
                        })
                    }),
                });
                texture.update(&context, frame, Arc::new(VideoPerformance::default()))?;
                let canvas = [(size.x + 20.0) as u32, (size.y + 20.0) as u32];
                unsafe {
                    gl.bind_framebuffer(glow::FRAMEBUFFER, Some(framebuffer));
                    gl.active_texture(glow::TEXTURE0);
                    gl.bind_texture(glow::TEXTURE_2D, Some(output));
                    gl.tex_image_2d(
                        glow::TEXTURE_2D,
                        0,
                        glow::RGBA8 as i32,
                        canvas[0] as i32,
                        canvas[1] as i32,
                        0,
                        glow::RGBA,
                        glow::UNSIGNED_BYTE,
                        glow::PixelUnpackData::Slice(None),
                    );
                    gl.framebuffer_texture_2d(
                        glow::FRAMEBUFFER,
                        glow::COLOR_ATTACHMENT0,
                        glow::TEXTURE_2D,
                        Some(output),
                        0,
                    );
                    if gl.check_framebuffer_status(glow::FRAMEBUFFER) != glow::FRAMEBUFFER_COMPLETE
                    {
                        return Err("Framebuffer de interação incompleto.".to_owned());
                    }
                }
                let mut baseline = None;
                let mut response_rect = egui::Rect::NOTHING;
                for phase in 0..4 {
                    let point = if response_rect.is_positive() {
                        response_rect.center()
                    } else {
                        egui::pos2(50.0, 50.0)
                    };
                    let mut events = vec![egui::Event::PointerMoved(point)];
                    if phase == 1 || phase == 3 {
                        events.push(egui::Event::PointerButton {
                            pos: point,
                            button: egui::PointerButton::Primary,
                            pressed: phase == 1,
                            modifiers: Default::default(),
                        });
                    }
                    let mut output_ui = context.run_ui(
                        egui::RawInput {
                            screen_rect: Some(egui::Rect::from_min_size(
                                egui::Pos2::ZERO,
                                egui::vec2(canvas[0] as f32, canvas[1] as f32),
                            )),
                            events,
                            ..Default::default()
                        },
                        |ui| {
                            // Even an interactive parent must not fade video pixels.
                            ui.set_opacity(if phase == 1 || phase == 2 { 0.55 } else { 1.0 });
                            let response = texture.show(ui, size, egui::Sense::click());
                            response_rect = response.rect;
                        },
                    );
                    let primitives =
                        context.tessellate(output_ui.shapes, output_ui.pixels_per_point);
                    unsafe {
                        gl.disable(glow::SCISSOR_TEST);
                        gl.clear_color(0.0, 0.0, 0.0, 1.0);
                        gl.clear(glow::COLOR_BUFFER_BIT);
                    }
                    painter.paint_and_update_textures(
                        canvas,
                        output_ui.pixels_per_point,
                        &primitives,
                        &mut output_ui.textures_delta,
                    );
                    let center = response_rect.center();
                    let mut pixel = [0; 4];
                    unsafe {
                        gl.read_pixels(
                            center.x as i32,
                            canvas[1] as i32 - 1 - center.y as i32,
                            1,
                            1,
                            glow::RGBA,
                            glow::UNSIGNED_BYTE,
                            glow::PixelPackData::Slice(Some(&mut pixel)),
                        );
                    }
                    if pixel[3] != 255 || pixel[..3].iter().all(|value| *value == 0) {
                        return Err(format!(
                            "Vídeo não foi apresentado opaco: NV12={nv12}, fase={phase}, pixel={pixel:?}."
                        ));
                    }
                    if let Some(baseline) = baseline {
                        if pixel != baseline {
                            return Err(format!(
                                "O clique mudou o brilho: NV12={nv12}, tamanho={size:?}, fase={phase}, pixel={pixel:?}, antes={baseline:?}."
                            ));
                        }
                    } else {
                        baseline = Some(pixel);
                    }
                }
                drop(texture);
                video.begin_frame(Some(gl));
            }
        }
        println!(
            "VIDEO_CLICK_VALIDATION_OK: RGBA/NV12 pixels unchanged during press, hold and release in grid, enlarged and fullscreen sizes."
        );
        Ok(())
    })();
    video.destroy(gl);
    painter.destroy();
    unsafe {
        gl.bind_framebuffer(glow::FRAMEBUFFER, None);
        gl.delete_framebuffer(framebuffer);
        gl.delete_texture(output);
    }
    result
}
