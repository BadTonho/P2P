use super::*;
use crate::settings::SavedHostProfile;
use std::mem::ManuallyDrop;
use std::net::{SocketAddr, UdpSocket};

fn address(
    adapter: &str,
    description: &str,
    ip: [u8; 4],
    lan: bool,
    has_gateway: bool,
    metric: u32,
) -> HostAddress {
    HostAddress {
        adapter: adapter.to_owned(),
        description: description.to_owned(),
        ipv4: ip.into(),
        prefix_len: 8,
        lan,
        has_gateway,
        metric,
    }
}

fn app() -> ManuallyDrop<ClientUi> {
    let mut app = ManuallyDrop::new(ClientUi::default());
    app.apply_preferences(AppSettings::default());
    app
}

#[test]
fn automatic_network_prefers_radmin_even_when_old_saved_address_is_lan() {
    let addresses = vec![
        address("Ethernet", "Ethernet", [192, 168, 1, 2], true, true, 1),
        address(
            "Renamed VPN",
            "Famatech Radmin VPN Ethernet Adapter",
            [26, 1, 2, 3],
            true,
            false,
            999,
        ),
    ];
    assert_eq!(
        select_network_address(
            &addresses,
            NetworkSelection::Automatic,
            Some(addresses[0].ipv4)
        ),
        Some(1)
    );
    assert_eq!(
        select_network_address(
            &addresses,
            NetworkSelection::Manual,
            Some(addresses[0].ipv4)
        ),
        Some(0)
    );
}

#[test]
fn automatic_network_falls_back_to_lan_and_manual_unavailable_keeps_preference() {
    let addresses = vec![
        address("Tunnel", "Other VPN", [10, 1, 2, 3], false, false, 1),
        address("Wi-Fi", "Wireless", [192, 168, 1, 2], true, true, 10),
        address("Ethernet", "Ethernet", [192, 168, 2, 2], true, false, 1),
    ];
    assert_eq!(
        select_network_address(&addresses, NetworkSelection::Automatic, None),
        Some(1)
    );
    assert_eq!(
        select_network_address(
            &addresses,
            NetworkSelection::Manual,
            Some([26, 1, 2, 3].into())
        ),
        Some(1)
    );
    assert_eq!(
        select_network_address(&[], NetworkSelection::Automatic, None),
        None
    );
}

#[test]
fn active_room_locks_adapter_refresh_and_network() {
    let mut app = app();
    app.host_addresses = vec![address(
        "Synthetic",
        "Synthetic",
        [26, 1, 2, 3],
        true,
        false,
        1,
    )];
    app.room_code = Some("ROOM".to_owned());
    app.refresh_host_addresses();
    assert_eq!(app.host_addresses[0].adapter, "Synthetic");
    assert!(app.network_is_locked());
    app.room_code = None;
    assert!(!app.network_is_locked());
}

#[test]
fn discovery_and_network_preferences_round_trip_without_saving_discovered_codes() {
    let mut app = app();
    app.network_selection = NetworkSelection::Manual;
    app.announce_local_room = false;
    app.preferred_control_ipv4 = Some([26, 1, 2, 3].into());
    app.discovered_rooms.push(DiscoveredRoom {
        ipv4: [26, 4, 5, 6].into(),
        advertisement: RoomAdvertisement {
            code: "DISCOVEREDONLY".to_owned(),
            host_name: "Friend".to_owned(),
            participants: 1,
        },
    });
    let preferences = app.preferences_snapshot();
    let bytes = serde_json::to_string(&preferences).unwrap();
    assert!(!bytes.contains("DISCOVEREDONLY"));
    let mut restored = ManuallyDrop::new(ClientUi::default());
    restored.apply_preferences(preferences);
    assert_eq!(restored.network_selection, NetworkSelection::Manual);
    assert!(!restored.announce_local_room);
    assert_eq!(restored.preferred_control_ipv4, Some([26, 1, 2, 3].into()));
}

