use super::*;

pub struct GpuNv12Surface {
    pub(super) device: ID3D11Device,
    pub(super) context: ID3D11DeviceContext,
    pub(super) texture: ID3D11Texture2D,
    pub(super) width: u32,
    pub(super) height: u32,
}

impl GpuNv12Surface {
    pub fn device(&self) -> &ID3D11Device {
        &self.device
    }

    pub fn readback_nv12_into(
        &self,
        staging: &mut Option<ID3D11Texture2D>,
    ) -> Result<(Vec<u8>, usize), String> {
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { self.texture.GetDesc(&mut desc) };
        let mut staging_desc = desc;
        staging_desc.Usage = D3D11_USAGE_STAGING;
        staging_desc.BindFlags = 0;
        staging_desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
        staging_desc.MiscFlags = 0;

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
            let mut new_staging = None;
            unsafe {
                self.device
                    .CreateTexture2D(&staging_desc, None, Some(&mut new_staging))
                    .map_err(|error| {
                        format!("Não foi possível criar a cópia NV12 para a prévia: {error}")
                    })?;
            }
            *staging = new_staging;
        }

        let staging_ref = staging
            .as_ref()
            .ok_or_else(|| "D3D11 não criou a cópia NV12 para a prévia.".to_owned())?;
        let source: ID3D11Resource = self
            .texture
            .cast()
            .map_err(|error| format!("Não foi possível acessar a superfície NV12: {error}"))?;
        let destination: ID3D11Resource = staging_ref
            .cast()
            .map_err(|error| format!("Não foi possível acessar a cópia NV12: {error}"))?;
        unsafe { self.context.CopyResource(&destination, &source) };

        let mut mapped = Default::default();
        unsafe {
            self.context
                .Map(staging_ref, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
                .map_err(|error| {
                    format!("Não foi possível ler a superfície NV12 reduzida: {error}")
                })?;
            let stride = mapped.RowPitch as usize;
            let length = stride
                .checked_mul(self.height as usize + self.height as usize / 2)
                .ok_or_else(|| "O tamanho da prévia NV12 excedeu o limite.".to_owned())?;
            let bytes = slice::from_raw_parts(mapped.pData.cast::<u8>(), length).to_vec();
            self.context.Unmap(staging_ref, 0);
            Ok((bytes, stride))
        }
    }

    #[allow(dead_code)]
    pub fn readback_nv12(&self) -> Result<(Vec<u8>, usize), String> {
        let mut staging = None;
        self.readback_nv12_into(&mut staging)
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
        frame_rate: u32,
    ) -> Result<Self, String> {
        // O dispositivo é usado pela thread de captura, pela leitura da prévia e pelo
        // codificador Media Foundation; o contexto imediato precisa ser protegido.
        if let Ok(multithread) =
            device.cast::<windows::Win32::Graphics::Direct3D11::ID3D11Multithread>()
        {
            unsafe {
                let _ = multithread.SetMultithreadProtected(true);
            }
        }
        let video_device: ID3D11VideoDevice = device.cast().map_err(|error| {
            format!("O dispositivo DXGI não oferece conversão D3D11 de vídeo: {error}")
        })?;
        let video_context: ID3D11VideoContext = context
            .cast()
            .map_err(|error| format!("O contexto DXGI não oferece ID3D11VideoContext: {error}"))?;
        let description = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
            InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            InputFrameRate: DXGI_RATIONAL {
                Numerator: frame_rate,
                Denominator: 1,
            },
            InputWidth: input_width,
            InputHeight: input_height,
            OutputFrameRate: DXGI_RATIONAL {
                Numerator: frame_rate,
                Denominator: 1,
            },
            OutputWidth: output_width,
            OutputHeight: output_height,
            Usage: D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
        };
        let enumerator = unsafe { video_device.CreateVideoProcessorEnumerator(&description) }
            .map_err(|error| format!("Não foi possível criar conversor D3D11: {error}"))?;
        let input_support =
            unsafe { enumerator.CheckVideoProcessorFormat(DXGI_FORMAT_B8G8R8A8_UNORM) }.map_err(
                |error| format!("Não foi possível consultar formato BGRA do monitor: {error}"),
            )?;
        if input_support & D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_INPUT.0 as u32 == 0 {
            return Err("A GPU não aceita BGRA como entrada do conversor de vídeo.".to_owned());
        }
        let output_support = unsafe { enumerator.CheckVideoProcessorFormat(DXGI_FORMAT_NV12) }
            .map_err(|error| format!("Não foi possível consultar saída NV12 da GPU: {error}"))?;
        if output_support & D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_OUTPUT.0 as u32 == 0 {
            return Err("A GPU não aceita NV12 como saída do conversor de vídeo.".to_owned());
        }
        let processor =
            unsafe { video_device.CreateVideoProcessor(&enumerator, 0) }.map_err(|error| {
                format!("Não foi possível iniciar o conversor de vídeo D3D11: {error}")
            })?;
        // D3D11 bitfields: RGB_Range bit 1, YCbCr_Matrix bit 2,
        // Nominal_Range bits 4-5. Match the existing CPU BT.601 converter.
        let input_color = windows::Win32::Graphics::Direct3D11::D3D11_VIDEO_PROCESSOR_COLOR_SPACE {
            _bitfield:
                (windows::Win32::Graphics::Direct3D11::D3D11_VIDEO_PROCESSOR_NOMINAL_RANGE_0_255.0
                    as u32)
                    << 4,
        };
        let output_color = windows::Win32::Graphics::Direct3D11::D3D11_VIDEO_PROCESSOR_COLOR_SPACE {
            _bitfield: (windows::Win32::Graphics::Direct3D11::D3D11_VIDEO_PROCESSOR_NOMINAL_RANGE_16_235.0 as u32) << 4,
        };
        unsafe {
            video_context.VideoProcessorSetStreamColorSpace(&processor, 0, &input_color);
            video_context.VideoProcessorSetOutputColorSpace(&processor, &output_color);
            video_context.VideoProcessorSetOutputAlphaFillMode(
                &processor,
                windows::Win32::Graphics::Direct3D11::D3D11_VIDEO_PROCESSOR_ALPHA_FILL_MODE_OPAQUE,
                0,
            );
        }
        tracing::info!(
            input_range = "RGB full 0-255",
            output_range = "NV12 limited 16-235",
            matrix = "BT.601",
            output_alpha = "opaque",
            "Contrato de cores do conversor D3D11 configurado"
        );
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
                    format!("Não foi possível criar textura NV12 para o encoder: {error}")
                })?;
        }
        let texture = texture.ok_or_else(|| "D3D11 não retornou a textura NV12.".to_owned())?;
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
                    format!("D3D11 não criou a visualização do quadro DXGI: {error}")
                })?;
            self.video_device
                .CreateVideoProcessorOutputView(
                    &texture,
                    &self.enumerator,
                    &output_description,
                    Some(&mut output_view),
                )
                .map_err(|error| format!("D3D11 não criou a superfície de saída NV12: {error}"))?;
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
                    output_view
                        .as_ref()
                        .ok_or_else(|| "D3D11 não retornou visualização de saída.".to_owned())?,
                    0,
                    &[stream],
                )
                .map_err(|error| format!("A GPU não converteu BGRA para NV12: {error}"))?;
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

