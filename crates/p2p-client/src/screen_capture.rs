use std::error::Error;
use std::sync::{
    Arc, Condvar, Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
    mpsc::{self, Receiver, SyncSender, TryRecvError},
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use eframe::egui;
use windows_capture::capture::{CaptureControl, Context, GraphicsCaptureApiHandler};
use windows_capture::dxgi_duplication_api::{DxgiDuplicationApi, DxgiDuplicationFormat};
use windows_capture::frame::Frame;
use windows_capture::graphics_capture_api::InternalCaptureControl;
use windows_capture::graphics_capture_picker::{Error as PickerError, GraphicsCapturePicker};
use windows_capture::monitor::Monitor;
use windows_capture::settings::{
    ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings,
    GraphicsCaptureItemType, MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings,
};

mod dxgi_backend;
mod wgc_backend;

const MAX_FRAME_WIDTH: u32 = 1280;
const MAX_FRAME_HEIGHT: u32 = 720;
const MIN_CAPTURE_FRAME_INTERVAL: Duration = Duration::from_nanos(1_000_000_000 / 30);
const CAPTURE_PACING_JITTER_TOLERANCE: Duration = Duration::from_micros(250);

#[derive(Clone, Debug)]
pub struct MonitorOption {
    pub device_id: String,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub is_primary: bool,
}

impl MonitorOption {
    pub fn label(&self, ordinal: usize) -> String {
        let device_label = self
            .device_id
            .rsplit('\\')
            .next()
            .unwrap_or(&self.device_id);
        let primary_label = if self.is_primary {
            " — principal"
        } else {
            ""
        };
        format!(
            "Monitor {ordinal} — {} ({device_label}) — {}×{}{primary_label}",
            self.name, self.width, self.height
        )
    }
}

#[derive(Clone)]
pub struct PreviewFrame {
    pub sequence: u64,
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    #[cfg(windows)]
    pub gpu_nv12: Option<Arc<crate::mf_video::GpuNv12Surface>>,
    #[cfg(windows)]
    pub cpu_nv12: Option<Arc<CpuNv12Frame>>,
}

#[cfg(windows)]
#[derive(Clone)]
pub struct CpuNv12Frame {
    pub bytes: Arc<Vec<u8>>,
    pub stride: usize,
}

#[derive(Clone, Default)]
pub struct LatestFrame {
    shared: Arc<LatestFrameShared>,
}

#[derive(Default)]
struct LatestFrameShared {
    slot: Mutex<LatestFrameSlot>,
    changed: Condvar,
}

#[derive(Default)]
struct LatestFrameSlot {
    generation: u64,
    frame: Option<Arc<PreviewFrame>>,
}

impl LatestFrame {
    pub fn latest(&self) -> Option<Arc<PreviewFrame>> {
        self.shared
            .slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .frame
            .clone()
    }

    pub fn publish(&self, frame: PreviewFrame) {
        let mut slot = self
            .shared
            .slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        slot.frame = Some(Arc::new(frame));
        slot.generation = slot.generation.wrapping_add(1);
        self.shared.changed.notify_one();
    }

    pub fn generation(&self) -> u64 {
        self.shared
            .slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .generation
    }

    pub fn wait_for_change(
        &self,
        observed_generation: u64,
        timeout: Duration,
        stop: &AtomicBool,
    ) -> (u64, Option<Arc<PreviewFrame>>, bool) {
        let deadline = Instant::now() + timeout;
        let mut slot = self
            .shared
            .slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while slot.generation == observed_generation && !stop.load(Ordering::Relaxed) {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            let (next_slot, result) = self
                .shared
                .changed
                .wait_timeout(slot, remaining)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            slot = next_slot;
            if result.timed_out() {
                break;
            }
        }
        let timed_out = slot.generation == observed_generation && !stop.load(Ordering::Relaxed);
        (slot.generation, slot.frame.clone(), timed_out)
    }

    pub fn wake_waiters(&self) {
        self.shared.changed.notify_all();
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct CapturePerformanceSnapshot {
    pub received_frames: u64,
    pub processed_frames: u64,
    pub unchanged_frames: u64,
    pub skipped_frames: u64,
    pub readback_nanos: u64,
    pub resize_nanos: u64,
    pub gpu_convert_nanos: u64,
}

#[derive(Default)]
struct CapturePerformanceCounters {
    received_frames: AtomicU64,
    processed_frames: AtomicU64,
    unchanged_frames: AtomicU64,
    skipped_frames: AtomicU64,
    readback_nanos: AtomicU64,
    resize_nanos: AtomicU64,
    gpu_convert_nanos: AtomicU64,
}

impl CapturePerformanceCounters {
    fn take_snapshot(&self) -> CapturePerformanceSnapshot {
        CapturePerformanceSnapshot {
            received_frames: self.received_frames.swap(0, Ordering::Relaxed),
            processed_frames: self.processed_frames.swap(0, Ordering::Relaxed),
            unchanged_frames: self.unchanged_frames.swap(0, Ordering::Relaxed),
            skipped_frames: self.skipped_frames.swap(0, Ordering::Relaxed),
            readback_nanos: self.readback_nanos.swap(0, Ordering::Relaxed),
            resize_nanos: self.resize_nanos.swap(0, Ordering::Relaxed),
            gpu_convert_nanos: self.gpu_convert_nanos.swap(0, Ordering::Relaxed),
        }
    }
}

type HandlerError = Box<dyn Error + Send + Sync>;
type Control = CaptureControl<wgc_backend::ScreenFrameHandler, HandlerError>;
type PickResult = Result<Option<(windows_capture::GraphicsCaptureItem, (i32, i32))>, String>;

pub struct ScreenCapture {
    backend_name: &'static str,
    control: Option<Control>,
    dxgi_worker: Option<DxgiCaptureWorker>,
    latest_frame: LatestFrame,
    performance: Arc<CapturePerformanceCounters>,
    fallback_reason: Arc<Mutex<Option<String>>>,
    source_closed: Arc<AtomicBool>,
    picker_owner: Option<PickerThreadOwner>,
}

struct DxgiCaptureWorker {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<Result<(), String>>>,
}

pub struct PendingScreenCapture {
    result: Receiver<PickResult>,
    picker_owner: Option<PickerThreadOwner>,
    preview_enabled: Arc<AtomicBool>,
}

struct PickerThreadOwner {
    release: Option<SyncSender<()>>,
    thread: Option<JoinHandle<()>>,
}

struct PickerSelection {
    item: windows_capture::GraphicsCaptureItem,
    size: (i32, i32),
    owner: PickerThreadOwner,
}

impl PendingScreenCapture {
    pub fn begin(preview_enabled: Arc<AtomicBool>) -> Result<Self, String> {
        let (result_tx, result) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        // windows-capture initializes WinRT in MTA mode; the eframe UI thread may use another mode.
        let picker_thread = thread::Builder::new()
            .name("windows-screen-picker".to_owned())
            .spawn(move || match GraphicsCapturePicker::pick_item() {
                Ok(Some(selected)) => {
                    // GraphicsCaptureItem.Size is apartment-bound. Read it on the picker thread,
                    // where the WinRT item was created, before handing the item to capture setup.
                    match selected.item.Size().map(|size| (size.Width, size.Height)) {
                        Ok(size) => {
                            tracing::info!(width = size.0, height = size.1, "Seletor do Windows escolheu uma tela ou janela");
                            if result_tx
                                .send(Ok(Some((selected.item.clone(), size))))
                                .is_ok()
                            {
                                // Keep the picker owner window and WinRT apartment on their creating thread.
                                let _ = release_rx.recv();
                            }
                        }
                        Err(error) => {
                            tracing::error!(error = %error, "Não foi possível consultar dimensão do item de captura");
                            let _ = result_tx.send(Err(format!(
                                "Não foi possível consultar o tamanho da tela ou janela: {error}"
                            )));
                        }
                    }
                    drop(selected);
                }
                Ok(None) | Err(PickerError::Canceled) => {
                    tracing::info!("Seletor de tela cancelado; captura não iniciada");
                    let _ = result_tx.send(Ok(None));
                }
                Err(error) => {
                    tracing::error!(error = %error, "Seletor do Windows falhou");
                    let _ = result_tx.send(Err(format!("O seletor do Windows falhou: {error}")));
                }
            })
            .map_err(|error| format!("Não foi possível abrir o seletor do Windows: {error}"))?;

        Ok(Self {
            result,
            picker_owner: Some(PickerThreadOwner {
                release: Some(release_tx),
                thread: Some(picker_thread),
            }),
            preview_enabled,
        })
    }

    pub fn poll(
        &mut self,
        context: egui::Context,
    ) -> Option<Result<Option<ScreenCapture>, String>> {
        let result = match self.result.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => {
                Err("O seletor do Windows foi encerrado inesperadamente.".to_owned())
            }
        };
        let picker_owner = self.picker_owner.take();

        match result {
            Ok(Some((item, size))) => {
                let Some(owner) = picker_owner else {
                    return Some(Err("Os recursos do seletor já foram liberados.".to_owned()));
                };
                Some(
                    ScreenCapture::start_selected(
                        PickerSelection { item, size, owner },
                        context,
                        Arc::clone(&self.preview_enabled),
                    )
                    .map(Some),
                )
            }
            Ok(None) => {
                if let Some(owner) = picker_owner {
                    owner.stop();
                }
                Some(Ok(None))
            }
            Err(error) => {
                if let Some(owner) = picker_owner {
                    owner.stop();
                }
                Some(Err(error))
            }
        }
    }
}

impl Drop for PickerThreadOwner {
    fn drop(&mut self) {
        self.release();
    }
}

impl PickerThreadOwner {
    fn release(&mut self) {
        if let Some(release) = self.release.take() {
            let _ = release.send(());
        }
    }

    fn stop(mut self) {
        self.release();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl ScreenCapture {
    pub fn monitors() -> Result<Vec<MonitorOption>, String> {
        let monitors = Monitor::enumerate().map_err(|error| {
            format!("NÃ£o foi possÃ­vel enumerar monitores para captura DXGI: {error}")
        })?;
        let primary = Monitor::primary().ok();
        monitors
            .into_iter()
            .enumerate()
            .map(|(index, monitor)| {
                let device_id = monitor
                    .device_name()
                    .map_err(|_| format!("Não foi possível identificar o monitor {}", index + 1))?;
                Ok(MonitorOption {
                    name: monitor
                        .name()
                        .unwrap_or_else(|_| format!("Monitor {}", index + 1)),
                    device_id,
                    width: monitor.width().map_err(|error| error.to_string())?,
                    height: monitor.height().map_err(|error| error.to_string())?,
                    is_primary: primary == Some(monitor),
                })
            })
            .collect()
    }

    pub fn start_monitor(
        device_id: &str,
        context: egui::Context,
        preview_enabled: Arc<AtomicBool>,
    ) -> Result<Self, String> {
        let monitor = Monitor::enumerate()
            .map_err(|error| {
                format!("Não foi possível atualizar a lista de monitores DXGI: {error}")
            })?
            .into_iter()
            .find(|monitor| {
                monitor
                    .device_name()
                    .is_ok_and(|current_id| current_id == device_id)
            })
            .ok_or_else(|| {
                format!(
                    "O monitor {device_id} não está mais disponível. Atualize a lista e escolha outro monitor."
                )
            })?;
        let latest_frame = LatestFrame::default();
        let performance = Arc::new(CapturePerformanceCounters::default());
        let fallback_reason = Arc::new(Mutex::new(None));
        let source_closed = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let worker_frame = latest_frame.clone();
        let worker_performance = Arc::clone(&performance);
        let worker_fallback_reason = Arc::clone(&fallback_reason);
        let worker_preview_enabled = Arc::clone(&preview_enabled);
        let thread = thread::Builder::new()
            .name("dxgi-monitor-capture".to_owned())
            .spawn(move || {
                dxgi_backend::capture_dxgi_monitor(
                    monitor,
                    context,
                    worker_frame,
                    worker_performance,
                    worker_fallback_reason,
                    worker_stop,
                    worker_preview_enabled,
                )
            })
            .map_err(|error| format!("NÃ£o foi possÃ­vel iniciar a thread DXGI: {error}"))?;

        tracing::info!(monitor_device_id = %device_id, "Captura DXGI do monitor iniciada");
        Ok(Self {
            backend_name: "DXGI Desktop Duplication",
            control: None,
            dxgi_worker: Some(DxgiCaptureWorker {
                stop,
                thread: Some(thread),
            }),
            latest_frame,
            performance,
            fallback_reason,
            source_closed,
            picker_owner: None,
        })
    }

    fn start_selected(
        selected: PickerSelection,
        context: egui::Context,
        preview_enabled: Arc<AtomicBool>,
    ) -> Result<Self, String> {
        let size = selected.size;
        if size.0 <= 0 || size.1 <= 0 {
            tracing::error!(
                width = size.0,
                height = size.1,
                "Item selecionado tem tamanho inválido"
            );
            return Err("A tela ou janela selecionada tem tamanho inválido.".to_owned());
        }
        let latest_frame = LatestFrame::default();
        let performance = Arc::new(CapturePerformanceCounters::default());
        let fallback_reason = Arc::new(Mutex::new(None));
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
                latest_frame: latest_frame.clone(),
                performance: Arc::clone(&performance),
                source_closed: Arc::clone(&source_closed),
                preview_enabled,
            },
        );
        let control =
            wgc_backend::ScreenFrameHandler::start_free_threaded(settings).map_err(|error| {
                tracing::error!(error = %error, "Windows Graphics Capture não iniciou");
                format!("O Windows não iniciou a captura da tela: {error}")
            })?;
        tracing::info!(
            width = size.0,
            height = size.1,
            "Captura local da tela iniciada"
        );

        Ok(Self {
            backend_name: "Windows Graphics Capture",
            control: Some(control),
            dxgi_worker: None,
            latest_frame,
            performance,
            fallback_reason,
            source_closed,
            picker_owner: Some(selected.owner),
        })
    }

    pub fn latest_frame(&self) -> Option<Arc<PreviewFrame>> {
        self.latest_frame.latest()
    }

    pub fn backend_name(&self) -> &'static str {
        self.backend_name
    }

    pub fn frame_source(&self) -> LatestFrame {
        self.latest_frame.clone()
    }

    pub(crate) fn take_performance_snapshot(&self) -> CapturePerformanceSnapshot {
        self.performance.take_snapshot()
    }

    pub fn fallback_reason(&self) -> Option<String> {
        self.fallback_reason
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub fn source_closed(&self) -> bool {
        self.source_closed.load(Ordering::Relaxed)
    }

    pub fn poll_finished(&mut self) -> Option<Result<(), String>> {
        if let Some(worker) = self.dxgi_worker.as_mut() {
            let thread = worker.thread.as_ref()?;
            if !thread.is_finished() {
                return None;
            }
            let mut worker = self.dxgi_worker.take().expect("DXGI capture worker exists");
            return Some(join_dxgi_worker(&mut worker));
        }
        let control = self.control.as_ref()?;
        if !control.is_finished() {
            return None;
        }
        let control = self
            .control
            .take()
            .expect("capture control was just checked");
        let result = control.wait().map_err(|error| format!("{error}"));
        self.stop_picker_owner();
        Some(result)
    }

    pub fn stop(&mut self) -> Result<(), String> {
        let worker_result = if let Some(mut worker) = self.dxgi_worker.take() {
            worker.stop.store(true, Ordering::Relaxed);
            join_dxgi_worker(&mut worker)
        } else {
            Ok(())
        };
        let result = if let Some(control) = self.control.take() {
            control.stop().map_err(|error| format!("{error}"))
        } else {
            Ok(())
        };
        self.stop_picker_owner();
        result.and(worker_result)
    }

    fn stop_picker_owner(&mut self) {
        if let Some(owner) = self.picker_owner.take() {
            owner.stop();
        }
    }
}

fn join_dxgi_worker(worker: &mut DxgiCaptureWorker) -> Result<(), String> {
    worker.stop.store(true, Ordering::Relaxed);
    let Some(thread) = worker.thread.take() else {
        return Ok(());
    };
    thread
        .join()
        .map_err(|_| "A thread de captura DXGI foi encerrada inesperadamente.".to_owned())?
}

impl Drop for ScreenCapture {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

struct HandlerFlags {
    _size: (i32, i32),
    context: egui::Context,
    latest_frame: LatestFrame,
    performance: Arc<CapturePerformanceCounters>,
    source_closed: Arc<AtomicBool>,
    preview_enabled: Arc<AtomicBool>,
}

// windows-capture's picker wrapper owns an HWND guard and is therefore not Send.
// Keep it on its dedicated thread and send only the WinRT capture item to the worker.
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

#[derive(Default)]
struct FrameRateLimiter {
    next_deadline: Option<Instant>,
}

impl FrameRateLimiter {
    fn should_process(&mut self, now: Instant) -> bool {
        if let Some(deadline) = self.next_deadline {
            if now + CAPTURE_PACING_JITTER_TOLERANCE < deadline {
                return false;
            }
            // Keep the target cadence anchored to the original timeline. A small early
            // tolerance absorbs callback jitter; late callbacks skip missed slots instead
            // of moving the cadence and causing alternating frame drops.
            let mut next_deadline = deadline + MIN_CAPTURE_FRAME_INTERVAL;
            while next_deadline <= now {
                next_deadline += MIN_CAPTURE_FRAME_INTERVAL;
            }
            self.next_deadline = Some(next_deadline);
        } else {
            self.next_deadline = Some(now + MIN_CAPTURE_FRAME_INTERVAL);
        }
        true
    }
}

fn downsample_rgba(bytes: &[u8], width: u32, height: u32, sequence: u64) -> PreviewFrame {
    let (out_width, out_height) = scaled_dimensions(width, height);
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

fn same_preview_pixels(previous: Option<&PreviewFrame>, candidate: &PreviewFrame) -> bool {
    previous.is_some_and(|previous| {
        if previous.width != candidate.width || previous.height != candidate.height {
            return false;
        }
        if !previous.rgba.is_empty() || !candidate.rgba.is_empty() {
            return previous.rgba == candidate.rgba;
        }
        #[cfg(windows)]
        if let (Some(previous), Some(candidate)) =
            (previous.cpu_nv12.as_ref(), candidate.cpu_nv12.as_ref())
        {
            return previous.stride == candidate.stride && previous.bytes == candidate.bytes;
        }
        false
    })
}

fn scaled_dimensions(width: u32, height: u32) -> (u32, u32) {
    let scale = (MAX_FRAME_WIDTH as f32 / width as f32)
        .min(MAX_FRAME_HEIGHT as f32 / height as f32)
        .min(1.0);
    (
        even_dimension((width as f32 * scale).round() as u32),
        even_dimension((height as f32 * scale).round() as u32),
    )
}

fn even_dimension(value: u32) -> u32 {
    value.max(2) & !1
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::{
        FrameRateLimiter, MIN_CAPTURE_FRAME_INTERVAL, MonitorOption, PreviewFrame,
        same_preview_pixels, scaled_dimensions,
    };

    #[test]
    fn monitor_labels_distinguish_same_name_and_resolution_by_device_id() {
        let primary = MonitorOption {
            device_id: r"\\.\DISPLAY1".to_owned(),
            name: "Monitor genérico PnP".to_owned(),
            width: 1920,
            height: 1080,
            is_primary: true,
        };
        let secondary = MonitorOption {
            device_id: r"\\.\DISPLAY2".to_owned(),
            is_primary: false,
            ..primary.clone()
        };

        let primary_label = primary.label(1);
        let secondary_label = secondary.label(2);
        assert_ne!(primary_label, secondary_label);
        assert!(primary_label.contains("DISPLAY1"));
        assert!(primary_label.contains("principal"));
        assert!(secondary_label.contains("DISPLAY2"));
        assert!(!secondary_label.contains("principal"));
    }

    #[test]
    fn capture_limiter_accepts_first_frame_and_caps_at_30_fps() {
        let start = Instant::now();
        let mut limiter = FrameRateLimiter::default();

        assert!(limiter.should_process(start));
        assert!(!limiter.should_process(start + Duration::from_millis(16)));
        assert!(limiter.should_process(start + MIN_CAPTURE_FRAME_INTERVAL));
    }

    #[test]
    fn capture_limiter_does_not_alternate_at_30_fps_with_small_jitter() {
        let start = Instant::now();
        let mut limiter = FrameRateLimiter::default();
        let mut accepted = 0;
        for frame in 0..90u64 {
            let jitter_ns = match frame % 3 {
                0 => 0,
                1 => 180_000,
                _ => -90_000i64,
            };
            let base = start + Duration::from_nanos(frame * 33_333_333);
            let time = if jitter_ns < 0 {
                base - Duration::from_nanos(jitter_ns.unsigned_abs())
            } else {
                base + Duration::from_nanos(jitter_ns as u64)
            };
            accepted += usize::from(limiter.should_process(time));
        }
        assert!(accepted >= 88, "accepted {accepted} of 90 30-FPS callbacks");
    }

    #[test]
    fn capture_limiter_caps_60_fps_without_bursts_and_keeps_15_fps() {
        let start = Instant::now();
        let mut limiter_60 = FrameRateLimiter::default();
        let accepted_60 = (0..180u64)
            .filter(|frame| {
                limiter_60.should_process(start + Duration::from_nanos(frame * 16_666_667))
            })
            .count();
        assert!(
            (89..=91).contains(&accepted_60),
            "accepted {accepted_60} frames at 60 FPS"
        );

        let mut limiter_15 = FrameRateLimiter::default();
        let accepted_15 = (0..45u64)
            .filter(|frame| {
                limiter_15.should_process(start + Duration::from_nanos(frame * 66_666_667))
            })
            .count();
        assert_eq!(accepted_15, 45);
    }

    #[test]
    fn capture_frames_fit_the_720p_h264_limit() {
        assert_eq!(scaled_dimensions(3840, 2160), (1280, 720));
        assert_eq!(scaled_dimensions(2560, 1080), (1280, 540));
    }

    #[test]
    fn capture_dimensions_are_even_for_h264() {
        let (width, height) = scaled_dimensions(1365, 767);
        assert_eq!(width % 2, 0);
        assert_eq!(height % 2, 0);
        assert!(width <= 1280);
        assert!(height <= 720);
    }

    #[test]
    fn identical_preview_pixels_are_detected_without_comparing_sequence() {
        let first = PreviewFrame {
            sequence: 1,
            width: 2,
            height: 2,
            rgba: [1, 2, 3, 255].repeat(4),
            #[cfg(windows)]
            gpu_nv12: None,
            #[cfg(windows)]
            cpu_nv12: None,
        };
        let identical = PreviewFrame {
            sequence: 2,
            ..first.clone()
        };
        let changed = PreviewFrame {
            rgba: [9, 2, 3, 255].repeat(4),
            ..identical.clone()
        };

        assert!(same_preview_pixels(Some(&first), &identical));
        assert!(!same_preview_pixels(Some(&first), &changed));
        assert!(!same_preview_pixels(None, &first));
    }

    #[cfg(windows)]
    #[test]
    fn hidden_preview_uses_nv12_to_detect_static_dxgi_frames() {
        use std::sync::Arc;

        use super::CpuNv12Frame;

        let first = PreviewFrame {
            sequence: 1,
            width: 2,
            height: 2,
            rgba: Vec::new(),
            gpu_nv12: None,
            cpu_nv12: Some(Arc::new(CpuNv12Frame {
                bytes: Arc::new(vec![16, 16, 16, 16, 128, 128]),
                stride: 2,
            })),
        };
        let identical = PreviewFrame {
            sequence: 2,
            ..first.clone()
        };
        let changed = PreviewFrame {
            cpu_nv12: Some(Arc::new(CpuNv12Frame {
                bytes: Arc::new(vec![32, 16, 16, 16, 128, 128]),
                stride: 2,
            })),
            ..identical.clone()
        };

        assert!(same_preview_pixels(Some(&first), &identical));
        assert!(!same_preview_pixels(Some(&first), &changed));
    }
}
