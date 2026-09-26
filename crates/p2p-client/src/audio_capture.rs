use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

pub struct MicrophoneTest {
    _stream: cpal::Stream,
    level: Arc<AtomicU32>,
    error: Arc<Mutex<Option<String>>>,
}

impl MicrophoneTest {
    pub fn start() -> Result<Self, String> {
        let host = cpal::default_host();
        let device = host.default_input_device().ok_or_else(|| {
            "Nenhum microfone padrão foi encontrado. Conecte um microfone ou escolha um como padrão nas configurações de Som do Windows.".to_owned()
        })?;
        let supported = device.default_input_config().map_err(|error| {
            format!("Não foi possível consultar o microfone padrão: {error}. Confira também as permissões de microfone do Windows.")
        })?;
        let sample_format = supported.sample_format();
        let config: cpal::StreamConfig = supported.into();
        let level = Arc::new(AtomicU32::new(0.0_f32.to_bits()));
        let error = Arc::new(Mutex::new(None));
        let stream = match sample_format {
            cpal::SampleFormat::I8 => build_stream::<i8>(&device, config, &level, &error),
            cpal::SampleFormat::I16 => build_stream::<i16>(&device, config, &level, &error),
            cpal::SampleFormat::I24 => build_stream::<cpal::I24>(&device, config, &level, &error),
            cpal::SampleFormat::I32 => build_stream::<i32>(&device, config, &level, &error),
            cpal::SampleFormat::I64 => build_stream::<i64>(&device, config, &level, &error),
            cpal::SampleFormat::U8 => build_stream::<u8>(&device, config, &level, &error),
            cpal::SampleFormat::U16 => build_stream::<u16>(&device, config, &level, &error),
            cpal::SampleFormat::U24 => build_stream::<cpal::U24>(&device, config, &level, &error),
            cpal::SampleFormat::U32 => build_stream::<u32>(&device, config, &level, &error),
            cpal::SampleFormat::U64 => build_stream::<u64>(&device, config, &level, &error),
            cpal::SampleFormat::F32 => build_stream::<f32>(&device, config, &level, &error),
            cpal::SampleFormat::F64 => build_stream::<f64>(&device, config, &level, &error),
            _ => Err("O formato de áudio do microfone não é compatível com o medidor.".to_owned()),
        }?;

        stream.play().map_err(|play_error| {
            microphone_error(format!(
                "O Windows não permitiu iniciar a captura: {play_error}"
            ))
        })?;

        Ok(Self {
            _stream: stream,
            level,
            error,
        })
    }

    pub fn level(&self) -> f32 {
        f32::from_bits(self.level.load(Ordering::Relaxed)).clamp(0.0, 1.0)
    }

    pub fn take_error(&self) -> Option<String> {
        self.error
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }
}

fn build_stream<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    level: &Arc<AtomicU32>,
    error: &Arc<Mutex<Option<String>>>,
) -> Result<cpal::Stream, String>
where
    T: cpal::SizedSample,
    f32: cpal::FromSample<T>,
{
    let level = Arc::clone(level);
    let callback_error = Arc::clone(error);
    device
        .build_input_stream::<T, _, _>(
            config,
            move |samples, _info| {
                if samples.is_empty() {
                    return;
                }
                let sum_squares = samples
                    .iter()
                    .map(|sample| {
                        let sample = <f32 as cpal::Sample>::from_sample(*sample);
                        sample * sample
                    })
                    .sum::<f32>();
                let rms = (sum_squares / samples.len() as f32).sqrt().clamp(0.0, 1.0);
                level.store(rms.to_bits(), Ordering::Relaxed);
            },
            move |stream_error| {
                let mut slot = callback_error
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                *slot = Some(microphone_error(format!(
                    "A captura do microfone falhou: {stream_error}"
                )));
            },
            None,
        )
        .map_err(|stream_error| {
            microphone_error(format!(
                "Não foi possível abrir o microfone: {stream_error}"
            ))
        })
}

fn microphone_error(cause: String) -> String {
    format!(
        "{cause} Se o microfone estiver bloqueado, abra Configurações > Privacidade e segurança > Microfone (no Windows 10, Privacidade > Microfone) e permita o acesso a aplicativos de área de trabalho."
    )
}
