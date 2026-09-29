use super::*;

pub(super) struct ScreenFrameHandler {
    context: egui::Context,
    latest_frame: LatestFrame,
    performance: Arc<CapturePerformanceCounters>,
    source_closed: Arc<AtomicBool>,
    preview_enabled: Arc<AtomicBool>,
    scratch: Vec<u8>,
    sequence: u64,
    frame_rate_limiter: FrameRateLimiter,
}

impl GraphicsCaptureApiHandler for ScreenFrameHandler {
    type Flags = HandlerFlags;
    type Error = HandlerError;

    fn new(context: Context<Self::Flags>) -> Result<Self, Self::Error> {
        Ok(Self {
            context: context.flags.context,
            latest_frame: context.flags.latest_frame,
            performance: context.flags.performance,
            source_closed: context.flags.source_closed,
            preview_enabled: context.flags.preview_enabled,
            scratch: Vec::new(),
            sequence: 0,
            frame_rate_limiter: FrameRateLimiter::default(),
        })
    }

    fn on_frame_arrived(
        &mut self,
        frame: &mut Frame,
        _capture_control: InternalCaptureControl,
    ) -> Result<(), Self::Error> {
        self.performance
            .received_frames
            .fetch_add(1, Ordering::Relaxed);
        if !self.frame_rate_limiter.should_process(Instant::now()) {
            self.performance
                .skipped_frames
                .fetch_add(1, Ordering::Relaxed);
            return Ok(());
        }

        let width = frame.width();
        let height = frame.height();
        if width == 0 || height == 0 {
            return Ok(());
        }

        let readback_started_at = Instant::now();
        let frame_buffer = frame.buffer()?;
        let rgba = frame_buffer.as_nopadding_buffer(&mut self.scratch);
        self.performance.readback_nanos.fetch_add(
            readback_started_at.elapsed().as_nanos() as u64,
            Ordering::Relaxed,
        );
        let next_sequence = self.sequence.wrapping_add(1);
        let resize_started_at = Instant::now();
        let preview = downsample_rgba(rgba, width, height, next_sequence);
        self.performance.resize_nanos.fetch_add(
            resize_started_at.elapsed().as_nanos() as u64,
            Ordering::Relaxed,
        );
        if same_preview_pixels(self.latest_frame.latest().as_deref(), &preview) {
            self.performance
                .unchanged_frames
                .fetch_add(1, Ordering::Relaxed);
            return Ok(());
        }
        self.sequence = next_sequence;
        self.latest_frame.publish(preview);
        self.performance
            .processed_frames
            .fetch_add(1, Ordering::Relaxed);
        if self.preview_enabled.load(Ordering::Relaxed) {
            self.context.request_repaint();
        }
        Ok(())
    }

    fn on_closed(&mut self) -> Result<(), Self::Error> {
        self.source_closed.store(true, Ordering::Relaxed);
        self.context.request_repaint();
        Ok(())
    }
}
