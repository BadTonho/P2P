use super::*;

pub struct HardwareEncoder {
    performance: Option<Arc<VideoPerformance>>,
    _device: Option<ID3D11Device>,
    _device_manager: Option<IMFDXGIDeviceManager>,
    readback_staging: Option<ID3D11Texture2D>,
    transform: Transform,
    width: u32,
    height: u32,
    bitrate: u32,
    frame_rate: u32,
    next_time_hns: i64,
    name: String,
    force_keyframe_available: Option<bool>,
    gpu_surface_input_enabled: bool,
    _apartment: ComApartment,
}

impl HardwareEncoder {
    #[allow(dead_code)]
    pub fn new(width: u32, height: u32) -> Result<Self, String> {
        Self::new_with_bitrate_and_frame_rate(width, height, BITRATE, DEFAULT_FRAME_RATE)
    }

    pub fn new_with_bitrate_and_frame_rate(
        width: u32,
        height: u32,
        bitrate: u32,
        frame_rate: u32,
    ) -> Result<Self, String> {
        let apartment = ComApartment::enter()?;
        ensure_media_foundation()?;
        let (transform, name) = activate_hardware_transform(
            MFT_CATEGORY_VIDEO_ENCODER,
            MFVideoFormat_NV12,
            MFVideoFormat_H264,
            width,
            height,
            None,
            bitrate,
            frame_rate,
        )?;
        Ok(Self {
            performance: None,
            _apartment: apartment,
            _device: None,
            _device_manager: None,
            readback_staging: None,
            transform,
            width,
            height,
            bitrate,
            frame_rate,
            next_time_hns: 0,
            name,
            force_keyframe_available: None,
            gpu_surface_input_enabled: false,
        })
    }

    #[allow(dead_code)]
    pub fn new_gpu(width: u32, height: u32, device: &ID3D11Device) -> Result<Self, String> {
        Self::new_gpu_with_bitrate_and_frame_rate(
            width,
            height,
            device,
            BITRATE,
            DEFAULT_FRAME_RATE,
        )
    }

