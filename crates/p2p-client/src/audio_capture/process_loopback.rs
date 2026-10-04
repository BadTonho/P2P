use std::mem::{ManuallyDrop, size_of};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use rtrb::Producer;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Media::Audio;
use windows::Win32::Media::Audio::{
    AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
    AUDCLNT_STREAMFLAGS_LOOPBACK, AUDIOCLIENT_ACTIVATION_PARAMS, AUDIOCLIENT_ACTIVATION_PARAMS_0,
    AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK, AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS,
    AudioSessionStateActive, IAudioCaptureClient, IAudioClient, IAudioSessionControl2,
    IAudioSessionManager2, IMMDeviceEnumerator, PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE,
    VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
};
use windows::Win32::System::Com::StructuredStorage::{
    PROPVARIANT, PROPVARIANT_0, PROPVARIANT_0_0, PROPVARIANT_0_0_0,
};
use windows::Win32::System::Com::{
    BLOB, CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemAlloc,
    CoTaskMemFree, CoUninitialize, IAgileObject,
};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::Win32::System::Variant::VT_BLOB;
use windows::core::{GUID, HRESULT, IUnknown, Interface, PWSTR};

use super::system_audio::{CaptureDiagnostics, OPUS_CHANNELS, OPUS_SAMPLE_RATE};

const MIN_PROCESS_LOOPBACK_BUILD: u32 = 20_348;
const PROCESS_POLL_INTERVAL: Duration = Duration::from_millis(5);
const ACTIVATION_TIMEOUT: Duration = Duration::from_secs(8);

type AudioActivationResult = windows::core::Result<IUnknown>;

#[repr(C)]
struct ActivationCompletionHandler {
    vtable: *const ActivationCompletionHandlerVtable,
    references: AtomicU32,
    sender: mpsc::SyncSender<AudioActivationResult>,
    // Keep the COM-owned blob alive even if the caller times out before completion.
    parameters: PROPVARIANT,
}

#[repr(C)]
struct ActivationCompletionHandlerVtable {
    query_interface: unsafe extern "system" fn(
        *mut core::ffi::c_void,
        *const GUID,
        *mut *mut core::ffi::c_void,
    ) -> HRESULT,
    add_ref: unsafe extern "system" fn(*mut core::ffi::c_void) -> u32,
    release: unsafe extern "system" fn(*mut core::ffi::c_void) -> u32,
    activate_completed:
        unsafe extern "system" fn(*mut core::ffi::c_void, *mut core::ffi::c_void) -> HRESULT,
}

unsafe extern "system" fn activation_query_interface(
    this: *mut core::ffi::c_void,
    iid: *const GUID,
    output: *mut *mut core::ffi::c_void,
) -> HRESULT {
    if iid.is_null() || output.is_null() {
        return HRESULT(0x80004003u32 as i32);
    }
    let iid = unsafe { &*iid };
    if iid == &IUnknown::IID
        || iid == &Audio::IActivateAudioInterfaceCompletionHandler::IID
        || iid == &IAgileObject::IID
    {
        unsafe {
            *output = this;
            activation_add_ref(this);
        }
        HRESULT(0)
    } else {
        unsafe { *output = std::ptr::null_mut() };
        HRESULT(0x80004002u32 as i32)
    }
}

unsafe extern "system" fn activation_add_ref(this: *mut core::ffi::c_void) -> u32 {
    let object = unsafe { &*this.cast::<ActivationCompletionHandler>() };
    object.references.fetch_add(1, Ordering::Relaxed) + 1
}

unsafe extern "system" fn activation_release(this: *mut core::ffi::c_void) -> u32 {
    let object = unsafe { &*this.cast::<ActivationCompletionHandler>() };
    let remaining = object.references.fetch_sub(1, Ordering::Release) - 1;
    if remaining == 0 {
        std::sync::atomic::fence(Ordering::Acquire);
        unsafe { drop(Box::from_raw(this.cast::<ActivationCompletionHandler>())) };
    }
    remaining
}

unsafe extern "system" fn activation_completed(
    this: *mut core::ffi::c_void,
    operation: *mut core::ffi::c_void,
) -> HRESULT {
    let object = unsafe { &*this.cast::<ActivationCompletionHandler>() };
    let result = if let Some(operation) =
        unsafe { Audio::IActivateAudioInterfaceAsyncOperation::from_raw_borrowed(&operation) }
    {
        let mut activation_result = HRESULT::default();
        let mut interface: Option<IUnknown> = None;
        unsafe { operation.GetActivateResult(&mut activation_result, &mut interface) }
            .and_then(|()| activation_result.ok())
            .and_then(|()| {
                interface.ok_or_else(|| {
                    windows::core::Error::new(
                        HRESULT(0x80004005u32 as i32),
                        "Windows process loopback activation returned no interface",
                    )
                })
            })
    } else {
        Err(windows::core::Error::new(
            HRESULT(0x80004003u32 as i32),
            "Windows process loopback activation returned no operation",
        ))
    };
    let _ = object.sender.send(result);
    HRESULT(0)
}

