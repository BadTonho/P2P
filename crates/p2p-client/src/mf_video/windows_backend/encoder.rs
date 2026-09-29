use super::*;

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
        Self::new_with_bitrate(width, height, BITRATE)
    }

    pub fn new_with_bitrate(width: u32, height: u32, bitrate: u32) -> Result<Self, String> {
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
        Self::new_gpu_with_bitrate(width, height, device, BITRATE)
    }

    pub fn new_gpu_with_bitrate(
        width: u32,
        height: u32,
        device: &ID3D11Device,
        bitrate: u32,
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
        self.encode_packed_nv12(&nv12)
    }

    pub fn encode_nv12(&mut self, frame: &CpuNv12Frame) -> Result<Vec<u8>, String> {
        let width = self.width as usize;
        let height = self.height as usize;
        if self.width % 2 != 0 || self.height % 2 != 0 || frame.stride < width {
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
        let buffer =
            unsafe { MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, &frame.texture, 0, false) }
                .map_err(|error| {
                format!("O Media Foundation nÃ£o aceitou a superfÃ­cie D3D11 NV12: {error}")
            })?;
        let sample = unsafe { MFCreateSample() }
            .map_err(|error| format!("NÃ£o foi possÃ­vel criar amostra da GPU: {error}"))?;
        unsafe {
            sample
                .AddBuffer(&buffer)
                .map_err(|error| format!("NÃ£o foi possÃ­vel anexar a superfÃ­cie D3D11: {error}"))?;
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
