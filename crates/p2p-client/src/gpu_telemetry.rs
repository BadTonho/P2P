//! Windows engine utilization belongs to the process, not to one media session.
//! Independent engines are kept separate; percentages are never added together.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

#[derive(Default)]
struct Samples {
    status: String,
    updated: Option<Instant>,
    windows: VecDeque<BTreeMap<String, f64>>,
    interval_seconds: Option<f64>,
}

impl Samples {
    fn update(&mut self, sample: Result<BTreeMap<String, f64>, String>, interval: f64) {
        self.updated = Some(Instant::now());
        self.interval_seconds = Some(interval);
        match sample {
            Ok(engines) if !engines.is_empty() => {
                self.status = "available".to_owned();
                self.windows.push_back(engines);
                while self.windows.len() > 5 {
                    self.windows.pop_front();
                }
            }
            Ok(_) => {
                self.status = "no_valid_process_instances; percentage=unavailable".to_owned();
                self.windows.clear();
            }
            Err(error) => {
                self.status = error;
                self.windows.clear();
            }
        }
    }

    fn label(&self) -> String {
        let mut engines: BTreeMap<&str, Vec<f64>> = BTreeMap::new();
        for sample in &self.windows {
            for (engine, percentage) in sample {
                engines.entry(engine).or_default().push(*percentage);
            }
        }
        let engines = engines
            .into_iter()
            .map(|(engine, values)| {
                let mean = values.iter().sum::<f64>() / values.len() as f64;
                let max = values.iter().copied().fold(0.0, f64::max);
                format!(
                    "{engine}: mean_percent={mean:.2}, max_percent={max:.2}, samples={}",
                    values.len()
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
        format!(
            "source=Windows_PDH, scope=process, pid={}, status={}, sample_interval_seconds={:?}, age_ms={:?}, engines=[{engines}]",
            std::process::id(),
            self.status,
            self.interval_seconds,
            self.updated.map(|updated| updated.elapsed().as_millis())
        )
    }
}

pub(crate) struct GpuTelemetry {
    samples: Arc<Mutex<Samples>>,
    stop: mpsc::Sender<()>,
    worker: Option<JoinHandle<()>>,
}

impl GpuTelemetry {
    pub fn start() -> Self {
        Self::start_with(NativeSampler::new, Duration::from_secs(1))
    }

    fn start_with<F, P>(factory: F, interval: Duration) -> Self
    where
        F: FnOnce() -> Result<P, String> + Send + 'static,
        P: Sampler + 'static,
    {
        let samples = Arc::new(Mutex::new(Samples {
            status: "warming_up; percentage=unavailable".to_owned(),
            ..Default::default()
        }));
        let (stop, stopping) = mpsc::channel();
        let shared = Arc::clone(&samples);
        let worker = thread::Builder::new()
            .name("p2p-gpu-metrics".to_owned())
            .spawn(move || {
                let mut provider = match factory() {
                    Ok(provider) => provider,
                    Err(error) => {
                        shared
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .update(Err(error), 0.0);
                        return;
                    }
                };
                let mut last_sample = Instant::now();
                while matches!(
                    stopping.recv_timeout(interval),
                    Err(mpsc::RecvTimeoutError::Timeout)
                ) {
                    let now = Instant::now();
                    let result = provider.sample();
                    shared
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .update(result, now.duration_since(last_sample).as_secs_f64());
                    last_sample = now;
                }
            });
        let worker = match worker {
            Ok(worker) => Some(worker),
            Err(error) => {
                samples
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .status = format!("worker_unavailable: {}", error.kind());
                None
            }
        };
        Self {
            samples,
            stop,
            worker,
        }
    }

    pub fn label(&self) -> String {
        self.samples
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .label()
    }
}

impl Drop for GpuTelemetry {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        // Native performance providers can be slow. Media/UI never wait for them.
        // The worker closes its query after the current call and releases its Arc.
        if let Some(worker) = self.worker.take()
            && worker.is_finished()
        {
            let _ = worker.join();
        }
    }
}

trait Sampler {
    fn sample(&mut self) -> Result<BTreeMap<String, f64>, String>;
}

fn process_engine(instance: &str, pid: u32, percentage: f64) -> Option<(String, f64)> {
    let (instance_pid, engine) = instance.strip_prefix("pid_")?.split_once('_')?;
    if instance_pid.parse::<u32>().ok()? != pid
        || !percentage.is_finite()
        || !(0.0..=100.0).contains(&percentage)
        || engine.len() > 255
        || !engine.contains("engtype_")
        || !engine
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '#'))
    {
        return None;
    }
    Some((engine.to_owned(), percentage))
}

