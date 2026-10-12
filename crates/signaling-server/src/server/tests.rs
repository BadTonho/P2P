use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use base64::Engine as _;
use futures_util::{SinkExt, StreamExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message as WebSocketMessage;
use tokio_tungstenite::{WebSocketStream, connect_async};

use super::websocket::{spawn_admitted_connection, write_outgoing_messages};
use super::{
    FAILED_JOIN_WINDOW, MAX_CONNECTIONS_PER_IP, MAX_FAILED_JOINS_PER_WINDOW, MAX_MESSAGE_BYTES,
    MAX_TOTAL_CONNECTIONS, OutboundMessage, Outgoing, OutgoingSendError, ROOM_CODE_LENGTH,
    RoomRegistry, TransferReservation, default_participant, is_small_baseline_jpeg,
    is_valid_room_code, sanitize_avatar, sanitize_display_name, serve,
};
use signaling_protocol::{ClientMessage, ParticipantInfo, RoomMode, ServerMessage, SignalKind};
use std::net::{IpAddr, Ipv4Addr};
use std::time::Instant;

fn small_jpeg_base64() -> String {
    // Minimal SOF0 header used to exercise the server's bounded metadata parser.
    let bytes = [
        0xff, 0xd8, 0xff, 0xc0, 0x00, 0x11, 0x08, 0x00, 0x60, 0x00, 0x60, 0x03, 0x01, 0x11, 0x00,
        0x02, 0x11, 0x00, 0x03, 0x11, 0x00, 0xff, 0xd9,
    ];
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn test_outgoing_channel() -> (Outgoing, mpsc::Receiver<OutboundMessage>) {
    let (outgoing, receiver, overload_events) = Outgoing::channel();
    drop(overload_events);
    (outgoing, receiver)
}

struct RecordingSink(Arc<std::sync::Mutex<Vec<WebSocketMessage>>>);

impl futures_util::Sink<WebSocketMessage> for RecordingSink {
    type Error = std::convert::Infallible;

    fn poll_ready(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn start_send(
        self: std::pin::Pin<&mut Self>,
        item: WebSocketMessage,
    ) -> Result<(), Self::Error> {
        self.get_mut()
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(item);
        Ok(())
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn poll_close(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }
}

#[tokio::test]
async fn websocket_writer_sends_room_left_before_close_and_drops_later_messages() {
    let (outgoing, receiver, overload_events) = Outgoing::channel_with_capacity(3);
    outgoing
        .send(OutboundMessage::Protocol(ServerMessage::RoomLeft))
        .unwrap();
    outgoing
        .send(OutboundMessage::Control(WebSocketMessage::Close(None)))
        .unwrap();
    outgoing
        .send(OutboundMessage::Protocol(ServerMessage::PeerLeft))
        .unwrap();
    drop(outgoing);

    let sent = Arc::new(std::sync::Mutex::new(Vec::new()));
    write_outgoing_messages(
        RecordingSink(Arc::clone(&sent)),
        receiver,
        overload_events,
        22,
    )
    .await;

    let sent = sent
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    assert_eq!(
        sent.len(),
        2,
        "nenhuma mensagem pode ser enviada após Close"
    );
    assert!(matches!(
        sent.first(),
        Some(WebSocketMessage::Text(text))
            if serde_json::from_str::<ServerMessage>(text.as_str())
                .is_ok_and(|message| matches!(message, ServerMessage::RoomLeft))
    ));
    assert!(matches!(sent.get(1), Some(WebSocketMessage::Close(_))));
}

#[tokio::test]
async fn websocket_writer_disconnects_when_bounded_outgoing_queue_overflows() {
    let (outgoing, receiver, overload_events) = Outgoing::channel_with_capacity(1);
    outgoing
        .send(OutboundMessage::Protocol(ServerMessage::PeerLeft))
        .unwrap();
    assert_eq!(
        outgoing.send(OutboundMessage::Protocol(ServerMessage::PeerJoined)),
        Err(OutgoingSendError::Full)
    );
    assert_eq!(
        receiver.len(),
        1,
        "fila não pode ultrapassar sua capacidade"
    );

    let sent = Arc::new(std::sync::Mutex::new(Vec::new()));
    write_outgoing_messages(
        RecordingSink(Arc::clone(&sent)),
        receiver,
        overload_events,
        23,
    )
    .await;

    assert!(
        sent.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_empty(),
        "o escritor deve encerrar o cliente lento ao receber o sinal de sobrecarga"
    );
}

fn server_message(receiver: &mut mpsc::Receiver<OutboundMessage>) -> ServerMessage {
    match receiver.try_recv().expect("expected a server message") {
        OutboundMessage::Protocol(message) => message,
        OutboundMessage::Control(_) | OutboundMessage::CloseAcknowledgement(_) => {
            panic!("expected a protocol message")
        }
    }
}

#[test]
fn rooms_have_eight_slots_and_reject_the_ninth_participant() {
    let mut registry = RoomRegistry::default();
    let (first_tx, mut first_rx) = test_outgoing_channel();

    let code = registry.create_room(1, first_tx).unwrap();
    assert_eq!(code.len(), ROOM_CODE_LENGTH);
    assert!(
        code.chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_lowercase())
    );
    assert_eq!(
        server_message(&mut first_rx),
        ServerMessage::RoomCreated { code: code.clone() }
    );

    for id in 2..=8 {
        let (sender, mut receiver) = test_outgoing_channel();
        registry.join_room(id, &code, sender).unwrap();
        assert_eq!(
            server_message(&mut receiver),
            ServerMessage::RoomJoined { code: code.clone() }
        );
    }
    let (ninth_tx, _ninth_rx) = test_outgoing_channel();
    assert!(
        registry
            .join_room(9, &code, ninth_tx)
            .unwrap_err()
            .contains("8")
    );
    assert_eq!(registry.rooms.get(&code).unwrap().participants.len(), 8);
}

#[test]
fn internet_test_rooms_limit_to_two_disable_handoff_and_hide_control_addresses() {
    let mut registry = RoomRegistry::default();
    let (host_tx, mut host_rx) = test_outgoing_channel();
    let (guest_tx, mut guest_rx) = test_outgoing_channel();
    let code = registry
        .create_room_with_mode(1, host_tx, RoomMode::InternetTest, None)
        .unwrap();
    let _ = server_message(&mut host_rx); // RoomCreated
    registry
        .identify_participant(
            1,
            ParticipantInfo {
                id: "host-id".to_owned(),
                display_name: "Anfitrião da sala".to_owned(),
                order: 0,
                may_host: true,
                control_address: "192.168.1.2:9001".to_owned(),
                avatar_jpeg_base64: Some(small_jpeg_base64()),
                supports_group_screen_share: false,
                supports_group_session_ids: false,
                public_key: None,
            },
        )
        .unwrap();
    let ServerMessage::RoomRoster {
        participants,
        room_mode,
        ..
    } = server_message(&mut host_rx)
    else {
        panic!("expected internet room roster")
    };
    assert_eq!(room_mode, RoomMode::InternetTest);
    assert_eq!(participants[0].display_name, "Anfitrião da sala");
    assert!(participants[0].avatar_jpeg_base64.is_none());
    assert!(!participants[0].may_host);
    assert!(participants[0].control_address.is_empty());

    registry
        .join_room_identified(
            2,
            &code,
            guest_tx,
            ParticipantInfo {
                id: "guest-id".to_owned(),
                display_name: "Convidado".to_owned(),
                order: 0,
                may_host: true,
                control_address: "192.168.1.3:9001".to_owned(),
                avatar_jpeg_base64: Some(small_jpeg_base64()),
                supports_group_screen_share: false,
                supports_group_session_ids: false,
                public_key: None,
            },
        )
        .unwrap();
    let _ = server_message(&mut guest_rx); // RoomJoined
    let _ = server_message(&mut host_rx); // PeerJoined
    for receiver in [&mut host_rx, &mut guest_rx] {
        let ServerMessage::RoomRoster {
            participants,
            room_mode,
            ..
        } = server_message(receiver)
        else {
            panic!("expected updated internet room roster")
        };
        assert_eq!(room_mode, RoomMode::InternetTest);
        assert_eq!(participants.len(), 2);
        assert!(participants.iter().all(|participant| {
            !participant.may_host && participant.control_address.is_empty()
        }));
        assert!(
            participants
                .iter()
                .all(|participant| participant.avatar_jpeg_base64.is_none())
        );
        assert_eq!(participants[0].display_name, "Anfitrião da sala");
        assert_eq!(participants[1].display_name, "Convidado");
    }

    let (third_tx, _third_rx) = test_outgoing_channel();
    let error = registry.join_room(3, &code, third_tx).unwrap_err();
    assert!(error.contains("duas pessoas"));
    assert!(
        registry
            .request_host_transfer(1)
            .unwrap_err()
            .contains("desativada")
    );
}

#[test]
fn roster_keeps_join_order_and_host_permission() {
    let mut registry = RoomRegistry::default();
    let (host_tx, mut host_rx) = test_outgoing_channel();
    let (guest_tx, mut guest_rx) = test_outgoing_channel();
    let code = registry.create_room(1, host_tx).unwrap();
    let _ = server_message(&mut host_rx);
    registry
        .identify_participant(
            1,
            ParticipantInfo {
                id: "host-id".to_owned(),
                display_name: "Anfitrião".to_owned(),
                order: 0,
                may_host: true,
                control_address: "192.168.1.2:9001".to_owned(),
                avatar_jpeg_base64: Some(small_jpeg_base64()),
                supports_group_screen_share: false,
                supports_group_session_ids: false,
                public_key: None,
            },
        )
        .unwrap();
    let ServerMessage::RoomRoster {
        participants,
        leader_id,
        ..
    } = server_message(&mut host_rx)
    else {
        panic!("expected a participant roster")
    };
    assert_eq!(leader_id, "host-id");
    assert_eq!(participants[0].order, 1);
    assert!(participants[0].may_host);
    assert_eq!(participants[0].display_name, "Anfitrião");
    assert_eq!(
        participants[0].avatar_jpeg_base64,
        Some(small_jpeg_base64())
    );

    registry.join_room(2, &code, guest_tx).unwrap();
    let _ = server_message(&mut guest_rx);
    let _ = server_message(&mut host_rx);
    registry
        .identify_participant(
            2,
            ParticipantInfo {
                id: "guest-id".to_owned(),
                display_name: "Amiga".to_owned(),
                order: 0,
                may_host: false,
                control_address: "192.168.1.3:9001".to_owned(),
                avatar_jpeg_base64: Some(small_jpeg_base64()),
                supports_group_screen_share: false,
                supports_group_session_ids: false,
                public_key: None,
            },
        )
        .unwrap();
    let ServerMessage::RoomRoster {
        participants,
        leader_id,
        ..
    } = server_message(&mut host_rx)
    else {
        panic!("expected updated roster")
    };
    assert_eq!(leader_id, "host-id");
    assert_eq!(participants[0].display_name, "Anfitrião");
    assert_eq!(participants[1].display_name, "Amiga");
    assert_eq!(
        participants[1].avatar_jpeg_base64,
        Some(small_jpeg_base64())
    );
    assert_eq!(
        participants
            .iter()
            .map(|item| item.id.as_str())
            .collect::<Vec<_>>(),
        ["host-id", "guest-id"]
    );
    assert_eq!(
        participants
            .iter()
            .map(|item| item.order)
            .collect::<Vec<_>>(),
        [1, 2]
    );
    assert!(!participants[1].may_host);
}

#[test]
fn leader_departure_promotes_next_participant_as_leader() {
    let mut registry = RoomRegistry::default();
    let (host_tx, mut host_rx) = test_outgoing_channel();
    let (guest_tx, mut guest_rx) = test_outgoing_channel();
    let code = registry.create_room(1, host_tx).unwrap();
    let _ = server_message(&mut host_rx);
    registry
        .identify_participant(
            1,
            ParticipantInfo {
                id: "host-id".to_owned(),
                display_name: "Host".to_owned(),
                order: 0,
                may_host: true,
                control_address: "192.168.1.2:9001".to_owned(),
                avatar_jpeg_base64: None,
                supports_group_screen_share: false,
                supports_group_session_ids: false,
                public_key: None,
            },
        )
        .unwrap();
    let _ = server_message(&mut host_rx);

    registry.join_room(2, &code, guest_tx).unwrap();
    let _ = server_message(&mut guest_rx);
    let _ = server_message(&mut host_rx);
    registry
        .identify_participant(
            2,
            ParticipantInfo {
                id: "guest-id".to_owned(),
                display_name: "Guest".to_owned(),
                order: 0,
                may_host: false,
                control_address: "192.168.1.3:9001".to_owned(),
                avatar_jpeg_base64: None,
                supports_group_screen_share: false,
                supports_group_session_ids: false,
                public_key: None,
            },
        )
        .unwrap();
    let _ = server_message(&mut host_rx);
    let _ = server_message(&mut guest_rx);

    registry.leave_room(1);

    let _ = server_message(&mut guest_rx); // PeerLeft
    let ServerMessage::RoomRoster {
        leader_id,
        participants,
        ..
    } = server_message(&mut guest_rx)
    else {
        panic!("esperado RoomRoster apos saida do host");
    };
    assert_eq!(leader_id, "guest-id");
    assert_eq!(participants.len(), 1);
    assert_eq!(participants[0].id, "guest-id");
}

#[test]
fn participant_profile_fields_are_bounded_and_sanitized() {
    let long_name = "x".repeat(64);
    assert_eq!(sanitize_display_name(&long_name, 4), "x".repeat(32));
    assert_eq!(sanitize_display_name("  \n\t  ", 4), "Participante 4");
    assert_eq!(sanitize_display_name("  Ana\n  ", 4), "Ana");

    let valid = small_jpeg_base64();
    assert_eq!(sanitize_avatar(Some(valid.clone())), Some(valid));
    assert!(sanitize_avatar(Some("not base64".to_owned())).is_none());
    assert!(sanitize_avatar(Some("A".repeat(30_000))).is_none());
    assert!(!is_small_baseline_jpeg(&[0xff, 0xd8, 0xff, 0xc2]));
}

#[test]
fn public_keys_are_validated_and_bound_to_the_participant_id() {
    use super::sanitize_public_key;
    let key_a = format!("{}=", "A".repeat(43));
    let key_b = format!("{}=", "Q".repeat(43));
    assert_eq!(
        sanitize_public_key(Some(key_a.clone())),
        Some(key_a.clone())
    );
    assert!(sanitize_public_key(Some("curta".to_owned())).is_none());
    assert!(sanitize_public_key(Some("!".repeat(44))).is_none());
    assert!(sanitize_public_key(None).is_none());

    let participant = |key: Option<String>| ParticipantInfo {
        id: "ghost-id".to_owned(),
        display_name: "Fantasma".to_owned(),
        order: 2,
        may_host: false,
        control_address: "192.168.1.9:9001".to_owned(),
        avatar_jpeg_base64: None,
        supports_group_screen_share: false,
        supports_group_session_ids: false,
        public_key: key,
    };
    let mut registry = RoomRegistry::default();
    let (host_tx, _host_rx) = test_outgoing_channel();
    let (guest_tx, _guest_rx) = test_outgoing_channel();
    let code = registry.create_room(1, host_tx).unwrap();
    registry
        .rooms
        .get_mut(&code)
        .unwrap()
        .members
        .push(participant(Some(key_a.clone())));
    registry.join_room(2, &code, guest_tx).unwrap();

    let wrong_key = registry
        .identify_participant(2, participant(Some(key_b)))
        .unwrap_err();
    assert!(wrong_key.contains("outra chave"));
    let missing_key = registry
        .identify_participant(2, participant(None))
        .unwrap_err();
    assert!(missing_key.contains("outra chave"));
    registry
        .identify_participant(2, participant(Some(key_a)))
        .unwrap();
}

#[test]
fn election_adoption_preserves_roster_and_allows_existing_members_to_rejoin() {
    let original_members = vec![
        ParticipantInfo {
            id: "host-id".to_owned(),
            display_name: "Participante 1".to_owned(),
            order: 1,
            may_host: true,
            control_address: "192.168.1.2:9001".to_owned(),
            avatar_jpeg_base64: None,
            supports_group_screen_share: false,
            supports_group_session_ids: false,
            public_key: None,
        },
        ParticipantInfo {
            id: "candidate-id".to_owned(),
            display_name: "Participante 2".to_owned(),
            order: 2,
            may_host: true,
            control_address: "192.168.1.3:9001".to_owned(),
            avatar_jpeg_base64: None,
            supports_group_screen_share: false,
            supports_group_session_ids: false,
            public_key: None,
        },
        ParticipantInfo {
            id: "guest-id".to_owned(),
            display_name: "Participante 3".to_owned(),
            order: 3,
            may_host: false,
            control_address: "192.168.1.4:9001".to_owned(),
            avatar_jpeg_base64: None,
            supports_group_screen_share: false,
            supports_group_session_ids: false,
            public_key: None,
        },
    ];
    let mut registry = RoomRegistry {
        transfer_reservation: Some(TransferReservation {
            code: "SAMECODE".to_owned(),
            token: "election-token".to_owned(),
            participants: original_members,
        }),
        ..RoomRegistry::default()
    };
    let (candidate_tx, mut candidate_rx) = test_outgoing_channel();
    registry
        .adopt_transferred_room(10, "SAMECODE", "election-token", candidate_tx)
        .unwrap();
    assert_eq!(
        server_message(&mut candidate_rx),
        ServerMessage::RoomAdopted {
            code: "SAMECODE".to_owned()
        }
    );
    registry
        .identify_participant(
            10,
            ParticipantInfo {
                id: "candidate-id".to_owned(),
                display_name: "Participante 2".to_owned(),
                order: 2,
                may_host: true,
                control_address: "192.168.1.3:9001".to_owned(),
                avatar_jpeg_base64: None,
                supports_group_screen_share: false,
                supports_group_session_ids: false,
                public_key: None,
            },
        )
        .unwrap();
    let ServerMessage::RoomRoster {
        participants,
        leader_id,
        ..
    } = server_message(&mut candidate_rx)
    else {
        panic!("expected the restored participant roster")
    };
    assert_eq!(leader_id, "candidate-id");
    assert_eq!(
        participants
            .iter()
            .map(|item| item.id.as_str())
            .collect::<Vec<_>>(),
        ["host-id", "candidate-id", "guest-id"]
    );

    let (host_tx, mut host_rx) = test_outgoing_channel();
    registry
        .join_room_identified(
            11,
            "SAMECODE",
            host_tx,
            ParticipantInfo {
                id: "host-id".to_owned(),
                display_name: "Participante 1".to_owned(),
                order: 1,
                may_host: true,
                control_address: "192.168.1.2:9001".to_owned(),
                avatar_jpeg_base64: None,
                supports_group_screen_share: false,
                supports_group_session_ids: false,
                public_key: None,
            },
        )
        .unwrap();
    assert_eq!(
        server_message(&mut host_rx),
        ServerMessage::RoomJoined {
            code: "SAMECODE".to_owned()
        }
    );
    let _ = server_message(&mut candidate_rx); // PeerJoined
    let ServerMessage::RoomRoster {
        participants,
        leader_id,
        ..
    } = server_message(&mut host_rx)
    else {
        panic!("expected the roster after the old host reconnects")
    };
    assert_eq!(leader_id, "candidate-id");
    assert_eq!(
        participants
            .iter()
            .map(|item| item.id.as_str())
            .collect::<Vec<_>>(),
        ["host-id", "candidate-id", "guest-id"]
    );
    assert_eq!(
        participants
            .iter()
            .map(|item| item.order)
            .collect::<Vec<_>>(),
        [1, 2, 3]
    );
}

#[test]
fn signaling_goes_only_to_the_other_participant_and_leave_frees_room() {
    let mut registry = RoomRegistry::default();
    let (first_tx, mut first_rx) = test_outgoing_channel();
    let (second_tx, mut second_rx) = test_outgoing_channel();
    let code = registry.create_room(1, first_tx).unwrap();
    let _ = server_message(&mut first_rx);
    registry.join_room(2, &code, second_tx).unwrap();
    let _ = server_message(&mut first_rx);
    let _ = server_message(&mut second_rx);

    registry
        .forward_signal(1, SignalKind::Diagnostic, "ping".to_owned(), None, None)
        .unwrap();
    assert_eq!(
        server_message(&mut second_rx),
        ServerMessage::Signal {
            kind: SignalKind::Diagnostic,
            payload: "ping".to_owned(),
            from_participant_id: Some("participant-1".to_owned()),
            stream_id: None,
        }
    );
    assert!(first_rx.try_recv().is_err());

    assert!(registry.leave_room(1));
    assert_eq!(server_message(&mut second_rx), ServerMessage::PeerLeft);
    assert!(registry.rooms.contains_key(&code));
    assert!(registry.leave_room(2));
    assert!(!registry.rooms.contains_key(&code));
}

#[test]
fn targeted_media_signaling_reaches_only_the_named_room_member() {
    let mut registry = RoomRegistry::default();
    let (first_tx, mut first_rx) = test_outgoing_channel();
    let (second_tx, mut second_rx) = test_outgoing_channel();
    let (third_tx, mut third_rx) = test_outgoing_channel();
    let code = registry.create_room(1, first_tx).unwrap();
    registry.join_room(2, &code, second_tx).unwrap();
    registry.join_room(3, &code, third_tx).unwrap();

    // Consume the room-created/joined notifications; this test concerns only
    // delivery of a directed WebRTC message.
    while first_rx.try_recv().is_ok() {}
    while second_rx.try_recv().is_ok() {}
    while third_rx.try_recv().is_ok() {}

    registry
        .forward_signal(
            1,
            SignalKind::Offer,
            "offer-sdp".to_owned(),
            Some("participant-2".to_owned()),
            Some("participant-1".to_owned()),
        )
        .unwrap();

    assert_eq!(
        server_message(&mut second_rx),
        ServerMessage::Signal {
            kind: SignalKind::Offer,
            payload: "offer-sdp".to_owned(),
            from_participant_id: Some("participant-1".to_owned()),
            stream_id: Some("participant-1".to_owned()),
        }
    );
    assert!(first_rx.try_recv().is_err());
    assert!(third_rx.try_recv().is_err());

    assert!(
        registry
            .forward_signal(
                1,
                SignalKind::IceCandidate,
                "candidate".to_owned(),
                Some("not-in-this-room".to_owned()),
                Some("participant-1".to_owned()),
            )
            .is_err()
    );
}

#[test]
fn canceled_or_expired_transfer_keeps_room_available_on_original_server() {
    let mut registry = RoomRegistry::default();
    let (host_tx, mut host_rx) = test_outgoing_channel();
    let (peer_tx, mut peer_rx) = test_outgoing_channel();
    let code = registry.create_room(1, host_tx).unwrap();
    let _ = server_message(&mut host_rx);
    registry.join_room(2, &code, peer_tx).unwrap();
    let _ = server_message(&mut host_rx);
    let _ = server_message(&mut peer_rx);

    let canceled = registry.request_host_transfer(1).unwrap();
    assert!(matches!(
        server_message(&mut peer_rx),
        ServerMessage::HostTransferRequested { .. }
    ));
    assert!(matches!(
        server_message(&mut host_rx),
        ServerMessage::HostTransferPending { .. }
    ));
    assert!(registry.cancel_host_transfer(1, &canceled.token));
    assert!(matches!(
        server_message(&mut host_rx),
        ServerMessage::HostTransferCanceled { .. }
    ));
    assert!(matches!(
        server_message(&mut peer_rx),
        ServerMessage::HostTransferCanceled { .. }
    ));
    assert!(registry.rooms.contains_key(&code));

    let expired = registry.request_host_transfer(1).unwrap();
    let _ = server_message(&mut peer_rx);
    let _ = server_message(&mut host_rx);
    registry.expire_host_transfer(&expired);
    assert!(matches!(
        server_message(&mut host_rx),
        ServerMessage::HostTransferCanceled { .. }
    ));
    assert!(matches!(
        server_message(&mut peer_rx),
        ServerMessage::HostTransferCanceled { .. }
    ));
    assert!(registry.rooms.contains_key(&code));
    assert_eq!(registry.rooms[&code].participants.len(), 2);

    let rejected = registry.request_host_transfer(1).unwrap();
    let _ = server_message(&mut peer_rx);
    let _ = server_message(&mut host_rx);
    assert!(registry.reject_host_transfer(2, &rejected.token));
    assert!(matches!(
        server_message(&mut host_rx),
        ServerMessage::HostTransferCanceled { .. }
    ));
    assert!(matches!(
        server_message(&mut peer_rx),
        ServerMessage::HostTransferCanceled { .. }
    ));
    assert!(registry.rooms.contains_key(&code));
}

async fn send_client_message<S>(socket: &mut WebSocketStream<S>, message: ClientMessage)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let text = serde_json::to_string(&message).unwrap();
    socket
        .send(WebSocketMessage::Text(text.into()))
        .await
        .unwrap();
}

async fn receive_server_message<S>(socket: &mut WebSocketStream<S>) -> ServerMessage
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let frame = timeout(Duration::from_secs(3), socket.next())
        .await
        .expect("server response timed out")
        .expect("server closed the connection")
        .expect("websocket read failed");
    let WebSocketMessage::Text(text) = frame else {
        panic!("expected a JSON text message")
    };
    serde_json::from_str(text.as_str()).unwrap()
}

async fn start_test_server(
    reservation: Option<TransferReservation>,
) -> (
    String,
    oneshot::Sender<()>,
    tokio::task::JoinHandle<std::io::Result<()>>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let task = tokio::spawn(serve(listener, shutdown_rx, reservation));
    (format!("ws://{address}"), shutdown_tx, task)
}

#[tokio::test]
async fn websocket_host_transfer_moves_room_to_new_server_and_keeps_same_code() {
    let (old_url, stop_old, old_server) = start_test_server(None).await;
    let (mut old_host, _) = connect_async(&old_url).await.unwrap();
    send_client_message(&mut old_host, ClientMessage::CreateRoom).await;
    let ServerMessage::RoomCreated { code } = receive_server_message(&mut old_host).await else {
        panic!("expected a new room")
    };

    let (mut old_peer, _) = connect_async(&old_url).await.unwrap();
    send_client_message(
        &mut old_peer,
        ClientMessage::JoinRoom { code: code.clone() },
    )
    .await;
    assert_eq!(
        receive_server_message(&mut old_peer).await,
        ServerMessage::RoomJoined { code: code.clone() }
    );
    assert_eq!(
        receive_server_message(&mut old_host).await,
        ServerMessage::PeerJoined
    );

    send_client_message(&mut old_host, ClientMessage::RequestHostTransfer).await;
    let ServerMessage::HostTransferRequested {
        code: requested_code,
        token,
    } = receive_server_message(&mut old_peer).await
    else {
        panic!("expected a handoff request")
    };
    assert_eq!(requested_code, code);
    assert_eq!(
        receive_server_message(&mut old_host).await,
        ServerMessage::HostTransferPending {
            code: code.clone(),
            token: token.clone()
        }
    );

    let (new_url, stop_new, new_server) = start_test_server(Some(TransferReservation {
        code: code.clone(),
        token: token.clone(),
        participants: Vec::new(),
    }))
    .await;
    let (mut new_host, _) = connect_async(&new_url).await.unwrap();
    send_client_message(
        &mut new_host,
        ClientMessage::AdoptTransferredRoom {
            code: code.clone(),
            token: token.clone(),
        },
    )
    .await;
    assert_eq!(
        receive_server_message(&mut new_host).await,
        ServerMessage::RoomAdopted { code: code.clone() }
    );

    send_client_message(
        &mut old_peer,
        ClientMessage::ConfirmHostTransfer {
            code: code.clone(),
            token,
        },
    )
    .await;
    assert_eq!(
        receive_server_message(&mut old_host).await,
        ServerMessage::HostTransferComplete { code: code.clone() }
    );
    assert_eq!(
        receive_server_message(&mut old_peer).await,
        ServerMessage::HostTransferComplete { code: code.clone() }
    );

    let (mut next_peer, _) = connect_async(&new_url).await.unwrap();
    send_client_message(
        &mut next_peer,
        ClientMessage::JoinRoom { code: code.clone() },
    )
    .await;
    assert_eq!(
        receive_server_message(&mut next_peer).await,
        ServerMessage::RoomJoined { code: code.clone() }
    );
    assert_eq!(
        receive_server_message(&mut new_host).await,
        ServerMessage::PeerJoined
    );

    let (mut stale_join, _) = connect_async(&old_url).await.unwrap();
    send_client_message(&mut stale_join, ClientMessage::JoinRoom { code }).await;
    assert!(matches!(
        receive_server_message(&mut stale_join).await,
        ServerMessage::Error { .. }
    ));

    old_host.close(None).await.unwrap();
    old_peer.close(None).await.unwrap();
    next_peer.close(None).await.unwrap();
    new_host.close(None).await.unwrap();
    stale_join.close(None).await.unwrap();
    let _ = stop_old.send(());
    let _ = stop_new.send(());
    old_server.await.unwrap().unwrap();
    new_server.await.unwrap().unwrap();
}

#[tokio::test]
async fn websocket_clients_create_join_and_exchange_diagnostic_signal() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let rooms = Arc::new(Mutex::new(RoomRegistry::default()));
    let next_id = Arc::new(AtomicU64::new(1));
    let accept_task = tokio::spawn(async move {
        loop {
            let Ok((stream, peer_address)) = listener.accept().await else {
                break;
            };
            let connection_id = next_id.fetch_add(1, Ordering::Relaxed);
            let rooms = Arc::clone(&rooms);
            let _ = spawn_admitted_connection(stream, connection_id, peer_address, rooms).await;
        }
    });

    let url = format!("ws://{address}");
    let (mut first, _) = connect_async(&url).await.unwrap();
    send_client_message(&mut first, ClientMessage::CreateRoom).await;
    let ServerMessage::RoomCreated { code } = receive_server_message(&mut first).await else {
        panic!("expected room creation confirmation")
    };

    let (mut second, _) = connect_async(&url).await.unwrap();
    send_client_message(&mut second, ClientMessage::JoinRoom { code: code.clone() }).await;
    assert_eq!(
        receive_server_message(&mut second).await,
        ServerMessage::RoomJoined { code: code.clone() }
    );
    assert_eq!(
        receive_server_message(&mut first).await,
        ServerMessage::PeerJoined
    );

    let (mut third, _) = connect_async(&url).await.unwrap();
    send_client_message(&mut third, ClientMessage::JoinRoom { code: code.clone() }).await;
    assert_eq!(
        receive_server_message(&mut third).await,
        ServerMessage::RoomJoined { code: code.clone() }
    );
    assert_eq!(
        receive_server_message(&mut first).await,
        ServerMessage::PeerJoined
    );
    assert_eq!(
        receive_server_message(&mut second).await,
        ServerMessage::PeerJoined
    );

    let (mut invalid, _) = connect_async(&url).await.unwrap();
    send_client_message(
        &mut invalid,
        ClientMessage::JoinRoom {
            code: "NOT-A-ROOM".to_owned(),
        },
    )
    .await;
    assert!(matches!(
        receive_server_message(&mut invalid).await,
        ServerMessage::Error { message } if message.contains("nao existe")
    ));
    invalid.close(None).await.unwrap();

    send_client_message(
        &mut first,
        ClientMessage::Signal {
            kind: SignalKind::Diagnostic,
            payload: "diagnostic-ping-v1".to_owned(),
            target_participant_id: None,
            stream_id: None,
        },
    )
    .await;
    assert_eq!(
        receive_server_message(&mut second).await,
        ServerMessage::Signal {
            kind: SignalKind::Diagnostic,
            payload: "diagnostic-ping-v1".to_owned(),
            from_participant_id: Some("participant-1".to_owned()),
            stream_id: None,
        }
    );
    assert_eq!(
        receive_server_message(&mut third).await,
        ServerMessage::Signal {
            kind: SignalKind::Diagnostic,
            payload: "diagnostic-ping-v1".to_owned(),
            from_participant_id: Some("participant-1".to_owned()),
            stream_id: None,
        }
    );
    send_client_message(
        &mut second,
        ClientMessage::Signal {
            kind: SignalKind::Diagnostic,
            payload: "diagnostic-pong-v1".to_owned(),
            target_participant_id: None,
            stream_id: None,
        },
    )
    .await;
    assert_eq!(
        receive_server_message(&mut first).await,
        ServerMessage::Signal {
            kind: SignalKind::Diagnostic,
            payload: "diagnostic-pong-v1".to_owned(),
            from_participant_id: Some("participant-2".to_owned()),
            stream_id: None,
        }
    );
    assert_eq!(
        receive_server_message(&mut third).await,
        ServerMessage::Signal {
            kind: SignalKind::Diagnostic,
            payload: "diagnostic-pong-v1".to_owned(),
            from_participant_id: Some("participant-2".to_owned()),
            stream_id: None,
        }
    );

    first.close(None).await.unwrap();
    assert_eq!(
        receive_server_message(&mut second).await,
        ServerMessage::PeerLeft
    );
    second.close(None).await.unwrap();
    accept_task.abort();
}