#[test]
fn old_settings_default_to_automatic_discovery_and_preserve_manual_address() {
    let settings: AppSettings =
        serde_json::from_str(r#"{"control_ipv4":"192.168.1.2","server_url":"192.168.1.9"}"#)
            .unwrap();
    assert_eq!(settings.network_selection, NetworkSelection::Automatic);
    assert!(settings.announce_local_room);
    assert_eq!(settings.control_ipv4, Some([192, 168, 1, 2].into()));
    assert_eq!(settings.server_url, "192.168.1.9");
}

#[test]
fn undiscovered_or_expired_room_does_not_start_connection() {
    let mut app = app();
    app.join_discovered_room(DiscoveredRoom {
        ipv4: [26, 1, 2, 3].into(),
        advertisement: RoomAdvertisement {
            code: "ROOM".to_owned(),
            host_name: "Friend".to_owned(),
            participants: 1,
        },
    });
    assert!(app.signaling.is_none());
    assert!(!app.connecting);
    assert!(app.connection_error.is_some());
}

#[tokio::test]
async fn discovery_loopback_selection_joins_existing_signaling_and_never_changes_manual_preferences()
 {
    use futures_util::{SinkExt, StreamExt};
    use signaling_protocol::{ClientMessage, ServerMessage};
    use tokio_tungstenite::tungstenite::Message;

    // The real signaling server on the advertised fixed port, isolated on another loopback IP.
    let listener = tokio::net::TcpListener::bind("127.0.0.2:9000")
        .await
        .unwrap();
    let (shutdown, shutdown_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(signaling_server::serve(listener, shutdown_rx, None));
    let (mut host_socket, _) = tokio_tungstenite::connect_async("ws://127.0.0.2:9000")
        .await
        .unwrap();
    host_socket
        .send(Message::Text(
            serde_json::to_string(&ClientMessage::CreateRoom)
                .unwrap()
                .into(),
        ))
        .await
        .unwrap();
    let Message::Text(created) = host_socket.next().await.unwrap().unwrap() else {
        panic!("missing room creation")
    };
    let ServerMessage::RoomCreated { code } = serde_json::from_str(&created).unwrap() else {
        panic!("missing room creation")
    };

    let host_receiver = UdpSocket::bind("127.0.0.2:0").unwrap();
    let guest_receiver = UdpSocket::bind("127.0.0.1:0").unwrap();
    let SocketAddr::V4(host_address) = host_receiver.local_addr().unwrap() else {
        unreachable!()
    };
    let SocketAddr::V4(guest_address) = guest_receiver.local_addr().unwrap() else {
        unreachable!()
    };
    let host_presence = DiscoveryService::start_loopback(host_receiver, vec![guest_address]);
    let guest_presence = DiscoveryService::start_loopback(guest_receiver, vec![host_address]);
    host_presence.advertise(Some(RoomAdvertisement {
        code: code.clone(),
        host_name: "Friend".to_owned(),
        participants: 1,
    }));
    let mut app = app();
    app.server_url = "192.168.1.10".to_owned();
    app.saved_hosts = vec![SavedHostProfile {
        name: "Saved".to_owned(),
        address: "192.168.1.11".to_owned(),
        room_code: None,
    }];
    app.selected_host_index = Some(0);
    let preferences_before = app.preferences_snapshot();
    tokio::time::timeout(Duration::from_secs(5), async {
        while guest_presence.snapshot().0.is_empty() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    app.discovered_rooms = guest_presence.snapshot().0;
    assert!(
        app.signaling.is_none(),
        "discovery alone must never connect"
    );
    let selected = app.discovered_rooms[0].clone();
    app.join_discovered_room(selected);
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(event) = app.signaling.as_ref().unwrap().try_recv() {
                match event {
                    SignalingEvent::RoomJoined(joined) => {
                        assert_eq!(joined, code);
                        break;
                    }
                    SignalingEvent::Error(error) | SignalingEvent::ServerError(error) => {
                        panic!("{error}")
                    }
                    _ => {}
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(app.preferences_snapshot() == preferences_before);
    assert!(app.join_code.is_empty());
    app.room_code = Some(code.clone());
    app.hosting_locally = true;
    app.profile_display_name = "Friend".to_owned();
    assert_eq!(app.local_room_advertisement().unwrap().participants, 1);
    let local = app.local_participant_info();
    app.participants.push(local);
    let mut second = app.local_participant_info();
    second.id = "second".to_owned();
    app.participants.push(second);
    assert_eq!(app.local_room_advertisement().unwrap().participants, 2);
    app.announce_local_room = false;
    assert!(app.local_room_advertisement().is_none());
    app.announce_local_room = true;
    app.room_mode = RoomMode::InternetTest;
    assert!(app.local_room_advertisement().is_none());
    app.room_mode = RoomMode::Local;
    app.current_leader_id = "another-host".to_owned();
    assert!(app.local_room_advertisement().is_none());
    app.current_leader_id.clear();
    app.hosting_locally = false;
    assert!(app.local_room_advertisement().is_none());
    app.hosting_locally = true;
    app.discovery = Some(host_presence);
    let diagnostic = app.diagnostic_snapshot().discovery_state;
    assert!(diagnostic.contains("UDP 8999"));
    assert!(diagnostic.contains("contadores="));
    assert!(!diagnostic.contains(&code));
    assert!(!diagnostic.contains("Friend"));
    // Explicit leave immediately clears presence; signaling shutdown runs off the async server thread.
    let signaling = app.signaling.take();
    let dropping = tokio::task::spawn_blocking(move || drop(signaling));
    app.leave_room();
    assert!(app.local_room_advertisement().is_none());
    app.discovery = None;
    dropping.await.unwrap();
    drop(guest_presence);
    let _ = host_socket.close(None).await;
    let _ = shutdown.send(());
    server.await.unwrap().unwrap();
}
