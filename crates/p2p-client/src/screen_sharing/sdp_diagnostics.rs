use std::collections::BTreeMap;

#[derive(Default)]
struct MediaSection {
    kind: String,
    port: u16,
    direction: Option<&'static str>,
    payload_types: Vec<String>,
    codecs: BTreeMap<String, String>,
    fmtp: BTreeMap<String, BTreeMap<String, String>>,
}

fn safe_payload_type(value: &str) -> bool {
    value
        .parse::<u8>()
        .is_ok_and(|payload_type| payload_type <= 127)
}

fn safe_h264_parameter(key: &str, value: &str) -> bool {
    match key {
        "profile-level-id" => {
            value.len() == 6 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
        }
        "packetization-mode" => value.bytes().all(|byte| byte.is_ascii_digit()),
        _ => false,
    }
}

fn format_section(section: &MediaSection) -> String {
    let direction = section.direction.unwrap_or("sendrecv");
    let codecs = section
        .payload_types
        .iter()
        .map(|payload_type| {
            let codec = section
                .codecs
                .get(payload_type)
                .map(String::as_str)
                .unwrap_or("desconhecido");
            let mut description = format!("{codec}(pt={payload_type}");
            if codec.starts_with("H264/")
                && let Some(parameters) = section.fmtp.get(payload_type)
            {
                for key in ["packetization-mode", "profile-level-id"] {
                    if let Some(value) = parameters.get(key) {
                        description.push_str(&format!(";{key}={value}"));
                    }
                }
            }
            description.push(')');
            description
        })
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "{}(accepted={},direction={},codecs=[{}])",
        section.kind,
        section.port != 0,
        direction,
        codecs
    )
}

/// Summarizes only negotiated media fields that are safe to put in logs.
/// ICE candidates, credentials, stream IDs, and arbitrary SDP attributes are ignored.
pub(super) fn summarize_sdp_media(sdp: &str) -> String {
    let mut session_direction = None;
    let mut sections = Vec::<MediaSection>::new();
    let mut current_section = None;

    for line in sdp.lines().map(str::trim) {
        if let Some(rest) = line.strip_prefix("m=") {
            let mut fields = rest.split_ascii_whitespace();
            let kind = fields.next().unwrap_or_default();
            let port = fields
                .next()
                .and_then(|value| value.split('/').next())
                .and_then(|value| value.parse::<u16>().ok())
                .unwrap_or_default();
            let _protocol = fields.next();
            let payload_types = fields
                .filter(|value| safe_payload_type(value))
                .map(str::to_owned)
                .collect();
            if kind == "audio" || kind == "video" {
                sections.push(MediaSection {
                    kind: kind.to_owned(),
                    port,
                    direction: session_direction,
                    payload_types,
                    ..Default::default()
                });
                current_section = Some(sections.len() - 1);
            } else {
                current_section = None;
            }
            continue;
        }

        let Some(section_index) = current_section else {
            if let Some(direction) = direction_attribute(line) {
                session_direction = Some(direction);
            }
            continue;
        };
        let section = &mut sections[section_index];

        if let Some(direction) = direction_attribute(line) {
            section.direction = Some(direction);
        } else if let Some(rest) = line.strip_prefix("a=rtpmap:") {
            let mut fields = rest.split_ascii_whitespace();
            let payload_type = fields.next().unwrap_or_default();
            let codec = fields.next().unwrap_or_default();
            if safe_payload_type(payload_type)
                && section
                    .payload_types
                    .iter()
                    .any(|value| value == payload_type)
                && !codec.is_empty()
                && codec.len() <= 64
                && codec
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'_'))
            {
                section
                    .codecs
                    .insert(payload_type.to_owned(), codec.to_ascii_uppercase());
            }
        } else if let Some(rest) = line.strip_prefix("a=fmtp:") {
            let Some((payload_type, parameters)) = rest.split_once(' ') else {
                continue;
            };
            if !safe_payload_type(payload_type)
                || !section
                    .payload_types
                    .iter()
                    .any(|value| value == payload_type)
                || !section
                    .codecs
                    .get(payload_type)
                    .is_some_and(|codec| codec.starts_with("H264/"))
            {
                continue;
            }
            let allowed = section.fmtp.entry(payload_type.to_owned()).or_default();
            for parameter in parameters.split(';').map(str::trim) {
                let Some((key, value)) = parameter.split_once('=') else {
                    continue;
                };
                let key = key.trim().to_ascii_lowercase();
                let value = value.trim().to_ascii_lowercase();
                if safe_h264_parameter(&key, &value) {
                    allowed.insert(key, value);
                }
            }
        }
    }

    let summary = sections
        .iter()
        .map(format_section)
        .collect::<Vec<_>>()
        .join(" ");
    if summary.is_empty() {
        "sem seções de áudio/vídeo reconhecidas".to_owned()
    } else {
        summary
    }
}

fn direction_attribute(line: &str) -> Option<&'static str> {
    match line {
        "a=sendrecv" => Some("sendrecv"),
        "a=sendonly" => Some("sendonly"),
        "a=recvonly" => Some("recvonly"),
        "a=inactive" => Some("inactive"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::summarize_sdp_media;

    #[test]
    fn summarizes_audio_video_direction_and_h264_profile_without_other_sdp_data() {
        let sdp = "v=0\r\na=sendrecv\r\na=ice-ufrag:secret\r\nm=audio 9 UDP/TLS/RTP/SAVPF 111\r\na=rtpmap:111 opus/48000/2\r\nm=video 9 UDP/TLS/RTP/SAVPF 102\r\na=sendonly\r\na=rtpmap:102 H264/90000\r\na=fmtp:102 level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f\r\na=candidate:secret-address\r\na=ssrc:123 cname:private-value\r\n";

        let summary = summarize_sdp_media(sdp);

        assert!(summary.contains("audio(accepted=true,direction=sendrecv"));
        assert!(summary.contains("OPUS/48000/2(pt=111)"));
        assert!(summary.contains("video(accepted=true,direction=sendonly"));
        assert!(
            summary.contains("H264/90000(pt=102;packetization-mode=1;profile-level-id=42e01f)")
        );
        assert!(!summary.contains("secret"));
        assert!(!summary.contains("123"));
        assert!(!summary.contains("private-value"));
    }

    #[test]
    fn marks_rejected_and_inactive_video_sections() {
        let sdp =
            "v=0\r\nm=video 0 UDP/TLS/RTP/SAVPF 102\r\na=inactive\r\na=rtpmap:102 H264/90000\r\n";

        let summary = summarize_sdp_media(sdp);

        assert!(summary.contains("video(accepted=false,direction=inactive"));
        assert!(summary.contains("H264/90000(pt=102)"));
    }

    #[test]
    fn ignores_unknown_sdp_sections_and_attributes() {
        let sdp = "v=0\r\nm=application 9 UDP/DTLS/SCTP webrtc-datachannel\r\na=mid:secret\r\n";

        assert_eq!(
            summarize_sdp_media(sdp),
            "sem seções de áudio/vídeo reconhecidas"
        );
    }
}
