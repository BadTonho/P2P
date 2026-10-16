//! NV12 presentation in the existing Glow context. Decoder readback remains.
//! GL objects belong to Renderer, are retired on the UI thread, and are destroyed
//! through eframe::App::on_exit while the graphics context is still current.

use crate::screen_capture::PreviewFrame;
use crate::video_performance::{FramePath, Stage, VideoPerformance};
use eframe::{egui, egui_glow, glow};
use glow::HasContext;
use std::collections::HashMap;
use std::sync::{
    Arc, Mutex, Weak,
    atomic::{AtomicU64, Ordering},
};

#[cfg(windows)]
#[path = "remote_video/validation.rs"]
mod validation;
#[cfg(windows)]
pub(super) use validation::run as run_gpu_validation;

static NEXT_SLOT: AtomicU64 = AtomicU64::new(1);

pub(super) const VERTEX_SHADER: &str = r#"
const vec2 positions[3] = vec2[3](vec2(-1.0,-1.0), vec2(3.0,-1.0), vec2(-1.0,3.0));
out vec2 uv;
void main() {
    vec2 p = positions[gl_VertexID];
    gl_Position = vec4(p, 0.0, 1.0);
    uv = vec2((p.x + 1.0) * 0.5, (1.0 - p.y) * 0.5);
}
"#;

// Match the existing integer BT.601 limited-range converter, including rounding.
pub(super) const FRAGMENT_SHADER: &str = r#"
in vec2 uv;
out vec4 color;
uniform sampler2D plane_y;
uniform sampler2D plane_uv;
void main() {
    ivec2 size = textureSize(plane_y, 0);
    ivec2 pixel = clamp(ivec2(floor(uv * vec2(size))), ivec2(0), size - 1);
    int y = int(round(texelFetch(plane_y, pixel, 0).r * 255.0)) - 16;
    ivec2 chroma = ivec2(round(texelFetch(plane_uv, pixel / 2, 0).rg * 255.0)) - 128;
    int r = (298*y + 409*chroma.y + 128) >> 8;
    int g = (298*y - 100*chroma.x - 208*chroma.y + 128) >> 8;
    int b = (298*y + 516*chroma.x + 128) >> 8;
    color = vec4(vec3(clamp(ivec3(r,g,b), ivec3(0), ivec3(255))) / 255.0, 1.0);
}
"#;

#[derive(Default)]
pub(super) struct RemoteVideoRenderer {
    resources: Arc<Mutex<Renderer>>,
    available: bool,
}

impl RemoteVideoRenderer {
    pub fn set_available(&mut self, available: bool) {
        self.available = available;
    }

    pub fn begin_frame(&mut self, gl: Option<&glow::Context>) {
        self.set_available(gl.is_some());
        if let Some(gl) = gl {
            self.resources
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .collect(gl);
        }
    }

    pub fn destroy(&mut self, gl: &glow::Context) {
        self.resources
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .destroy(gl);
    }

    pub fn texture(&self) -> RemoteVideoTexture {
        RemoteVideoTexture {
            renderer: Arc::clone(&self.resources),
            slot: Arc::new(Mutex::new(Slot {
                id: NEXT_SLOT.fetch_add(1, Ordering::Relaxed),
                ..Default::default()
            })),
            rgba: None,
            size: egui::Vec2::ZERO,
            available: self.available,
        }
    }
}

#[derive(Default)]
struct Slot {
    id: u64,
    frame: Option<Arc<PreviewFrame>>,
    performance: Option<Arc<VideoPerformance>>,
    failed: Option<String>,
    backend: &'static str,
}

#[derive(Clone)]
pub(super) struct RemoteVideoTexture {
    renderer: Arc<Mutex<Renderer>>,
    slot: Arc<Mutex<Slot>>,
    rgba: Option<egui::TextureHandle>,
    size: egui::Vec2,
    available: bool,
}

impl RemoteVideoTexture {
    pub fn size_vec2(&self) -> egui::Vec2 {
        self.size
    }

    pub fn needs_fallback(&self) -> bool {
        let slot = self
            .slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        slot.frame.is_some() && slot.failed.is_some()
    }