fn ip(last: u8) -> Option<IpAddr> {
    Some(IpAddr::V4(Ipv4Addr::new(203, 0, 113, last)))
}

#[test]
fn connection_admission_enforces_per_address_and_global_limits() {
    let mut registry = RoomRegistry::default();
    for _ in 0..MAX_CONNECTIONS_PER_IP {
        registry.admit_connection(ip(1)).unwrap();
    }
    assert!(registry.admit_connection(ip(1)).is_err());
    // Outro endereço continua sendo aceito.
    registry.admit_connection(ip(2)).unwrap();

    registry.release_connection(ip(1));
    registry.admit_connection(ip(1)).unwrap();

    let mut registry = RoomRegistry::default();
    for index in 0..MAX_TOTAL_CONNECTIONS {
        let octets = [10, (index / 250) as u8, (index % 250) as u8, 1];
        registry
            .admit_connection(Some(IpAddr::V4(Ipv4Addr::from(octets))))
            .unwrap();
    }
    assert!(registry.admit_connection(ip(9)).is_err());
    registry.release_connection(ip(9));
    assert_eq!(registry.total_connections, MAX_TOTAL_CONNECTIONS - 1);
}

#[tokio::test]
async fn accepted_connections_are_admitted_before_handler_is_spawned() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let rooms = Arc::new(Mutex::new(RoomRegistry::default()));
    let mut clients = Vec::new();
    let mut handlers = Vec::new();

    for connection_id in 1..=MAX_CONNECTIONS_PER_IP as u64 + 1 {
        clients.push(TcpStream::connect(address).await.unwrap());
        let (stream, peer_address) = listener.accept().await.unwrap();
        let handler =
            spawn_admitted_connection(stream, connection_id, peer_address, Arc::clone(&rooms))
                .await;

        if connection_id <= MAX_CONNECTIONS_PER_IP as u64 {
            handlers.push(handler.expect("connection under the per-IP limit is admitted"));
        } else {
            assert!(
                handler.is_none(),
                "excess connection must be rejected before spawn"
            );
        }
    }

    assert_eq!(rooms.lock().await.total_connections, MAX_CONNECTIONS_PER_IP);
    drop(clients);
    for handler in handlers {
        handler.await.unwrap();
    }
    assert_eq!(rooms.lock().await.total_connections, 0);
}

