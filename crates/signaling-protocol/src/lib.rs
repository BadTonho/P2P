use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    CreateRoom,
    JoinRoom { code: String },
    LeaveRoom,
    Signal { kind: SignalKind, payload: String },
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    RoomCreated { code: String },
    RoomJoined { code: String },
    RoomLeft,
    PeerJoined,
    PeerLeft,
    Signal { kind: SignalKind, payload: String },
    Error { message: String },
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