    pub fn update(
        &mut self,
        context: &egui::Context,
        frame: Arc<PreviewFrame>,
        performance: Arc<VideoPerformance>,
    ) -> Result<(), String> {
        let mut slot = self
            .slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.size = egui::vec2(frame.width as f32, frame.height as f32);
        #[cfg(windows)]
        if let Some(nv12) = &frame.cpu_nv12 {
            let layout = crate::nv12::Nv12Layout {
                surface_width: frame.width as usize,
                surface_height: frame.height as usize,
                pitch: nv12.stride,
                x: 0,
                y: 0,
                width: frame.width as usize,
                height: frame.height as usize,
            };
            layout.packed_len()?;
            if nv12.stride != frame.width as usize || nv12.bytes.len() != layout.source_len()? {
                return Err("A apresentação requer NV12 normalizado e completo.".to_owned());
            }
            if self.available && slot.failed.is_none() {
                if slot.backend != "nv12_glow" {
                    tracing::info!(
                        presentation_id = slot.id,
                        backend = "nv12_glow",
                        "Apresentação remota NV12 pela GPU selecionada"
                    );
                }
                slot.backend = "nv12_glow";
                slot.frame = Some(frame);
                slot.performance = Some(performance);
                self.rgba = None;
                return Ok(());
            }
            if slot.failed.is_none() {
                slot.failed = Some("Contexto Glow indisponível.".to_owned());
                tracing::warn!(presentation_id = slot.id, cause = ?slot.failed, "Apresentação NV12 indisponível; usando RGBA nesta sessão");
            }
        }
        let started = std::time::Instant::now();
        let image = super::local_preview_image(&frame)?
            .ok_or_else(|| "Quadro remoto sem pixels para apresentação.".to_owned())?;
        performance.record_duration(Stage::UiRgbaConvert, started.elapsed());
        performance.record_path(FramePath::UiRgbaFallback);
        if let Some(texture) = &mut self.rgba {
            texture.set(image, egui::TextureOptions::LINEAR);
        } else {
            self.rgba = Some(context.load_texture(
                format!("remote-rgba-{}", slot.id),
                image,
                egui::TextureOptions::LINEAR,
            ));
        }
        if slot.backend != "rgba" {
            tracing::info!(presentation_id = slot.id, backend = "rgba", fallback = ?slot.failed, "Apresentação remota RGBA selecionada");
        }
        slot.backend = "rgba";
        slot.frame = None;
        slot.performance = None;
        Ok(())
    }

    pub fn show(&self, ui: &mut egui::Ui, size: egui::Vec2, sense: egui::Sense) -> egui::Response {
        if let Some(texture) = &self.rgba {
            return show_opaque_texture(ui, texture, size, sense);
        }
        let (rect, response) = ui.allocate_exact_size(size, sense);
        let renderer = Arc::clone(&self.renderer);
        let slot = Arc::clone(&self.slot);
        let context = ui.ctx().clone();
        ui.painter().add(egui::PaintCallback { rect, callback: Arc::new(egui_glow::CallbackFn::new(move |_, painter| {
            let mut renderer = renderer.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let mut state = slot.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.failed.is_some() { return; }
            if let Err(error) = renderer.paint(painter.gl(), &slot, &state) {
                tracing::warn!(presentation_id = state.id, cause = %error, "Falha na apresentação NV12; usando RGBA nesta sessão");
                state.failed = Some(error);
                context.request_repaint();
            }
        })) });
        response
    }
}

#[derive(Default)]
struct Renderer {
    pipeline: Option<Pipeline>,
    textures: HashMap<u64, Planes>,
}

struct Pipeline {
    program: glow::Program,
    vao: glow::VertexArray,
}
struct Planes {
    y: glow::Texture,
    uv: glow::Texture,
    size: (i32, i32),
    sequence: u64,
    owner: Weak<Mutex<Slot>>,
}

impl Renderer {
    fn collect(&mut self, gl: &glow::Context) {
        self.textures.retain(|_, planes| {
            let alive = planes.owner.upgrade().is_some_and(|owner| {
                let state = owner
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                state.frame.is_some() && state.failed.is_none()
            });
            if !alive {
                unsafe {
                    gl.delete_texture(planes.y);
                    gl.delete_texture(planes.uv);
                }
            }
            alive
        });
    }

    fn destroy(&mut self, gl: &glow::Context) {
        unsafe {
            for (_, planes) in self.textures.drain() {
                gl.delete_texture(planes.y);
                gl.delete_texture(planes.uv);
            }
            if let Some(pipeline) = self.pipeline.take() {
                gl.delete_program(pipeline.program);
                gl.delete_vertex_array(pipeline.vao);
            }
        }
    }