#[test]
fn repeated_invalid_join_attempts_are_blocked_until_the_window_expires() {
    let mut registry = RoomRegistry::default();
    let start = Instant::now();
    for _ in 0..MAX_FAILED_JOINS_PER_WINDOW {
        assert!(!registry.join_attempts_blocked(ip(1), start));
        registry.record_failed_join(ip(1), start);
    }
    assert!(registry.join_attempts_blocked(ip(1), start));
    assert!(!registry.join_attempts_blocked(ip(2), start));
    assert!(!registry.join_attempts_blocked(ip(1), start + FAILED_JOIN_WINDOW));
    assert!(registry.failed_joins.is_empty());
}

#[tokio::test]
async fn websocket_blocks_room_code_guessing_and_rejects_oversized_messages() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let rooms = Arc::new(Mutex::new(RoomRegistry::default()));
    let next_id = Arc::new(AtomicU64::new(1));
    let accept_task = tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let connection_id = next_id.fetch_add(1, Ordering::Relaxed);
            let rooms = Arc::clone(&rooms);
            let peer_address = match stream.peer_addr() {
                Ok(address) => address,
                Err(_) => continue,
            };
            let _ = spawn_admitted_connection(stream, connection_id, peer_address, rooms).await;
        }
    });
    let url = format!("ws://{address}");

    let (mut guesser, _) = connect_async(&url).await.unwrap();
    for _ in 0..MAX_FAILED_JOINS_PER_WINDOW {
        send_client_message(
            &mut guesser,
            ClientMessage::JoinRoom {
                code: "AAAAAAAAAAAA".to_owned(),
            },
        )
        .await;
        assert!(matches!(
            receive_server_message(&mut guesser).await,
            ServerMessage::Error { message } if message.contains("nao existe")
        ));
    }
    send_client_message(
        &mut guesser,
        ClientMessage::JoinRoom {
            code: "BBBBBBBBBBBB".to_owned(),
        },
    )
    .await;
    assert!(matches!(
        receive_server_message(&mut guesser).await,
        ServerMessage::Error { message } if message.contains("Muitas tentativas")
    ));
    // Criar uma sala continua permitido; apenas as tentativas de entrada são limitadas.
    send_client_message(&mut guesser, ClientMessage::CreateRoom).await;
    assert!(matches!(
        receive_server_message(&mut guesser).await,
        ServerMessage::RoomCreated { code } if code.len() == ROOM_CODE_LENGTH
    ));
    let _ = guesser.close(None).await;

    let (mut oversized, _) = connect_async(&url).await.unwrap();
    oversized
        .send(WebSocketMessage::Text(
            "x".repeat(MAX_MESSAGE_BYTES + 1).into(),
        ))
        .await
        .unwrap();
    let ended = timeout(Duration::from_secs(5), async {
        loop {
            match oversized.next().await {
                None | Some(Err(_)) | Some(Ok(WebSocketMessage::Close(_))) => break,
                Some(Ok(_)) => {}
            }
        }
    })
    .await;
    assert!(
        ended.is_ok(),
        "o servidor deve encerrar a conexão com mensagem grande demais"
    );
    accept_task.abort();
}

