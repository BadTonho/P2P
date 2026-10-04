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
    D3D11_MAP_READ, D3D11_SDK_VERSION, D3D11_TEX2D_VPIV, D3D11_TEX2D_VPOV, D3D11_TEXTURE2D_DESC,
    D3D11_USAGE_DEFAULT, D3D11_USAGE_STAGING, D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
    D3D11_VIDEO_PROCESSOR_CONTENT_DESC, D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_INPUT,
    D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_OUTPUT, D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC,
    D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0, D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC,
    D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0, D3D11_VIDEO_PROCESSOR_STREAM,
    D3D11_VIDEO_USAGE_PLAYBACK_NORMAL, D3D11_VPIV_DIMENSION_TEXTURE2D,
    D3D11_VPOV_DIMENSION_TEXTURE2D, D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext,
    ID3D11Resource, ID3D11Texture2D, ID3D11VideoContext, ID3D11VideoDevice, ID3D11VideoProcessor,
    ID3D11VideoProcessorEnumerator,
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
        .get_or_init(|| unsafe { MFStartup(MF_VERSION, MFSTARTUP_FULL) }.map_err(|e| e.to_string()))
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
        bitrate: u32,
    ) -> Result<Self, String> {
        let transform: IMFTransform = unsafe { activate.ActivateObject() }
            .map_err(|e| format!("Ativação do transform falhou: {e}"))?;

        let attributes = unsafe { transform.GetAttributes() }
            .map_err(|e| format!("Não foi possível consultar atributos do codec: {e}"))?;
        if decoder_device_manager.is_some()
            && unsafe { attributes.GetUINT32(&MF_SA_D3D11_AWARE) }.unwrap_or_default() == 0
        {
            return Err(
                "O MFT de hardware não anuncia suporte a superfícies D3D11 (MF_SA_D3D11_AWARE)."
                    .to_owned(),
            );
        }

        if let Some(manager) = decoder_device_manager {
            unsafe {
                transform
                    .ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, manager.as_raw() as usize)
                    .map_err(|e| {
                        format!(
                            "Não foi possível associar o dispositivo D3D11 ao decodificador: {e}"
                        )
                    })?;
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
                    .SetUINT32(&MF_MT_AVG_BITRATE, bitrate)
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
                transform
                    .SetInputType(0, &input_type, 0)
                    .map_err(|e| format!("O decodificador não aceitou H.264 como entrada: {e}"))?;
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

        let output_info = unsafe { transform.GetOutputStreamInfo(0) }
            .map_err(|e| format!("Não foi possível consultar o buffer de saída do codec: {e}"))?;
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
            let sample = unsafe { MFCreateSample() }
                .map_err(|e| format!("Não foi possível criar amostra de saída do codec: {e}"))?;
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
            Err(error) if error.code() == MF_E_TRANSFORM_STREAM_CHANGE && allow_stream_change => {
                self.renegotiate_output_type().map_err(|reason| {
                    format!("MF_E_TRANSFORM_STREAM_CHANGE: não foi possível manter a saída NV12: {reason}")
                })?;
                self.process_output_sample_inner(wait, self.transform_provides_sample, false)
            }
            Err(error) if error.code() == MF_E_TRANSFORM_STREAM_CHANGE => Err(
                "O Media Foundation continuou solicitando mudança de formato após renegociar NV12."
                    .to_owned(),
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

fn activate_hardware_transform(
    category: windows::core::GUID,
    input_subtype: windows::core::GUID,
    output_subtype: windows::core::GUID,
    width: u32,
    height: u32,
    device_manager: Option<&IMFDXGIDeviceManager>,
    bitrate: u32,
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
            bitrate,
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
        buffer
            .SetCurrentLength(size)
            .map_err(|e| format!("Não foi possível definir tamanho do quadro de entrada: {e}"))?;
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
            .map_err(|e| format!("O buffer de saída do codec não pode ser lido pela CPU: {e}"))?;
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
    let context = context.ok_or_else(|| "O D3D11 não retornou um contexto de vídeo.".to_owned())?;
    let mut token = 0u32;
    let mut manager = None;
    unsafe {
        MFCreateDXGIDeviceManager(&mut token, &mut manager)
            .map_err(|e| format!("Não foi possível criar gerenciador DXVA: {e}"))?;
    }
    let manager =
        manager.ok_or_else(|| "O Media Foundation não retornou gerenciador DXVA.".to_owned())?;
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
    let manager = manager.ok_or_else(|| "Media Foundation returned no D3D manager.".to_owned())?;
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
    staging: &mut Option<ID3D11Texture2D>,
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

    let needs_new_staging = match staging.as_ref() {
        Some(existing) => {
            let mut existing_desc = D3D11_TEXTURE2D_DESC::default();
            unsafe { existing.GetDesc(&mut existing_desc) };
            existing_desc.Width != staging_desc.Width
                || existing_desc.Height != staging_desc.Height
                || existing_desc.Format != staging_desc.Format
        }
        None => true,
    };

    if needs_new_staging {
        let device = unsafe { context.GetDevice() }
            .map_err(|e| format!("Não foi possível obter dispositivo do contexto DXVA: {e}"))?;
        let mut new_staging = None;
        unsafe {
            device
                .CreateTexture2D(&staging_desc, None, Some(&mut new_staging))
                .map_err(|e| format!("Não foi possível criar superfície de leitura DXVA: {e}"))?;
        }
        *staging = new_staging;
    }

    let staging_ref = staging
        .as_ref()
        .ok_or_else(|| "O D3D11 não criou superfície de leitura DXVA.".to_owned())?;
    let source_resource: ID3D11Resource = texture
        .cast()
        .map_err(|e| format!("A textura DXVA não pôde ser copiada: {e}"))?;
    let staging_resource: ID3D11Resource = staging_ref
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
            .Map(staging_ref, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
            .map_err(|e| format!("Não foi possível copiar quadro DXVA para a CPU: {e}"))?;
        let stride = mapped.RowPitch as usize;
        let height = desc.Height as usize;
        let width = desc.Width as usize;
        if mapped.pData.is_null() || stride < width || height == 0 {
            context.Unmap(staging_ref, 0);
            return Err(format!(
                "O DXVA retornou uma superfície sem leitura válida: stride {stride}, dimensões {width}×{height}, dados nulos={}.",
                mapped.pData.is_null()
            ));
        }
        let length = stride
            .checked_mul(height + height / 2)
            .ok_or_else(|| "O tamanho do quadro DXVA excedeu o limite.".to_owned())?;
        let bytes = slice::from_raw_parts(mapped.pData.cast::<u8>(), length).to_vec();
        context.Unmap(staging_ref, 0);
        Ok((bytes, stride))
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

pub(crate) fn cpu_nv12_to_rgba(
    frame: &CpuNv12Frame,
    width: u32,
    height: u32,
) -> Result<Vec<u8>, String> {
    nv12_to_rgba(&frame.bytes, width as usize, height as usize, frame.stride)
}

fn clamp_color(value: i32) -> u8 {
    value.clamp(0, 255) as u8
}

mod decoder;
mod encoder;
mod gpu_nv12;
mod h264;

pub use decoder::HardwareDecoder;
pub use encoder::HardwareEncoder;
pub use gpu_nv12::{GpuNv12Processor, GpuNv12Surface};
use h264::normalize_annex_b;
pub use h264::sps_dimensions;
