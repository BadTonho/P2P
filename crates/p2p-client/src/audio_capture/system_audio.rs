use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use rtrb::{Consumer, Producer, RingBuffer};

pub(crate) const OPUS_SAMPLE_RATE: u32 = 48_000;
pub(crate) const OPUS_CHANNELS: usize = 2;
pub(crate) const OPUS_FRAME_SAMPLES_PER_CHANNEL: usize = 960;
const CAPTURE_QUEUE_MILLIS: usize = 250;
const PLAYBACK_QUEUE_MILLIS: usize = 250;
const PLAYBACK_PRIME_MILLIS: usize = 40;

pub(crate) struct SystemAudioCapture {
    _stream: cpal::Stream,
    consumer: Consumer<f32>,
    input_rate: u32,
    input_channels: usize,
    captured_frames: Arc<AtomicU64>,
    dropped_frames: Arc<AtomicU64>,
    callback_error: Arc<Mutex<Option<String>>>,
}

impl SystemAudioCapture {
    pub(crate) fn start(session_id: u64) -> Result<Self, String> {
        let host = cpal::default_host();
        let device = host.default_output_device().ok_or_else(|| {
            "O Windows não disponibilizou uma saída de áudio padrão para capturar o som do computador.".to_owned()
        })?;
        let device_name = "saída padrão do Windows";
        let supported = device.default_output_config().map_err(|error| {
            format!("Não foi possível consultar a saída padrão para captura de áudio do sistema: {error}")
        })?;
        let sample_format = supported.sample_format();
        let config: cpal::StreamConfig = supported.into();
        let input_rate = config.sample_rate.max(1);
        let input_channels = usize::from(config.channels).max(1);
        let queue_capacity =
            (OPUS_SAMPLE_RATE as usize * OPUS_CHANNELS * CAPTURE_QUEUE_MILLIS / 1000).max(2);
        let (producer, consumer) = RingBuffer::<f32>::new(queue_capacity);
        let captured_frames = Arc::new(AtomicU64::new(0));
        let dropped_frames = Arc::new(AtomicU64::new(0));
        let callback_error = Arc::new(Mutex::new(None));

        let stream = match sample_format {
            cpal::SampleFormat::I8 => build_loopback_stream::<i8>(
                &device,
                config,
                input_channels,
                input_rate,
                producer,
                &captured_frames,
                &dropped_frames,
                &callback_error,
                session_id,
            ),
            cpal::SampleFormat::I16 => build_loopback_stream::<i16>(
                &device,
                config,
                input_channels,
                input_rate,
                producer,
                &captured_frames,
                &dropped_frames,
                &callback_error,
                session_id,
            ),
            cpal::SampleFormat::I24 => build_loopback_stream::<cpal::I24>(
                &device,
                config,
                input_channels,
                input_rate,
                producer,
                &captured_frames,
                &dropped_frames,
                &callback_error,
                session_id,
            ),
            cpal::SampleFormat::I32 => build_loopback_stream::<i32>(
                &device,
                config,
                input_channels,
                input_rate,
                producer,
                &captured_frames,
                &dropped_frames,
                &callback_error,
                session_id,
            ),
            cpal::SampleFormat::I64 => build_loopback_stream::<i64>(
                &device,
                config,
                input_channels,
                input_rate,
                producer,
                &captured_frames,
                &dropped_frames,
                &callback_error,
                session_id,
            ),
            cpal::SampleFormat::U8 => build_loopback_stream::<u8>(
                &device,
                config,
                input_channels,
                input_rate,
                producer,
                &captured_frames,
                &dropped_frames,
                &callback_error,
                session_id,
            ),
            cpal::SampleFormat::U16 => build_loopback_stream::<u16>(
                &device,
                config,
                input_channels,
                input_rate,
                producer,
                &captured_frames,
                &dropped_frames,
                &callback_error,
                session_id,
            ),
            cpal::SampleFormat::U24 => build_loopback_stream::<cpal::U24>(
                &device,
                config,
                input_channels,
                input_rate,
                producer,
                &captured_frames,
                &dropped_frames,
                &callback_error,
                session_id,
            ),
            cpal::SampleFormat::U32 => build_loopback_stream::<u32>(
                &device,
                config,
                input_channels,
                input_rate,
                producer,
                &captured_frames,
                &dropped_frames,
                &callback_error,
                session_id,
            ),
            cpal::SampleFormat::U64 => build_loopback_stream::<u64>(
                &device,
                config,
                input_channels,
                input_rate,
                producer,
                &captured_frames,
                &dropped_frames,
                &callback_error,
                session_id,
            ),
            cpal::SampleFormat::F32 => build_loopback_stream::<f32>(
                &device,
                config,
                input_channels,
                input_rate,
                producer,
                &captured_frames,
                &dropped_frames,
                &callback_error,
                session_id,
            ),
            cpal::SampleFormat::F64 => build_loopback_stream::<f64>(
                &device,
                config,
                input_channels,
                input_rate,
                producer,
                &captured_frames,
                &dropped_frames,
                &callback_error,
                session_id,
            ),
            format => Err(format!(
                "Formato de captura de áudio não suportado: {format:?}"
            )),
        }?;

        stream.play().map_err(|error| {
            format!("Não foi possível iniciar o loopback de áudio do Windows: {error}")
        })?;
        tracing::info!(
            screen_share_session = session_id,
            audio_capture = "WASAPI loopback via CPAL",
            output_device = %device_name,
            input_rate_hz = input_rate,
            input_channels,
            opus_rate_hz = OPUS_SAMPLE_RATE,
            "Captura do áudio reproduzido pelo computador iniciada"
        );

        Ok(Self {
            _stream: stream,
            consumer,
            input_rate,
            input_channels,
            captured_frames,
            dropped_frames,
            callback_error,
        })
    }