#[test]
fn test_is_valid_room_code() {
    assert!(is_valid_room_code("ABCD"));
    assert!(is_valid_room_code("MINHA-SALA-01"));
    assert!(is_valid_room_code("ROOM_123_TEST"));
    assert!(is_valid_room_code("12345678901234567890123456789012")); // 32 chars
    // Inválidos
    assert!(!is_valid_room_code("ABC")); // menos de 4 chars
    assert!(!is_valid_room_code("123456789012345678901234567890123")); // 33 chars
    assert!(!is_valid_room_code("SALA COM ESPACO"));
    assert!(!is_valid_room_code("SALA@!#"));
}

#[tokio::test]
async fn websocket_creates_and_joins_fixed_room_and_rejects_duplicates() {
    let (url, stop, accept_task) = start_test_server(None).await;
    let (mut host, _) = connect_async(&url).await.unwrap();

    // Criação com código fixo
    send_client_message(
        &mut host,
        ClientMessage::CreateRoomIdentified {
            participant: default_participant(1, 1),
            room_mode: RoomMode::Local,
            fixed_code: Some("MINHA-SALA-FIXA".to_owned()),
        },
    )
    .await;

    let created = receive_server_message(&mut host).await;
    assert_eq!(
        created,
        ServerMessage::RoomCreated {
            code: "MINHA-SALA-FIXA".to_owned()
        }
    );
    let initial_roster = receive_server_message(&mut host).await;
    assert!(matches!(initial_roster, ServerMessage::RoomRoster { .. }));

    // Tentativa de outro host criar com o mesmo código deve falhar
    let (mut duplicate_host, _) = connect_async(&url).await.unwrap();
    send_client_message(
        &mut duplicate_host,
        ClientMessage::CreateRoomIdentified {
            participant: default_participant(2, 1),
            room_mode: RoomMode::Local,
            fixed_code: Some("MINHA-SALA-FIXA".to_owned()),
        },
    )
    .await;
    let duplicate_response = receive_server_message(&mut duplicate_host).await;
    assert!(matches!(
        duplicate_response,
        ServerMessage::Error { message } if message.contains("já está em uso")
    ));
    let _ = duplicate_host.close(None).await;

    // Convidado entra com sucesso na sala fixa
    let (mut guest, _) = connect_async(&url).await.unwrap();
    send_client_message(
        &mut guest,
        ClientMessage::JoinRoomIdentified {
            code: "MINHA-SALA-FIXA".to_owned(),
            participant: default_participant(3, 2),
        },
    )
    .await;
    assert_eq!(
        receive_server_message(&mut guest).await,
        ServerMessage::RoomJoined {
            code: "MINHA-SALA-FIXA".to_owned()
        }
    );
    assert_eq!(
        receive_server_message(&mut host).await,
        ServerMessage::PeerJoined
    );

    let _ = guest.close(None).await;
    let _ = host.close(None).await;
    let _ = stop.send(());
    accept_task.abort();
}
