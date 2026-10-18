//! Wall-clock measurements of application calls, not GPU execution timestamps.
//! Fixed-size counters keep diagnostics independent of frame/payload contents.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone, Copy)]
#[repr(usize)]
pub(crate) enum Stage {
    CaptureGpuConvert,
    CaptureReadback,
    CaptureCpuConvert,
    EncoderInit,
    EncoderReadback,
    EncoderCpuConvert,
    EncoderSubmit,
    EncoderOutputWait,
    CpuEncode,
    DecoderInit,
    DecoderQueueWait,
    DecoderSubmit,
    DecoderOutputWait,
    DecoderReadback,
    DecoderCpuConvert,
    CpuDecode,
    DecodeTotal,
    UiRgbaConvert,
    UiTextureUpdate,
    UiNv12Upload,
    UiNv12Draw,
}

const STAGES: [(Stage, &str); 21] = [
    (Stage::CaptureGpuConvert, "capture_gpu_convert_call"),
    (Stage::CaptureReadback, "capture_readback"),
    (Stage::CaptureCpuConvert, "capture_cpu_convert_resize"),
    (Stage::EncoderInit, "encoder_init"),
    (Stage::EncoderReadback, "encoder_readback"),
    (Stage::EncoderCpuConvert, "encoder_cpu_convert_pack"),
    (Stage::EncoderSubmit, "encoder_mf_submit"),
    (Stage::EncoderOutputWait, "encoder_mf_output_wait"),
    (Stage::CpuEncode, "encoder_openh264_call"),
    (Stage::DecoderInit, "decoder_init"),
    (Stage::DecoderQueueWait, "decoder_queue_wait"),
    (Stage::DecoderSubmit, "decoder_mf_submit"),
    (Stage::DecoderOutputWait, "decoder_mf_output_wait"),
    (Stage::DecoderReadback, "decoder_readback"),
    (Stage::DecoderCpuConvert, "decoder_cpu_color_convert"),
    (Stage::CpuDecode, "decoder_openh264_call"),
    (Stage::DecodeTotal, "decode_total"),
    (Stage::UiRgbaConvert, "ui_rgba_image_conversion"),
    (Stage::UiTextureUpdate, "ui_texture_update_enqueue"),
    (Stage::UiNv12Upload, "ui_nv12_upload_call"),
    (Stage::UiNv12Draw, "ui_nv12_shader_draw_call"),
];

#[derive(Clone, Copy)]
#[repr(usize)]
pub(crate) enum FramePath {
    CaptureGpuNv12,
    CaptureCpuRgba,
    EncoderGpuSurface,
    EncoderHardwareCpuNv12,
    EncoderHardwareCpuRgba,
    EncoderOpenH264,
    DecoderDxva,
    DecoderOpenH264,
    UiNv12Shader,
    UiRgbaFallback,
}

const PATHS: [(FramePath, &str); 10] = [
    (FramePath::CaptureGpuNv12, "capture_gpu_nv12"),
    (FramePath::CaptureCpuRgba, "capture_cpu_rgba"),
    (FramePath::EncoderGpuSurface, "encoder_hw_gpu_surface"),
    (FramePath::EncoderHardwareCpuNv12, "encoder_hw_cpu_nv12"),
    (FramePath::EncoderHardwareCpuRgba, "encoder_hw_cpu_rgba"),
    (FramePath::EncoderOpenH264, "encoder_openh264"),
    (FramePath::DecoderDxva, "decoder_dxva"),
    (FramePath::DecoderOpenH264, "decoder_openh264"),
    (FramePath::UiNv12Shader, "ui_nv12_shader"),
    (FramePath::UiRgbaFallback, "ui_rgba_fallback"),
];

#[derive(Clone, Copy)]
#[repr(usize)]
pub(crate) enum Readback {
    Capture,
    Encoder,
    Decoder,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Timing {
    pub samples: u64,
    pub total_nanos: u64,
    pub max_nanos: u64,
}

impl Timing {
    fn record(&mut self, duration: Duration) {
        let nanos = duration.as_nanos().min(u128::from(u64::MAX)) as u64;
        self.samples = self.samples.saturating_add(1);
        self.total_nanos = self.total_nanos.saturating_add(nanos);
        self.max_nanos = self.max_nanos.max(nanos);
    }