    pub(crate) fn read_samples(&mut self, output: &mut [f32]) -> usize {
        let mut read = 0;
        while read < output.len() {
            match self.consumer.pop() {
                Ok(sample) => {
                    output[read] = sample;
                    read += 1;
                }
                Err(_) => break,
            }
        }
        read
    }

    pub(crate) fn captured_frames(&self) -> u64 {
        self.captured_frames.load(Ordering::Relaxed)
    }

    pub(crate) fn dropped_frames(&self) -> u64 {
        self.dropped_frames.load(Ordering::Relaxed)
    }

    pub(crate) fn take_error(&self) -> Option<String> {
        self.callback_error
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }

    pub(crate) fn input_format(&self) -> (u32, usize) {
        (self.input_rate, self.input_channels)
    }
}

fn build_loopback_stream<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    channels: usize,
    input_rate: u32,
    mut producer: Producer<f32>,
    captured_frames: &Arc<AtomicU64>,
    dropped_frames: &Arc<AtomicU64>,
    callback_error: &Arc<Mutex<Option<String>>>,
    session_id: u64,
) -> Result<cpal::Stream, String>
where
    T: cpal::SizedSample,
    f32: cpal::FromSample<T>,
{
    let captured_frames = Arc::clone(captured_frames);
    let dropped_frames = Arc::clone(dropped_frames);
    let callback_error = Arc::clone(callback_error);
    let channels = channels.max(1);
    let rate_ratio = OPUS_SAMPLE_RATE as f64 / f64::from(input_rate.max(1));
    let mut resample_phase = 0.0_f64;

    device
        .build_input_stream::<T, _, _>(
            config,
            move |input, _| {
                for frame in input.chunks(channels) {
                    if frame.is_empty() {
                        continue;
                    }
                    let left = <f32 as cpal::Sample>::from_sample(frame[0]);
                    let right = if frame.len() > 1 {
                        <f32 as cpal::Sample>::from_sample(frame[1])
                    } else {
                        left
                    };
                    resample_phase += rate_ratio;
                    while resample_phase >= 1.0 {
                        if producer.slots() >= OPUS_CHANNELS {
                            let _ = producer.push(left.clamp(-1.0, 1.0));
                            let _ = producer.push(right.clamp(-1.0, 1.0));
                            captured_frames.fetch_add(1, Ordering::Relaxed);
                        } else {
                            dropped_frames.fetch_add(1, Ordering::Relaxed);
                        }
                        resample_phase -= 1.0;
                    }
                }
            },
            move |error| {
                let detail = format!("Falha no callback do loopback de áudio: {error}");
                tracing::error!(
                    screen_share_session = session_id,
                    stage = "wasapi_loopback_callback",
                    error = %detail,
                    "Captura de áudio do sistema falhou"
                );
                *callback_error
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(detail);
            },
            None,
        )
        .map_err(|error| {
            format!("Não foi possível abrir captura loopback da saída do Windows: {error}")
        })
}

