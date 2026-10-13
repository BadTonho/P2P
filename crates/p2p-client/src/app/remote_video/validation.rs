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

fn check(gl: &glow::Context) -> Result<(), String> {
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
    result
}
