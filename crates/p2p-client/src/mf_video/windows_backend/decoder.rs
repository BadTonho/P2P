use super::*;

pub struct DecodedFrame {
    pub width: u32,
    pub height: u32,
    pub nv12: CpuNv12Frame,
}

pub struct HardwareDecoder {
    performance: Option<Arc<VideoPerformance>>,
    _device: ID3D11Device,
    context: ID3D11DeviceContext,
    _device_manager: IMFDXGIDeviceManager,
    transform: Transform,
    width: u32,
    height: u32,
    next_time_hns: i64,
    name: String,
    staging_texture: Option<ID3D11Texture2D>,
    output_generation: Option<u64>,
    visible_region: (usize, usize, usize, usize),
    last_layout: Option<crate::nv12::Nv12Layout>,
    // COM must outlive all transform/device references during field destruction.
    _apartment: ComApartment,
}

impl HardwareDecoder {
    pub fn new(width: u32, height: u32) -> Result<Self, String> {
        if width < 2
            || height < 2
            || width > 1920
            || height > 1080
            || !width.is_multiple_of(2)
            || !height.is_multiple_of(2)
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
            BITRATE,
            MAX_SUPPORTED_FRAME_RATE,
        )?;
        Ok(Self {
            performance: None,
            _apartment: apartment,
            _device: device,
            context,
            _device_manager: device_manager,
            transform,
            width,
            height,
            next_time_hns: 0,
            name,
            staging_texture: None,
            output_generation: None,
            visible_region: (0, 0, width as usize, height as usize),
            last_layout: None,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub(crate) fn adapter_label(&self) -> String {
        d3d11_adapter_label(&self._device)
    }

    pub(crate) fn set_performance(&mut self, performance: Arc<VideoPerformance>) {
        self.performance = Some(performance);
    }

    fn output_visible_region(&self) -> Result<(usize, usize, usize, usize), String> {
        let media_type = unsafe { self.transform.transform.GetOutputCurrentType(0) }
            .map_err(|error| format!("Não foi possível ler o layout do decoder: {error}"))?;
        for key in [MF_MT_MINIMUM_DISPLAY_APERTURE, MF_MT_GEOMETRIC_APERTURE] {
            let mut bytes = [0u8; std::mem::size_of::<MFVideoArea>()];
            if unsafe { media_type.GetBlob(&key, &mut bytes, None) }.is_ok() {
                let area =
                    unsafe { std::ptr::read_unaligned(bytes.as_ptr().cast::<MFVideoArea>()) };
                if area.OffsetX.fract != 0
                    || area.OffsetY.fract != 0
                    || area.OffsetX.value < 0
                    || area.OffsetY.value < 0
                {
                    return Err(
                        "O decoder retornou um recorte NV12 fracionário ou negativo.".to_owned(),
                    );
                }
                // Some MFTs include padding in the aperture dimensions. SPS owns
                // the visible size, but the aperture origin must still be honored.
                if area.Area.cx < self.width as i32 || area.Area.cy < self.height as i32 {
                    return Err("O recorte do decoder é menor que a imagem H.264.".to_owned());
                }
                return Ok((
                    area.OffsetX.value as usize,
                    area.OffsetY.value as usize,
                    self.width as usize,
                    self.height as usize,
                ));
            }
        }
        Ok((0, 0, self.width as usize, self.height as usize))
    }

    pub fn decode(&mut self, access_unit: &[u8]) -> Result<Option<DecodedFrame>, String> {
        {
            let _timer = StageTimer::optional(self.performance.as_ref(), Stage::DecoderSubmit);
            self.transform.send_input(access_unit, self.next_time_hns)?;
        }
        self.next_time_hns += HNS_PER_SECOND / i64::from(MAX_SUPPORTED_FRAME_RATE);
        let mut decoded = None;
        for output_index in 0..8 {
            let wait = if output_index == 0 {
                Duration::from_millis(25)
            } else {
                Duration::from_millis(2)
            };
            let sample = {
                let _timer =
                    StageTimer::optional(self.performance.as_ref(), Stage::DecoderOutputWait);
                self.transform
                    .receive_output_sample(wait, self.transform.transform_provides_sample)?
            };
            let Some(sample) = sample else {
                break;
            };
            if self.output_generation != Some(self.transform.output_generation) {
                self.visible_region = self.output_visible_region()?;
                self.output_generation = Some(self.transform.output_generation);
                self.staging_texture = None;
            }
            let (nv12, layout, copied_bytes) = {
                let _timer =
                    StageTimer::optional(self.performance.as_ref(), Stage::DecoderReadback);
                read_dxgi_nv12(
                    &sample,
                    &self.context,
                    &mut self.staging_texture,
                    self.visible_region,
                )?
            };
            if let Some(performance) = &self.performance {
                performance.record_readback(Readback::Decoder, copied_bytes);
            }
            if self.last_layout != Some(layout) {
                tracing::info!(surface_width = layout.surface_width, surface_height = layout.surface_height,
                    visible_width = layout.width, visible_height = layout.height, crop_x = layout.x, crop_y = layout.y,
                    pitch = layout.pitch, uv_offset = layout.uv_offset()?, copied_bytes,
                    adapter = %self.adapter_label(), "Layout NV12 do decoder DXVA atualizado");
                self.last_layout = Some(layout);
            }
            decoded = Some(DecodedFrame {
                width: layout.width as u32,
                height: layout.height as u32,
                nv12: CpuNv12Frame {
                    bytes: Arc::new(nv12),
                    stride: layout.width,
                },
            });
        }
        Ok(decoded)
    }
}