pub(crate) struct RemoteAudioPlayback {
    _stream: cpal::Stream,
    producer: Producer<f32>,
    output_rate: u32,
    resample_phase: f64,
    output_underflow_frames: Arc<AtomicU64>,
    dropped_frames: Arc<AtomicU64>,
    callback_error: Arc<Mutex<Option<String>>>,
}

impl RemoteAudioPlayback {
    pub(crate) fn start(session_id: u64, ssrc: u32) -> Result<Self, String> {
        let host = cpal::default_host();
        let device = host.default_output_device().ok_or_else(|| {
            "Nenhuma saída padrão do Windows está disponível para reproduzir o áudio recebido."
                .to_owned()
        })?;
        let device_name = "saída padrão do Windows";
        let supported = device.default_output_config().map_err(|error| {
            format!(
                "Não foi possível consultar a saída padrão para reproduzir áudio recebido: {error}"
            )
        })?;
        let sample_format = supported.sample_format();
        let config: cpal::StreamConfig = supported.into();
        let output_rate = config.sample_rate.max(1);
        let channels = usize::from(config.channels).max(1);
        let queue_capacity = (output_rate as usize * OPUS_CHANNELS * PLAYBACK_QUEUE_MILLIS / 1000)
            .max(OPUS_CHANNELS);
        let (producer, consumer) = RingBuffer::<f32>::new(queue_capacity);
        let output_underflow_frames = Arc::new(AtomicU64::new(0));
        let dropped_frames = Arc::new(AtomicU64::new(0));
        let callback_error = Arc::new(Mutex::new(None));
        let stream = match sample_format {
            cpal::SampleFormat::I8 => build_playback_stream::<i8>(
                &device,
                config,
                channels,
                consumer,
                &output_underflow_frames,
                &callback_error,
                session_id,
                ssrc,
            ),
            cpal::SampleFormat::I16 => build_playback_stream::<i16>(
                &device,
                config,
                channels,
                consumer,
                &output_underflow_frames,
                &callback_error,
                session_id,
                ssrc,
            ),
            cpal::SampleFormat::I24 => build_playback_stream::<cpal::I24>(
                &device,
                config,
                channels,
                consumer,
                &output_underflow_frames,
                &callback_error,
                session_id,
                ssrc,
            ),
            cpal::SampleFormat::I32 => build_playback_stream::<i32>(
                &device,
                config,
                channels,
                consumer,
                &output_underflow_frames,
                &callback_error,
                session_id,
                ssrc,
            ),
            cpal::SampleFormat::I64 => build_playback_stream::<i64>(
                &device,
                config,
                channels,
                consumer,
                &output_underflow_frames,
                &callback_error,
                session_id,
                ssrc,
            ),
            cpal::SampleFormat::U8 => build_playback_stream::<u8>(
                &device,
                config,
                channels,
                consumer,
                &output_underflow_frames,
                &callback_error,
                session_id,
                ssrc,
            ),
            cpal::SampleFormat::U16 => build_playback_stream::<u16>(
                &device,
                config,
                channels,
                consumer,
                &output_underflow_frames,
                &callback_error,
                session_id,
                ssrc,
            ),
            cpal::SampleFormat::U24 => build_playback_stream::<cpal::U24>(
                &device,
                config,
                channels,
                consumer,
                &output_underflow_frames,
                &callback_error,
                session_id,
                ssrc,
            ),
            cpal::SampleFormat::U32 => build_playback_stream::<u32>(
                &device,
                config,
                channels,
                consumer,
                &output_underflow_frames,
                &callback_error,
                session_id,
                ssrc,
            ),
            cpal::SampleFormat::U64 => build_playback_stream::<u64>(
                &device,
                config,
                channels,
                consumer,
                &output_underflow_frames,
                &callback_error,
                session_id,
                ssrc,
            ),
            cpal::SampleFormat::F32 => build_playback_stream::<f32>(
                &device,
                config,
                channels,
                consumer,
                &output_underflow_frames,
                &callback_error,
                session_id,
                ssrc,
            ),
            cpal::SampleFormat::F64 => build_playback_stream::<f64>(
                &device,
                config,
                channels,
                consumer,
                &output_underflow_frames,
                &callback_error,
                session_id,
                ssrc,
            ),
            format => Err(format!(
                "Formato da saída de áudio não suportado: {format:?}"
            )),
        }?;
        stream.play().map_err(|error| {
            format!("Não foi possível iniciar a reprodução do áudio recebido: {error}")
        })?;

        tracing::info!(
            screen_share_session = session_id,
            audio_track_ssrc = ssrc,
            audio_output = "CPAL/WASAPI",
            output_device = %device_name,
            output_rate_hz = output_rate,
            output_channels = channels,
            "Reprodução da faixa de áudio remota iniciada"
        );
        Ok(Self {
            _stream: stream,
            producer,
            output_rate,
            resample_phase: 0.0,
            output_underflow_frames,
            dropped_frames,
            callback_error,
        })
    }