static ACTIVATION_COMPLETION_HANDLER_VTABLE: ActivationCompletionHandlerVtable =
    ActivationCompletionHandlerVtable {
        query_interface: activation_query_interface,
        add_ref: activation_add_ref,
        release: activation_release,
        activate_completed: activation_completed,
    };

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AudioApplication {
    pub(crate) display_name: String,
    pub(crate) executable_path: String,
}

pub(crate) struct ProcessLoopbackCapture {
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl Drop for ProcessLoopbackCapture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl ProcessLoopbackCapture {
    pub(crate) fn start(
        process_id: u32,
        producer: Producer<f32>,
        diagnostics: Arc<CaptureDiagnostics>,
    ) -> Result<Self, String> {
        if !process_loopback_supported()? {
            return Err("A exclusão de aplicativos do áudio exige Windows build 20348 ou posterior. O vídeo continuará, mas o áudio não será enviado.".to_owned());
        }

        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let (started_tx, started_rx) = mpsc::sync_channel(1);
        let worker = thread::Builder::new()
            .name("p2p-process-audio-capture".to_owned())
            .spawn(move || {
                let result = process_capture_loop(process_id, producer, diagnostics, worker_stop, &started_tx);
                if let Err(error) = result {
                    // The callback does not carry user or process details.
                    tracing::error!(stage = "process_loopback", error = %error, "Captura seletiva de áudio parou");
                }
            })
            .map_err(|error| format!("Não foi possível iniciar a captura seletiva de áudio: {error}"))?;

        match started_rx.recv_timeout(ACTIVATION_TIMEOUT) {
            Ok(Ok(())) => Ok(Self {
                stop,
                worker: Some(worker),
            }),
            Ok(Err(error)) => {
                stop.store(true, Ordering::Relaxed);
                let _ = worker.join();
                Err(error)
            }
            Err(_) => {
                stop.store(true, Ordering::Relaxed);
                let _ = worker.join();
                Err(
                    "O Windows não concluiu a inicialização da captura seletiva de áudio."
                        .to_owned(),
                )
            }
        }
    }
}

pub(crate) fn available_audio_applications() -> Result<Vec<AudioApplication>, String> {
    run_com_task(enumerate_audio_applications)
}

pub(crate) fn find_process_id(executable_path: &str) -> Result<Option<u32>, String> {
    find_process_id_by_path(executable_path)
}

pub(crate) fn process_loopback_supported() -> Result<bool, String> {
    let mut version = RtlOsVersionInfo {
        dwOSVersionInfoSize: size_of::<RtlOsVersionInfo>() as u32,
        ..Default::default()
    };
    // RtlGetVersion reports the real build number regardless of the app manifest.
    let status = unsafe { RtlGetVersion(&mut version) };
    if status < 0 {
        return Err(
            "Não foi possível consultar a versão do Windows para a captura seletiva de áudio."
                .to_owned(),
        );
    }
    Ok(version.dwBuildNumber >= MIN_PROCESS_LOOPBACK_BUILD)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum InitialCaptureRoute {
    FullSystemAudio,
    ExcludeProcessTree(u32),
    BlockUnsupported,
}

pub(super) fn initial_capture_route(
    exclusion_configured: bool,
    supported: bool,
    matching_process_id: Option<u32>,
) -> InitialCaptureRoute {
    if exclusion_configured && !supported {
        InitialCaptureRoute::BlockUnsupported
    } else if exclusion_configured {
        matching_process_id
            .map(InitialCaptureRoute::ExcludeProcessTree)
            .unwrap_or(InitialCaptureRoute::FullSystemAudio)
    } else {
        InitialCaptureRoute::FullSystemAudio
    }
}

pub(super) fn should_switch_to_process_tree(
    current_process_id: Option<u32>,
    found_process_id: u32,
) -> bool {
    current_process_id != Some(found_process_id)
}

fn exclusion_target_mode() -> windows::Win32::Media::Audio::PROCESS_LOOPBACK_MODE {
    PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE
}

#[repr(C)]
#[allow(non_snake_case)]
struct RtlOsVersionInfo {
    dwOSVersionInfoSize: u32,
    dwMajorVersion: u32,
    dwMinorVersion: u32,
    dwBuildNumber: u32,
    dwPlatformId: u32,
    szCSDVersion: [u16; 128],
}

impl Default for RtlOsVersionInfo {
    fn default() -> Self {
        unsafe { std::mem::zeroed() }
    }
}

#[link(name = "ntdll")]
unsafe extern "system" {
    fn RtlGetVersion(version: *mut RtlOsVersionInfo) -> i32;
}

fn process_capture_loop(
    process_id: u32,
    mut producer: Producer<f32>,
    diagnostics: Arc<CaptureDiagnostics>,
    stop: Arc<AtomicBool>,
    started: &mpsc::SyncSender<Result<(), String>>,
) -> Result<(), String> {
    let com_result = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    if com_result.is_err() {
        let error = format!("Não foi possível inicializar o Windows Audio: {com_result:?}");
        let _ = started.send(Err(error.clone()));
        return Err(error);
    }
    let _com = ComApartment;

    let setup = unsafe { activate_process_audio_client(process_id) }
        .and_then(|client| unsafe { initialize_capture_client(client) });
    let (client, capture, input_rate, channels, block_align, sample_format) = match setup {
        Ok(setup) => setup,
        Err(error) => {
            let _ = started.send(Err(error.clone()));
            return Err(error);
        }
    };

    if let Err(error) = unsafe { client.Start() } {
        let message = format!("O Windows não iniciou a captura seletiva de áudio: {error}");
        let _ = started.send(Err(message.clone()));
        return Err(message);
    }
    let _ = started.send(Ok(()));

    let mut resample_phase = 0.0;
    let rate_ratio = f64::from(OPUS_SAMPLE_RATE) / f64::from(input_rate.max(1));
    let channels = channels.max(1);

    while !stop.load(Ordering::Relaxed) {
        let packet_frames = match unsafe { capture.GetNextPacketSize() } {
            Ok(frames) => frames,
            Err(error) => {
                let message = format!("Falha ao consultar os pacotes de áudio do Windows: {error}");
                diagnostics.set_fatal_error(None, message.clone());
                let _ = unsafe { client.Stop() };
                return Err(message);
            }
        };
        if packet_frames == 0 {
            thread::sleep(PROCESS_POLL_INTERVAL);
            continue;
        }

        let mut data = std::ptr::null_mut();
        let mut frames = 0u32;
        let mut flags = 0u32;
        if let Err(error) =
            unsafe { capture.GetBuffer(&mut data, &mut frames, &mut flags, None, None) }
        {
            let message = format!("Falha ao obter áudio do Windows: {error}");
            diagnostics.set_fatal_error(None, message.clone());
            let _ = unsafe { client.Stop() };
            return Err(message);
        }

        diagnostics.callbacks.fetch_add(1, Ordering::Relaxed);
        diagnostics
            .input_frames
            .fetch_add(u64::from(frames), Ordering::Relaxed);
        let mut non_silent = 0u64;
        let mut captured = 0u64;
        let mut dropped = 0u64;
        if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 == 0 && !data.is_null() {
            let samples =
                unsafe { std::slice::from_raw_parts(data, frames as usize * block_align) };
            for frame in samples.chunks_exact(block_align) {
                let (left, right) = match decode_stereo_frame(frame, channels, sample_format) {
                    Ok(samples) => samples,
                    Err(error) => {
                        let _ = unsafe { capture.ReleaseBuffer(frames) };
                        if captured > 0 {
                            diagnostics
                                .captured_frames
                                .fetch_add(captured, Ordering::Relaxed);
                        }
                        if dropped > 0 {
                            diagnostics
                                .dropped_frames
                                .fetch_add(dropped, Ordering::Relaxed);
                        }
                        if non_silent > 0 {
                            diagnostics
                                .non_silent_samples
                                .fetch_add(non_silent, Ordering::Relaxed);
                        }
                        diagnostics.set_fatal_error(None, error.clone());
                        let _ = unsafe { client.Stop() };
                        return Err(error);
                    }
                };
                non_silent += u64::from(left.abs() > 0.001);
                non_silent += u64::from(right.abs() > 0.001);
                resample_phase += rate_ratio;
                while resample_phase >= 1.0 {
                    if producer.slots() >= OPUS_CHANNELS {
                        let _ = producer.push(left);
                        let _ = producer.push(right);
                        captured += 1;
                    } else {
                        dropped += 1;
                    }
                    resample_phase -= 1.0;
                }
            }
        } else {
            // Silent packets still advance the resampler and deliver silence.
            for _ in 0..frames {
                resample_phase += rate_ratio;
                while resample_phase >= 1.0 {
                    if producer.slots() >= OPUS_CHANNELS {
                        let _ = producer.push(0.0);
                        let _ = producer.push(0.0);
                        captured += 1;
                    } else {
                        dropped += 1;
                    }
                    resample_phase -= 1.0;
                }
            }
        }
        if captured > 0 {
            diagnostics
                .captured_frames
                .fetch_add(captured, Ordering::Relaxed);
        }
        if dropped > 0 {
            diagnostics
                .dropped_frames
                .fetch_add(dropped, Ordering::Relaxed);
        }
        if non_silent > 0 {
            diagnostics
                .non_silent_samples
                .fetch_add(non_silent, Ordering::Relaxed);
        }
        if let Err(error) = unsafe { capture.ReleaseBuffer(frames) } {
            let message = format!("Falha ao liberar um pacote de áudio do Windows: {error}");
            diagnostics.set_fatal_error(None, message.clone());
            let _ = unsafe { client.Stop() };
            return Err(message);
        }
    }

    let _ = unsafe { client.Stop() };
    Ok(())
}

#[derive(Clone, Copy)]
struct SampleFormat {
    encoding: SampleEncoding,
    bits: u16,
}

#[derive(Clone, Copy)]
enum SampleEncoding {
    Float,
    Pcm,
}

fn process_loopback_pcm_format() -> Audio::WAVEFORMATEX {
    let channels = OPUS_CHANNELS as u16;
    let bits = 16;
    let block_align = channels * bits / 8;
    Audio::WAVEFORMATEX {
        wFormatTag: 1, // WAVE_FORMAT_PCM
        nChannels: channels,
        nSamplesPerSec: OPUS_SAMPLE_RATE,
        nAvgBytesPerSec: OPUS_SAMPLE_RATE * u32::from(block_align),
        nBlockAlign: block_align,
        wBitsPerSample: bits,
        cbSize: 0,
    }
}

struct MixFormatAllocation(*const Audio::WAVEFORMATEX);

impl Drop for MixFormatAllocation {
    fn drop(&mut self) {
        unsafe { CoTaskMemFree(Some(self.0.cast())) };
    }
}

unsafe fn initialize_capture_client(
    client: IAudioClient,
) -> Result<
    (
        IAudioClient,
        IAudioCaptureClient,
        u32,
        usize,
        usize,
        SampleFormat,
    ),
    String,
> {
    // The virtual process-loopback client can return E_NOTIMPL for GetMixFormat.
    // In that case request PCM explicitly, as in Microsoft's ApplicationLoopback
    // sample, and let WASAPI convert the process mix to the requested format.
    let requested_format = process_loopback_pcm_format();
    let (wave, _mix_format_allocation, stream_flags) = match unsafe { client.GetMixFormat() } {
        Ok(wave) => (
            wave.cast_const(),
            Some(MixFormatAllocation(wave)),
            AUDCLNT_STREAMFLAGS_LOOPBACK,
        ),
        Err(error) if error.code() == HRESULT(0x80004001u32 as i32) => (
            &requested_format as *const Audio::WAVEFORMATEX,
            None,
            AUDCLNT_STREAMFLAGS_LOOPBACK | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
        ),
        Err(error) => {
            return Err(format!(
                "O Windows não forneceu o formato da captura seletiva: {error}"
            ));
        }
    };
    if wave.is_null() {
        return Err(
            "O Windows não forneceu um formato de áudio válido para a exclusão.".to_owned(),
        );
    }
    let format = unsafe { *wave };
    let encoding = match format.wFormatTag {
        3 => SampleEncoding::Float,
        1 => SampleEncoding::Pcm,
        0xfffe => {
            let subformat = unsafe {
                std::ptr::addr_of!((*wave.cast::<Audio::WAVEFORMATEXTENSIBLE>()).SubFormat)
                    .read_unaligned()
            };
            if subformat == float_subformat_guid() {
                SampleEncoding::Float
            } else if subformat == pcm_subformat_guid() {
                SampleEncoding::Pcm
            } else {
                return Err(
                    "O formato de áudio do Windows não é compatível com a exclusão.".to_owned(),
                );
            }
        }
        _ => {
            return Err(
                "O formato de áudio do Windows não é compatível com a exclusão.".to_owned(),
            );
        }
    };
    let bits = format.wBitsPerSample;
    if !matches!(bits, 8 | 16 | 24 | 32)
        || (matches!(encoding, SampleEncoding::Float) && bits != 32)
    {
        return Err(
            "A profundidade do áudio do Windows não é compatível com a exclusão.".to_owned(),
        );
    }

    let sample_format = SampleFormat { encoding, bits };
    let rate = format.nSamplesPerSec.max(1);
    let channels = usize::from(format.nChannels).max(1);
    let block_align = usize::from(format.nBlockAlign).max(1);
    unsafe {
        client
            .Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                stream_flags,
                2_000_000,
                0,
                wave,
                None,
            )
            .map_err(|error| {
                format!("O Windows não conseguiu configurar a captura seletiva: {error}")
            })?;
        let capture = client
            .GetService::<IAudioCaptureClient>()
            .map_err(|error| {
                format!("O Windows não disponibilizou os dados da captura seletiva: {error}")
            })?;
        Ok((client, capture, rate, channels, block_align, sample_format))
    }
}

fn process_activation_parameters(process_id: u32) -> Result<PROPVARIANT, String> {
    let process_loopback = AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
        TargetProcessId: process_id,
        ProcessLoopbackMode: exclusion_target_mode(),
    };
    let params = AUDIOCLIENT_ACTIVATION_PARAMS {
        ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
        Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
            ProcessLoopbackParams: process_loopback,
        },
    };
    // PROPVARIANT::drop calls PropVariantClear, which frees VT_BLOB through the COM
    // task allocator. Pointing it at a stack local causes STATUS_HEAP_CORRUPTION.
    let allocation = unsafe { CoTaskMemAlloc(size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>()) }
        .cast::<AUDIOCLIENT_ACTIVATION_PARAMS>();
    if allocation.is_null() {
        return Err(
            "Não foi possível alocar os parâmetros da captura seletiva de áudio.".to_owned(),
        );
    }
    unsafe { allocation.write(params) };
    let blob = BLOB {
        cbSize: size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32,
        pBlobData: allocation.cast(),
    };
    let mut property = PROPVARIANT::default();
    property.Anonymous = PROPVARIANT_0 {
        Anonymous: ManuallyDrop::new(PROPVARIANT_0_0 {
            vt: VT_BLOB,
            wReserved1: 0,
            wReserved2: 0,
            wReserved3: 0,
            Anonymous: PROPVARIANT_0_0_0 { blob },
        }),
    };
    Ok(property)
}

