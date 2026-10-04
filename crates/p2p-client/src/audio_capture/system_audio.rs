use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use rtrb::{Consumer, Producer, RingBuffer};

#[cfg(windows)]
use super::process_loopback::{
    InitialCaptureRoute, ProcessLoopbackCapture, find_process_id, initial_capture_route,
    process_loopback_supported, should_switch_to_process_tree,
};
use super::{
    AudioPlaybackFactory, AudioPlaybackSink, AudioSampleSource, RemoteAudioVolume,
    scale_remote_audio_sample,
};

pub(crate) const OPUS_SAMPLE_RATE: u32 = 48_000;
pub(crate) const OPUS_CHANNELS: usize = 2;
pub(crate) const OPUS_FRAME_SAMPLES_PER_CHANNEL: usize = 960;
pub(super) const CAPTURE_QUEUE_MILLIS: usize = 250;
const PLAYBACK_QUEUE_MILLIS: usize = 250;
const PLAYBACK_PRIME_MILLIS: usize = 40;

pub(crate) struct SystemAudioCapture {
    stream: Option<cpal::Stream>,
    #[cfg(windows)]
    process_capture: Option<ProcessLoopbackCapture>,
    consumer: Consumer<f32>,
    input_rate: u32,
    input_channels: usize,
    diagnostics: Arc<CaptureDiagnostics>,
    #[cfg(windows)]
    excluded_application_path: Option<String>,
    #[cfg(windows)]
    excluded_process_id: Option<u32>,
    #[cfg(windows)]
    last_process_scan: Instant,
}

#[derive(Default)]
pub(super) struct CaptureDiagnostics {
    pub(super) callbacks: AtomicU64,
    pub(super) input_frames: AtomicU64,
    pub(super) non_silent_samples: AtomicU64,
    pub(super) captured_frames: AtomicU64,
    pub(super) dropped_frames: AtomicU64,
    pub(super) xruns: AtomicU64,
    pub(super) device_changes: AtomicU64,
    pub(super) realtime_denied: AtomicU64,
    fatal_error: Mutex<Option<(Option<cpal::ErrorKind>, String)>>,
}