    fn paint(
        &mut self,
        gl: &glow::Context,
        owner: &Arc<Mutex<Slot>>,
        state: &Slot,
    ) -> Result<(), String> {
        #[cfg(not(windows))]
        {
            let _ = (gl, owner, state);
            Err("Apresentação NV12 indisponível nesta plataforma.".to_owned())
        }
        #[cfg(windows)]
        {
            let frame = state
                .frame
                .as_ref()
                .ok_or_else(|| "Quadro NV12 ausente.".to_owned())?;
            let nv12 = frame
                .cpu_nv12
                .as_ref()
                .ok_or_else(|| "Pixels NV12 ausentes.".to_owned())?;
            let performance = state
                .performance
                .as_ref()
                .ok_or_else(|| "Diagnóstico de apresentação ausente.".to_owned())?;
            if self.pipeline.is_none() {
                self.pipeline = Some(Pipeline::new(gl)?);
            }
            let size = (frame.width as i32, frame.height as i32);
            if !self.textures.contains_key(&state.id) {
                let y = unsafe { gl.create_texture()? };
                let uv = match unsafe { gl.create_texture() } {
                    Ok(uv) => uv,
                    Err(error) => {
                        unsafe {
                            gl.delete_texture(y);
                        }
                        return Err(error);
                    }
                };
                self.textures.insert(
                    state.id,
                    Planes {
                        y,
                        uv,
                        size: (0, 0),
                        sequence: 0,
                        owner: Arc::downgrade(owner),
                    },
                );
            }
            let planes = self.textures.get_mut(&state.id).expect("planes inserted");
            if planes.sequence != frame.sequence || planes.size != size {
                let _timer = performance.timer(Stage::UiNv12Upload);
                unsafe {
                    let unpack = gl.get_parameter_i32(glow::UNPACK_ALIGNMENT);
                    gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);
                    let split = frame.width as usize * frame.height as usize;
                    upload_plane(
                        gl,
                        planes.y,
                        0,
                        size.0,
                        size.1,
                        glow::R8,
                        glow::RED,
                        &nv12.bytes[..split],
                        planes.size != size,
                    );
                    upload_plane(
                        gl,
                        planes.uv,
                        1,
                        size.0 / 2,
                        size.1 / 2,
                        glow::RG8,
                        glow::RG,
                        &nv12.bytes[split..],
                        planes.size != size,
                    );
                    gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, unpack);
                    gl.active_texture(glow::TEXTURE0);
                    let error = gl.get_error();
                    if error != glow::NO_ERROR {
                        return Err(format!("Upload NV12 OpenGL falhou: 0x{error:04x}."));
                    }
                }
                planes.size = size;
                planes.sequence = frame.sequence;
                performance.record_path(FramePath::UiNv12Shader);
            }
            let _timer = performance.timer(Stage::UiNv12Draw);
            let pipeline = self.pipeline.as_ref().expect("pipeline initialized");
            unsafe {
                gl.use_program(Some(pipeline.program));
                gl.bind_vertex_array(Some(pipeline.vao));
                gl.active_texture(glow::TEXTURE0);
                gl.bind_texture(glow::TEXTURE_2D, Some(planes.y));
                gl.active_texture(glow::TEXTURE1);
                gl.bind_texture(glow::TEXTURE_2D, Some(planes.uv));
                gl.uniform_1_i32(
                    gl.get_uniform_location(pipeline.program, "plane_y")
                        .as_ref(),
                    0,
                );
                gl.uniform_1_i32(
                    gl.get_uniform_location(pipeline.program, "plane_uv")
                        .as_ref(),
                    1,
                );
                gl.draw_arrays(glow::TRIANGLES, 0, 3);
                gl.active_texture(glow::TEXTURE0);
                let error = gl.get_error();
                if error != glow::NO_ERROR {
                    return Err(format!("Shader NV12 OpenGL falhou: 0x{error:04x}."));
                }
            }
            Ok(())
        }
    }
}

unsafe fn upload_plane(
    gl: &glow::Context,
    texture: glow::Texture,
    unit: u32,
    width: i32,
    height: i32,
    internal: u32,
    format: u32,
    bytes: &[u8],
    allocate: bool,
) {
    unsafe {
        gl.active_texture(glow::TEXTURE0 + unit);
        gl.bind_texture(glow::TEXTURE_2D, Some(texture));
        if allocate {
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_MIN_FILTER,
                glow::NEAREST as i32,
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_MAG_FILTER,
                glow::NEAREST as i32,
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_WRAP_S,
                glow::CLAMP_TO_EDGE as i32,
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_WRAP_T,
                glow::CLAMP_TO_EDGE as i32,
            );
            gl.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                internal as i32,
                width,
                height,
                0,
                format,
                glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(Some(bytes)),
            );
        } else {
            gl.tex_sub_image_2d(
                glow::TEXTURE_2D,
                0,
                0,
                0,
                width,
                height,
                format,
                glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(Some(bytes)),
            );
        }
    }
}