    pub fn mean_ms(self) -> Option<f64> {
        (self.samples > 0).then(|| self.total_nanos as f64 / self.samples as f64 / 1_000_000.0)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct VideoPerformanceSnapshot {
    timings: [Timing; STAGES.len()],
    paths: [u64; PATHS.len()],
    readback_bytes: [u64; 3],
}

impl VideoPerformanceSnapshot {
    pub fn timing(&self, stage: Stage) -> Timing {
        self.timings[stage as usize]
    }

    pub fn path_count(&self, path: FramePath) -> u64 {
        self.paths[path as usize]
    }

    pub fn readback_bytes(&self, kind: Readback) -> u64 {
        self.readback_bytes[kind as usize]
    }

    #[cfg(test)]
    pub(crate) fn diagnostic_json(self) -> serde_json::Value {
        let timings = STAGES.iter().filter_map(|(stage, name)| {
            let timing = self.timing(*stage);
            (timing.samples > 0).then(|| {
                (
                    (*name).to_owned(),
                    serde_json::json!({
                        "samples": timing.samples,
                        "mean_ms": timing.mean_ms(),
                        "max_ms": timing.max_nanos as f64 / 1_000_000.0,
                        "total_ms": timing.total_nanos as f64 / 1_000_000.0,
                    }),
                )
            })
        });
        let paths = PATHS.iter().filter_map(|(path, name)| {
            let count = self.path_count(*path);
            (count > 0).then_some((*name, count))
        });
        serde_json::json!({
            "timings": serde_json::Map::from_iter(timings),
            "frame_paths": serde_json::Map::from_iter(paths.map(|(name, count)| {
                (name.to_owned(), serde_json::Value::from(count))
            })),
            "readback_bytes": {
                "capture": self.readback_bytes(Readback::Capture),
                "encoder": self.readback_bytes(Readback::Encoder),
                "decoder": self.readback_bytes(Readback::Decoder),
            },
            "timing_note": "wall-clock call durations; GPU execution time is not measured",
        })
    }

    pub fn label(&self) -> String {
        let timings = STAGES.iter().filter_map(|(stage, name)| {
            let timing = self.timing(*stage);
            timing.mean_ms().map(|mean| {
                format!(
                    "{name}: samples={}, mean_ms={mean:.3}, max_ms={:.3}, total_ms={:.3}",
                    timing.samples,
                    timing.max_nanos as f64 / 1_000_000.0,
                    timing.total_nanos as f64 / 1_000_000.0,
                )
            })
        });
        let paths = PATHS.iter().filter_map(|(path, name)| {
            let count = self.path_count(*path);
            (count > 0).then(|| format!("{name}_attempts={count}"))
        });
        let mut fields = timings.chain(paths).collect::<Vec<_>>();
        if fields.is_empty() {
            return "sem amostras; tempos são de parede, não execução exclusiva da GPU".to_owned();
        }
        fields.push(format!(
            "readback_bytes_capture/encoder/decoder={}/{}/{}",
            self.readback_bytes(Readback::Capture),
            self.readback_bytes(Readback::Encoder),
            self.readback_bytes(Readback::Decoder)
        ));
        fields.join("; ")
    }
}

#[derive(Default)]
struct State {
    total: VideoPerformanceSnapshot,
    interval: VideoPerformanceSnapshot,
}

#[derive(Default)]
pub(crate) struct VideoPerformance {
    state: Mutex<State>,
}

impl VideoPerformance {
    pub fn record_duration(&self, stage: Stage, duration: Duration) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.total.timings[stage as usize].record(duration);
        state.interval.timings[stage as usize].record(duration);
    }

    pub fn record_path(&self, path: FramePath) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.total.paths[path as usize] += 1;
        state.interval.paths[path as usize] += 1;
    }

    pub fn record_readback(&self, kind: Readback, bytes: usize) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.total.readback_bytes[kind as usize] =
            state.total.readback_bytes[kind as usize].saturating_add(bytes as u64);
        state.interval.readback_bytes[kind as usize] =
            state.interval.readback_bytes[kind as usize].saturating_add(bytes as u64);
    }

    pub fn snapshot(&self) -> VideoPerformanceSnapshot {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .total
    }

    pub fn take_interval(&self) -> VideoPerformanceSnapshot {
        std::mem::take(
            &mut self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .interval,
        )
    }

    pub fn timer(self: &Arc<Self>, stage: Stage) -> StageTimer {
        StageTimer::optional(Some(self), stage)
    }
}

pub(crate) struct StageTimer {
    measurement: Option<(Arc<VideoPerformance>, Instant)>,
    stage: Stage,
}

impl StageTimer {
    pub fn optional(performance: Option<&Arc<VideoPerformance>>, stage: Stage) -> Self {
        Self {
            measurement: performance.map(|p| (Arc::clone(p), Instant::now())),
            stage,
        }
    }
}

impl Drop for StageTimer {
    fn drop(&mut self) {
        if let Some((performance, started)) = &self.measurement {
            performance.record_duration(self.stage, started.elapsed());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn measurements_preserve_totals_reset_intervals_and_separate_sessions() {
        let first = VideoPerformance::default();
        let second = VideoPerformance::default();
        first.record_duration(Stage::DecoderReadback, Duration::from_millis(2));
        first.record_duration(Stage::DecoderReadback, Duration::from_millis(8));
        first.record_path(FramePath::DecoderDxva);
        first.record_readback(Readback::Decoder, 300);
        let interval = first.take_interval();
        assert_eq!(interval.timing(Stage::DecoderReadback).mean_ms(), Some(5.0));
        assert_eq!(interval.timing(Stage::DecoderReadback).max_nanos, 8_000_000);
        assert_eq!(interval.path_count(FramePath::DecoderDxva), 1);
        assert_eq!(interval.readback_bytes(Readback::Decoder), 300);
        assert_eq!(first.take_interval(), VideoPerformanceSnapshot::default());
        assert_eq!(first.snapshot(), interval);
        assert_eq!(second.snapshot(), VideoPerformanceSnapshot::default());
        assert_eq!(interval.timing(Stage::CpuDecode).mean_ms(), None);
        assert!(interval.label().contains("mean_ms=5.000"));
        assert!(!interval.label().contains("decoder_openh264"));
    }

    #[test]
    fn timers_record_early_error_returns_without_payloads() {
        let performance = Arc::new(VideoPerformance::default());
        let call = || -> Result<(), &str> {
            let _timer = performance.timer(Stage::EncoderSubmit);
            Err("private payload")
        };
        assert!(call().is_err());
        let snapshot = performance.snapshot();
        assert_eq!(snapshot.timing(Stage::EncoderSubmit).samples, 1);
        assert!(!snapshot.label().contains("private"));
    }
}
