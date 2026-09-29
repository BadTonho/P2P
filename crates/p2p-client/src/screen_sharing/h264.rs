#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum H264FrameKind {
    Idr,
    Delta,
}

pub(super) fn classify_h264_access_unit(nals: &[u8]) -> Option<H264FrameKind> {
    if nals.contains(&5) {
        Some(H264FrameKind::Idr)
    } else if nals.iter().any(|nal_type| (1..=4).contains(nal_type)) {
        Some(H264FrameKind::Delta)
    } else {
        None
    }
}

pub(super) fn annex_b_nals(data: &[u8]) -> Vec<&[u8]> {
    fn next_start_code(data: &[u8], from: usize) -> Option<(usize, usize)> {
        let mut index = from;
        while index + 3 <= data.len() {
            if index + 4 <= data.len() && data[index..index + 4] == [0, 0, 0, 1] {
                return Some((index, 4));
            }
            if data[index..index + 3] == [0, 0, 1] {
                return Some((index, 3));
            }
            index += 1;
        }
        None
    }

    let Some((start, start_code_len)) = next_start_code(data, 0) else {
        return Vec::new();
    };
    let mut nal_start = start + start_code_len;
    let mut nals = Vec::new();

    while nal_start < data.len() {
        if let Some((next_start, next_start_code_len)) = next_start_code(data, nal_start) {
            if next_start > nal_start {
                nals.push(&data[nal_start..next_start]);
            }
            nal_start = next_start + next_start_code_len;
        } else {
            nals.push(&data[nal_start..]);
            break;
        }
    }
    nals
}

pub(super) fn annex_b_nal_types(data: &[u8]) -> Vec<u8> {
    annex_b_nals(data)
        .into_iter()
        .filter_map(|nal| nal.first().map(|header| header & 0x1f))
        .collect()
}

pub(super) fn rtp_payload_nal_types(payload: &[u8]) -> Vec<u8> {
    let Some(header) = payload.first() else {
        return Vec::new();
    };
    match header & 0x1f {
        1..=23 => vec![header & 0x1f],
        24 => {
            let mut types = Vec::new();
            let mut offset = 1;
            while offset + 2 <= payload.len() {
                let nal_len = u16::from_be_bytes([payload[offset], payload[offset + 1]]) as usize;
                offset += 2;
                if nal_len == 0 || offset + nal_len > payload.len() {
                    return vec![24];
                }
                types.push(payload[offset] & 0x1f);
                offset += nal_len;
            }
            if offset != payload.len() || types.is_empty() {
                vec![24]
            } else {
                types
            }
        }
        28 if payload.len() >= 2 && payload[1] & 0x80 != 0 => {
            vec![payload[1] & 0x1f]
        }
        _ => Vec::new(),
    }
}

pub(super) fn describe_nal_types(nal_types: &[u8], byte_len: usize) -> String {
    if nal_types.is_empty() {
        return format!("sem início Annex-B, {byte_len} bytes");
    }
    let names = nal_types
        .iter()
        .map(|nal_type| match nal_type {
            1 => "1(slice)".to_owned(),
            5 => "5(IDR)".to_owned(),
            6 => "6(SEI)".to_owned(),
            7 => "7(SPS)".to_owned(),
            8 => "8(PPS)".to_owned(),
            9 => "9(AUD)".to_owned(),
            other => other.to_string(),
        })
        .collect::<Vec<_>>()
        .join(",");
    format!("{byte_len} bytes, NAL [{names}]")
}

#[cfg(test)]
pub(super) fn annex_b_access_unit(nals: &[&[u8]]) -> Vec<u8> {
    let mut access_unit = Vec::new();
    for nal in nals {
        access_unit.extend_from_slice(&[0, 0, 0, 1]);
        access_unit.extend_from_slice(nal);
    }
    access_unit
}