impl CaptureDiagnostics {
    pub(super) fn set_fatal_error(&self, kind: Option<cpal::ErrorKind>, message: String) {
        let mut fatal_error = self
            .fatal_error
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if fatal_error.is_none() {
            *fatal_error = Some((kind, message));
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RecoverableCaptureError {
    Xrun,
    DeviceChanged,
    RealtimeDenied,
}

fn classify_capture_error(kind: cpal::ErrorKind) -> Option<RecoverableCaptureError> {
    match kind {
        cpal::ErrorKind::Xrun => Some(RecoverableCaptureError::Xrun),
        cpal::ErrorKind::DeviceChanged => Some(RecoverableCaptureError::DeviceChanged),
        cpal::ErrorKind::RealtimeDenied => Some(RecoverableCaptureError::RealtimeDenied),
        _ => None,
    }
}

impl SystemAudioCapture {
    pub(crate) fn start(
        session_id: u64,
        excluded_application_path: Option<String>,
    ) -> Result<Self, String> {
        #[cfg(not(windows))]
        if excluded_application_path.is_some() {
            return Err("A exclusão de aplicativos no áudio está disponível somente no Windows compatível. O vídeo continuará, mas o áudio não será enviado.".to_owned());
        }
        #[cfg(windows)]
        let process_loopback_is_supported = if excluded_application_path.is_some() {
            process_loopback_supported()?
        } else {
            true
        };
        #[cfg(windows)]
        if matches!(
            initial_capture_route(
                excluded_application_path.is_some(),
                process_loopback_is_supported,
                None
            ),
            InitialCaptureRoute::BlockUnsupported
        ) {
            return Err("A exclusão de aplicativos do áudio exige Windows build 20348 ou posterior. O vídeo continuará, mas o áudio não será enviado.".to_owned());
        }
        let capture_diagnostics = Arc::new(CaptureDiagnostics::default());
        #[cfg(windows)]
        let excluded_process_id = match excluded_application_path.as_deref() {
            Some(path) => find_process_id(path)?,
            None => None,
        };
        #[cfg(windows)]
        let initial_route = initial_capture_route(
            excluded_application_path.is_some(),
            process_loopback_is_supported,
            excluded_process_id,
        );

        #[cfg(windows)]
        if let InitialCaptureRoute::ExcludeProcessTree(process_id) = initial_route {
            let queue_capacity =
                (OPUS_SAMPLE_RATE as usize * OPUS_CHANNELS * CAPTURE_QUEUE_MILLIS / 1000).max(2);
            let (producer, consumer) = RingBuffer::<f32>::new(queue_capacity);
            let process_capture = ProcessLoopbackCapture::start(
                process_id,
                producer,
                Arc::clone(&capture_diagnostics),
            )?;
            tracing::info!(
                screen_share_session = session_id,
                audio_capture = "WASAPI process loopback",
                excluded_process_tree = true,
                "Captura seletiva de áudio iniciada"
            );
            return Ok(Self {
                stream: None,
                process_capture: Some(process_capture),
                consumer,
                input_rate: OPUS_SAMPLE_RATE,
                input_channels: OPUS_CHANNELS,
                diagnostics: capture_diagnostics,
                excluded_application_path,
                excluded_process_id: Some(process_id),
                last_process_scan: Instant::now(),
            });
        }

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
        let stream = match sample_format {
            cpal::SampleFormat::I8 => build_loopback_stream::<i8>(
                &device,
                config,
                input_channels,
                input_rate,
                producer,
                &capture_diagnostics,
            ),
            cpal::SampleFormat::I16 => build_loopback_stream::<i16>(
                &device,
                config,
                input_channels,
                input_rate,
                producer,
                &capture_diagnostics,
            ),
            cpal::SampleFormat::I24 => build_loopback_stream::<cpal::I24>(
                &device,
                config,
                input_channels,
                input_rate,
                producer,
                &capture_diagnostics,
            ),
            cpal::SampleFormat::I32 => build_loopback_stream::<i32>(
                &device,
                config,
                input_channels,
                input_rate,
                producer,
                &capture_diagnostics,
            ),
            cpal::SampleFormat::I64 => build_loopback_stream::<i64>(
                &device,
                config,
                input_channels,
                input_rate,
                producer,
                &capture_diagnostics,
            ),
            cpal::SampleFormat::U8 => build_loopback_stream::<u8>(
                &device,
                config,
                input_channels,
                input_rate,
                producer,
                &capture_diagnostics,
            ),
            cpal::SampleFormat::U16 => build_loopback_stream::<u16>(
                &device,
                config,
                input_channels,
                input_rate,
                producer,
                &capture_diagnostics,
            ),
            cpal::SampleFormat::U24 => build_loopback_stream::<cpal::U24>(
                &device,
                config,
                input_channels,
                input_rate,
                producer,
                &capture_diagnostics,
            ),
            cpal::SampleFormat::U32 => build_loopback_stream::<u32>(
                &device,
                config,
                input_channels,
                input_rate,
                producer,
                &capture_diagnostics,
            ),
            cpal::SampleFormat::U64 => build_loopback_stream::<u64>(
                &device,
                config,
                input_channels,
                input_rate,
                producer,
                &capture_diagnostics,
            ),
            cpal::SampleFormat::F32 => build_loopback_stream::<f32>(
                &device,
                config,
                input_channels,
                input_rate,
                producer,
                &capture_diagnostics,
            ),
            cpal::SampleFormat::F64 => build_loopback_stream::<f64>(
                &device,
                config,
                input_channels,
                input_rate,
                producer,
                &capture_diagnostics,
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

        let capture = Self {
            stream: Some(stream),
            #[cfg(windows)]
            process_capture: None,
            consumer,
            input_rate,
            input_channels,
            diagnostics: capture_diagnostics,
            #[cfg(windows)]
            excluded_application_path,
            #[cfg(windows)]
            excluded_process_id: None,
            #[cfg(windows)]
            last_process_scan: Instant::now() - Duration::from_secs(1),
        };
        #[cfg(windows)]
        if capture.excluded_application_path.is_some() {
            tracing::info!(
                screen_share_session = session_id,
                audio_capture = "WASAPI loopback via CPAL; selected process not running",
                "A captura completa do áudio será usada até o aplicativo selecionado iniciar"
            );
        }
        Ok(capture)
    }

    pub(crate) fn read_samples(&mut self, output: &mut [f32]) -> usize {
        #[cfg(windows)]
        self.update_excluded_process();
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
        self.diagnostics.captured_frames.load(Ordering::Relaxed)
    }

    pub(crate) fn dropped_frames(&self) -> u64 {
        self.diagnostics.dropped_frames.load(Ordering::Relaxed)
    }

    pub(crate) fn callbacks(&self) -> u64 {
        self.diagnostics.callbacks.load(Ordering::Relaxed)
    }

    pub(crate) fn input_frames(&self) -> u64 {
        self.diagnostics.input_frames.load(Ordering::Relaxed)
    }

    pub(crate) fn non_silent_samples(&self) -> u64 {
        self.diagnostics.non_silent_samples.load(Ordering::Relaxed)
    }

    pub(crate) fn xruns(&self) -> u64 {
        self.diagnostics.xruns.load(Ordering::Relaxed)
    }

    pub(crate) fn device_changes(&self) -> u64 {
        self.diagnostics.device_changes.load(Ordering::Relaxed)
    }

    pub(crate) fn realtime_denied(&self) -> u64 {
        self.diagnostics.realtime_denied.load(Ordering::Relaxed)
    }

    pub(crate) fn take_error(&self) -> Option<(Option<cpal::ErrorKind>, String)> {
        self.diagnostics
            .fatal_error
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }

    pub(crate) fn input_format(&self) -> (u32, usize) {
        (self.input_rate, self.input_channels)
    }

    #[cfg(windows)]
    fn update_excluded_process(&mut self) {
        let Some(path) = self.excluded_application_path.as_deref() else {
            return;
        };
        if self.last_process_scan.elapsed() < Duration::from_millis(500) {
            return;
        }
        self.last_process_scan = Instant::now();
        let process_id = match find_process_id(path) {
            Ok(Some(process_id)) => process_id,
            Ok(None) => return,
            Err(error) => {
                self.stream.take();
                self.process_capture.take();
                self.diagnostics.set_fatal_error(None, error);
                return;
            }
        };
        if !should_switch_to_process_tree(self.excluded_process_id, process_id) {
            return;
        }
        if let Err(error) = self.switch_to_excluded_process(process_id) {
            self.stream.take();
            self.process_capture.take();
            self.diagnostics.set_fatal_error(None, error);
        }
    }

    #[cfg(windows)]
    fn switch_to_excluded_process(&mut self, process_id: u32) -> Result<(), String> {
        let queue_capacity =
            (OPUS_SAMPLE_RATE as usize * OPUS_CHANNELS * CAPTURE_QUEUE_MILLIS / 1000).max(2);
        let (producer, consumer) = RingBuffer::<f32>::new(queue_capacity);
        let process_capture =
            ProcessLoopbackCapture::start(process_id, producer, Arc::clone(&self.diagnostics))?;
        self.stream.take();
        self.process_capture = Some(process_capture);
        self.consumer = consumer;
        self.input_rate = OPUS_SAMPLE_RATE;
        self.input_channels = OPUS_CHANNELS;
        self.excluded_process_id = Some(process_id);
        tracing::info!(
            audio_capture = "WASAPI process loopback",
            excluded_process_tree = true,
            "Exclusão seletiva de aplicativo aplicada à captura de áudio"
        );
        Ok(())
    }
}

impl AudioSampleSource for SystemAudioCapture {
    fn input_format(&self) -> (u32, usize) {
        SystemAudioCapture::input_format(self)
    }

    fn read_samples(&mut self, output: &mut [f32]) -> usize {
        SystemAudioCapture::read_samples(self, output)
    }

    fn callbacks(&self) -> u64 {
        SystemAudioCapture::callbacks(self)
    }

    fn input_frames(&self) -> u64 {
        SystemAudioCapture::input_frames(self)
    }

    fn non_silent_samples(&self) -> u64 {
        SystemAudioCapture::non_silent_samples(self)
    }

    fn captured_frames(&self) -> u64 {
        SystemAudioCapture::captured_frames(self)
    }

    fn dropped_frames(&self) -> u64 {
        SystemAudioCapture::dropped_frames(self)
    }

    fn xruns(&self) -> u64 {
        SystemAudioCapture::xruns(self)
    }

    fn device_changes(&self) -> u64 {
        SystemAudioCapture::device_changes(self)
    }

    fn realtime_denied(&self) -> u64 {
        SystemAudioCapture::realtime_denied(self)
    }

    fn take_error(&self) -> Option<(Option<cpal::ErrorKind>, String)> {
        SystemAudioCapture::take_error(self)
    }
}

fn build_loopback_stream<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    channels: usize,
    input_rate: u32,
    mut producer: Producer<f32>,
    diagnostics: &Arc<CaptureDiagnostics>,
) -> Result<cpal::Stream, String>
where
    T: cpal::SizedSample,
    f32: cpal::FromSample<T>,
{
    let diagnostics = Arc::clone(diagnostics);
    let error_diagnostics = Arc::clone(&diagnostics);
    let channels = channels.max(1);
    let rate_ratio = OPUS_SAMPLE_RATE as f64 / f64::from(input_rate.max(1));
    let mut resample_phase = 0.0_f64;

    device
        .build_input_stream::<T, _, _>(
            config,
            move |input, _| {
                diagnostics.callbacks.fetch_add(1, Ordering::Relaxed);
                diagnostics
                    .input_frames
                    .fetch_add((input.len() / channels) as u64, Ordering::Relaxed);
                let mut non_silent_samples = 0_u64;
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
                    non_silent_samples += u64::from(left.abs() > 0.001);
                    non_silent_samples += u64::from(right.abs() > 0.001);
                    resample_phase += rate_ratio;
                    while resample_phase >= 1.0 {
                        if producer.slots() >= OPUS_CHANNELS {
                            let _ = producer.push(left.clamp(-1.0, 1.0));
                            let _ = producer.push(right.clamp(-1.0, 1.0));
                            diagnostics.captured_frames.fetch_add(1, Ordering::Relaxed);
                        } else {
                            diagnostics.dropped_frames.fetch_add(1, Ordering::Relaxed);
                        }
                        resample_phase -= 1.0;
                    }
                }
                diagnostics
                    .non_silent_samples
                    .fetch_add(non_silent_samples, Ordering::Relaxed);
            },
            move |error| {
                let kind = error.kind();
                if let Some(recoverable) = classify_capture_error(kind) {
                    match recoverable {
                        RecoverableCaptureError::Xrun => {
                            error_diagnostics.xruns.fetch_add(1, Ordering::Relaxed);
                        }
                        RecoverableCaptureError::DeviceChanged => {
                            error_diagnostics
                                .device_changes
                                .fetch_add(1, Ordering::Relaxed);
                        }
                        RecoverableCaptureError::RealtimeDenied => {
                            error_diagnostics
                                .realtime_denied
                                .fetch_add(1, Ordering::Relaxed);
                        }
                    }
                } else {
                    let detail = format!("Falha no callback do loopback de áudio: {error}");
                    let mut fatal_error = error_diagnostics
                        .fatal_error
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    if fatal_error.is_none() {
                        *fatal_error = Some((Some(kind), detail));
                    }
                }
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
    diagnostics: Arc<PlaybackDiagnostics>,
    callback_error: Arc<Mutex<Option<String>>>,
}

#[derive(Default)]
struct PlaybackDiagnostics {
    callbacks: AtomicU64,
    non_silent_samples: AtomicU64,
    volume: RemoteAudioVolume,
}

impl RemoteAudioPlayback {
    pub(crate) fn start(
        session_id: u64,
        ssrc: u32,
        volume: RemoteAudioVolume,
    ) -> Result<Self, String> {
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
        let diagnostics = Arc::new(PlaybackDiagnostics {
            volume,
            ..PlaybackDiagnostics::default()
        });
        let callback_error = Arc::new(Mutex::new(None));
        let stream = match sample_format {
            cpal::SampleFormat::I8 => build_playback_stream::<i8>(
                &device,
                config,
                channels,
                consumer,
                &output_underflow_frames,
                &callback_error,
                &diagnostics,
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
                &diagnostics,
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
                &diagnostics,
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
                &diagnostics,
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
                &diagnostics,
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
                &diagnostics,
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
                &diagnostics,
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
                &diagnostics,
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
                &diagnostics,
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
                &diagnostics,
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
                &diagnostics,
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
                &diagnostics,
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
            diagnostics,
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

    pub(crate) fn callbacks(&self) -> u64 {
        self.diagnostics.callbacks.load(Ordering::Relaxed)
    }

    pub(crate) fn non_silent_samples(&self) -> u64 {
        self.diagnostics.non_silent_samples.load(Ordering::Relaxed)
    }

    pub(crate) fn take_error(&self) -> Option<String> {
        self.callback_error
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }
}

impl AudioPlaybackSink for RemoteAudioPlayback {
    fn push_decoded(&mut self, samples: &[f32], frames_per_channel: usize) {
        RemoteAudioPlayback::push_decoded(self, samples, frames_per_channel);
    }

    fn output_underflow_frames(&self) -> u64 {
        RemoteAudioPlayback::output_underflow_frames(self)
    }

    fn dropped_frames(&self) -> u64 {
        RemoteAudioPlayback::dropped_frames(self)
    }

    fn callbacks(&self) -> u64 {
        RemoteAudioPlayback::callbacks(self)
    }

    fn non_silent_samples(&self) -> u64 {
        RemoteAudioPlayback::non_silent_samples(self)
    }

    fn take_error(&self) -> Option<String> {
        RemoteAudioPlayback::take_error(self)
    }
}

pub(crate) struct SystemAudioPlaybackFactory;

impl AudioPlaybackFactory for SystemAudioPlaybackFactory {
    fn start(
        &self,
        session_id: u64,
        ssrc: u32,
        volume: RemoteAudioVolume,
    ) -> Result<Box<dyn AudioPlaybackSink>, String> {
        RemoteAudioPlayback::start(session_id, ssrc, volume)
            .map(|playback| Box::new(playback) as Box<dyn AudioPlaybackSink>)
    }
}

#[allow(clippy::too_many_arguments)]
fn build_playback_stream<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    channels: usize,
    mut consumer: Consumer<f32>,
    underflow_frames: &Arc<AtomicU64>,
    callback_error: &Arc<Mutex<Option<String>>>,
    diagnostics: &Arc<PlaybackDiagnostics>,
    session_id: u64,
    ssrc: u32,
) -> Result<cpal::Stream, String>
where
    T: cpal::SizedSample + cpal::FromSample<f32>,
{
    let underflow_frames = Arc::clone(underflow_frames);
    let callback_error = Arc::clone(callback_error);
    let diagnostics = Arc::clone(diagnostics);
    let channels = channels.max(1);
    let prime_samples = config.sample_rate as usize * OPUS_CHANNELS * PLAYBACK_PRIME_MILLIS / 1000;
    let mut primed = false;
    device
        .build_output_stream::<T, _, _>(
            config,
            move |output, _| {
                diagnostics.callbacks.fetch_add(1, Ordering::Relaxed);
                if !primed {
                    if consumer.slots() < prime_samples {
                        output.fill(<T as cpal::Sample>::from_sample(0.0_f32));
                        return;
                    }
                    primed = true;
                }
                let volume_gain = diagnostics.volume.gain();
                let mut underrun = false;
                let mut non_silent_samples = 0_u64;
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
                        let value = scale_remote_audio_sample(value, volume_gain);
                        non_silent_samples += u64::from(value.abs() > 0.001);
                        *sample = <T as cpal::Sample>::from_sample(value);
                    }
                }
                diagnostics
                    .non_silent_samples
                    .fetch_add(non_silent_samples, Ordering::Relaxed);
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

#[cfg(test)]
mod tests {
    use super::{RecoverableCaptureError, classify_capture_error};

    #[test]
    fn classifies_xrun_device_change_and_realtime_refusal_as_recoverable() {
        assert_eq!(
            classify_capture_error(cpal::ErrorKind::Xrun),
            Some(RecoverableCaptureError::Xrun)
        );
        assert_eq!(
            classify_capture_error(cpal::ErrorKind::DeviceChanged),
            Some(RecoverableCaptureError::DeviceChanged)
        );
        assert_eq!(
            classify_capture_error(cpal::ErrorKind::RealtimeDenied),
            Some(RecoverableCaptureError::RealtimeDenied)
        );
    }

    #[test]
    fn treats_device_stream_and_backend_failures_as_fatal() {
        assert_eq!(
            classify_capture_error(cpal::ErrorKind::DeviceNotAvailable),
            None
        );
        assert_eq!(
            classify_capture_error(cpal::ErrorKind::StreamInvalidated),
            None
        );
        assert_eq!(classify_capture_error(cpal::ErrorKind::BackendError), None);
    }
}
