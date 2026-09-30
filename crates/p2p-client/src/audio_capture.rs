use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

mod system_audio;
pub(crate) use system_audio::{
    OPUS_CHANNELS, OPUS_FRAME_SAMPLES_PER_CHANNEL, OPUS_SAMPLE_RATE, SystemAudioCapture,
    SystemAudioPlaybackFactory,
};

pub(crate) trait AudioSampleSource: Send {
    fn input_format(&self) -> (u32, usize);
    fn read_samples(&mut self, output: &mut [f32]) -> usize;
    fn callbacks(&self) -> u64;
    fn input_frames(&self) -> u64;
    fn non_silent_samples(&self) -> u64;
    fn captured_frames(&self) -> u64;
    fn dropped_frames(&self) -> u64;
    fn xruns(&self) -> u64;
    fn device_changes(&self) -> u64;
    fn realtime_denied(&self) -> u64;
    fn take_error(&self) -> Option<(cpal::ErrorKind, String)>;
}

pub(crate) trait AudioPlaybackSink: Send {
    fn push_decoded(&mut self, samples: &[f32], frames_per_channel: usize);
    fn output_underflow_frames(&self) -> u64;
    fn dropped_frames(&self) -> u64;
    fn callbacks(&self) -> u64;
    fn non_silent_samples(&self) -> u64;
    fn take_error(&self) -> Option<String>;
}

pub(crate) trait AudioPlaybackFactory: Send + Sync {
    fn start(&self, session_id: u64, ssrc: u32) -> Result<Box<dyn AudioPlaybackSink>, String>;
}

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use rtrb::{Consumer, Producer, RingBuffer};

const MAX_MONITOR_BUFFER_MS: usize = 100;
const PRIME_MONITOR_BUFFER_MS: usize = 20;

pub struct MicrophoneTest {
    _input_stream: cpal::Stream,
    monitor_stream: Option<cpal::Stream>,
    level: Arc<AtomicU32>,
    microphone_error: Arc<Mutex<Option<String>>>,
    monitor_error: Arc<Mutex<Option<String>>>,
    audio_warning: Arc<AtomicBool>,
    monitor_queue_enabled: Arc<AtomicBool>,
    monitor_gain: Arc<AtomicU32>,
    clipping_warning: Arc<AtomicBool>,
}