fn activation_completion_handler(
    process_id: u32,
) -> Result<
    (
        Audio::IActivateAudioInterfaceCompletionHandler,
        mpsc::Receiver<AudioActivationResult>,
    ),
    String,
> {
    let (sender, receiver) = mpsc::sync_channel(1);
    let handler = Box::into_raw(Box::new(ActivationCompletionHandler {
        vtable: &ACTIVATION_COMPLETION_HANDLER_VTABLE,
        references: AtomicU32::new(1),
        sender,
        parameters: process_activation_parameters(process_id)?,
    }));
    // from_raw owns the initial COM reference and releases it on every exit path.
    let handler =
        unsafe { Audio::IActivateAudioInterfaceCompletionHandler::from_raw(handler.cast()) };
    Ok((handler, receiver))
}

unsafe fn activate_process_audio_client(process_id: u32) -> Result<IAudioClient, String> {
    let (handler, receiver) = activation_completion_handler(process_id)?;
    let parameters =
        unsafe { &(*handler.as_raw().cast::<ActivationCompletionHandler>()).parameters };
    let _operation = unsafe {
        Audio::ActivateAudioInterfaceAsync(
            VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
            &IAudioClient::IID,
            Some(parameters),
            &handler,
        )
    }
    .map_err(|error| format!("Windows rejected selective audio capture: {error}"))?;
    let activated = receiver
        .recv_timeout(ACTIVATION_TIMEOUT)
        .map_err(|_| "Windows timed out while opening selective audio capture.".to_owned())?
        .map_err(|error| format!("Windows could not open selective audio capture: {error}"))?;
    activated
        .cast::<IAudioClient>()
        .map_err(|error| format!("Windows returned an incompatible audio interface: {error}"))
}