    pub fn new_gpu_with_bitrate_and_frame_rate(
        width: u32,
        height: u32,
        device: &ID3D11Device,
        bitrate: u32,
        frame_rate: u32,
    ) -> Result<Self, String> {
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
            bitrate,
            frame_rate,
        )?;
        Ok(Self {
            performance: None,
            _apartment: apartment,
            _device: Some(device.clone()),
            _device_manager: Some(device_manager),
            readback_staging: None,
            transform,
            width,
            height,
            bitrate,
            frame_rate,
            next_time_hns: 0,
            name,
            force_keyframe_available: None,
            gpu_surface_input_enabled: true,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub(crate) fn adapter_label(&self) -> String {
        self._device.as_ref().map_or_else(
            || "unknown; system-memory MFT input, no adapter bound".to_owned(),
            d3d11_adapter_label,
        )
    }

    pub(crate) fn set_performance(&mut self, performance: Arc<VideoPerformance>) {
        self.performance = Some(performance);
    }

    fn switch_to_cpu_input(&mut self) -> Result<(), String> {
        self.gpu_surface_input_enabled = false;
        if self._device_manager.is_some() {
            let (transform, name) = activate_hardware_transform(
                MFT_CATEGORY_VIDEO_ENCODER,
                MFVideoFormat_NV12,
                MFVideoFormat_H264,
                self.width,
                self.height,
                None,
                self.bitrate,
                self.frame_rate,
            )?;
            self.transform = transform;
            self.name = name;
            self._device = None;
            self._device_manager = None;
            self.next_time_hns = 0;
            self.force_keyframe_available = None;
        }
        Ok(())
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
        if let Some(performance) = &self.performance {
            performance.record_path(FramePath::EncoderHardwareCpuRgba);
        }
        let conversion = StageTimer::optional(self.performance.as_ref(), Stage::EncoderCpuConvert);
        let expected = self.width as usize * self.height as usize * 4;
        if rgba.len() != expected {
            return Err(
                "O quadro RGBA não corresponde às dimensões do codificador de hardware.".to_owned(),
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
        drop(conversion);
        self.encode_packed_nv12(&nv12)
    }

    pub fn encode_nv12(&mut self, frame: &CpuNv12Frame) -> Result<Vec<u8>, String> {
        if let Some(performance) = &self.performance {
            performance.record_path(FramePath::EncoderHardwareCpuNv12);
        }
        let conversion = StageTimer::optional(self.performance.as_ref(), Stage::EncoderCpuConvert);
        let width = self.width as usize;
        let height = self.height as usize;
        if !self.width.is_multiple_of(2) || !self.height.is_multiple_of(2) || frame.stride < width {
            return Err("O quadro NV12 da captura tem dimensões ou stride inválidos.".to_owned());
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
        drop(conversion);
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
        {
            let _timer = StageTimer::optional(self.performance.as_ref(), Stage::EncoderSubmit);
            self.transform.send_input(nv12, self.next_time_hns)?;
        }
        self.next_time_hns += HNS_PER_SECOND / i64::from(self.frame_rate);
        self.drain_output()
    }

    pub fn encode_gpu(&mut self, frame: &GpuNv12Surface) -> Result<Vec<u8>, String> {
        if let Some(performance) = &self.performance {
            performance.record_path(FramePath::EncoderGpuSurface);
        }
        let submission = StageTimer::optional(self.performance.as_ref(), Stage::EncoderSubmit);
        if frame.width != self.width || frame.height != self.height {
            return Err(
                "A superfície NV12 da GPU não corresponde às dimensões do codificador.".to_owned(),
            );
        }
        if self._device.is_none() {
            return Err(
                "O codificador Media Foundation não foi configurado para superfícies D3D11."
                    .to_owned(),
            );
        }
        let buffer =
            unsafe { MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, &frame.texture, 0, false) }
                .map_err(|error| {
                format!("O Media Foundation não aceitou a superfície D3D11 NV12: {error}")
            })?;
        let sample = unsafe { MFCreateSample() }
            .map_err(|error| format!("Não foi possível criar amostra da GPU: {error}"))?;
        unsafe {
            sample
                .AddBuffer(&buffer)
                .map_err(|error| format!("Não foi possível anexar a superfície D3D11: {error}"))?;
        }
        self.transform.send_sample(&sample, self.next_time_hns)?;
        drop(submission);
        self.next_time_hns += HNS_PER_SECOND / i64::from(self.frame_rate);
        self.drain_output()
    }

    fn resolve_cpu_nv12<'a>(
        &mut self,
        frame: Option<&'a GpuNv12Surface>,
        cpu_nv12: Option<&'a CpuNv12Frame>,
        rgba: &[u8],
        storage: &'a mut Option<CpuNv12Frame>,
    ) -> Option<&'a CpuNv12Frame> {
        if cpu_nv12.is_some() {
            return cpu_nv12;
        }
        if let (true, Some((bytes, stride))) = (
            rgba.is_empty(),
            frame.and_then(|surface| {
                let _timer =
                    StageTimer::optional(self.performance.as_ref(), Stage::EncoderReadback);
                let result = surface.readback_nv12_into(&mut self.readback_staging).ok();
                if let (Some(performance), Some((bytes, _))) = (&self.performance, &result) {
                    performance.record_readback(Readback::Encoder, bytes.len());
                }
                result
            }),
        ) {
            *storage = Some(CpuNv12Frame {
                bytes: std::sync::Arc::new(bytes),
                stride,
            });
            return storage.as_ref();
        }
        None
    }

    pub fn encode_gpu_or_nv12_or_rgba(
        &mut self,
        frame: Option<&GpuNv12Surface>,
        cpu_nv12: Option<&CpuNv12Frame>,
        rgba: &[u8],
    ) -> Result<(Vec<u8>, bool, Option<String>), String> {
        let mut storage = None;
        if !self.gpu_surface_input_enabled {
            let effective_nv12 = self.resolve_cpu_nv12(frame, cpu_nv12, rgba, &mut storage);
            let result = match effective_nv12 {
                Some(frame) => self.encode_nv12(frame),
                None => self.encode_rgba(rgba),
            }?;
            return Ok((result, false, None));
        }
        let Some(frame) = frame else {
            if self.gpu_surface_input_enabled {
                self.switch_to_cpu_input()?;
                let effective_nv12 = self.resolve_cpu_nv12(None, cpu_nv12, rgba, &mut storage);
                return self.encode_cpu_input(effective_nv12, rgba).map(|bytes| {
                    (
                        bytes,
                        false,
                        Some("A captura não forneceu uma superfície NV12 D3D11; usando entrada pela CPU.".to_owned()),
                    )
                });
            }
            let effective_nv12 = self.resolve_cpu_nv12(None, cpu_nv12, rgba, &mut storage);
            return self
                .encode_cpu_input(effective_nv12, rgba)
                .map(|bytes| (bytes, false, None));
        };
        match self.encode_gpu(frame) {
            Ok(bytes) => Ok((bytes, true, None)),
            Err(gpu_error) => {
                self.switch_to_cpu_input().map_err(|reinit_error| {
                    format!(
                        "A entrada D3D11 falhou ({gpu_error}) e não foi possível reconfigurar o MFT para CPU: {reinit_error}"
                    )
                })?;
                let effective_nv12 =
                    self.resolve_cpu_nv12(Some(frame), cpu_nv12, rgba, &mut storage);
                match self.encode_cpu_input(effective_nv12, rgba) {
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
        let _timer = StageTimer::optional(self.performance.as_ref(), Stage::EncoderOutputWait);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gpu_encoder_recreates_cpu_transform_cleanly_when_falling_back_to_cpu_input() {
        let _apartment = match ComApartment::enter() {
            Ok(apartment) => apartment,
            Err(_) => return,
        };
        let Ok((device, _context, _manager)) = create_d3d11_device_manager() else {
            return;
        };
        let (width, height) = (320u32, 240u32);
        let Ok(mut encoder) = HardwareEncoder::new_gpu_with_bitrate_and_frame_rate(
            width, height, &device, 2_000_000, 30,
        ) else {
            return;
        };
        assert!(encoder.gpu_surface_input_enabled);
        assert!(encoder._device_manager.is_some());

        let rgba = vec![128u8; (width * height * 4) as usize];
        let mut produced_output = false;
        for frame_index in 0..30 {
            let (bytes, used_gpu, fallback) = encoder
                .encode_gpu_or_nv12_or_rgba(None, None, &rgba)
                .expect("CPU fallback after D3D11 init should not hang or fail");
            assert!(!used_gpu);
            if frame_index == 0 {
                assert!(fallback.is_some());
            }
            assert!(!encoder.gpu_surface_input_enabled);
            assert!(encoder._device_manager.is_none());
            if !bytes.is_empty() {
                produced_output = true;
            }
        }
        assert!(produced_output, "recreated CPU MFT should emit H.264 bytes");
    }
}