impl MicrophoneTest {
    pub fn start(monitor_gain_db: f32) -> Result<Self, String> {
        let host = cpal::default_host();
        let input_device = host.default_input_device().ok_or_else(|| {
            "Nenhum microfone padrão foi encontrado. Conecte um microfone ou escolha um como padrão nas configurações de Som do Windows.".to_owned()
        })?;
        let input_supported = input_device.default_input_config().map_err(|error| {
            format_microphone_error(format!(
                "Não foi possível consultar o microfone padrão: {error}"
            ))
        })?;
        let input_sample_format = input_supported.sample_format();
        let input_config: cpal::StreamConfig = input_supported.into();
        let input_rate = input_config.sample_rate;
        let input_channels = input_config.channels as usize;

        let level = Arc::new(AtomicU32::new(0.0_f32.to_bits()));
        let microphone_error = Arc::new(Mutex::new(None));
        let queue_capacity = ((input_rate as usize * MAX_MONITOR_BUFFER_MS) / 1000).max(1);
        let (producer, consumer) = RingBuffer::<f32>::new(queue_capacity);
        let overflow_pending = Arc::new(AtomicBool::new(false));
        let audio_warning = Arc::new(AtomicBool::new(false));
        let monitor_queue_enabled = Arc::new(AtomicBool::new(true));
        let monitor_gain = Arc::new(AtomicU32::new(
            gain_db_to_amplitude(monitor_gain_db).to_bits(),
        ));
        let clipping_warning = Arc::new(AtomicBool::new(false));

        let input_stream = match input_sample_format {
            cpal::SampleFormat::I8 => build_input_stream::<i8>(
                &input_device,
                input_config,
                input_channels,
                &level,
                &microphone_error,
                producer,
                &overflow_pending,
                &audio_warning,
                &monitor_queue_enabled,
            ),
            cpal::SampleFormat::I16 => build_input_stream::<i16>(
                &input_device,
                input_config,
                input_channels,
                &level,
                &microphone_error,
                producer,
                &overflow_pending,
                &audio_warning,
                &monitor_queue_enabled,
            ),
            cpal::SampleFormat::I24 => build_input_stream::<cpal::I24>(
                &input_device,
                input_config,
                input_channels,
                &level,
                &microphone_error,
                producer,
                &overflow_pending,
                &audio_warning,
                &monitor_queue_enabled,
            ),
            cpal::SampleFormat::I32 => build_input_stream::<i32>(
                &input_device,
                input_config,
                input_channels,
                &level,
                &microphone_error,
                producer,
                &overflow_pending,
                &audio_warning,
                &monitor_queue_enabled,
            ),
            cpal::SampleFormat::I64 => build_input_stream::<i64>(
                &input_device,
                input_config,
                input_channels,
                &level,
                &microphone_error,
                producer,
                &overflow_pending,
                &audio_warning,
                &monitor_queue_enabled,
            ),
            cpal::SampleFormat::U8 => build_input_stream::<u8>(
                &input_device,
                input_config,
                input_channels,
                &level,
                &microphone_error,
                producer,
                &overflow_pending,
                &audio_warning,
                &monitor_queue_enabled,
            ),
            cpal::SampleFormat::U16 => build_input_stream::<u16>(
                &input_device,
                input_config,
                input_channels,
                &level,
                &microphone_error,
                producer,
                &overflow_pending,
                &audio_warning,
                &monitor_queue_enabled,
            ),
            cpal::SampleFormat::U24 => build_input_stream::<cpal::U24>(
                &input_device,
                input_config,
                input_channels,
                &level,
                &microphone_error,
                producer,
                &overflow_pending,
                &audio_warning,
                &monitor_queue_enabled,
            ),
            cpal::SampleFormat::U32 => build_input_stream::<u32>(
                &input_device,
                input_config,
                input_channels,
                &level,
                &microphone_error,
                producer,
                &overflow_pending,
                &audio_warning,
                &monitor_queue_enabled,
            ),
            cpal::SampleFormat::U64 => build_input_stream::<u64>(
                &input_device,
                input_config,
                input_channels,
                &level,
                &microphone_error,
                producer,
                &overflow_pending,
                &audio_warning,
                &monitor_queue_enabled,
            ),
            cpal::SampleFormat::F32 => build_input_stream::<f32>(
                &input_device,
                input_config,
                input_channels,
                &level,
                &microphone_error,
                producer,
                &overflow_pending,
                &audio_warning,
                &monitor_queue_enabled,
            ),
            cpal::SampleFormat::F64 => build_input_stream::<f64>(
                &input_device,
                input_config,
                input_channels,
                &level,
                &microphone_error,
                producer,
                &overflow_pending,
                &audio_warning,
                &monitor_queue_enabled,
            ),
            _ => Err("O formato de áudio do microfone não é compatível com o medidor.".to_owned()),
        }?;

        let monitor_error = Arc::new(Mutex::new(None));
        let mut monitor_start_error = None;
        let mut monitor_stream = match host.default_output_device() {
            Some(output_device) => {
                let output_supported = match output_device.default_output_config() {
                    Ok(config) => Some(config),
                    Err(error) => {
                        monitor_start_error = Some(format!(
                            "Não foi possível consultar a saída padrão do Windows: {error}"
                        ));
                        None
                    }
                };

                if let Some(output_supported) = output_supported {
                    let output_sample_format = output_supported.sample_format();
                    let output_config: cpal::StreamConfig = output_supported.into();
                    let output_channels = output_config.channels as usize;
                    let stream = match output_sample_format {
                        cpal::SampleFormat::I8 => build_output_stream::<i8>(
                            &output_device,
                            output_config,
                            output_channels,
                            input_rate,
                            consumer,
                            &overflow_pending,
                            &audio_warning,
                            &monitor_queue_enabled,
                            &monitor_gain,
                            &clipping_warning,
                            &monitor_error,
                        ),
                        cpal::SampleFormat::I16 => build_output_stream::<i16>(
                            &output_device,
                            output_config,
                            output_channels,
                            input_rate,
                            consumer,
                            &overflow_pending,
                            &audio_warning,
                            &monitor_queue_enabled,
                            &monitor_gain,
                            &clipping_warning,
                            &monitor_error,
                        ),
                        cpal::SampleFormat::I24 => build_output_stream::<cpal::I24>(
                            &output_device,
                            output_config,
                            output_channels,
                            input_rate,
                            consumer,
                            &overflow_pending,
                            &audio_warning,
                            &monitor_queue_enabled,
                            &monitor_gain,
                            &clipping_warning,
                            &monitor_error,
                        ),
                        cpal::SampleFormat::I32 => build_output_stream::<i32>(
                            &output_device,
                            output_config,
                            output_channels,
                            input_rate,
                            consumer,
                            &overflow_pending,
                            &audio_warning,
                            &monitor_queue_enabled,
                            &monitor_gain,
                            &clipping_warning,
                            &monitor_error,
                        ),
                        cpal::SampleFormat::I64 => build_output_stream::<i64>(
                            &output_device,
                            output_config,
                            output_channels,
                            input_rate,
                            consumer,
                            &overflow_pending,
                            &audio_warning,
                            &monitor_queue_enabled,
                            &monitor_gain,
                            &clipping_warning,
                            &monitor_error,
                        ),
                        cpal::SampleFormat::U8 => build_output_stream::<u8>(
                            &output_device,
                            output_config,
                            output_channels,
                            input_rate,
                            consumer,
                            &overflow_pending,
                            &audio_warning,
                            &monitor_queue_enabled,
                            &monitor_gain,
                            &clipping_warning,
                            &monitor_error,
                        ),
                        cpal::SampleFormat::U16 => build_output_stream::<u16>(
                            &output_device,
                            output_config,
                            output_channels,
                            input_rate,
                            consumer,
                            &overflow_pending,
                            &audio_warning,
                            &monitor_queue_enabled,
                            &monitor_gain,
                            &clipping_warning,
                            &monitor_error,
                        ),
                        cpal::SampleFormat::U24 => build_output_stream::<cpal::U24>(
                            &output_device,
                            output_config,
                            output_channels,
                            input_rate,
                            consumer,
                            &overflow_pending,
                            &audio_warning,
                            &monitor_queue_enabled,
                            &monitor_gain,
                            &clipping_warning,
                            &monitor_error,
                        ),
                        cpal::SampleFormat::U32 => build_output_stream::<u32>(
                            &output_device,
                            output_config,
                            output_channels,
                            input_rate,
                            consumer,
                            &overflow_pending,
                            &audio_warning,
                            &monitor_queue_enabled,
                            &monitor_gain,
                            &clipping_warning,
                            &monitor_error,
                        ),
                        cpal::SampleFormat::U64 => build_output_stream::<u64>(
                            &output_device,
                            output_config,
                            output_channels,
                            input_rate,
                            consumer,
                            &overflow_pending,
                            &audio_warning,
                            &monitor_queue_enabled,
                            &monitor_gain,
                            &clipping_warning,
                            &monitor_error,
                        ),
                        cpal::SampleFormat::F32 => build_output_stream::<f32>(
                            &output_device,
                            output_config,
                            output_channels,
                            input_rate,
                            consumer,
                            &overflow_pending,
                            &audio_warning,
                            &monitor_queue_enabled,
                            &monitor_gain,
                            &clipping_warning,
                            &monitor_error,
                        ),
                        cpal::SampleFormat::F64 => build_output_stream::<f64>(
                            &output_device,
                            output_config,
                            output_channels,
                            input_rate,
                            consumer,
                            &overflow_pending,
                            &audio_warning,
                            &monitor_queue_enabled,
                            &monitor_gain,
                            &clipping_warning,
                            &monitor_error,
                        ),
                        _ => Err("O formato de áudio da saída padrão não é compatível com o monitoramento.".to_owned()),
                    };

                    match stream {
                        Ok(stream) => Some(stream),
                        Err(error) => {
                            monitor_start_error = Some(error);
                            None
                        }
                    }
                } else {
                    None
                }
            }
            None => {
                monitor_start_error = Some(
                    "Nenhum dispositivo de saída padrão foi encontrado. O medidor do microfone continuará funcionando.".to_owned(),
                );
                None
            }
        };

        input_stream.play().map_err(|error| {
            format_microphone_error(format!("O Windows não permitiu iniciar a captura: {error}"))
        })?;

        if let Some(stream) = monitor_stream.as_ref() {
            if let Err(error) = stream.play() {
                monitor_start_error = Some(format!(
                    "Não foi possível iniciar o retorno de áudio: {error}"
                ));
                monitor_stream = None;
            }
        }

        if monitor_stream.is_none() {
            monitor_queue_enabled.store(false, Ordering::Release);
        }

        if let Some(error) = monitor_start_error {
            *monitor_error
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(error);
        }

        Ok(Self {
            _input_stream: input_stream,
            monitor_stream,
            level,
            microphone_error,
            monitor_error,
            audio_warning,
            monitor_queue_enabled,
            monitor_gain,
            clipping_warning,
        })
    }