/// Controlled native comparison; no desktop capture or user pixels are read.
pub(crate) fn validate_color_conversion() -> Result<(), String> {
    let _apartment = ComApartment::enter()?;
    let (device, context, _manager) = create_d3d11_device_manager()?;
    let colors = [
        [0, 0, 0],
        [255, 255, 255],
        [64, 64, 64],
        [128, 128, 128],
        [192, 192, 192],
        [255, 0, 0],
        [0, 255, 0],
        [0, 0, 255],
    ];
    let (width, height) = (64u32, 16u32);
    let mut processor = GpuNv12Processor::new(&device, &context, width, height, width, height, 60)?;
    let mut first = None;
    for alpha in [0, 96, 255] {
        let mut rgba = Vec::new();
        let mut bgra = Vec::new();
        for _ in 0..height {
            for x in 0..width {
                let [r, g, b] = colors[(x / 8) as usize];
                rgba.extend_from_slice(&[r, g, b, 255]);
                bgra.extend_from_slice(&[b, g, r, alpha]);
            }
        }
        let cpu_yuv = YUVBuffer::from_rgba8_source(RgbaSliceU8::new(
            &rgba,
            (width as usize, height as usize),
        ));
        let mut packed = cpu_yuv.y().to_vec();
        for (&u, &v) in cpu_yuv.u().iter().zip(cpu_yuv.v()) {
            packed.extend_from_slice(&[u, v]);
        }
        let cpu = nv12_to_rgba(&packed, width as usize, height as usize, width as usize)?;
        let description = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
            ..Default::default()
        };
        let data = windows::Win32::Graphics::Direct3D11::D3D11_SUBRESOURCE_DATA {
            pSysMem: bgra.as_ptr().cast(),
            SysMemPitch: width * 4,
            SysMemSlicePitch: 0,
        };
        let mut texture = None;
        unsafe { device.CreateTexture2D(&description, Some(&data), Some(&mut texture)) }
            .map_err(|error| format!("Textura de validação D3D11 indisponível: {error}"))?;
        let surface = processor
            .process(&texture.ok_or_else(|| "Textura de validação ausente.".to_owned())?)?;
        let (bytes, pitch) = surface.readback_nv12()?;
        let actual = surface.to_rgba(&bytes, pitch)?;
        // Sample the interior of each color patch; exclude chroma boundary filtering.
        for patch in 0..colors.len() {
            let index = (8 * width as usize + patch * 8 + 4) * 4;
            for channel in 0..3 {
                if actual[index + channel].abs_diff(cpu[index + channel]) > 4 {
                    return Err(format!(
                        "Conversões CPU/GPU divergem: alpha {alpha}, faixa {patch}, canal {channel}, GPU {}, CPU {}.",
                        actual[index + channel],
                        cpu[index + channel]
                    ));
                }
            }
        }
        if let Some(first) = &first {
            if first != &actual {
                return Err("Alpha da captura alterou cores na conversão GPU.".to_owned());
            }
        } else {
            first = Some(actual);
        }
    }
    println!(
        "VIDEO_COLOR_VALIDATION_OK: GPU/CPU black, white, grays and RGB matched; capture alpha does not change GPU colors."
    );
    Ok(())
}
