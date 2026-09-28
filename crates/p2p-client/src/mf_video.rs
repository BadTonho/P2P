//! Hardware H.264 transforms backed by Windows Media Foundation.
//!
//! Media Foundation performs H.264 encode/decode on a hardware MFT when one is
//! available. DXGI monitor capture can pass scaled NV12 D3D11 surfaces directly
//! to a D3D-aware encoder; other sources keep the system-memory path.

#[cfg(windows)]
mod windows_backend {
    use std::collections::VecDeque;
    use std::mem::ManuallyDrop;
    use std::ptr;
    use std::slice;
    use std::sync::{Mutex, OnceLock};
    use std::thread;
    use std::time::{Duration, Instant};

    use crate::screen_capture::CpuNv12Frame;
    use openh264::formats::{RgbaSliceU8, YUVBuffer, YUVSource};
    use windows::Win32::Foundation::RECT;
    use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL};
    use windows::Win32::Graphics::Direct3D11::{
        D3D11_BIND_RENDER_TARGET, D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
        D3D11_MAP_READ, D3D11_SDK_VERSION, D3D11_TEX2D_VPIV, D3D11_TEX2D_VPOV,
        D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, D3D11_USAGE_STAGING,
        D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE, D3D11_VIDEO_PROCESSOR_CONTENT_DESC,
        D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_INPUT, D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_OUTPUT,
        D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC, D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0,
        D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC, D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0,
        D3D11_VIDEO_PROCESSOR_STREAM, D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
        D3D11_VPIV_DIMENSION_TEXTURE2D, D3D11_VPOV_DIMENSION_TEXTURE2D, D3D11CreateDevice,
        ID3D11Device, ID3D11DeviceContext, ID3D11Resource, ID3D11Texture2D, ID3D11VideoContext,
        ID3D11VideoDevice, ID3D11VideoProcessor, ID3D11VideoProcessorEnumerator,
    };
    use windows::Win32::Graphics::Dxgi::Common::{
        DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_NV12, DXGI_RATIONAL, DXGI_SAMPLE_DESC,
    };
    use windows::Win32::Media::MediaFoundation::*;
    use windows::Win32::System::Com::{
        COINIT_MULTITHREADED, CoInitializeEx, CoTaskMemFree, CoUninitialize,
    };
    use windows::Win32::System::Variant::{VARIANT, VARIANT_0, VARIANT_0_0, VARIANT_0_0_0, VT_UI4};
    use windows::core::{IUnknown, Interface};

    const FRAME_RATE: u32 = 30;
    const BITRATE: u32 = 4_000_000;
    const HNS_PER_SECOND: i64 = 10_000_000;
    const MF_OUTPUT_CAPACITY: u32 = 4 * 1024 * 1024;
    const MF_EVENT_WAIT: Duration = Duration::from_millis(500);

    static MF_STARTUP: OnceLock<Result<(), String>> = OnceLock::new();

    fn ensure_media_foundation() -> Result<(), String> {
        MF_STARTUP
            .get_or_init(|| {
                unsafe { MFStartup(MF_VERSION, MFSTARTUP_FULL) }.map_err(|e| e.to_string())
            })
            .clone()
    }

    struct ComApartment;

    impl ComApartment {
        fn enter() -> Result<Self, String> {
            let result = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
            if result.is_ok() {
                Ok(Self)
            } else {
                Err(format!("COM MTA indisponível: {result:?}"))
            }
        }
    }

    impl Drop for ComApartment {
        fn drop(&mut self) {
            unsafe { CoUninitialize() };
        }
    }

    struct Transform {
        transform: IMFTransform,
        events: Option<IMFMediaEventGenerator>,
        async_transform: bool,
        pending_events: Mutex<VecDeque<u32>>,
        pending_output_samples: Mutex<VecDeque<IMFSample>>,
        transform_provides_sample: bool,
        output_capacity: u32,
        output_subtype: windows::core::GUID,
        width: u32,
        height: u32,
    }

    impl Transform {
        fn configure(
            activate: &IMFActivate,
            input_subtype: windows::core::GUID,
            output_subtype: windows::core::GUID,
            width: u32,
            height: u32,
            decoder_device_manager: Option<&IMFDXGIDeviceManager>,
        ) -> Result<Self, String> {
            let transform: IMFTransform = unsafe { activate.ActivateObject() }
                .map_err(|e| format!("Ativação do transform falhou: {e}"))?;

            let attributes = unsafe { transform.GetAttributes() }
                .map_err(|e| format!("Não foi possível consultar atributos do codec: {e}"))?;
            if decoder_device_manager.is_some()
                && unsafe { attributes.GetUINT32(&MF_SA_D3D11_AWARE) }.unwrap_or_default() == 0
            {
                return Err("O MFT de hardware não anuncia suporte a superfícies D3D11 (MF_SA_D3D11_AWARE).".to_owned());
            }

            if let Some(manager) = decoder_device_manager {
                unsafe {
                    transform
                        .ProcessMessage(
                            MFT_MESSAGE_SET_D3D_MANAGER,
                            manager.as_raw() as usize,
                        )
                        .map_err(|e| format!("Não foi possível associar o dispositivo D3D11 ao decodificador: {e}"))?;
                }
            }

            let async_transform =
                unsafe { attributes.GetUINT32(&MF_TRANSFORM_ASYNC) }.unwrap_or_default() != 0;
            let events = if async_transform {
                unsafe { attributes.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1) }
                    .map_err(|e| format!("Não foi possível liberar o transform assíncrono: {e}"))?;
                Some(transform.cast::<IMFMediaEventGenerator>().map_err(|e| {
                    format!("O codec assíncrono não expõe eventos do Media Foundation: {e}")
                })?)
            } else {
                None
            };

            let input_type = make_video_type(input_subtype, width, height)?;
            let output_type = make_video_type(output_subtype, width, height)?;
            if output_subtype == MFVideoFormat_H264 {
                unsafe {
                    output_type
                        .SetUINT32(&MF_MT_AVG_BITRATE, BITRATE)
                        .map_err(|e| format!("Não foi possível definir o bitrate H.264: {e}"))?;
                    output_type
                        .SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_Base.0 as u32)
                        .map_err(|e| format!("Não foi possível solicitar H.264 Baseline: {e}"))?;
                    output_type
                        .SetUINT32(&MF_MT_MPEG2_LEVEL, 31)
                        .map_err(|e| format!("Não foi possível solicitar H.264 nível 3.1: {e}"))?;
                    output_type
                        .SetUINT32(&MF_MT_MAX_KEYFRAME_SPACING, FRAME_RATE)
                        .map_err(|e| {
                            format!("Não foi possível definir o intervalo de quadros-chave: {e}")
                        })?;
                }
            }

            unsafe {
                if output_subtype == MFVideoFormat_H264 {
                    transform.SetOutputType(0, &output_type, 0).map_err(|e| {
                        format!("O codificador não aceitou H.264 Baseline como saída: {e}")
                    })?;
                    transform
                        .SetInputType(0, &input_type, 0)
                        .map_err(|e| format!("O codificador não aceitou NV12 como entrada: {e}"))?;
                } else {
                    transform.SetInputType(0, &input_type, 0).map_err(|e| {
                        format!("O decodificador não aceitou H.264 como entrada: {e}")
                    })?;
                    transform
                        .SetOutputType(0, &output_type, 0)
                        .map_err(|e| format!("O decodificador não aceitou NV12 como saída: {e}"))?;
                }
                transform
                    .ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)
                    .map_err(|e| format!("Não foi possível iniciar o codec: {e}"))?;
                transform
                    .ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)
                    .map_err(|e| format!("Não foi possível iniciar o fluxo do codec: {e}"))?;
            }

            let output_info = unsafe { transform.GetOutputStreamInfo(0) }.map_err(|e| {
                format!("Não foi possível consultar o buffer de saída do codec: {e}")
            })?;
            let output_capacity = output_info.cbSize.max(MF_OUTPUT_CAPACITY);
            let transform_provides_sample =
                output_info.dwFlags & (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32) != 0;

            Ok(Self {
                transform,
                events,
                async_transform,
                pending_events: Mutex::new(VecDeque::new()),
                pending_output_samples: Mutex::new(VecDeque::new()),
                transform_provides_sample,
                output_capacity,
                output_subtype,
                width,
                height,
            })
        }

        fn wait_for_event(&self, desired: u32, timeout: Duration) -> Result<bool, String> {
            let Some(events) = &self.events else {
                return Ok(true);
            };
            {
                let mut pending = self
                    .pending_events
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if let Some(index) = pending.iter().position(|event| *event == MEError.0 as u32) {
                    pending.remove(index);
                    return Err("O Media Foundation reportou falha assíncrona no codec.".to_owned());
                }
                if let Some(index) = pending.iter().position(|event| *event == desired) {
                    pending.remove(index);
                    return Ok(true);
                }
            }
            let deadline = Instant::now() + timeout;
            while Instant::now() < deadline {
                let event = unsafe { events.GetEvent(MF_EVENT_FLAG_NO_WAIT) };
                if let Ok(event) = event {
                    let event_type = unsafe { event.GetType() }
                        .map_err(|e| format!("Não foi possível ler evento do codec: {e}"))?;
                    if event_type == MEError.0 as u32 {
                        let status = unsafe { event.GetStatus() }
                            .map(|status| format!("{status:?}"))
                            .unwrap_or_else(|e| e.to_string());
                        return Err(format!(
                            "O Media Foundation notificou erro no codec: {status}"
                        ));
                    }
                    if event_type == desired {
                        return Ok(true);
                    }
                    let mut pending = self
                        .pending_events
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    if pending.len() < 64 {
                        pending.push_back(event_type);
                    }
                }
                thread::sleep(Duration::from_millis(1));
            }
            Ok(false)
        }

        fn send_input(&mut self, bytes: &[u8], time_hns: i64) -> Result<(), String> {
            let sample = make_sample(bytes, time_hns)?;
            self.send_sample(&sample, time_hns)
        }

        fn send_sample(&mut self, sample: &IMFSample, time_hns: i64) -> Result<(), String> {
            if self.async_transform
                && !self.wait_for_event(METransformNeedInput.0 as u32, MF_EVENT_WAIT)?
            {
                return Err(
                    "O codec de hardware não solicitou um quadro de entrada no prazo.".to_owned(),
                );
            }
            unsafe {
                sample
                    .SetSampleTime(time_hns)
                    .map_err(|e| format!("Não foi possível marcar o tempo do quadro: {e}"))?;
                sample
                    .SetSampleDuration(HNS_PER_SECOND / i64::from(FRAME_RATE))
                    .map_err(|e| format!("Não foi possível marcar a duração do quadro: {e}"))?;
            }
            let mut result = unsafe { self.transform.ProcessInput(0, sample, 0) };
            for _ in 0..8 {
                match result {
                    Ok(()) => return Ok(()),
                    Err(error) if error.code() == MF_E_NOTACCEPTING => {
                        if let Some(output) = self.process_output_sample(
                            Duration::from_millis(20),
                            self.transform_provides_sample,
                        )? {
                            self.pending_output_samples
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                                .push_back(output);
                        }
                        result = unsafe { self.transform.ProcessInput(0, sample, 0) };
                    }
                    Err(error) => {
                        return Err(format!("O codec de hardware recusou o quadro: {error}"));
                    }
                }
            }
            Err(format!(
                "O codec de hardware continuou sem aceitar entrada após drenar a saída: {}",
                result.unwrap_err()
            ))
        }

        fn receive_output_sample(
            &mut self,
            wait: Duration,
            transform_provides_sample: bool,
        ) -> Result<Option<IMFSample>, String> {
            if let Some(sample) = self
                .pending_output_samples
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .pop_front()
            {
                return Ok(Some(sample));
            }
            self.process_output_sample(wait, transform_provides_sample)
        }

        fn process_output_sample(
            &mut self,
            wait: Duration,
            transform_provides_sample: bool,
        ) -> Result<Option<IMFSample>, String> {
            self.process_output_sample_inner(wait, transform_provides_sample, true)
        }

        fn process_output_sample_inner(
            &mut self,
            wait: Duration,
            transform_provides_sample: bool,
            allow_stream_change: bool,
        ) -> Result<Option<IMFSample>, String> {
            if self.async_transform && !self.wait_for_event(METransformHaveOutput.0 as u32, wait)? {
                return Ok(None);
            }

            let sample = if transform_provides_sample {
                None
            } else {
                let buffer = unsafe { MFCreateMemoryBuffer(self.output_capacity) }
                    .map_err(|e| format!("Não foi possível alocar saída do codec: {e}"))?;
                let sample = unsafe { MFCreateSample() }.map_err(|e| {
                    format!("Não foi possível criar amostra de saída do codec: {e}")
                })?;
                unsafe {
                    sample
                        .AddBuffer(&buffer)
                        .map_err(|e| format!("Não foi possível anexar o buffer de saída: {e}"))?;
                }
                Some(sample)
            };
            let mut output = [MFT_OUTPUT_DATA_BUFFER {
                dwStreamID: 0,
                pSample: ManuallyDrop::new(sample),
                dwStatus: 0,
                pEvents: ManuallyDrop::new(None),
            }];
            let mut status = 0;
            let result = unsafe { self.transform.ProcessOutput(0, &mut output, &mut status) };
            let sample = unsafe { ManuallyDrop::take(&mut output[0].pSample) };
            let _events = unsafe { ManuallyDrop::take(&mut output[0].pEvents) };
            match result {
                Ok(()) => sample
                    .map(Some)
                    .ok_or_else(|| "O codec terminou sem amostra de saída.".to_owned()),
                Err(error) if error.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => Ok(None),
                Err(error)
                    if error.code() == MF_E_TRANSFORM_STREAM_CHANGE && allow_stream_change =>
                {
                    self.renegotiate_output_type().map_err(|reason| {
                        format!("MF_E_TRANSFORM_STREAM_CHANGE: não foi possível manter a saída NV12: {reason}")
                    })?;
                    self.process_output_sample_inner(
                        wait,
                        self.transform_provides_sample,
                        false,
                    )
                }
                Err(error) if error.code() == MF_E_TRANSFORM_STREAM_CHANGE => Err(
                    "O Media Foundation continuou solicitando mudança de formato após renegociar NV12.".to_owned(),
                ),
                Err(error) => Err(format!(
                    "O codec de hardware falhou ao produzir um quadro: {error}"
                )),
            }
        }

        fn renegotiate_output_type(&mut self) -> Result<(), String> {
            if self.output_subtype != MFVideoFormat_NV12 {
                return Err("O codec pediu uma mudança inesperada do formato de saída.".to_owned());
            }
            let mut accepted = false;
            for index in 0..64 {
                let available = match unsafe { self.transform.GetOutputAvailableType(0, index) } {
                    Ok(available) => available,
                    Err(error) if error.code() == MF_E_NO_MORE_TYPES => break,
                    Err(error) => {
                        return Err(format!(
                            "Não foi possível enumerar saídas após a mudança do fluxo: {error}"
                        ));
                    }
                };
                let subtype = unsafe { available.GetGUID(&MF_MT_SUBTYPE) };
                if subtype
                    .as_ref()
                    .is_ok_and(|subtype| *subtype == MFVideoFormat_NV12)
                    && unsafe { self.transform.SetOutputType(0, &available, 0) }.is_ok()
                {
                    accepted = true;
                    break;
                }
            }
            if !accepted {
                let fallback = make_video_type(MFVideoFormat_NV12, self.width, self.height)?;
                unsafe { self.transform.SetOutputType(0, &fallback, 0) }.map_err(|error| {
                    format!("O decodificador não aceitou NV12 ao renegociar o fluxo: {error}")
                })?;
            }
            let info = unsafe { self.transform.GetOutputStreamInfo(0) }.map_err(|error| {
                format!("Não foi possível consultar o buffer após renegociar NV12: {error}")
            })?;
            self.output_capacity = info.cbSize.max(MF_OUTPUT_CAPACITY);
            self.transform_provides_sample =
                info.dwFlags & (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32) != 0;
            tracing::warn!(
                width = self.width,
                height = self.height,
                "Decoder Media Foundation renegociou saída NV12 após MF_E_TRANSFORM_STREAM_CHANGE"
            );
            Ok(())
        }
    }

    pub struct HardwareEncoder {
        _apartment: ComApartment,
        _device: Option<ID3D11Device>,
        _device_manager: Option<IMFDXGIDeviceManager>,
        transform: Transform,
        width: u32,
        height: u32,
        next_time_hns: i64,
        name: String,
        force_keyframe_available: Option<bool>,
        gpu_surface_input_enabled: bool,
    }

    impl HardwareEncoder {
        pub fn new(width: u32, height: u32) -> Result<Self, String> {
            let apartment = ComApartment::enter()?;
            ensure_media_foundation()?;
            let (transform, name) = activate_hardware_transform(
                MFT_CATEGORY_VIDEO_ENCODER,
                MFVideoFormat_NV12,
                MFVideoFormat_H264,
                width,
                height,
                None,
            )?;
            Ok(Self {
                _apartment: apartment,
                _device: None,
                _device_manager: None,
                transform,
                width,
                height,
                next_time_hns: 0,
                name,
                force_keyframe_available: None,
                gpu_surface_input_enabled: false,
            })
        }

        pub fn new_gpu(width: u32, height: u32, device: &ID3D11Device) -> Result<Self, String> {
            let apartment = ComApartment::enter()?;
            ensure_media_foundation()?;
            let device_manager = create_device_manager(device)?;
            let (transform, name) = activate_hardware_transform(
                MFT_CATEGORY_VIDEO_ENCODER,
                MFVideoFormat_NV12,
                MFVideoFormat_H264,
                width,
                height,
                Some(&device_manager),
            )?;
            Ok(Self {
                _apartment: apartment,
                _device: Some(device.clone()),
                _device_manager: Some(device_manager),
                transform,
                width,
                height,
                next_time_hns: 0,
                name,
                force_keyframe_available: None,
                gpu_surface_input_enabled: true,
            })
        }

        pub fn name(&self) -> &str {
            &self.name
        }

        pub fn force_keyframe(&mut self) -> Result<bool, String> {
            if self.force_keyframe_available == Some(false) {
                return Ok(false);
            }
            let codec_api = match self.transform.transform.cast::<ICodecAPI>() {
                Ok(codec_api) => codec_api,
                Err(_) => {
                    self.force_keyframe_available = Some(false);
                    return Ok(false);
                }
            };
            if unsafe { codec_api.IsSupported(&CODECAPI_AVEncVideoForceKeyFrame) }.is_err() {
                self.force_keyframe_available = Some(false);
                return Ok(false);
            }

            let value = VARIANT {
                Anonymous: VARIANT_0 {
                    Anonymous: ManuallyDrop::new(VARIANT_0_0 {
                        vt: VT_UI4,
                        wReserved1: 0,
                        wReserved2: 0,
                        wReserved3: 0,
                        Anonymous: VARIANT_0_0_0 { ulVal: 1 },
                    }),
                },
            };
            match unsafe { codec_api.SetValue(&CODECAPI_AVEncVideoForceKeyFrame, &value) } {
                Ok(()) => {
                    self.force_keyframe_available = Some(true);
                    Ok(true)
                }
                Err(error) => {
                    self.force_keyframe_available = Some(false);
                    Err(format!(
                        "Media Foundation recusou o controle de quadro-chave: {error}"
                    ))
                }
            }
        }

        pub fn encode_rgba(&mut self, rgba: &[u8]) -> Result<Vec<u8>, String> {
            let expected = self.width as usize * self.height as usize * 4;
            if rgba.len() != expected {
                return Err(
                    "O quadro RGBA não corresponde às dimensões do codificador de hardware."
                        .to_owned(),
                );
            }
            let yuv = YUVBuffer::from_rgba8_source(RgbaSliceU8::new(
                rgba,
                (self.width as usize, self.height as usize),
            ));
            let mut nv12 = Vec::with_capacity(self.width as usize * self.height as usize * 3 / 2);
            nv12.extend_from_slice(yuv.y());
            for (&u, &v) in yuv.u().iter().zip(yuv.v()) {
                nv12.push(u);
                nv12.push(v);
            }
            self.encode_packed_nv12(&nv12)
        }

        pub fn encode_nv12(&mut self, frame: &CpuNv12Frame) -> Result<Vec<u8>, String> {
            let width = self.width as usize;
            let height = self.height as usize;
            if self.width % 2 != 0 || self.height % 2 != 0 || frame.stride < width {
                return Err(
                    "O quadro NV12 da captura tem dimensões ou stride inválidos.".to_owned(),
                );
            }
            let required = frame
                .stride
                .checked_mul(height + height / 2)
                .ok_or_else(|| "O tamanho do quadro NV12 excedeu o limite permitido.".to_owned())?;
            if frame.bytes.len() < required {
                return Err(format!(
                    "A captura NV12 forneceu {} bytes, mas eram necessários pelo menos {required}.",
                    frame.bytes.len()
                ));
            }
            let mut packed = Vec::with_capacity(width * height * 3 / 2);
            for row in 0..height {
                let start = row * frame.stride;
                packed.extend_from_slice(&frame.bytes[start..start + width]);
            }
            let uv_start = frame.stride * height;
            for row in 0..height / 2 {
                let start = uv_start + row * frame.stride;
                packed.extend_from_slice(&frame.bytes[start..start + width]);
            }
            self.encode_packed_nv12(&packed)
        }

        fn encode_packed_nv12(&mut self, nv12: &[u8]) -> Result<Vec<u8>, String> {
            let expected = self.width as usize * self.height as usize * 3 / 2;
            if nv12.len() != expected {
                return Err(format!(
                    "O quadro NV12 tem {} bytes; eram esperados {expected}.",
                    nv12.len()
                ));
            }
            self.transform.send_input(nv12, self.next_time_hns)?;
            self.next_time_hns += HNS_PER_SECOND / i64::from(FRAME_RATE);
            self.drain_output()
        }

        pub fn encode_gpu(&mut self, frame: &GpuNv12Surface) -> Result<Vec<u8>, String> {
            if frame.width != self.width || frame.height != self.height {
                return Err(
                    "A superfÃ­cie NV12 da GPU nÃ£o corresponde Ã s dimensÃµes do codificador."
                        .to_owned(),
                );
            }
            if self._device.is_none() {
                return Err(
                    "O codificador Media Foundation nÃ£o foi configurado para superfÃ­cies D3D11."
                        .to_owned(),
                );
            }
            let buffer = unsafe {
                MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, &frame.texture, 0, false)
            }
            .map_err(|error| {
                format!("O Media Foundation nÃ£o aceitou a superfÃ­cie D3D11 NV12: {error}")
            })?;
            let sample = unsafe { MFCreateSample() }
                .map_err(|error| format!("NÃ£o foi possÃ­vel criar amostra da GPU: {error}"))?;
            unsafe {
                sample.AddBuffer(&buffer).map_err(|error| {
                    format!("NÃ£o foi possÃ­vel anexar a superfÃ­cie D3D11: {error}")
                })?;
            }
            self.transform.send_sample(&sample, self.next_time_hns)?;
            self.next_time_hns += HNS_PER_SECOND / i64::from(FRAME_RATE);
            self.drain_output()
        }

        pub fn encode_gpu_or_nv12_or_rgba(
            &mut self,
            frame: Option<&GpuNv12Surface>,
            cpu_nv12: Option<&CpuNv12Frame>,
            rgba: &[u8],
        ) -> Result<(Vec<u8>, bool, Option<String>), String> {
            if !self.gpu_surface_input_enabled {
                let result = match cpu_nv12 {
                    Some(frame) => self.encode_nv12(frame),
                    None => self.encode_rgba(rgba),
                }?;
                return Ok((result, false, None));
            }
            let Some(frame) = frame else {
                if self.gpu_surface_input_enabled {
                    self.gpu_surface_input_enabled = false;
                    return self.encode_cpu_input(cpu_nv12, rgba).map(|bytes| {
                        (
                            bytes,
                            false,
                            Some("A captura não forneceu uma superfície NV12 D3D11; usando entrada pela CPU.".to_owned()),
                        )
                    });
                }
                return self
                    .encode_cpu_input(cpu_nv12, rgba)
                    .map(|bytes| (bytes, false, None));
            };
            match self.encode_gpu(frame) {
                Ok(bytes) => Ok((bytes, true, None)),
                Err(gpu_error) => {
                    self.gpu_surface_input_enabled = false;
                    match self.encode_cpu_input(cpu_nv12, rgba) {
                        Ok(bytes) => Ok((
                            bytes,
                            false,
                            Some(format!(
                                "O encoder não aceitou a superfície D3D11; usando entrada pela CPU: {gpu_error}"
                            )),
                        )),
                        Err(cpu_error) => Err(format!(
                            "A entrada D3D11 falhou ({gpu_error}) e a nova tentativa pela CPU também falhou: {cpu_error}"
                        )),
                    }
                }
            }
        }

        fn encode_cpu_input(
            &mut self,
            cpu_nv12: Option<&CpuNv12Frame>,
            rgba: &[u8],
        ) -> Result<Vec<u8>, String> {
            match cpu_nv12 {
                Some(frame) => self.encode_nv12(frame),
                None => self.encode_rgba(rgba),
            }
        }

        fn drain_output(&mut self) -> Result<Vec<u8>, String> {
            let mut output = Vec::new();
            let deadline = Instant::now() + Duration::from_millis(250);
            loop {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    break;
                }
                match self.transform.receive_output_sample(
                    remaining.min(MF_EVENT_WAIT),
                    self.transform.transform_provides_sample,
                )? {
                    Some(sample) => {
                        let bytes = read_sample(&sample)?;
                        if bytes.is_empty() {
                            break;
                        }
                        output.extend_from_slice(&bytes);
                    }
                    None if self.transform.async_transform => break,
                    None => break,
                }
                if self.transform.async_transform {
                    break;
                }
            }
            if output.is_empty() {
                // Hardware MFTs can buffer initial frames before output.
                // Treat that as warm-up and let the caller wait for SPS/PPS/IDR.
                return Ok(Vec::new());
            }
            Ok(normalize_annex_b(output))
        }
    }

    pub struct GpuNv12Surface {
        device: ID3D11Device,
        context: ID3D11DeviceContext,
        texture: ID3D11Texture2D,
        width: u32,
        height: u32,
    }

    impl GpuNv12Surface {
        pub fn device(&self) -> &ID3D11Device {
            &self.device
        }

        pub fn readback_nv12(&self) -> Result<(Vec<u8>, usize), String> {
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            unsafe { self.texture.GetDesc(&mut desc) };
            let mut staging_desc = desc;
            staging_desc.Usage = D3D11_USAGE_STAGING;
            staging_desc.BindFlags = 0;
            staging_desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
            staging_desc.MiscFlags = 0;
            let mut staging = None;
            unsafe {
                self.device
                    .CreateTexture2D(&staging_desc, None, Some(&mut staging))
                    .map_err(|error| {
                        format!("Não foi possível criar a cópia NV12 para a prévia: {error}")
                    })?;
            }
            let staging =
                staging.ok_or_else(|| "D3D11 não criou a cópia NV12 para a prévia.".to_owned())?;
            let source: ID3D11Resource = self
                .texture
                .cast()
                .map_err(|error| format!("Não foi possível acessar a superfície NV12: {error}"))?;
            let destination: ID3D11Resource = staging
                .cast()
                .map_err(|error| format!("Não foi possível acessar a cópia NV12: {error}"))?;
            unsafe { self.context.CopyResource(&destination, &source) };

            let mut mapped = Default::default();
            unsafe {
                self.context
                    .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
                    .map_err(|error| {
                        format!("Não foi possível ler a superfície NV12 reduzida: {error}")
                    })?;
                let stride = mapped.RowPitch as usize;
                let length = stride
                    .checked_mul(self.height as usize + self.height as usize / 2)
                    .ok_or_else(|| "O tamanho da prévia NV12 excedeu o limite.".to_owned())?;
                let bytes = slice::from_raw_parts(mapped.pData.cast::<u8>(), length).to_vec();
                self.context.Unmap(&staging, 0);
                Ok((bytes, stride))
            }
        }

        pub fn to_rgba(&self, nv12: &[u8], stride: usize) -> Result<Vec<u8>, String> {
            nv12_to_rgba(nv12, self.width as usize, self.height as usize, stride)
        }
    }

    pub struct GpuNv12Processor {
        device: ID3D11Device,
        context: ID3D11DeviceContext,
        video_device: ID3D11VideoDevice,
        video_context: ID3D11VideoContext,
        enumerator: ID3D11VideoProcessorEnumerator,
        processor: ID3D11VideoProcessor,
        input_width: u32,
        input_height: u32,
        output_width: u32,
        output_height: u32,
    }

    impl GpuNv12Processor {
        pub fn new(
            device: &ID3D11Device,
            context: &ID3D11DeviceContext,
            input_width: u32,
            input_height: u32,
            output_width: u32,
            output_height: u32,
        ) -> Result<Self, String> {
            let video_device: ID3D11VideoDevice = device.cast().map_err(|error| {
                format!("O dispositivo DXGI nÃ£o oferece conversÃ£o D3D11 de vÃ­deo: {error}")
            })?;
            let video_context: ID3D11VideoContext = context.cast().map_err(|error| {
                format!("O contexto DXGI nÃ£o oferece ID3D11VideoContext: {error}")
            })?;
            let description = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
                InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
                InputFrameRate: DXGI_RATIONAL {
                    Numerator: FRAME_RATE,
                    Denominator: 1,
                },
                InputWidth: input_width,
                InputHeight: input_height,
                OutputFrameRate: DXGI_RATIONAL {
                    Numerator: FRAME_RATE,
                    Denominator: 1,
                },
                OutputWidth: output_width,
                OutputHeight: output_height,
                Usage: D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
            };
            let enumerator =
                unsafe { video_device.CreateVideoProcessorEnumerator(&description) }
                    .map_err(|error| format!("NÃ£o foi possÃ­vel criar conversor D3D11: {error}"))?;
            let input_support =
                unsafe { enumerator.CheckVideoProcessorFormat(DXGI_FORMAT_B8G8R8A8_UNORM) }
                    .map_err(|error| {
                        format!("NÃ£o foi possÃ­vel consultar formato BGRA do monitor: {error}")
                    })?;
            if input_support & D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_INPUT.0 as u32 == 0 {
                return Err("A GPU nÃ£o aceita BGRA como entrada do conversor de vÃ­deo.".to_owned());
            }
            let output_support = unsafe { enumerator.CheckVideoProcessorFormat(DXGI_FORMAT_NV12) }
                .map_err(|error| {
                    format!("NÃ£o foi possÃ­vel consultar saÃ­da NV12 da GPU: {error}")
                })?;
            if output_support & D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_OUTPUT.0 as u32 == 0 {
                return Err("A GPU nÃ£o aceita NV12 como saÃ­da do conversor de vÃ­deo.".to_owned());
            }
            let processor =
                unsafe { video_device.CreateVideoProcessor(&enumerator, 0) }.map_err(|error| {
                    format!("NÃ£o foi possÃ­vel iniciar o conversor de vÃ­deo D3D11: {error}")
                })?;
            Ok(Self {
                device: device.clone(),
                context: context.clone(),
                video_device,
                video_context,
                enumerator,
                processor,
                input_width,
                input_height,
                output_width,
                output_height,
            })
        }

        pub fn process(&mut self, input: &ID3D11Texture2D) -> Result<GpuNv12Surface, String> {
            let texture_desc = D3D11_TEXTURE2D_DESC {
                Width: self.output_width,
                Height: self.output_height,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_NV12,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
                CPUAccessFlags: 0,
                MiscFlags: 0,
            };
            let mut texture = None;
            unsafe {
                self.device
                    .CreateTexture2D(&texture_desc, None, Some(&mut texture))
                    .map_err(|error| {
                        format!("NÃ£o foi possÃ­vel criar textura NV12 para o encoder: {error}")
                    })?;
            }
            let texture =
                texture.ok_or_else(|| "D3D11 nÃ£o retornou a textura NV12.".to_owned())?;
            let input_description = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
                FourCC: 0,
                ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
                    Texture2D: D3D11_TEX2D_VPIV {
                        MipSlice: 0,
                        ArraySlice: 0,
                    },
                },
            };
            let output_description = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
                ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 {
                    Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 },
                },
            };
            let mut input_view = None;
            let mut output_view = None;
            unsafe {
                self.video_device
                    .CreateVideoProcessorInputView(
                        input,
                        &self.enumerator,
                        &input_description,
                        Some(&mut input_view),
                    )
                    .map_err(|error| {
                        format!("D3D11 nÃ£o criou a visualizaÃ§Ã£o do quadro DXGI: {error}")
                    })?;
                self.video_device
                    .CreateVideoProcessorOutputView(
                        &texture,
                        &self.enumerator,
                        &output_description,
                        Some(&mut output_view),
                    )
                    .map_err(|error| {
                        format!("D3D11 nÃ£o criou a superfÃ­cie de saÃ­da NV12: {error}")
                    })?;
                let source_rect = RECT {
                    left: 0,
                    top: 0,
                    right: self.input_width as i32,
                    bottom: self.input_height as i32,
                };
                let destination_rect = RECT {
                    left: 0,
                    top: 0,
                    right: self.output_width as i32,
                    bottom: self.output_height as i32,
                };
                self.video_context.VideoProcessorSetStreamSourceRect(
                    &self.processor,
                    0,
                    true,
                    Some(&source_rect),
                );
                self.video_context.VideoProcessorSetStreamDestRect(
                    &self.processor,
                    0,
                    true,
                    Some(&destination_rect),
                );
                let stream = D3D11_VIDEO_PROCESSOR_STREAM {
                    Enable: true.into(),
                    OutputIndex: 0,
                    InputFrameOrField: 0,
                    PastFrames: 0,
                    FutureFrames: 0,
                    pInputSurface: std::mem::ManuallyDrop::new(input_view),
                    ..Default::default()
                };
                self.video_context
                    .VideoProcessorBlt(
                        &self.processor,
                        output_view.as_ref().ok_or_else(|| {
                            "D3D11 nÃ£o retornou visualizaÃ§Ã£o de saÃ­da.".to_owned()
                        })?,
                        0,
                        &[stream],
                    )
                    .map_err(|error| format!("A GPU nÃ£o converteu BGRA para NV12: {error}"))?;
            }
            Ok(GpuNv12Surface {
                device: self.device.clone(),
                context: self.context.clone(),
                texture,
                width: self.output_width,
                height: self.output_height,
            })
        }
    }

    pub struct DecodedFrame {
        pub width: u32,
        pub height: u32,
        pub rgba: Vec<u8>,
    }

    pub struct HardwareDecoder {
        _apartment: ComApartment,
        _device: ID3D11Device,
        context: ID3D11DeviceContext,
        _device_manager: IMFDXGIDeviceManager,
        transform: Transform,
        width: u32,
        height: u32,
        next_time_hns: i64,
        name: String,
    }

    impl HardwareDecoder {
        pub fn new(width: u32, height: u32) -> Result<Self, String> {
            if width < 2
                || height < 2
                || width > 1280
                || height > 720
                || width % 2 != 0
                || height % 2 != 0
            {
                return Err(format!(
                    "Dimensões H.264 inválidas para DXVA: {width}×{height}."
                ));
            }
            let apartment = ComApartment::enter()?;
            ensure_media_foundation()?;
            let (device, context, device_manager) = create_d3d11_device_manager()?;
            let (transform, name) = activate_hardware_transform(
                MFT_CATEGORY_VIDEO_DECODER,
                MFVideoFormat_H264,
                MFVideoFormat_NV12,
                width,
                height,
                Some(&device_manager),
            )?;
            Ok(Self {
                _apartment: apartment,
                _device: device,
                context,
                _device_manager: device_manager,
                transform,
                width,
                height,
                next_time_hns: 0,
                name,
            })
        }

        pub fn name(&self) -> &str {
            &self.name
        }

        pub fn decode(&mut self, access_unit: &[u8]) -> Result<Option<DecodedFrame>, String> {
            self.transform.send_input(access_unit, self.next_time_hns)?;
            self.next_time_hns += HNS_PER_SECOND / i64::from(FRAME_RATE);
            let mut decoded = None;
            for output_index in 0..8 {
                let wait = if output_index == 0 {
                    Duration::from_millis(8)
                } else {
                    Duration::from_millis(2)
                };
                let Some(sample) = self
                    .transform
                    .receive_output_sample(wait, self.transform.transform_provides_sample)?
                else {
                    break;
                };
                let (nv12, stride) = read_dxgi_nv12(&sample, &self.context)?;
                let rgba = nv12_to_rgba(&nv12, self.width as usize, self.height as usize, stride)?;
                decoded = Some(DecodedFrame {
                    width: self.width,
                    height: self.height,
                    rgba,
                });
            }
            Ok(decoded)
        }
    }

    fn activate_hardware_transform(
        category: windows::core::GUID,
        input_subtype: windows::core::GUID,
        output_subtype: windows::core::GUID,
        width: u32,
        height: u32,
        device_manager: Option<&IMFDXGIDeviceManager>,
    ) -> Result<(Transform, String), String> {
        let input_info = MFT_REGISTER_TYPE_INFO {
            guidMajorType: MFMediaType_Video,
            guidSubtype: input_subtype,
        };
        let output_info = MFT_REGISTER_TYPE_INFO {
            guidMajorType: MFMediaType_Video,
            guidSubtype: output_subtype,
        };
        let flags = MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SYNCMFT | MFT_ENUM_FLAG_ASYNCMFT;
        let mut raw_activations: *mut Option<IMFActivate> = ptr::null_mut();
        let mut count = 0u32;
        unsafe {
            MFTEnumEx(
                category,
                flags,
                Some(&input_info),
                Some(&output_info),
                &mut raw_activations,
                &mut count,
            )
            .map_err(|e| format!("MFTEnumEx não encontrou codecs H.264 de hardware: {e}"))?;
        }
        if raw_activations.is_null() || count == 0 {
            return Err("O Windows não encontrou codec H.264 de hardware compatível.".to_owned());
        }

        let mut activations = Vec::with_capacity(count as usize);
        unsafe {
            for index in 0..count as usize {
                if let Some(activation) = (*raw_activations.add(index)).take() {
                    activations.push(activation);
                }
            }
            CoTaskMemFree(Some(raw_activations.cast()));
        }

        let mut failures = Vec::new();
        for activation in activations {
            let name = unsafe {
                activation
                    .GetStringLength(&MFT_FRIENDLY_NAME_Attribute)
                    .and_then(|length| {
                        let mut value = vec![0u16; length as usize + 1];
                        activation.GetString(&MFT_FRIENDLY_NAME_Attribute, &mut value, None)?;
                        Ok(String::from_utf16_lossy(&value[..length as usize]))
                    })
            }
            .unwrap_or_else(|_| "Media Foundation H.264 de hardware".to_owned());
            match Transform::configure(
                &activation,
                input_subtype,
                output_subtype,
                width,
                height,
                device_manager,
            ) {
                Ok(transform) => return Ok((transform, name)),
                Err(error) => failures.push(format!("{name}: {error}")),
            }
        }
        Err(format!(
            "Os codecs H.264 de hardware foram encontrados, mas nenhum aceitou o fluxo: {}",
            failures.join(" | ")
        ))
    }

    fn make_video_type(
        subtype: windows::core::GUID,
        width: u32,
        height: u32,
    ) -> Result<IMFMediaType, String> {
        let media_type = unsafe { MFCreateMediaType() }
            .map_err(|e| format!("Não foi possível criar tipo de mídia: {e}"))?;
        unsafe {
            media_type
                .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
                .map_err(|e| format!("Não foi possível definir tipo de vídeo: {e}"))?;
            media_type
                .SetGUID(&MF_MT_SUBTYPE, &subtype)
                .map_err(|e| format!("Não foi possível definir formato de vídeo: {e}"))?;
            media_type
                .SetUINT64(
                    &MF_MT_FRAME_SIZE,
                    (u64::from(width) << 32) | u64::from(height),
                )
                .map_err(|e| format!("Não foi possível definir dimensões de vídeo: {e}"))?;
            media_type
                .SetUINT64(&MF_MT_FRAME_RATE, (u64::from(FRAME_RATE) << 32) | 1)
                .map_err(|e| format!("Não foi possível definir taxa de quadros: {e}"))?;
            media_type
                .SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, (1u64 << 32) | 1)
                .map_err(|e| format!("Não foi possível definir proporção de pixels: {e}"))?;
            media_type
                .SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)
                .map_err(|e| format!("Não foi possível definir vídeo progressivo: {e}"))?;
        }
        Ok(media_type)
    }

    fn make_sample(bytes: &[u8], time_hns: i64) -> Result<IMFSample, String> {
        let size = u32::try_from(bytes.len())
            .map_err(|_| "Quadro maior que o limite do Media Foundation.".to_owned())?;
        let buffer = unsafe { MFCreateMemoryBuffer(size.max(1)) }
            .map_err(|e| format!("Não foi possível alocar entrada do codec: {e}"))?;
        let mut destination = ptr::null_mut();
        unsafe {
            buffer
                .Lock(&mut destination, None, None)
                .map_err(|e| format!("Não foi possível bloquear buffer de entrada: {e}"))?;
            if !bytes.is_empty() {
                ptr::copy_nonoverlapping(bytes.as_ptr(), destination, bytes.len());
            }
            buffer
                .Unlock()
                .map_err(|e| format!("Não foi possível liberar buffer de entrada: {e}"))?;
            buffer.SetCurrentLength(size).map_err(|e| {
                format!("Não foi possível definir tamanho do quadro de entrada: {e}")
            })?;
        }
        let sample = unsafe { MFCreateSample() }
            .map_err(|e| format!("Não foi possível criar amostra de entrada: {e}"))?;
        unsafe {
            sample
                .AddBuffer(&buffer)
                .map_err(|e| format!("Não foi possível anexar buffer de entrada: {e}"))?;
            sample.SetSampleTime(time_hns).map_err(|e| e.to_string())?;
            sample
                .SetSampleDuration(HNS_PER_SECOND / i64::from(FRAME_RATE))
                .map_err(|e| e.to_string())?;
        }
        Ok(sample)
    }

    fn read_sample(sample: &IMFSample) -> Result<Vec<u8>, String> {
        let buffer = unsafe { sample.ConvertToContiguousBuffer() }
            .map_err(|e| format!("Não foi possível ler a amostra produzida pelo codec: {e}"))?;
        let mut data = ptr::null_mut();
        let mut current_length = 0u32;
        unsafe {
            buffer
                .Lock(&mut data, None, Some(&mut current_length))
                .map_err(|e| {
                    format!("O buffer de saída do codec não pode ser lido pela CPU: {e}")
                })?;
            let bytes = slice::from_raw_parts(data, current_length as usize).to_vec();
            buffer
                .Unlock()
                .map_err(|e| format!("Não foi possível liberar buffer de saída: {e}"))?;
            Ok(bytes)
        }
    }

    fn create_d3d11_device_manager()
    -> Result<(ID3D11Device, ID3D11DeviceContext, IMFDXGIDeviceManager), String> {
        let mut device = None;
        let mut feature_level = D3D_FEATURE_LEVEL(0);
        let mut context = None;
        unsafe {
            D3D11CreateDevice(
                None::<&windows::Win32::Graphics::Dxgi::IDXGIAdapter>,
                D3D_DRIVER_TYPE_HARDWARE,
                Default::default(),
                D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                Some(&mut feature_level),
                Some(&mut context),
            )
            .map_err(|e| format!("Não foi possível criar dispositivo D3D11 para DXVA: {e}"))?;
        }
        let device =
            device.ok_or_else(|| "O D3D11 não retornou um dispositivo de vídeo.".to_owned())?;
        let context =
            context.ok_or_else(|| "O D3D11 não retornou um contexto de vídeo.".to_owned())?;
        let mut token = 0u32;
        let mut manager = None;
        unsafe {
            MFCreateDXGIDeviceManager(&mut token, &mut manager)
                .map_err(|e| format!("Não foi possível criar gerenciador DXVA: {e}"))?;
        }
        let manager = manager
            .ok_or_else(|| "O Media Foundation não retornou gerenciador DXVA.".to_owned())?;
        let unknown: IUnknown = device
            .cast()
            .map_err(|e| format!("O dispositivo D3D11 não pôde ser associado ao DXVA: {e}"))?;
        unsafe {
            manager
                .ResetDevice(&unknown, token)
                .map_err(|e| format!("Não foi possível ativar DXVA no dispositivo D3D11: {e}"))?;
        }
        Ok((device, context, manager))
    }

    fn create_device_manager(device: &ID3D11Device) -> Result<IMFDXGIDeviceManager, String> {
        let mut token = 0u32;
        let mut manager = None;
        unsafe {
            MFCreateDXGIDeviceManager(&mut token, &mut manager).map_err(|error| {
                format!("Could not create the Media Foundation D3D manager: {error}")
            })?;
        }
        let manager =
            manager.ok_or_else(|| "Media Foundation returned no D3D manager.".to_owned())?;
        let unknown: IUnknown = device.cast().map_err(|error| {
            format!("Could not expose the D3D11 device to Media Foundation: {error}")
        })?;
        unsafe {
            manager.ResetDevice(&unknown, token).map_err(|error| {
                format!("Could not register the D3D11 device with Media Foundation: {error}")
            })?;
        }
        Ok(manager)
    }

    fn read_dxgi_nv12(
        sample: &IMFSample,
        context: &ID3D11DeviceContext,
    ) -> Result<(Vec<u8>, usize), String> {
        let buffer = unsafe { sample.ConvertToContiguousBuffer() }
            .map_err(|e| format!("Não foi possível acessar o quadro decodificado: {e}"))?;
        let dxgi_buffer = buffer
            .cast::<IMFDXGIBuffer>()
            .map_err(|e| format!("O decodificador DXVA não retornou uma superfície D3D11: {e}"))?;
        let source_subresource = unsafe { dxgi_buffer.GetSubresourceIndex() }
            .map_err(|e| format!("Não foi possível localizar a superfície do quadro DXVA: {e}"))?;
        let mut raw_texture = ptr::null_mut();
        unsafe {
            dxgi_buffer
                .GetResource(&ID3D11Texture2D::IID, &mut raw_texture)
                .map_err(|e| format!("Não foi possível obter textura do quadro DXVA: {e}"))?;
        }
        if raw_texture.is_null() {
            return Err("O quadro DXVA veio sem textura D3D11.".to_owned());
        }
        let texture: ID3D11Texture2D = unsafe { ID3D11Texture2D::from_raw(raw_texture) };
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { texture.GetDesc(&mut desc) };
        if desc.Format != DXGI_FORMAT_NV12 {
            return Err(format!(
                "O decodificador DXVA retornou formato {:?}, esperado NV12.",
                desc.Format
            ));
        }
        let mut staging_desc = desc;
        staging_desc.Usage = D3D11_USAGE_STAGING;
        staging_desc.BindFlags = 0;
        staging_desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
        staging_desc.MiscFlags = 0;
        staging_desc.ArraySize = 1;
        let device = unsafe { context.GetDevice() }
            .map_err(|e| format!("Não foi possível obter dispositivo do contexto DXVA: {e}"))?;
        let mut staging = None;
        unsafe {
            device
                .CreateTexture2D(&staging_desc, None, Some(&mut staging))
                .map_err(|e| format!("Não foi possível criar superfície de leitura DXVA: {e}"))?;
        }
        let staging =
            staging.ok_or_else(|| "O D3D11 não criou superfície de leitura DXVA.".to_owned())?;
        let source_resource: ID3D11Resource = texture
            .cast()
            .map_err(|e| format!("A textura DXVA não pôde ser copiada: {e}"))?;
        let staging_resource: ID3D11Resource = staging
            .cast()
            .map_err(|e| format!("A superfície DXVA não pôde ser mapeada: {e}"))?;
        unsafe {
            context.CopySubresourceRegion(
                &staging_resource,
                0,
                0,
                0,
                0,
                &source_resource,
                source_subresource,
                None,
            )
        };

        let mut mapped = Default::default();
        unsafe {
            context
                .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
                .map_err(|e| format!("Não foi possível copiar quadro DXVA para a CPU: {e}"))?;
            let stride = mapped.RowPitch as usize;
            let height = desc.Height as usize;
            let width = desc.Width as usize;
            if mapped.pData.is_null() || stride < width || height == 0 {
                context.Unmap(&staging, 0);
                return Err(format!(
                    "O DXVA retornou uma superfície sem leitura válida: stride {stride}, dimensões {width}×{height}, dados nulos={}.",
                    mapped.pData.is_null()
                ));
            }
            let length = stride
                .checked_mul(height + height / 2)
                .ok_or_else(|| "O tamanho do quadro DXVA excedeu o limite.".to_owned())?;
            let bytes = slice::from_raw_parts(mapped.pData.cast::<u8>(), length).to_vec();
            context.Unmap(&staging, 0);
            Ok((bytes, stride))
        }
    }

    fn normalize_annex_b(bytes: Vec<u8>) -> Vec<u8> {
        if bytes.starts_with(&[0, 0, 1]) || bytes.starts_with(&[0, 0, 0, 1]) {
            return bytes;
        }
        let mut output = Vec::with_capacity(bytes.len() + 32);
        let mut offset = 0;
        while offset + 4 <= bytes.len() {
            let length = u32::from_be_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
            offset += 4;
            if length == 0 || offset + length > bytes.len() {
                return bytes;
            }
            output.extend_from_slice(&[0, 0, 0, 1]);
            output.extend_from_slice(&bytes[offset..offset + length]);
            offset += length;
        }
        if offset == bytes.len() && !output.is_empty() {
            output
        } else {
            bytes
        }
    }

    fn nv12_to_rgba(
        nv12: &[u8],
        width: usize,
        height: usize,
        stride: usize,
    ) -> Result<Vec<u8>, String> {
        let y_bytes = stride
            .checked_mul(height)
            .ok_or_else(|| "Dimensões NV12 excederam o limite.".to_owned())?;
        let uv_bytes = stride
            .checked_mul(height / 2)
            .ok_or_else(|| "Dimensões UV excederam o limite.".to_owned())?;
        if stride < width || nv12.len() < y_bytes + uv_bytes {
            return Err(format!(
                "Saída DXVA NV12 incompleta: {} bytes, esperado ao menos {} para {width}×{height}.",
                nv12.len(),
                y_bytes + uv_bytes
            ));
        }
        let mut rgba = vec![0u8; width * height * 4];
        for row in 0..height {
            for column in 0..width {
                let y = i32::from(nv12[row * stride + column]).saturating_sub(16);
                let uv_offset = y_bytes + (row / 2) * stride + (column & !1);
                let u = i32::from(nv12[uv_offset]) - 128;
                let v = i32::from(nv12[uv_offset + 1]) - 128;
                let output = (row * width + column) * 4;
                rgba[output] = clamp_color((298 * y + 409 * v + 128) >> 8);
                rgba[output + 1] = clamp_color((298 * y - 100 * u - 208 * v + 128) >> 8);
                rgba[output + 2] = clamp_color((298 * y + 516 * u + 128) >> 8);
                rgba[output + 3] = 255;
            }
        }
        Ok(rgba)
    }

    fn clamp_color(value: i32) -> u8 {
        value.clamp(0, 255) as u8
    }

    /// Parses frame dimensions from an Annex-B H.264 SPS. The current app sends
    /// Baseline H.264, while the parser also skips high-profile SPS extensions.
    pub fn sps_dimensions(access_unit: &[u8]) -> Option<(u32, u32)> {
        let mut start = 0;
        while start + 4 < access_unit.len() {
            let (prefix, nal_start) = if access_unit[start..].starts_with(&[0, 0, 0, 1]) {
                (4, start + 4)
            } else if access_unit[start..].starts_with(&[0, 0, 1]) {
                (3, start + 3)
            } else {
                start += 1;
                continue;
            };
            let _ = prefix;
            let end = find_start_code(access_unit, nal_start).unwrap_or(access_unit.len());
            if nal_start < end && access_unit[nal_start] & 0x1f == 7 {
                return parse_sps_dimensions(&access_unit[nal_start + 1..end]);
            }
            start = end;
        }
        None
    }

    fn find_start_code(bytes: &[u8], from: usize) -> Option<usize> {
        (from..bytes.len().saturating_sub(2)).find(|&index| {
            bytes[index..].starts_with(&[0, 0, 1]) || bytes[index..].starts_with(&[0, 0, 0, 1])
        })
    }

    fn parse_sps_dimensions(nal: &[u8]) -> Option<(u32, u32)> {
        let mut rbsp = Vec::with_capacity(nal.len());
        let mut zeros = 0;
        for &byte in nal {
            if zeros >= 2 && byte == 3 {
                zeros = 0;
                continue;
            }
            rbsp.push(byte);
            if byte == 0 { zeros += 1 } else { zeros = 0 }
        }
        let mut bits = BitReader {
            data: &rbsp,
            bit: 0,
        };
        let profile = bits.read_bits(8)?;
        bits.read_bits(8)?;
        bits.read_bits(8)?;
        bits.read_ue()?;
        let mut chroma_format = 1;
        if matches!(
            profile,
            100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 139 | 134 | 135
        ) {
            chroma_format = bits.read_ue()?;
            if chroma_format == 3 {
                bits.read_bit()?;
            }
            bits.read_ue()?;
            bits.read_ue()?;
            bits.read_bit()?;
            if bits.read_bit()? != 0 {
                let count = if chroma_format != 3 { 8 } else { 12 };
                for index in 0..count {
                    if bits.read_bit()? != 0 {
                        skip_scaling_list(&mut bits, if index < 6 { 16 } else { 64 })?;
                    }
                }
            }
        }
        bits.read_ue()?;
        let pic_order = bits.read_ue()?;
        if pic_order == 0 {
            bits.read_ue()?;
        } else if pic_order == 1 {
            bits.read_bit()?;
            bits.read_se()?;
            bits.read_se()?;
            let cycle = bits.read_ue()?;
            if cycle > 256 {
                return None;
            }
            for _ in 0..cycle {
                bits.read_se()?;
            }
        }
        bits.read_ue()?;
        bits.read_bit()?;
        let width_mbs = bits.read_ue()?.checked_add(1)?;
        let height_map = bits.read_ue()?.checked_add(1)?;
        let frame_mbs_only = bits.read_bit()?;
        if frame_mbs_only == 0 {
            bits.read_bit()?;
        }
        bits.read_bit()?;
        let crop = bits.read_bit()?;
        let (left, right, top, bottom) = if crop != 0 {
            (
                bits.read_ue()?,
                bits.read_ue()?,
                bits.read_ue()?,
                bits.read_ue()?,
            )
        } else {
            (0, 0, 0, 0)
        };
        let width = width_mbs.checked_mul(16)?;
        let height = height_map
            .checked_mul(16)?
            .checked_mul(2 - frame_mbs_only)?;
        let sub_width = if chroma_format == 1 || chroma_format == 2 {
            2
        } else {
            1
        };
        let sub_height = if chroma_format == 1 { 2 } else { 1 };
        let crop_x = if chroma_format == 0 { 1 } else { sub_width };
        let crop_y = if chroma_format == 0 {
            2 - frame_mbs_only
        } else {
            sub_height * (2 - frame_mbs_only)
        };
        let width = width.checked_sub((left + right).checked_mul(crop_x)?)?;
        let height = height.checked_sub((top + bottom).checked_mul(crop_y)?)?;
        if width == 0
            || height == 0
            || width > 1280
            || height > 720
            || width % 2 != 0
            || height % 2 != 0
        {
            return None;
        }
        Some((width, height))
    }

    fn skip_scaling_list(bits: &mut BitReader<'_>, size: usize) -> Option<()> {
        let mut last = 8i32;
        let mut next = 8i32;
        for _ in 0..size {
            if next != 0 {
                let delta = bits.read_se()?;
                next = (last + delta + 256) % 256;
            }
            if next != 0 {
                last = next;
            }
        }
        Some(())
    }

    struct BitReader<'a> {
        data: &'a [u8],
        bit: usize,
    }

    impl BitReader<'_> {
        fn read_bit(&mut self) -> Option<u32> {
            let byte = *self.data.get(self.bit / 8)?;
            let value = u32::from((byte >> (7 - self.bit % 8)) & 1);
            self.bit += 1;
            Some(value)
        }

        fn read_bits(&mut self, count: usize) -> Option<u32> {
            let mut value = 0;
            for _ in 0..count {
                value = (value << 1) | self.read_bit()?;
            }
            Some(value)
        }

        fn read_ue(&mut self) -> Option<u32> {
            let mut zeros = 0;
            while self.read_bit()? == 0 {
                zeros += 1;
                if zeros > 31 {
                    return None;
                }
            }
            Some(((1u32 << zeros) - 1) + self.read_bits(zeros)?)
        }

        fn read_se(&mut self) -> Option<i32> {
            let code = self.read_ue()? as i32;
            Some(if code & 1 == 0 {
                -(code / 2)
            } else {
                (code + 1) / 2
            })
        }
    }
}

#[cfg(windows)]
pub(crate) use windows_backend::{
    GpuNv12Processor, GpuNv12Surface, HardwareDecoder, HardwareEncoder, sps_dimensions,
};

#[cfg(not(windows))]
pub struct HardwareEncoder;
#[cfg(not(windows))]
pub struct HardwareDecoder;
#[cfg(not(windows))]
pub struct DecodedFrame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}
#[cfg(not(windows))]
pub(crate) struct GpuNv12Processor;
#[cfg(not(windows))]
pub(crate) struct GpuNv12Surface;
#[cfg(not(windows))]
impl HardwareEncoder {
    pub fn new(_: u32, _: u32) -> Result<Self, String> {
        Err("Media Foundation só está disponível no Windows.".to_owned())
    }
}
#[cfg(not(windows))]
impl HardwareDecoder {
    pub fn new(_: u32, _: u32) -> Result<Self, String> {
        Err("DXVA só está disponível no Windows.".to_owned())
    }
}
#[cfg(not(windows))]
pub fn sps_dimensions(_: &[u8]) -> Option<(u32, u32)> {
    None
}