    pub fn level(&self) -> f32 {
        f32::from_bits(self.level.load(Ordering::Relaxed)).clamp(0.0, 1.0)
    }

    pub fn take_microphone_error(&self) -> Option<String> {
        take_shared_error(&self.microphone_error)
    }

    pub fn take_monitor_error(&self) -> Option<String> {
        take_shared_error(&self.monitor_error)
    }

    pub fn take_audio_warning(&self) -> bool {
        self.audio_warning.swap(false, Ordering::AcqRel)
    }

    pub fn set_monitor_gain_db(&self, gain_db: f32) {
        self.monitor_gain
            .store(gain_db_to_amplitude(gain_db).to_bits(), Ordering::Relaxed);
    }

    pub fn take_clipping_warning(&self) -> bool {
        self.clipping_warning.swap(false, Ordering::AcqRel)
    }

    pub fn stop_monitoring(&mut self) {
        self.monitor_queue_enabled.store(false, Ordering::Release);
        self.monitor_stream = None;
    }
}

fn gain_db_to_amplitude(gain_db: f32) -> f32 {
    10.0_f32.powf(gain_db.clamp(0.0, 18.0) / 20.0)
}

fn build_input_stream<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    channels: usize,
    level: &Arc<AtomicU32>,
    microphone_error: &Arc<Mutex<Option<String>>>,
    mut producer: Producer<f32>,
    overflow_pending: &Arc<AtomicBool>,
    audio_warning: &Arc<AtomicBool>,
    monitor_queue_enabled: &Arc<AtomicBool>,
) -> Result<cpal::Stream, String>
where
    T: cpal::SizedSample,
    f32: cpal::FromSample<T>,
{
    let level = Arc::clone(level);
    let callback_error = Arc::clone(microphone_error);
    let overflow_pending = Arc::clone(overflow_pending);
    let audio_warning = Arc::clone(audio_warning);
    let monitor_queue_enabled = Arc::clone(monitor_queue_enabled);
    let channels = channels.max(1);
    let mut mono_samples = Vec::new();
    device
        .build_input_stream::<T, _, _>(
            config,
            move |samples, _info| {
                if samples.is_empty() {
                    return;
                }

                let mut sum_squares = 0.0_f32;
                let mut sample_count = 0_usize;
                mono_samples.clear();
                let frame_count = samples.len().div_ceil(channels);
                if mono_samples.capacity() < frame_count {
                    mono_samples.reserve(frame_count);
                }
                for frame in samples.chunks(channels) {
                    let mut mono = 0.0_f32;
                    for sample in frame {
                        let sample = <f32 as cpal::Sample>::from_sample(*sample);
                        mono += sample;
                        sum_squares += sample * sample;
                        sample_count += 1;
                    }
                    mono_samples.push(mono / frame.len() as f32);
                }

                if monitor_queue_enabled.load(Ordering::Acquire) {
                    let (_, remainder) = producer.push_partial_slice(&mono_samples);
                    if !remainder.is_empty() {
                        overflow_pending.store(true, Ordering::Release);
                        audio_warning.store(true, Ordering::Release);
                    }
                }

                if sample_count > 0 {
                    let rms = (sum_squares / sample_count as f32).sqrt().clamp(0.0, 1.0);
                    level.store(rms.to_bits(), Ordering::Relaxed);
                }
            },
            move |stream_error| {
                *callback_error
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) =
                    Some(format_microphone_error(format!(
                        "A captura do microfone falhou: {stream_error}"
                    )));
            },
            None,
        )
        .map_err(|error| {
            format_microphone_error(format!("Não foi possível abrir o microfone: {error}"))
        })
}

