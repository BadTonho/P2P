use std::error::Error;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

use eframe::egui;
use windows_capture::capture::{CaptureControl, Context, GraphicsCaptureApiHandler};
use windows_capture::frame::Frame;
use windows_capture::graphics_capture_api::InternalCaptureControl;
use windows_capture::graphics_capture_picker::{
    Error as PickerError, GraphicsCapturePicker, PickedGraphicsCaptureItem,
};
use windows_capture::monitor::Monitor;
use windows_capture::settings::{
    ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings,
    GraphicsCaptureItemType, MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings,
};

const MAX_PREVIEW_WIDTH: u32 = 640;
const MAX_PREVIEW_HEIGHT: u32 = 360;

pub struct PreviewFrame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

type HandlerError = Box<dyn Error + Send + Sync>;
type Control = CaptureControl<ScreenFrameHandler, HandlerError>;

pub struct ScreenCapture {
    control: Option<Control>,
    latest_frame: Arc<Mutex<Option<PreviewFrame>>>,
    source_closed: Arc<AtomicBool>,
    // Keep the picker's owner window and WinRT apartment alive until capture stops.
    _picker_owner: PickedGraphicsCaptureItem,
}

impl ScreenCapture {
    pub fn pick_and_start(context: egui::Context) -> Result<Option<Self>, String> {
        let selected = match GraphicsCapturePicker::pick_item() {
            Ok(Some(selected)) => selected,
            Ok(None) | Err(PickerError::Canceled) => return Ok(None),
            Err(error) => return Err(format!("O seletor do Windows falhou: {error}")),
        };
        let size = selected.size().map_err(|error| {
            format!("Não foi possível consultar o tamanho da tela ou janela: {error}")
        })?;
        if size.0 <= 0 || size.1 <= 0 {
            return Err("The selected screen or window has an invalid size.".to_owned());
        }
        let latest_frame = Arc::new(Mutex::new(None));
        let source_closed = Arc::new(AtomicBool::new(false));
        let settings = Settings::new(
            PickerItemForThread(selected.item.clone()),
            CursorCaptureSettings::Default,
            DrawBorderSettings::Default,
            SecondaryWindowSettings::Default,
            MinimumUpdateIntervalSettings::Default,
            DirtyRegionSettings::Default,
            ColorFormat::Rgba8,
            HandlerFlags {
                _size: size,
                context,
                latest_frame: Arc::clone(&latest_frame),
                source_closed: Arc::clone(&source_closed),
            },
        );
        let control = ScreenFrameHandler::start_free_threaded(settings)
            .map_err(|error| format!("O Windows não iniciou a captura da tela: {error}"))?;

        Ok(Some(Self {
            control: Some(control),
            latest_frame,
            source_closed,
            _picker_owner: selected,
        }))
    }

    pub fn take_latest_frame(&self) -> Option<PreviewFrame> {
        self.latest_frame
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }

    pub fn source_closed(&self) -> bool {
        self.source_closed.load(Ordering::Relaxed)
    }

    pub fn poll_finished(&mut self) -> Option<Result<(), String>> {
        let control = self.control.as_ref()?;
        if !control.is_finished() {
            return None;
        }
        let control = self
            .control
            .take()
            .expect("capture control was just checked");
        Some(control.wait().map_err(|error| format!("{error}")))
    }

    pub fn stop(&mut self) -> Result<(), String> {
        let Some(control) = self.control.take() else {
            return Ok(());
        };
        control.stop().map_err(|error| format!("{error}"))
    }
}

impl Drop for ScreenCapture {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

struct HandlerFlags {
    _size: (i32, i32),
    context: egui::Context,
    latest_frame: Arc<Mutex<Option<PreviewFrame>>>,
    source_closed: Arc<AtomicBool>,
}

// windows-capture's picker wrapper also owns an HWND guard and is therefore not Send.
// Keep that wrapper on the UI thread, and send only the agile WinRT capture item to the worker.
// The Monitor tag makes the library skip window-title-bar cropping; capture still uses this exact
// item returned by the system picker.
struct PickerItemForThread(windows_capture::GraphicsCaptureItem);

impl TryInto<GraphicsCaptureItemType> for PickerItemForThread {
    type Error = windows_capture::monitor::Error;

    fn try_into(self) -> Result<GraphicsCaptureItemType, Self::Error> {
        Ok(GraphicsCaptureItemType::Monitor((
            self.0,
            Monitor::primary()?,
        )))
    }
}

struct ScreenFrameHandler {
    context: egui::Context,
    latest_frame: Arc<Mutex<Option<PreviewFrame>>>,
    source_closed: Arc<AtomicBool>,
    scratch: Vec<u8>,
}

impl GraphicsCaptureApiHandler for ScreenFrameHandler {
    type Flags = HandlerFlags;
    type Error = HandlerError;

    fn new(context: Context<Self::Flags>) -> Result<Self, Self::Error> {
        Ok(Self {
            context: context.flags.context,
            latest_frame: context.flags.latest_frame,
            source_closed: context.flags.source_closed,
            scratch: Vec::new(),
        })
    }

    fn on_frame_arrived(
        &mut self,
        frame: &mut Frame,
        _capture_control: InternalCaptureControl,
    ) -> Result<(), Self::Error> {
        let width = frame.width();
        let height = frame.height();
        if width == 0 || height == 0 {
            return Ok(());
        }

        let frame_buffer = frame.buffer()?;
        let rgba = frame_buffer.as_nopadding_buffer(&mut self.scratch);
        let preview = downsample_rgba(rgba, width, height);
        *self
            .latest_frame
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(preview);
        self.context.request_repaint();
        Ok(())
    }

    fn on_closed(&mut self) -> Result<(), Self::Error> {
        self.source_closed.store(true, Ordering::Relaxed);
        self.context.request_repaint();
        Ok(())
    }
}

fn downsample_rgba(bytes: &[u8], width: u32, height: u32) -> PreviewFrame {
    let scale = (MAX_PREVIEW_WIDTH as f32 / width as f32)
        .min(MAX_PREVIEW_HEIGHT as f32 / height as f32)
        .min(1.0);
    let out_width = ((width as f32 * scale).round() as u32).max(1);
    let out_height = ((height as f32 * scale).round() as u32).max(1);
    let mut rgba = Vec::with_capacity((out_width * out_height * 4) as usize);

    for y in 0..out_height {
        let source_y = (y * height / out_height).min(height - 1);
        for x in 0..out_width {
            let source_x = (x * width / out_width).min(width - 1);
            let offset = ((source_y * width + source_x) * 4) as usize;
            if let Some(pixel) = bytes.get(offset..offset + 4) {
                rgba.extend_from_slice(pixel);
            } else {
                rgba.extend_from_slice(&[0, 0, 0, 255]);
            }
        }
    }

    PreviewFrame {
        width: out_width,
        height: out_height,
        rgba,
    }
}
