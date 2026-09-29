pub(super) fn normalize_annex_b(bytes: Vec<u8>) -> Vec<u8> {
    if bytes.starts_with(&[0, 0, 1]) || bytes.starts_with(&[0, 0, 0, 1]) {
        return bytes;
    }
    let mut output = Vec::with_capacity(bytes.len() + 32);
    let mut offset = 0;
    while offset + 4 <= bytes.len() {
        let length = u32::from_be_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
        offset += 4;
        if length == 0 || offset + length > bytes.len() {
            return bytes;
        }
        output.extend_from_slice(&[0, 0, 0, 1]);
        output.extend_from_slice(&bytes[offset..offset + length]);
        offset += length;
    }
    if offset == bytes.len() && !output.is_empty() {
        output
    } else {
        bytes
    }
}

/// Parses frame dimensions from an Annex-B H.264 SPS. The current app sends
/// Baseline H.264, while the parser also skips high-profile SPS extensions.
pub fn sps_dimensions(access_unit: &[u8]) -> Option<(u32, u32)> {
    let mut start = 0;
    while start + 4 < access_unit.len() {
        let (prefix, nal_start) = if access_unit[start..].starts_with(&[0, 0, 0, 1]) {
            (4, start + 4)
        } else if access_unit[start..].starts_with(&[0, 0, 1]) {
            (3, start + 3)
        } else {
            start += 1;
            continue;
        };
        let _ = prefix;
        let end = find_start_code(access_unit, nal_start).unwrap_or(access_unit.len());
        if nal_start < end && access_unit[nal_start] & 0x1f == 7 {
            return parse_sps_dimensions(&access_unit[nal_start + 1..end]);
        }
        start = end;
    }
    None
}

fn find_start_code(bytes: &[u8], from: usize) -> Option<usize> {
    (from..bytes.len().saturating_sub(2)).find(|&index| {
        bytes[index..].starts_with(&[0, 0, 1]) || bytes[index..].starts_with(&[0, 0, 0, 1])
    })
}

fn parse_sps_dimensions(nal: &[u8]) -> Option<(u32, u32)> {
    let mut rbsp = Vec::with_capacity(nal.len());
    let mut zeros = 0;
    for &byte in nal {
        if zeros >= 2 && byte == 3 {
            zeros = 0;
            continue;
        }
        rbsp.push(byte);
        if byte == 0 { zeros += 1 } else { zeros = 0 }
    }
    let mut bits = BitReader {
        data: &rbsp,
        bit: 0,
    };
    let profile = bits.read_bits(8)?;
    bits.read_bits(8)?;
    bits.read_bits(8)?;
    bits.read_ue()?;
    let mut chroma_format = 1;
    if matches!(
        profile,
        100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 139 | 134 | 135
    ) {
        chroma_format = bits.read_ue()?;
        if chroma_format == 3 {
            bits.read_bit()?;
        }
        bits.read_ue()?;
        bits.read_ue()?;
        bits.read_bit()?;
        if bits.read_bit()? != 0 {
            let count = if chroma_format != 3 { 8 } else { 12 };
            for index in 0..count {
                if bits.read_bit()? != 0 {
                    skip_scaling_list(&mut bits, if index < 6 { 16 } else { 64 })?;
                }
            }
        }
    }
    bits.read_ue()?;
    let pic_order = bits.read_ue()?;
    if pic_order == 0 {
        bits.read_ue()?;
    } else if pic_order == 1 {
        bits.read_bit()?;
        bits.read_se()?;
        bits.read_se()?;
        let cycle = bits.read_ue()?;
        if cycle > 256 {
            return None;
        }
        for _ in 0..cycle {
            bits.read_se()?;
        }
    }
    bits.read_ue()?;
    bits.read_bit()?;
    let width_mbs = bits.read_ue()?.checked_add(1)?;
    let height_map = bits.read_ue()?.checked_add(1)?;
    let frame_mbs_only = bits.read_bit()?;
    if frame_mbs_only == 0 {
        bits.read_bit()?;
    }
    bits.read_bit()?;
    let crop = bits.read_bit()?;
    let (left, right, top, bottom) = if crop != 0 {
        (
            bits.read_ue()?,
            bits.read_ue()?,
            bits.read_ue()?,
            bits.read_ue()?,
        )
    } else {
        (0, 0, 0, 0)
    };
    let width = width_mbs.checked_mul(16)?;
    let height = height_map
        .checked_mul(16)?
        .checked_mul(2 - frame_mbs_only)?;
    let sub_width = if chroma_format == 1 || chroma_format == 2 {
        2
    } else {
        1
    };
    let sub_height = if chroma_format == 1 { 2 } else { 1 };
    let crop_x = if chroma_format == 0 { 1 } else { sub_width };
    let crop_y = if chroma_format == 0 {
        2 - frame_mbs_only
    } else {
        sub_height * (2 - frame_mbs_only)
    };
    let width = width.checked_sub((left + right).checked_mul(crop_x)?)?;
    let height = height.checked_sub((top + bottom).checked_mul(crop_y)?)?;
    if width == 0
        || height == 0
        || width > 1280
        || height > 720
        || width % 2 != 0
        || height % 2 != 0
    {
        return None;
    }
    Some((width, height))
}

fn skip_scaling_list(bits: &mut BitReader<'_>, size: usize) -> Option<()> {
    let mut last = 8i32;
    let mut next = 8i32;
    for _ in 0..size {
        if next != 0 {
            let delta = bits.read_se()?;
            next = (last + delta + 256) % 256;
        }
        if next != 0 {
            last = next;
        }
    }
    Some(())
}

struct BitReader<'a> {
    data: &'a [u8],
    bit: usize,
}

impl BitReader<'_> {
    fn read_bit(&mut self) -> Option<u32> {
        let byte = *self.data.get(self.bit / 8)?;
        let value = u32::from((byte >> (7 - self.bit % 8)) & 1);
        self.bit += 1;
        Some(value)
    }

    fn read_bits(&mut self, count: usize) -> Option<u32> {
        let mut value = 0;
        for _ in 0..count {
            value = (value << 1) | self.read_bit()?;
        }
        Some(value)
    }

    fn read_ue(&mut self) -> Option<u32> {
        let mut zeros = 0;
        while self.read_bit()? == 0 {
            zeros += 1;
            if zeros > 31 {
                return None;
            }
        }
        Some(((1u32 << zeros) - 1) + self.read_bits(zeros)?)
    }

    fn read_se(&mut self) -> Option<i32> {
        let code = self.read_ue()? as i32;
        Some(if code & 1 == 0 {
            -(code / 2)
        } else {
            (code + 1) / 2
        })
    }
}