fn decode_stereo_frame(
    frame: &[u8],
    channels: usize,
    format: SampleFormat,
) -> Result<(f32, f32), String> {
    let bytes_per_sample = usize::from(format.bits / 8);
    let sample_at = |channel: usize| -> Result<f32, String> {
        let start = channel.min(channels - 1) * bytes_per_sample;
        let bytes = frame.get(start..start + bytes_per_sample).ok_or_else(|| {
            "O Windows retornou um pacote de áudio incompleto para a exclusão.".to_owned()
        })?;
        match format.encoding {
            SampleEncoding::Float => {
                Ok(f32::from_le_bytes(bytes.try_into().expect("32-bit float")))
            }
            SampleEncoding::Pcm => match format.bits {
                8 => Ok((f32::from(bytes[0]) - 128.0) / 128.0),
                16 => {
                    Ok(i16::from_le_bytes(bytes.try_into().expect("16-bit PCM")) as f32 / 32768.0)
                }
                24 => {
                    let raw = i32::from(bytes[0])
                        | (i32::from(bytes[1]) << 8)
                        | (i32::from(bytes[2]) << 16);
                    let signed = (raw << 8) >> 8;
                    Ok(signed as f32 / 8_388_608.0)
                }
                32 => Ok(
                    i32::from_le_bytes(bytes.try_into().expect("32-bit PCM")) as f32
                        / 2_147_483_648.0,
                ),
                _ => Err("Profundidade PCM não suportada.".to_owned()),
            },
        }
    };
    let left = sample_at(0)?.clamp(-1.0, 1.0);
    let right = if channels > 1 { sample_at(1)? } else { left };
    Ok((left, right.clamp(-1.0, 1.0)))
}