#[cfg(windows)]
#[path = "gpu_telemetry/windows.rs"]
mod windows_sampler;
#[cfg(windows)]
use windows_sampler::NativeSampler;

#[cfg(not(windows))]
struct NativeSampler;
#[cfg(not(windows))]
impl NativeSampler {
    fn new() -> Result<Self, String> {
        Err("unsupported_platform; percentage=unavailable".to_owned())
    }
}
#[cfg(not(windows))]
impl Sampler for NativeSampler {
    fn sample(&mut self) -> Result<BTreeMap<String, f64>, String> {
        unreachable!()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn engines_filter_exact_pid_validate_values_and_keep_adapters_separate() {
        let instance = "pid_12_luid_0x0_0x123_phys_0_eng_1_engtype_VideoEncode";
        assert!(process_engine(instance, 12, 0.0).is_some());
        assert!(process_engine(instance, 1, 10.0).is_none());
        for value in [-1.0, 101.0, f64::NAN, f64::INFINITY] {
            assert!(process_engine(instance, 12, value).is_none());
        }
        assert!(process_engine("pid_12_PRIVATE ROOM CODE", 12, 10.0).is_none());
        let mut samples = Samples::default();
        let first = process_engine(instance, 12, 60.0).unwrap();
        let second = process_engine(&instance.replace("0x123", "0x456"), 12, 80.0).unwrap();
        samples.update(Ok(BTreeMap::from([first, second])), 1.0);
        let label = samples.label();
        assert!(label.contains("0x123"));
        assert!(label.contains("0x456"));
        assert!(!label.contains("140.00"));
    }

    #[test]
    fn missing_counters_clear_stale_percentages_and_window_is_bounded() {
        let mut samples = Samples::default();
        for n in 0..8 {
            samples.update(
                Ok(BTreeMap::from([("VideoDecode".to_owned(), n as f64)])),
                1.0,
            );
        }
        assert_eq!(samples.windows.len(), 5);
        assert!(samples.label().contains("mean_percent=5.00"));
        samples.update(Err("counter_unavailable".to_owned()), 1.0);
        assert!(!samples.label().contains("mean_percent"));
        samples.update(Ok(BTreeMap::new()), 1.0);
        assert!(samples.label().contains("percentage=unavailable"));
        samples.update(Ok(BTreeMap::from([("VideoDecode".to_owned(), 0.0)])), 1.0);
        assert!(samples.label().contains("status=available"));
        assert!(samples.label().contains("mean_percent=0.00"));
    }

    #[test]
    fn stopping_monitor_releases_provider_without_waiting_for_sample_interval() {
        struct Fake(Arc<AtomicBool>);
        impl Drop for Fake {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Release);
            }
        }
        impl Sampler for Fake {
            fn sample(&mut self) -> Result<BTreeMap<String, f64>, String> {
                Ok(BTreeMap::new())
            }
        }
        let released = Arc::new(AtomicBool::new(false));
        let cloned = Arc::clone(&released);
        let monitor = GpuTelemetry::start_with(move || Ok(Fake(cloned)), Duration::from_secs(30));
        drop(monitor);
        let deadline = Instant::now() + Duration::from_secs(3);
        while !released.load(Ordering::Acquire) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(1));
        }
        assert!(released.load(Ordering::Acquire));
    }

    #[test]
    fn startup_failure_is_explicit_and_does_not_invent_zero_utilization() {
        let monitor = GpuTelemetry::start_with(
            || -> Result<NativeSampler, String> {
                Err(
                    "counter_unavailable: add_gpu_engine_counter; percentage=unavailable"
                        .to_owned(),
                )
            },
            Duration::from_secs(1),
        );
        let deadline = Instant::now() + Duration::from_secs(3);
        while !monitor.worker.as_ref().unwrap().is_finished() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(1));
        }
        assert!(monitor.worker.as_ref().unwrap().is_finished());
        let label = monitor.label();
        assert!(label.contains("counter_unavailable: add_gpu_engine_counter"));
        assert!(label.contains("percentage=unavailable"));
        assert!(!label.contains("mean_percent"));
    }
}
