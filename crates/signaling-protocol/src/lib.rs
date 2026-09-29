use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    CreateRoom,
    JoinRoom {
        code: String,
    },
    CreateRoomIdentified {
        participant: ParticipantInfo,
        #[serde(default)]
        room_mode: RoomMode,
    },
    JoinRoomIdentified {
        code: String,
        participant: ParticipantInfo,
    },
    LeaveRoom,
    RequestHostTransfer,
    CancelHostTransfer {
        token: String,
    },
    AdoptTransferredRoom {
        code: String,
        token: String,
    },
    ConfirmHostTransfer {
        code: String,
        token: String,
    },
    RejectHostTransfer {
        token: String,
    },
    Signal {
        kind: SignalKind,
        payload: String,
        #[serde(default)]
        #[serde(skip_serializing_if = "Option::is_none")]
        target_participant_id: Option<String>,
        #[serde(default)]
        #[serde(skip_serializing_if = "Option::is_none")]
        stream_id: Option<String>,
    },
    IdentifyParticipant {
        participant: ParticipantInfo,
    },
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    RoomCreated {
        code: String,
    },
    RoomJoined {
        code: String,
    },
    RoomLeft,
    PeerJoined,
    PeerLeft,
    HostTransferPending {
        code: String,
        token: String,
    },
    HostTransferRequested {
        code: String,
        token: String,
    },
    RoomAdopted {
        code: String,
    },
    HostTransferComplete {
        code: String,
    },
    HostTransferCanceled {
        message: String,
    },
    Signal {
        kind: SignalKind,
        payload: String,
        #[serde(default)]
        #[serde(skip_serializing_if = "Option::is_none")]
        from_participant_id: Option<String>,
        #[serde(default)]
        #[serde(skip_serializing_if = "Option::is_none")]
        stream_id: Option<String>,
    },
    Error {
        message: String,
    },
    RoomRoster {
        participants: Vec<ParticipantInfo>,
        leader_id: String,
        #[serde(default)]
        room_mode: RoomMode,
    },
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RoomMode {
    #[default]
    Local,
    InternetTest,
}

/// Informações de presença usadas pela interface e pela malha de controle.
/// `control_address` é o IPv4 escolhido pelo participante na porta 9001.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct ParticipantInfo {
    pub id: String,
    pub display_name: String,
    pub order: u8,
    pub may_host: bool,
    pub control_address: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar_jpeg_base64: Option<String>,
    #[serde(default)]
    pub supports_group_screen_share: bool,
}

/// Mensagens da conexão direta entre os clientes. Não transporta mídia.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ControlMessage {
    Hello {
        room_code: String,
        participant: ParticipantInfo,
    },
    Ping {
        sequence: u64,
        epoch: u64,
    },
    Pong {
        sequence: u64,
        epoch: u64,
    },
    Status {
        participant_id: String,
        leader_id: String,
        leader_address: String,
        epoch: u64,
        loss_percent: f32,
        jitter_ms: f32,
        latency_ms: f32,
        probe_count: u32,
        consecutive_losses: u32,
        eligible: bool,
    },
    HostLeaving {
        participant_id: String,
        epoch: u64,
    },
    ElectionStart {
        epoch: u64,
        candidates: Vec<String>,
        departing_id: Option<String>,
    },
    CandidateFailed {
        participant_id: String,
        epoch: u64,
    },
    LeaderElected {
        participant_id: String,
        address: String,
        epoch: u64,
        order: u8,
    },
    EndRoom {
        epoch: u64,
    },
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalKind {
    Diagnostic,
    Offer,
    Answer,
    IceCandidate,
    ScreenShareRequest,
    ScreenShareAccept,
    ScreenShareBusy,
    ScreenShareStopped,
    ScreenShareAvailable,
    ScreenShareUnavailable,
    ScreenShareWatch,
    ScreenShareUnwatch,
}

#[cfg(test)]
mod tests {
    use super::{ClientMessage, ParticipantInfo, ServerMessage, SignalKind};

    #[test]
    fn messages_use_stable_tagged_json_names() {
        let request = ClientMessage::JoinRoom {
            code: "12AB34CD".to_owned(),
        };
        assert_eq!(
            serde_json::to_string(&request).unwrap(),
            r#"{"type":"join_room","code":"12AB34CD"}"#
        );

        let response = ServerMessage::Signal {
            kind: SignalKind::IceCandidate,
            payload: "candidate-payload".to_owned(),
            from_participant_id: None,
            stream_id: None,
        };
        let json = serde_json::to_string(&response).unwrap();
        assert_eq!(
            serde_json::from_str::<ServerMessage>(&json).unwrap(),
            response
        );

        let stopped = ClientMessage::Signal {
            kind: SignalKind::ScreenShareStopped,
            payload: "share-request-1".to_owned(),
            target_participant_id: None,
            stream_id: None,
        };
        assert_eq!(
            serde_json::to_string(&stopped).unwrap(),
            r#"{"type":"signal","kind":"screen_share_stopped","payload":"share-request-1"}"#
        );
    }

    #[test]
    fn participant_messages_without_avatar_remain_compatible() {
        let participant: ParticipantInfo = serde_json::from_str(
            r#"{"id":"peer-1","display_name":"Participante 1","order":1,"may_host":false,"control_address":"","supports_group_screen_share":true}"#,
        )
        .expect("older participant message should deserialize");

        assert_eq!(participant.avatar_jpeg_base64, None);
        assert!(participant.supports_group_screen_share);
        assert!(
            !serde_json::to_string(&participant)
                .expect("participant should serialize")
                .contains("avatar_jpeg_base64")
        );
    }
}