fn float_subformat_guid() -> windows::core::GUID {
    windows::core::GUID::from_u128(0x00000003_0000_0010_8000_00aa00389b71)
}

fn pcm_subformat_guid() -> windows::core::GUID {
    windows::core::GUID::from_u128(0x00000001_0000_0010_8000_00aa00389b71)
}

struct ComApartment;

impl Drop for ComApartment {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}

fn run_com_task<T: Send + 'static>(
    task: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    thread::Builder::new()
        .name("p2p-audio-app-list".to_owned())
        .spawn(move || {
            let result = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
            if result.is_err() {
                return Err(format!(
                    "Não foi possível consultar sessões de áudio do Windows: {result:?}"
                ));
            }
            let _com = ComApartment;
            task()
        })
        .map_err(|error| format!("Não foi possível iniciar a consulta de aplicativos: {error}"))?
        .join()
        .map_err(|_| "A consulta de aplicativos com áudio foi interrompida.".to_owned())?
}

fn enumerate_audio_applications() -> Result<Vec<AudioApplication>, String> {
    unsafe {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&Audio::MMDeviceEnumerator, None, CLSCTX_ALL).map_err(|error| {
                format!("Não foi possível acessar as sessões de áudio do Windows: {error}")
            })?;
        let device = enumerator
            .GetDefaultAudioEndpoint(Audio::eRender, Audio::eMultimedia)
            .map_err(|error| {
                format!("Não foi possível consultar a saída de áudio padrão do Windows: {error}")
            })?;
        let manager: IAudioSessionManager2 =
            device.Activate(CLSCTX_ALL, None).map_err(|error| {
                format!("Não foi possível listar aplicativos com sessão de áudio: {error}")
            })?;
        let sessions = manager.GetSessionEnumerator().map_err(|error| {
            format!("Não foi possível listar aplicativos com sessão de áudio: {error}")
        })?;
        let count = sessions
            .GetCount()
            .map_err(|error| format!("Não foi possível contar as sessões de áudio: {error}"))?;
        let mut applications = Vec::<AudioApplication>::new();
        for index in 0..count {
            let control = match sessions.GetSession(index) {
                Ok(control) => control,
                Err(_) => continue,
            };
            let control: IAudioSessionControl2 = match control.cast() {
                Ok(control) => control,
                Err(_) => continue,
            };
            if control.GetState().ok() != Some(AudioSessionStateActive) {
                continue;
            }
            let process_id = match control.GetProcessId() {
                Ok(process_id) if process_id > 0 => process_id,
                _ => continue,
            };
            let Some(path) = process_image_path(process_id) else {
                continue;
            };
            if applications
                .iter()
                .any(|app| paths_match(&app.executable_path, &path))
            {
                continue;
            }
            let display_name = Path::new(&path)
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("Aplicativo")
                .to_owned();
            applications.push(AudioApplication {
                display_name,
                executable_path: path,
            });
        }
        applications.sort_by(|a, b| {
            a.display_name
                .to_lowercase()
                .cmp(&b.display_name.to_lowercase())
        });
        Ok(applications)
    }
}