fn build_output_stream<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    channels: usize,
    input_rate: u32,
    mut consumer: Consumer<f32>,
    overflow_pending: &Arc<AtomicBool>,
    audio_warning: &Arc<AtomicBool>,
    monitor_queue_enabled: &Arc<AtomicBool>,
    monitor_gain: &Arc<AtomicU32>,
    clipping_warning: &Arc<AtomicBool>,
    monitor_error: &Arc<Mutex<Option<String>>>,
) -> Result<cpal::Stream, String>
where
    T: cpal::SizedSample + cpal::FromSample<f32>,
{
    let overflow_pending = Arc::clone(overflow_pending);
    let audio_warning = Arc::clone(audio_warning);
    let monitor_gain = Arc::clone(monitor_gain);
    let clipping_warning = Arc::clone(clipping_warning);
    let callback_error = Arc::clone(monitor_error);
    let callback_queue_enabled = Arc::clone(monitor_queue_enabled);
    let channels = channels.max(1);
    let output_rate = config.sample_rate.max(1);
    let mut resampler = LinearResampler::new(input_rate as f64 / output_rate as f64);

    device
        .build_output_stream::<T, _, _>(
            config,
            move |output, _info| {
                if overflow_pending.swap(false, Ordering::AcqRel) {
                    let stale_samples = consumer.slots();
                    for _ in 0..stale_samples {
                        let _ = consumer.pop();
                    }
                    resampler.reset();
                }

                let output_frames = output.len().div_ceil(channels);
                let startup_buffer_samples =
                    (input_rate as usize * PRIME_MONITOR_BUFFER_MS / 1000).max(1);
                if !resampler.primed && consumer.slots() < startup_buffer_samples {
                    output.fill(<T as cpal::Sample>::from_sample(0.0_f32));
                    return;
                }

                let required_source_samples = (output_frames as f64
                    * resampler.source_samples_per_output_sample)
                    .ceil() as usize
                    + 4;
                resampler.refill(&mut consumer, required_source_samples);
                let gain = f32::from_bits(monitor_gain.load(Ordering::Relaxed));

                for frame in output.chunks_mut(channels) {
                    let next_sample = resampler.next_sample();
                    if next_sample.is_none() && resampler.has_produced {
                        audio_warning.store(true, Ordering::Release);
                    }
                    let amplified_sample = next_sample.unwrap_or(0.0) * gain;
                    if amplified_sample.abs() > 1.0 {
                        clipping_warning.store(true, Ordering::Release);
                    }
                    let sample = amplified_sample.clamp(-1.0, 1.0);
                    frame.fill(<T as cpal::Sample>::from_sample(sample));
                }
            },
            move |stream_error| {
                callback_queue_enabled.store(false, Ordering::Release);
                *callback_error
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) =
                    Some(format!("O retorno do microfone falhou: {stream_error}"));
            },
            None,
        )
        .map_err(|error| format!("Não foi possível abrir a saída padrão do Windows: {error}"))
}

