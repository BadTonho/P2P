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
        let video_context: ID3D11VideoContext = context
            .cast()
            .map_err(|error| format!("O contexto DXGI nÃ£o oferece ID3D11VideoContext: {error}"))?;
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
        let enumerator = unsafe { video_device.CreateVideoProcessorEnumerator(&description) }
            .map_err(|error| format!("NÃ£o foi possÃ­vel criar conversor D3D11: {error}"))?;
        let input_support =
            unsafe { enumerator.CheckVideoProcessorFormat(DXGI_FORMAT_B8G8R8A8_UNORM) }.map_err(
                |error| format!("NÃ£o foi possÃ­vel consultar formato BGRA do monitor: {error}"),
            )?;
        if input_support & D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_INPUT.0 as u32 == 0 {
            return Err("A GPU nÃ£o aceita BGRA como entrada do conversor de vÃ­deo.".to_owned());
        }
        let output_support = unsafe { enumerator.CheckVideoProcessorFormat(DXGI_FORMAT_NV12) }
            .map_err(|error| format!("NÃ£o foi possÃ­vel consultar saÃ­da NV12 da GPU: {error}"))?;
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
        let texture = texture.ok_or_else(|| "D3D11 nÃ£o retornou a textura NV12.".to_owned())?;
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
                .map_err(|error| format!("D3D11 nÃ£o criou a superfÃ­cie de saÃ­da NV12: {error}"))?;
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
                        .ok_or_else(|| "D3D11 nÃ£o retornou visualizaÃ§Ã£o de saÃ­da.".to_owned())?,
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
