//! Checked NV12 geometry. Visible pixels and allocated surface rows are distinct.
//! The normalized output owns tightly packed Y followed by interleaved UV.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Nv12Layout {
    pub surface_width: usize,
    pub surface_height: usize,
    pub pitch: usize,
    pub x: usize,
    pub y: usize,
    pub width: usize,
    pub height: usize,
}

impl Nv12Layout {
    pub fn uv_offset(self) -> Result<usize, String> {
        self.pitch
            .checked_mul(self.surface_height)
            .ok_or_else(|| "Layout NV12 excedeu o limite.".to_owned())
    }

    pub fn source_len(self) -> Result<usize, String> {
        self.surface_height
            .checked_add(self.surface_height / 2)
            .and_then(|rows| rows.checked_mul(self.pitch))
            .ok_or_else(|| "Buffer NV12 excedeu o limite.".to_owned())
    }

    pub fn packed_len(self) -> Result<usize, String> {
        if self.width < 2
            || self.height < 2
            || self.width > 1920
            || self.height > 1080
            || !self.width.is_multiple_of(2)
            || !self.height.is_multiple_of(2)
            || !self.surface_width.is_multiple_of(2)
            || !self.surface_height.is_multiple_of(2)
            || !self.x.is_multiple_of(2)
            || !self.y.is_multiple_of(2)
            || self.pitch < self.surface_width
            || self
                .x
                .checked_add(self.width)
                .is_none_or(|end| end > self.surface_width)
            || self
                .y
                .checked_add(self.height)
                .is_none_or(|end| end > self.surface_height)
        {
            return Err(format!("Layout NV12 inválido: {self:?}."));
        }
        self.width
            .checked_mul(self.height + self.height / 2)
            .ok_or_else(|| "Imagem NV12 excedeu o limite.".to_owned())
    }

    pub fn normalize(self, bytes: &[u8]) -> Result<Vec<u8>, String> {
        let length = self.packed_len()?;
        let required = self.source_len()?;
        if bytes.len() < required {
            return Err(format!(
                "Buffer NV12 incompleto: {} bytes, esperado {required}.",
                bytes.len()
            ));
        }
        let mut packed = vec![0; length];
        for row in 0..self.height {
            let start = (self.y + row) * self.pitch + self.x;
            packed[row * self.width..(row + 1) * self.width]
                .copy_from_slice(&bytes[start..start + self.width]);
        }
        let uv_source = self.uv_offset()? + (self.y / 2) * self.pitch + self.x;
        let uv_output = self.width * self.height;
        for row in 0..self.height / 2 {
            let start = uv_source + row * self.pitch;
            let output = uv_output + row * self.width;
            packed[output..output + self.width].copy_from_slice(&bytes[start..start + self.width]);
        }
        Ok(packed)
    }
}

pub(crate) fn to_rgba(
    bytes: &[u8],
    width: usize,
    height: usize,
    pitch: usize,
) -> Result<Vec<u8>, String> {
    let layout = Nv12Layout {
        surface_width: width,
        surface_height: height,
        pitch,
        x: 0,
        y: 0,
        width,
        height,
    };
    layout.packed_len()?;
    let y_bytes = layout.uv_offset()?;
    if bytes.len() < layout.source_len()? {
        return Err("Buffer NV12 incompleto na conversão RGBA.".to_owned());
    }
    let mut rgba = vec![0; width * height * 4];
    for row in 0..height {
        for column in 0..width {
            let y = i32::from(bytes[row * pitch + column]) - 16;
            let uv = y_bytes + (row / 2) * pitch + (column & !1);
            let u = i32::from(bytes[uv]) - 128;
            let v = i32::from(bytes[uv + 1]) - 128;
            let output = (row * width + column) * 4;
            rgba[output] = ((298 * y + 409 * v + 128) >> 8).clamp(0, 255) as u8;
            rgba[output + 1] = ((298 * y - 100 * u - 208 * v + 128) >> 8).clamp(0, 255) as u8;
            rgba[output + 2] = ((298 * y + 516 * u + 128) >> 8).clamp(0, 255) as u8;
            rgba[output + 3] = 255;
        }
    }
    Ok(rgba)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn padded_1080p_reads_uv_after_1088_rows_instead_of_visible_height() {
        let layout = Nv12Layout {
            surface_width: 1920,
            surface_height: 1088,
            pitch: 2048,
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
        };
        let mut source = vec![0; layout.source_len().unwrap()];
        for row in 0..1080 {
            source[row * 2048..row * 2048 + 1920].fill(126);
        }
        source[layout.uv_offset().unwrap()..].fill(128);
        let packed = layout.normalize(&source).unwrap();
        assert_eq!(packed.len(), 3_110_400);
        assert!(packed[..1920 * 1080].iter().all(|value| *value == 126));
        assert!(packed[1920 * 1080..].iter().all(|value| *value == 128));
        let fixed = to_rgba(&packed, 1920, 1080, 1920).unwrap();
        assert!(
            fixed
                .chunks_exact(4)
                .all(|pixel| pixel == [128, 128, 128, 255])
        );
        let old = to_rgba(&source, 1920, 1080, 2048).unwrap();
        assert_ne!(
            &old[..4],
            &fixed[..4],
            "the fixture must reproduce the original green strip"
        );
    }

    #[test]
    fn unpadded_720p_and_resolution_changes_preserve_pixels() {
        for (width, height) in [(1280, 720), (1920, 1080), (1280, 720)] {
            let layout = Nv12Layout {
                surface_width: width,
                surface_height: height,
                pitch: width,
                x: 0,
                y: 0,
                width,
                height,
            };
            let bytes = vec![128; layout.source_len().unwrap()];
            assert_eq!(layout.normalize(&bytes).unwrap(), bytes);
        }
    }

    #[test]
    fn crop_uses_the_surface_uv_offset_and_excludes_padding() {
        let layout = Nv12Layout {
            surface_width: 8,
            surface_height: 8,
            pitch: 12,
            x: 2,
            y: 2,
            width: 4,
            height: 4,
        };
        let mut source = vec![0; layout.source_len().unwrap()];
        for row in 2..6 {
            source[row * 12 + 2..row * 12 + 6].fill(100);
        }
        for row in 1..3 {
            let start = layout.uv_offset().unwrap() + row * 12 + 2;
            source[start..start + 4].fill(128);
        }
        let packed = layout.normalize(&source).unwrap();
        assert_eq!(&packed[..16], &[100; 16]);
        assert_eq!(&packed[16..], &[128; 8]);
    }

    #[test]
    fn rejects_truncation_invalid_geometry_and_overflow() {
        let layout = Nv12Layout {
            surface_width: 4,
            surface_height: 4,
            pitch: 4,
            x: 0,
            y: 0,
            width: 4,
            height: 4,
        };
        assert!(layout.normalize(&[0; 23]).is_err());
        for invalid in [
            Nv12Layout { width: 3, ..layout },
            Nv12Layout {
                height: 0,
                ..layout
            },
            Nv12Layout { pitch: 2, ..layout },
            Nv12Layout { x: 2, ..layout },
            Nv12Layout { y: 1, ..layout },
            Nv12Layout {
                pitch: usize::MAX,
                ..layout
            },
        ] {
            assert!(invalid.normalize(&[0; 24]).is_err());
        }
    }
}
