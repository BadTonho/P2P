use super::*;

pub(super) struct ScreenFrameHandler {
    context: egui::Context,
    latest_frame: LatestFrame,
    performance: Arc<CapturePerformanceCounters>,
    source_closed: Arc<AtomicBool>,
    preview_enabled: Arc<AtomicBool>,
    quality: ScreenShareQuality,
    scratch: Vec<u8>,
    sequence: u64,
    frame_rate_limiter: FrameRateLimiter,
    local_preview_limiter: LocalPreviewLimiter,
    gpu_processor: Option<(crate::mf_video::GpuNv12Processor, (u32, u32, u32, u32))>,
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
            quality: context.flags.quality,
            scratch: Vec::new(),
            sequence: 0,
            frame_rate_limiter: FrameRateLimiter::new(context.flags.quality),
            local_preview_limiter: LocalPreviewLimiter::default(),
            gpu_processor: None,
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
        if frame.dirty_regions()?.is_empty() {
            self.performance
                .unchanged_frames
                .fetch_add(1, Ordering::Relaxed);
            return Ok(());
        }

        // Windows Graphics Capture already provides a D3D11 texture. Keep the
        // frame on the GPU through resize/color conversion and into Media
        // Foundation instead of reading it back just to re-upload it.
        let (out_width, out_height) = scaled_dimensions(width, height, self.quality);
        let dimensions = (width, height, out_width, out_height);
        if self
            .gpu_processor
            .as_ref()
            .is_none_or(|(_, current)| *current != dimensions)
        {
            let processor = crate::mf_video::GpuNv12Processor::new(
                frame.device(),
                frame.device_context(),
                width,
                height,
                out_width,
                out_height,
                self.quality.frames_per_second(),
            )
            .map_err(|error| {
                std::io::Error::other(format!("GPU indisponível para captura WGC: {error}"))
            })?;
            self.gpu_processor = Some((processor, dimensions));
        }
        let gpu_started_at = Instant::now();
        let gpu_surface = self
            .gpu_processor
            .as_mut()
            .expect("GPU processor initialized")
            .0
            .process(frame.as_raw_texture())
            .map_err(|error| {
                std::io::Error::other(format!("Falha na conversão WGC pela GPU: {error}"))
            })?;
        self.performance
            .pipeline
            .record_path(crate::video_performance::FramePath::CaptureGpuNv12);
        self.performance.gpu_convert_nanos.fetch_add(
            gpu_started_at.elapsed().as_nanos() as u64,
            Ordering::Relaxed,
        );

        let next_sequence = self.sequence.wrapping_add(1);
        let wants_preview = self
            .local_preview_limiter
            .should_update(self.preview_enabled.load(Ordering::Relaxed), Instant::now());
        let preview = if wants_preview {
            let readback_started_at = Instant::now();
            let reading = self
                .performance
                .pipeline
                .timer(crate::video_performance::Stage::CaptureReadback);
            let frame_buffer = frame.buffer()?;
            let rgba = frame_buffer.as_nopadding_buffer(&mut self.scratch);
            drop(reading);
            self.performance
                .pipeline
                .record_readback(crate::video_performance::Readback::Capture, rgba.len());
            self.performance
                .pipeline
                .record_path(crate::video_performance::FramePath::CaptureCpuRgba);
            self.performance.readback_nanos.fetch_add(
                readback_started_at.elapsed().as_nanos() as u64,
                Ordering::Relaxed,
            );
            let conversion = self
                .performance
                .pipeline
                .timer(crate::video_performance::Stage::CaptureCpuConvert);
            let mut preview = downsample_rgba(rgba, width, height, next_sequence, self.quality);
            drop(conversion);
            preview.gpu_nv12 = Some(Arc::new(gpu_surface));
            preview
        } else {
            PreviewFrame {
                sequence: next_sequence,
                width: out_width,
                height: out_height,
                rgba: Vec::new(),
                #[cfg(windows)]
                gpu_nv12: Some(Arc::new(gpu_surface)),
                #[cfg(windows)]
                cpu_nv12: None,
            }
        };
        self.sequence = next_sequence;
        self.latest_frame.publish(preview);
        self.performance
            .processed_frames
            .fetch_add(1, Ordering::Relaxed);
        if wants_preview {
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