fn find_process_id_by_path(executable_path: &str) -> Result<Option<u32>, String> {
    let expected_name = Path::new(executable_path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_lowercase();
    if expected_name.is_empty() {
        return Ok(None);
    }

    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0).map_err(|_| {
            "Não foi possível consultar processos para aplicar a exclusão de áudio.".to_owned()
        })?;
        let _snapshot = HandleGuard(snapshot);
        let mut entry = PROCESSENTRY32W {
            dwSize: size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        if Process32FirstW(snapshot, &mut entry).is_err() {
            return Ok(None);
        }
        loop {
            let name = String::from_utf16_lossy(&entry.szExeFile)
                .trim_end_matches('\0')
                .to_lowercase();
            if name == expected_name
                && process_image_path(entry.th32ProcessID)
                    .is_some_and(|path| paths_match(&path, executable_path))
            {
                return Ok(Some(entry.th32ProcessID));
            }
            if Process32NextW(snapshot, &mut entry).is_err() {
                break;
            }
        }
    }
    Ok(None)
}

fn normalize_path(path: &str) -> PathBuf {
    PathBuf::from(path)
        .to_string_lossy()
        .replace('/', "\\")
        .to_lowercase()
        .into()
}

fn paths_match(left: &str, right: &str) -> bool {
    normalize_path(left) == normalize_path(right)
}

