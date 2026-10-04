use super::*;

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
