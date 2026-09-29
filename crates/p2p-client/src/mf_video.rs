//! Hardware H.264 transforms backed by Windows Media Foundation.
//!
//! Media Foundation performs H.264 encode/decode on a hardware MFT when one is
//! available. DXGI monitor capture can pass scaled NV12 D3D11 surfaces directly
//! to a D3D-aware encoder; other sources keep the system-memory path.

#[cfg(windows)]
mod windows_backend;
#[cfg(windows)]
pub(crate) use windows_backend::{
    GpuNv12Processor, GpuNv12Surface, HardwareDecoder, HardwareEncoder, cpu_nv12_to_rgba,
    sps_dimensions,
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