fn process_image_path(process_id: u32) -> Option<String> {
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, process_id).ok()?;
        let _process = HandleGuard(process);
        let mut buffer = vec![0u16; 32_768];
        let mut length = buffer.len() as u32;
        QueryFullProcessImageNameW(
            process,
            Default::default(),
            PWSTR(buffer.as_mut_ptr()),
            &mut length,
        )
        .ok()?;
        buffer.truncate(length as usize);
        String::from_utf16(&buffer).ok()
    }
}

struct HandleGuard(HANDLE);

impl Drop for HandleGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        InitialCaptureRoute, SampleEncoding, SampleFormat, decode_stereo_frame,
        exclusion_target_mode, float_subformat_guid, initial_capture_route, normalize_path,
        paths_match, pcm_subformat_guid, should_switch_to_process_tree,
    };

    #[test]
    fn native_activation_cleanup_does_not_corrupt_the_process() {
        use std::os::windows::process::CommandExt;

        const CHILD_FLAG: &str = "P2P_TEST_PROCESS_LOOPBACK_ACTIVATION_CHILD";
        const TEST_NAME: &str = "audio_capture::process_loopback::tests::native_activation_cleanup_does_not_corrupt_the_process";
        if std::env::var_os(CHILD_FLAG).as_deref() == Some(std::ffi::OsStr::new("1")) {
            super::run_com_task(|| {
                // Activate and initialize the virtual client without starting it or reading audio.
                // Both success and unavailable-API errors must release native resources safely.
                for _ in 0..3 {
                    match unsafe { super::activate_process_audio_client(std::process::id()) } {
                        Ok(client) => {
                            println!("Native process-loopback activation succeeded");
                            match unsafe { super::initialize_capture_client(client) } {
                                Ok(capture) => {
                                    println!("Native process-loopback initialization succeeded");
                                    drop(capture);
                                }
                                Err(error) => {
                                    println!("Native initialization unavailable: {error}")
                                }
                            }
                        }
                        Err(error) => println!("Native activation unavailable: {error}"),
                    }
                }
                Ok(())
            })
            .unwrap();
            return;
        }

        // A native heap-corruption exception cannot unwind. Isolate it so a regression
        // becomes a failed test instead of terminating the entire workspace test suite.
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", TEST_NAME, "--nocapture"])
            .env(CHILD_FLAG, "1")
            .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "Native activation/cleanup terminated the process: {:?}\n{}\n{}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        println!("{}", String::from_utf8_lossy(&output.stdout));
    }

    #[test]
    fn activation_blob_owns_its_memory_and_can_be_copied_and_released_repeatedly() {
        for process_id in 1..=32 {
            let parameters = super::process_activation_parameters(process_id).unwrap();
            let header = unsafe { &parameters.Anonymous.Anonymous };
            assert_eq!(header.vt, super::VT_BLOB);
            let blob = unsafe { header.Anonymous.blob };
            assert_eq!(
                blob.cbSize as usize,
                std::mem::size_of::<super::AUDIOCLIENT_ACTIVATION_PARAMS>()
            );

            // PROPVARIANT uses the native copy/clear operations. A copy must own a
            // separate blob and stay valid after the original has been released.
            let copy = parameters.clone();
            let copied_blob = unsafe { copy.Anonymous.Anonymous.Anonymous.blob };
            assert_ne!(copied_blob.pBlobData, blob.pBlobData);
            drop(parameters);

            let actual = unsafe {
                copied_blob
                    .pBlobData
                    .cast::<super::AUDIOCLIENT_ACTIVATION_PARAMS>()
                    .read()
            };
            assert_eq!(
                actual.ActivationType,
                super::AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK
            );
            let target = unsafe { actual.Anonymous.ProcessLoopbackParams };
            assert_eq!(target.TargetProcessId, process_id);
            assert_eq!(target.ProcessLoopbackMode, exclusion_target_mode());
            drop(copy);
        }
    }

    #[test]
    fn virtual_capture_has_a_complete_pcm_format_when_mix_format_is_unavailable() {
        let format = super::process_loopback_pcm_format();
        assert_eq!(
            (
                format.wFormatTag,
                format.nSamplesPerSec,
                format.nChannels,
                format.wBitsPerSample
            ),
            (1, 48_000, 2, 16)
        );
        assert_eq!(
            (format.nBlockAlign, format.nAvgBytesPerSec, format.cbSize),
            (4, 192_000, 0)
        );
        let sample_format = SampleFormat {
            encoding: SampleEncoding::Pcm,
            bits: format.wBitsPerSample,
        };
        let frame = [16_384i16.to_le_bytes(), (-16_384i16).to_le_bytes()].concat();
        assert_eq!(
            decode_stereo_frame(&frame, format.nChannels as usize, sample_format).unwrap(),
            (0.5, -0.5)
        );
    }

    #[test]
    fn completion_handler_is_agile_and_handles_a_callback_after_the_caller_times_out() {
        use windows::core::Interface;

        let (handler, receiver) = super::activation_completion_handler(42).unwrap();
        assert!(handler.cast::<super::IAudioClient>().is_err());
        let windows_reference = handler.cast::<super::IAgileObject>().unwrap();
        drop(handler);
        drop(receiver); // The waiting caller timed out; Windows still owns a reference.

        let completion = windows_reference
            .cast::<super::Audio::IActivateAudioInterfaceCompletionHandler>()
            .unwrap();
        drop(windows_reference);
        assert!(
            unsafe { super::activation_completed(completion.as_raw(), std::ptr::null_mut()) }
                .is_ok()
        );
        drop(completion);

        let (handler, receiver) = super::activation_completion_handler(84).unwrap();
        let windows_reference = handler.clone();
        drop(handler);
        assert!(matches!(
            receiver.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty)
        ));
        drop(windows_reference);
        assert!(matches!(
            receiver.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Disconnected)
        ));
    }

    #[test]
    fn chooses_full_audio_without_a_target_and_waits_for_a_closed_application() {
        assert_eq!(
            initial_capture_route(false, true, None),
            InitialCaptureRoute::FullSystemAudio
        );
        assert_eq!(
            initial_capture_route(true, true, None),
            InitialCaptureRoute::FullSystemAudio
        );
    }

    #[test]
    fn excludes_the_selected_process_tree_and_fails_closed_when_unsupported() {
        assert_eq!(
            initial_capture_route(true, true, Some(42)),
            InitialCaptureRoute::ExcludeProcessTree(42)
        );
        assert_eq!(
            initial_capture_route(true, false, Some(42)),
            InitialCaptureRoute::BlockUnsupported
        );
        assert_eq!(
            initial_capture_route(true, false, None),
            InitialCaptureRoute::BlockUnsupported
        );
        assert_eq!(exclusion_target_mode().0, 1);
    }

    #[test]
    fn applies_only_a_new_matching_process_and_keeps_independent_targets_separate() {
        assert!(!should_switch_to_process_tree(Some(42), 42));
        assert!(should_switch_to_process_tree(None, 42));
        assert!(should_switch_to_process_tree(Some(42), 84));
    }

    #[test]
    fn normalizes_executable_paths_without_changing_the_saved_value() {
        assert_eq!(
            normalize_path("C:/Apps/Player.exe"),
            normalize_path("c:\\apps\\player.EXE")
        );
        assert!(!paths_match("C:\\Apps\\Player.exe", "C:\\Apps\\Other.exe"));
    }

    #[test]
    fn decodes_float_pcm_and_mono_frames_for_loopback() {
        let float = SampleFormat {
            encoding: SampleEncoding::Float,
            bits: 32,
        };
        let samples = [0.25f32.to_le_bytes(), (-0.5f32).to_le_bytes()].concat();
        assert_eq!(
            decode_stereo_frame(&samples, 2, float).unwrap(),
            (0.25, -0.5)
        );

        let pcm16 = SampleFormat {
            encoding: SampleEncoding::Pcm,
            bits: 16,
        };
        let mono = 16384i16.to_le_bytes();
        assert_eq!(decode_stereo_frame(&mono, 1, pcm16).unwrap(), (0.5, 0.5));
    }

    #[test]
    fn recognizes_float_and_pcm_wave_subformats() {
        assert_ne!(float_subformat_guid(), pcm_subformat_guid());
    }
}