struct LinearResampler {
    source_samples_per_output_sample: f64,
    source_position: f64,
    current: Option<f32>,
    next: Option<f32>,
    source_samples: VecDeque<f32>,
    has_produced: bool,
    primed: bool,
}

impl LinearResampler {
    fn new(source_samples_per_output_sample: f64) -> Self {
        Self {
            source_samples_per_output_sample,
            source_position: 0.0,
            current: None,
            next: None,
            source_samples: VecDeque::with_capacity(4096),
            has_produced: false,
            primed: false,
        }
    }

    fn refill(&mut self, consumer: &mut Consumer<f32>, target_len: usize) {
        let buffered_samples = self.source_samples.len()
            + usize::from(self.current.is_some())
            + usize::from(self.next.is_some());
        let missing = target_len.saturating_sub(buffered_samples);
        for _ in 0..missing {
            let Ok(sample) = consumer.pop() else {
                break;
            };
            self.source_samples.push_back(sample);
        }
    }

    fn reset(&mut self) {
        self.source_position = 0.0;
        self.current = None;
        self.next = None;
        self.source_samples.clear();
        self.primed = false;
    }

    fn next_sample(&mut self) -> Option<f32> {
        if self.current.is_none() {
            self.current = self.source_samples.pop_front();
        }
        if self.next.is_none() {
            self.next = self.source_samples.pop_front();
        }

        let (Some(current), Some(next)) = (self.current, self.next) else {
            self.reset();
            return None;
        };

        let output = current + (next - current) * self.source_position as f32;
        self.source_position += self.source_samples_per_output_sample;

        while self.source_position >= 1.0 {
            self.source_position -= 1.0;
            self.current = self.next;
            self.next = self.source_samples.pop_front();
            if self.next.is_none() {
                self.reset();
                break;
            }
        }

        self.has_produced = true;
        self.primed = true;
        Some(output)
    }
}

fn take_shared_error(error: &Mutex<Option<String>>) -> Option<String> {
    error
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take()
}

fn format_microphone_error(cause: String) -> String {
    format!(
        "{cause} Se o microfone estiver bloqueado, abra Configurações > Privacidade e segurança > Microfone (no Windows 10, Privacidade > Microfone) e permita o acesso a aplicativos de área de trabalho."
    )
}
