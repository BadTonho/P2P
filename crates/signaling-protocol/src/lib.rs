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
    },
    Error {
        message: String,
    },
    RoomRoster {
        participants: Vec<ParticipantInfo>,
        leader_id: String,
    },
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
}

#[cfg(test)]
mod tests {
    use super::{ClientMessage, ServerMessage, SignalKind};

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
        };
        let json = serde_json::to_string(&response).unwrap();
        assert_eq!(
            serde_json::from_str::<ServerMessage>(&json).unwrap(),
            response
        );
    }
}