    pub(crate) fn push_decoded(&mut self, samples: &[f32], frames_per_channel: usize) {
        let output_frames_per_input = f64::from(self.output_rate) / f64::from(OPUS_SAMPLE_RATE);
        for frame in samples.chunks_exact(OPUS_CHANNELS).take(frames_per_channel) {
            self.resample_phase += output_frames_per_input;
            while self.resample_phase >= 1.0 {
                if self.producer.slots() >= OPUS_CHANNELS {
                    let _ = self.producer.push(frame[0].clamp(-1.0, 1.0));
                    let _ = self.producer.push(frame[1].clamp(-1.0, 1.0));
                } else {
                    self.dropped_frames.fetch_add(1, Ordering::Relaxed);
                }
                self.resample_phase -= 1.0;
            }
        }
    }

    pub(crate) fn output_underflow_frames(&self) -> u64 {
        self.output_underflow_frames.load(Ordering::Relaxed)
    }

    pub(crate) fn dropped_frames(&self) -> u64 {
        self.dropped_frames.load(Ordering::Relaxed)
    }

    pub(crate) fn take_error(&self) -> Option<String> {
        self.callback_error
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }
}

fn build_playback_stream<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    channels: usize,
    mut consumer: Consumer<f32>,
    underflow_frames: &Arc<AtomicU64>,
    callback_error: &Arc<Mutex<Option<String>>>,
    session_id: u64,
    ssrc: u32,
) -> Result<cpal::Stream, String>
where
    T: cpal::SizedSample + cpal::FromSample<f32>,
{
    let underflow_frames = Arc::clone(underflow_frames);
    let callback_error = Arc::clone(callback_error);
    let channels = channels.max(1);
    let prime_samples = config.sample_rate as usize * OPUS_CHANNELS * PLAYBACK_PRIME_MILLIS / 1000;
    let mut primed = false;
    device
        .build_output_stream::<T, _, _>(
            config,
            move |output, _| {
                if !primed {
                    if consumer.slots() < prime_samples {
                        output.fill(<T as cpal::Sample>::from_sample(0.0_f32));
                        return;
                    }
                    primed = true;
                }
                let mut underrun = false;
                for frame in output.chunks_mut(channels) {
                    let left = consumer.pop().ok();
                    let right = consumer.pop().ok();
                    if left.is_none() || right.is_none() {
                        underflow_frames.fetch_add(1, Ordering::Relaxed);
                        underrun = true;
                    }
                    let left = left.unwrap_or(0.0);
                    let right = right.unwrap_or(left);
                    let mono = (left + right) * 0.5;
                    for (index, sample) in frame.iter_mut().enumerate() {
                        let value = match index {
                            0 => left,
                            1 => right,
                            _ => mono,
                        };
                        *sample = <T as cpal::Sample>::from_sample(value);
                    }
                }
                if underrun {
                    primed = false;
                }
            },
            move |error| {
                let detail = format!("Falha no callback da saída de áudio: {error}");
                tracing::error!(
                    screen_share_session = session_id,
                    audio_track_ssrc = ssrc,
                    stage = "audio_output_callback",
                    error = %detail,
                    "Saída de áudio recebido falhou"
                );
                *callback_error
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(detail);
            },
            None,
        )
        .map_err(|error| format!("Não foi possível abrir a saída para áudio remoto: {error}"))
}