impl Pipeline {
    fn new(gl: &glow::Context) -> Result<Self, String> {
        unsafe {
            let prefix = if gl.version().is_embedded {
                "#version 300 es\nprecision highp float;\nprecision highp int;\n"
            } else {
                "#version 330 core\n"
            };
            let program = gl.create_program()?;
            let mut shaders = Vec::new();
            let result = (|| {
                for (kind, source) in [
                    (glow::VERTEX_SHADER, VERTEX_SHADER),
                    (glow::FRAGMENT_SHADER, FRAGMENT_SHADER),
                ] {
                    let shader = gl.create_shader(kind)?;
                    shaders.push(shader);
                    gl.shader_source(shader, &format!("{prefix}{source}"));
                    gl.compile_shader(shader);
                    if !gl.get_shader_compile_status(shader) {
                        return Err(gl.get_shader_info_log(shader));
                    }
                    gl.attach_shader(program, shader);
                }
                gl.link_program(program);
                if !gl.get_program_link_status(program) {
                    return Err(gl.get_program_info_log(program));
                }
                gl.create_vertex_array()
            })();
            for shader in shaders {
                gl.delete_shader(shader);
            }
            match result {
                Ok(vao) => Ok(Self { program, vao }),
                Err(error) => {
                    gl.delete_program(program);
                    Err(error)
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    fn frame(sequence: u64, width: u32, height: u32) -> Arc<PreviewFrame> {
        Arc::new(PreviewFrame {
            sequence,
            width,
            height,
            rgba: Vec::new(),
            gpu_nv12: None,
            cpu_nv12: Some(Arc::new(crate::screen_capture::CpuNv12Frame {
                stride: width as usize,
                bytes: Arc::new(vec![128; (width * height * 3 / 2) as usize]),
            })),
        })
    }

    #[cfg(windows)]
    #[test]
    fn gpu_publication_avoids_cpu_conversion_and_keeps_only_latest_frame() {
        let renderer = RemoteVideoRenderer {
            available: true,
            ..Default::default()
        };
        let mut texture = renderer.texture();
        let metrics = Arc::new(VideoPerformance::default());
        let context = egui::Context::default();
        let first = frame(1, 4, 4);
        let old = Arc::downgrade(&first);
        texture
            .update(&context, first, Arc::clone(&metrics))
            .unwrap();
        texture
            .update(&context, frame(2, 8, 4), Arc::clone(&metrics))
            .unwrap();
        assert!(old.upgrade().is_none());
        assert!(texture.rgba.is_none());
        assert_eq!(texture.size_vec2(), egui::vec2(8.0, 4.0));
        assert_eq!(metrics.snapshot().timing(Stage::UiRgbaConvert).samples, 0);
        assert_eq!(
            metrics.snapshot().timing(Stage::DecoderCpuConvert).samples,
            0
        );
    }

    #[cfg(windows)]
    #[test]
    fn graphics_failure_is_sticky_for_session_and_restart_can_retry() {
        let renderer = RemoteVideoRenderer {
            available: true,
            ..Default::default()
        };
        let mut texture = renderer.texture();
        let metrics = Arc::new(VideoPerformance::default());
        let context = egui::Context::default();
        texture
            .update(&context, frame(1, 4, 4), Arc::clone(&metrics))
            .unwrap();
        texture.slot.lock().unwrap().failed = Some("synthetic shader failure".to_owned());
        assert!(texture.needs_fallback());
        texture
            .update(&context, frame(1, 4, 4), Arc::clone(&metrics))
            .unwrap();
        texture
            .update(&context, frame(2, 4, 4), Arc::clone(&metrics))
            .unwrap();
        assert!(texture.rgba.is_some());
        assert!(!texture.needs_fallback());
        assert_eq!(texture.slot.lock().unwrap().backend, "rgba");
        assert_eq!(metrics.snapshot().path_count(FramePath::UiRgbaFallback), 2);
        let mut restarted = renderer.texture();
        restarted.update(&context, frame(1, 4, 4), metrics).unwrap();
        assert!(restarted.rgba.is_none());
        assert!(restarted.slot.lock().unwrap().failed.is_none());
    }
}

/// Video pixels remain opaque independently of an interactive parent style.
pub(super) fn show_opaque_texture(
    ui: &mut egui::Ui,
    texture: &egui::TextureHandle,
    size: egui::Vec2,
    sense: egui::Sense,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(size, sense);
    let mut painter = ui.painter().clone();
    painter.set_opacity(1.0);
    let mut mesh = egui::Mesh::with_texture(texture.id());
    mesh.add_rect_with_uv(
        rect,
        egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
        egui::Color32::WHITE,
    );
    painter.add(mesh);
    response.widget_info(|| egui::WidgetInfo::new(egui::WidgetType::Image));
    response
}
