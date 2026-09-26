use std::collections::VecDeque;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

const MAX_MONITOR_BUFFER_MS: usize = 100;

pub struct MicrophoneTest {
    _input_stream: cpal::Stream,
    monitor_stream: Option<cpal::Stream>,
    level: Arc<AtomicU32>,
    microphone_error: Arc<Mutex<Option<String>>>,
    monitor_error: Arc<Mutex<Option<String>>>,
}

impl MicrophoneTest {
    pub fn start() -> Result<Self, String> {
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
        let audio_queue = Arc::new(Mutex::new(BoundedAudioQueue::new(queue_capacity)));

        let input_stream = match input_sample_format {
            cpal::SampleFormat::I8 => build_input_stream::<i8>(
                &input_device,
                input_config,
                input_channels,
                &level,
                &microphone_error,
                &audio_queue,
            ),
            cpal::SampleFormat::I16 => build_input_stream::<i16>(
                &input_device,
                input_config,
                input_channels,
                &level,
                &microphone_error,
                &audio_queue,
            ),
            cpal::SampleFormat::I24 => build_input_stream::<cpal::I24>(
                &input_device,
                input_config,
                input_channels,
                &level,
                &microphone_error,
                &audio_queue,
            ),
            cpal::SampleFormat::I32 => build_input_stream::<i32>(
                &input_device,
                input_config,
                input_channels,
                &level,
                &microphone_error,
                &audio_queue,
            ),
            cpal::SampleFormat::I64 => build_input_stream::<i64>(
                &input_device,
                input_config,
                input_channels,
                &level,
                &microphone_error,
                &audio_queue,
            ),
            cpal::SampleFormat::U8 => build_input_stream::<u8>(
                &input_device,
                input_config,
                input_channels,
                &level,
                &microphone_error,
                &audio_queue,
            ),
            cpal::SampleFormat::U16 => build_input_stream::<u16>(
                &input_device,
                input_config,
                input_channels,
                &level,
                &microphone_error,
                &audio_queue,
            ),
            cpal::SampleFormat::U24 => build_input_stream::<cpal::U24>(
                &input_device,
                input_config,
                input_channels,
                &level,
                &microphone_error,
                &audio_queue,
            ),
            cpal::SampleFormat::U32 => build_input_stream::<u32>(
                &input_device,
                input_config,
                input_channels,
                &level,
                &microphone_error,
                &audio_queue,
            ),
            cpal::SampleFormat::U64 => build_input_stream::<u64>(
                &input_device,
                input_config,
                input_channels,
                &level,
                &microphone_error,
                &audio_queue,
            ),
            cpal::SampleFormat::F32 => build_input_stream::<f32>(
                &input_device,
                input_config,
                input_channels,
                &level,
                &microphone_error,
                &audio_queue,
            ),
            cpal::SampleFormat::F64 => build_input_stream::<f64>(
                &input_device,
                input_config,
                input_channels,
                &level,
                &microphone_error,
                &audio_queue,
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
                            &audio_queue,
                            &monitor_error,
                        ),
                        cpal::SampleFormat::I16 => build_output_stream::<i16>(
                            &output_device,
                            output_config,
                            output_channels,
                            input_rate,
                            &audio_queue,
                            &monitor_error,
                        ),
                        cpal::SampleFormat::I24 => build_output_stream::<cpal::I24>(
                            &output_device,
                            output_config,
                            output_channels,
                            input_rate,
                            &audio_queue,
                            &monitor_error,
                        ),
                        cpal::SampleFormat::I32 => build_output_stream::<i32>(
                            &output_device,
                            output_config,
                            output_channels,
                            input_rate,
                            &audio_queue,
                            &monitor_error,
                        ),
                        cpal::SampleFormat::I64 => build_output_stream::<i64>(
                            &output_device,
                            output_config,
                            output_channels,
                            input_rate,
                            &audio_queue,
                            &monitor_error,
                        ),
                        cpal::SampleFormat::U8 => build_output_stream::<u8>(
                            &output_device,
                            output_config,
                            output_channels,
                            input_rate,
                            &audio_queue,
                            &monitor_error,
                        ),
                        cpal::SampleFormat::U16 => build_output_stream::<u16>(
                            &output_device,
                            output_config,
                            output_channels,
                            input_rate,
                            &audio_queue,
                            &monitor_error,
                        ),
                        cpal::SampleFormat::U24 => build_output_stream::<cpal::U24>(
                            &output_device,
                            output_config,
                            output_channels,
                            input_rate,
                            &audio_queue,
                            &monitor_error,
                        ),
                        cpal::SampleFormat::U32 => build_output_stream::<u32>(
                            &output_device,
                            output_config,
                            output_channels,
                            input_rate,
                            &audio_queue,
                            &monitor_error,
                        ),
                        cpal::SampleFormat::U64 => build_output_stream::<u64>(
                            &output_device,
                            output_config,
                            output_channels,
                            input_rate,
                            &audio_queue,
                            &monitor_error,
                        ),
                        cpal::SampleFormat::F32 => build_output_stream::<f32>(
                            &output_device,
                            output_config,
                            output_channels,
                            input_rate,
                            &audio_queue,
                            &monitor_error,
                        ),
                        cpal::SampleFormat::F64 => build_output_stream::<f64>(
                            &output_device,
                            output_config,
                            output_channels,
                            input_rate,
                            &audio_queue,
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

    pub fn stop_monitoring(&mut self) {
        self.monitor_stream = None;
    }
}

fn build_input_stream<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    channels: usize,
    level: &Arc<AtomicU32>,
    microphone_error: &Arc<Mutex<Option<String>>>,
    audio_queue: &Arc<Mutex<BoundedAudioQueue>>,
) -> Result<cpal::Stream, String>
where
    T: cpal::SizedSample,
    f32: cpal::FromSample<T>,
{
    let level = Arc::clone(level);
    let callback_error = Arc::clone(microphone_error);
    let audio_queue = Arc::clone(audio_queue);
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

                // Do conversion and level measurement before taking the queue lock.
                // The output callback also reads this queue, so keep the critical
                // section limited to a bulk transfer.
                if let Ok(mut queue) = audio_queue.try_lock() {
                    queue.push_slice(&mono_samples);
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
    audio_queue: &Arc<Mutex<BoundedAudioQueue>>,
    monitor_error: &Arc<Mutex<Option<String>>>,
) -> Result<cpal::Stream, String>
where
    T: cpal::SizedSample + cpal::FromSample<f32>,
{
    let audio_queue = Arc::clone(audio_queue);
    let callback_error = Arc::clone(monitor_error);
    let channels = channels.max(1);
    let output_rate = config.sample_rate.max(1);
    let mut resampler = LinearResampler::new(input_rate as f64 / output_rate as f64);

    device
        .build_output_stream::<T, _, _>(
            config,
            move |output, _info| {
                let output_frames = output.len().div_ceil(channels);
                let required_source_samples = (output_frames as f64
                    * resampler.source_samples_per_output_sample)
                    .ceil() as usize
                    + 4;

                // Move a whole callback's worth of source samples at once, then
                // resample without holding the mutex for every output frame.
                if let Ok(mut queue) = audio_queue.try_lock() {
                    resampler.refill(&mut queue, required_source_samples);
                }

                for frame in output.chunks_mut(channels) {
                    let sample = resampler.next_sample().clamp(-1.0, 1.0);
                    frame.fill(<T as cpal::Sample>::from_sample(sample));
                }
            },
            move |stream_error| {
                *callback_error
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) =
                    Some(format!("O retorno do microfone falhou: {stream_error}"));
            },
            None,
        )
        .map_err(|error| format!("Não foi possível abrir a saída padrão do Windows: {error}"))
}

struct BoundedAudioQueue {
    samples: VecDeque<f32>,
    capacity: usize,
}

impl BoundedAudioQueue {
    fn new(capacity: usize) -> Self {
        Self {
            samples: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    fn push_slice(&mut self, incoming: &[f32]) {
        if incoming.len() >= self.capacity {
            self.samples.clear();
            self.samples
                .extend(incoming[incoming.len() - self.capacity..].iter().copied());
            return;
        }

        let overflow = self
            .samples
            .len()
            .saturating_add(incoming.len())
            .saturating_sub(self.capacity);
        self.samples.drain(..overflow);
        self.samples.extend(incoming.iter().copied());
    }

    fn pop_into(&mut self, destination: &mut VecDeque<f32>, count: usize) {
        let count = count.min(self.samples.len());
        destination.extend(self.samples.drain(..count));
    }
}

struct LinearResampler {
    source_samples_per_output_sample: f64,
    source_position: f64,
    current: Option<f32>,
    next: Option<f32>,
    source_samples: VecDeque<f32>,
}

impl LinearResampler {
    fn new(source_samples_per_output_sample: f64) -> Self {
        Self {
            source_samples_per_output_sample,
            source_position: 0.0,
            current: None,
            next: None,
            source_samples: VecDeque::with_capacity(4096),
        }
    }

    fn refill(&mut self, queue: &mut BoundedAudioQueue, target_len: usize) {
        let buffered_samples = self.source_samples.len()
            + usize::from(self.current.is_some())
            + usize::from(self.next.is_some());
        let missing = target_len.saturating_sub(buffered_samples);
        queue.pop_into(&mut self.source_samples, missing);
    }

    fn reset(&mut self) {
        self.source_position = 0.0;
        self.current = None;
        self.next = None;
        self.source_samples.clear();
    }

    fn next_sample(&mut self) -> f32 {
        if self.current.is_none() {
            self.current = self.source_samples.pop_front();
        }
        if self.next.is_none() {
            self.next = self.source_samples.pop_front();
        }

        let (Some(current), Some(next)) = (self.current, self.next) else {
            self.reset();
            return 0.0;
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

        output
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
