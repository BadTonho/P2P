use super::*;

fn room(code: &str, participants: u8) -> RoomAdvertisement {
    RoomAdvertisement {
        code: code.to_owned(),
        host_name: "Anfitrião".to_owned(),
        participants,
    }
}

#[test]
fn discovery_packet_accepts_only_its_protocol_and_version() {
    let bytes = encode(Message::Room {
        room: room("ROOM123", 2),
    });
    assert!(matches!(decode(&bytes), Some(Message::Room { .. })));
    assert!(matches!(
        decode(&encode(Message::Query)),
        Some(Message::Query)
    ));
    let mut value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    value["version"] = 2.into();
    assert!(decode(&serde_json::to_vec(&value).unwrap()).is_none());
    value["version"] = 1.into();
    value["protocol"] = "another-app".into();
    assert!(decode(&serde_json::to_vec(&value).unwrap()).is_none());
    value["protocol"] = PROTOCOL.into();
    value["unknown"] = true.into();
    assert!(decode(&serde_json::to_vec(&value).unwrap()).is_none());
}

#[test]
fn discovery_rejects_truncated_oversized_and_invalid_advertisements() {
    assert!(decode(b"{").is_none());
    assert!(decode(&vec![b' '; MAX_PACKET + 1]).is_none());
    let mut examples = vec![
        room("BAD SPACE", 1),
        room(" ROOM ", 1),
        room("ABC", 1),
        room("ROOM", 0),
        room("ROOM", 9),
    ];
    let mut control = room("ROOM", 1);
    control.host_name = "name\n".to_owned();
    examples.push(control);
    let mut long = room("ROOM", 1);
    long.host_name = "á".repeat(65);
    examples.push(long);
    let mut empty = room("ROOM", 1);
    empty.host_name = " ".to_owned();
    examples.push(empty);
    for advertisement in examples {
        assert!(
            decode(&encode(Message::Room {
                room: advertisement
            }))
            .is_none()
        );
    }
}

#[test]
fn discovery_cache_deduplicates_updates_occupancy_and_refreshes_expiry() {
    let now = Instant::now();
    let ip = Ipv4Addr::new(26, 1, 2, 3);
    let mut cache = RoomCache::default();
    cache.receive(ip, room("ROOM123", 1), now);
    cache.receive(ip, room("ROOM123", 4), now + INTERVAL);
    let rooms = cache.snapshot(now + EXPIRY);
    assert_eq!(rooms.len(), 1);
    assert_eq!(rooms[0].advertisement.participants, 4);
    assert!(cache.snapshot(now + INTERVAL + EXPIRY).is_empty());
}

#[test]
fn discovery_keeps_same_code_on_different_hosts_and_different_rooms_on_same_host() {
    let now = Instant::now();
    let mut cache = RoomCache::default();
    cache.receive(Ipv4Addr::new(26, 1, 2, 3), room("ROOM", 1), now);
    cache.receive(Ipv4Addr::new(26, 1, 2, 4), room("ROOM", 2), now);
    cache.receive(Ipv4Addr::new(26, 1, 2, 3), room("OTHER", 3), now);
    assert_eq!(cache.snapshot(now).len(), 3);
}

#[test]
fn discovery_bounds_cache_and_accepts_updates_when_full() {
    let now = Instant::now();
    let mut cache = RoomCache::default();
    for index in 1..=MAX_ROOMS + 1 {
        cache.receive(Ipv4Addr::new(26, 0, 0, index as u8), room("ROOM", 1), now);
    }
    assert_eq!(cache.snapshot(now).len(), MAX_ROOMS);
    let ip = Ipv4Addr::new(26, 0, 0, 1);
    cache.receive(ip, room("ROOM", 8), now);
    assert_eq!(
        cache
            .snapshot(now)
            .iter()
            .find(|room| room.ipv4 == ip)
            .unwrap()
            .advertisement
            .participants,
        8
    );
}

#[test]
fn discovery_computes_broadcast_and_rejects_other_networks_self_and_invalid_ips() {
    let network = DiscoveryNetwork {
        adapter: "Radmin VPN".to_owned(),
        ipv4: Ipv4Addr::new(26, 1, 2, 3),
        prefix_len: 8,
    };
    assert_eq!(network.broadcast(), Some(Ipv4Addr::new(26, 255, 255, 255)));
    assert!(network.accepts(Ipv4Addr::new(26, 99, 88, 77), false));
    for ip in [
        network.ipv4,
        Ipv4Addr::BROADCAST,
        Ipv4Addr::UNSPECIFIED,
        Ipv4Addr::LOCALHOST,
        Ipv4Addr::new(192, 168, 1, 2),
        Ipv4Addr::new(224, 0, 0, 1),
        network.broadcast().unwrap(),
    ] {
        assert!(!network.accepts(ip, false));
    }
    for prefix_len in [0, 32, 33] {
        let changed = DiscoveryNetwork {
            prefix_len,
            ..network.clone()
        };
        assert_eq!(changed.broadcast(), None);
        assert!(!changed.accepts(Ipv4Addr::new(26, 1, 2, 4), false));
    }
}

fn wait_for(predicate: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !predicate() {
        assert!(
            Instant::now() < deadline,
            "loopback discovery did not complete"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn discovery_loopback_receives_room_updates_without_persisting_or_connecting() {
    let host_receiver = UdpSocket::bind((Ipv4Addr::new(127, 0, 0, 2), 0)).unwrap();
    let guest_receiver = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let SocketAddr::V4(host_address) = host_receiver.local_addr().unwrap() else {
        unreachable!()
    };
    let SocketAddr::V4(guest_address) = guest_receiver.local_addr().unwrap() else {
        unreachable!()
    };
    let host = DiscoveryService::start_loopback(host_receiver, vec![guest_address]);
    let guest = DiscoveryService::start_loopback(guest_receiver, vec![host_address]);
    host.advertise(Some(room("DISCOVERED", 1)));
    wait_for(|| guest.snapshot().0.len() == 1);
    let discovered = guest.snapshot().0.remove(0);
    assert_eq!(discovered.endpoint(), "ws://127.0.0.2:9000");
    assert_eq!(discovered.advertisement.code, "DISCOVERED");
    assert!(host.diagnostics().announcements_sent > 0);
    assert!(guest.diagnostics().announcements_received > 0);
    let source = UdpSocket::bind((Ipv4Addr::new(127, 0, 0, 3), 0)).unwrap();
    source
        .send_to(b"invalid PRIVATE_CODE", guest_address)
        .unwrap();
    wait_for(|| guest.diagnostics().invalid_datagrams > 0);
    let diagnostics = format!("{:?}", guest.diagnostics());
    assert!(!diagnostics.contains("DISCOVERED"));
    assert!(!diagnostics.contains("PRIVATE_CODE"));
    host.advertise(Some(room("DISCOVERED", 5)));
    wait_for(|| guest.snapshot().0[0].advertisement.participants == 5);
    host.advertise(None);
    // Allow an in-flight packet, then prove the next periodic tick sends no room.
    thread::sleep(Duration::from_millis(150));
    guest.shared.lock().unwrap().cache.entries.clear();
    thread::sleep(INTERVAL + Duration::from_millis(300));
    assert!(guest.snapshot().0.is_empty());
}

#[test]
fn dropping_discovery_releases_udp_port_before_restart() {
    let receiver = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = receiver.local_addr().unwrap();
    let service = DiscoveryService::start_loopback(receiver, Vec::new());
    drop(service);
    let rebound = UdpSocket::bind(address).expect("discovery socket must be released at Drop");
    drop(DiscoveryService::start_loopback(rebound, Vec::new()));
    assert!(UdpSocket::bind(address).is_ok());
}

#[cfg(windows)]
#[test]
fn discovery_windows_interface_restriction_preserves_selected_source_on_loopback() {
    use windows::Win32::NetworkManagement::IpHelper::GetBestInterface;
    let mut index = 0;
    // The OS loopback interface exists independently of physical network adapters.
    let result = unsafe { GetBestInterface(u32::from_ne_bytes([127, 0, 0, 1]), &mut index) };
    assert_eq!(result, 0);
    let receiver = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).unwrap();
    receiver
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let source = Ipv4Addr::new(127, 0, 0, 2);
    let sender = UdpSocket::bind((source, 0)).unwrap();
    restrict_socket_interfaces(&receiver, &sender, index).unwrap();
    sender
        .send_to(
            b"presence",
            (Ipv4Addr::LOCALHOST, receiver.local_addr().unwrap().port()),
        )
        .unwrap();
    let mut bytes = [0; 16];
    let (length, address) = receiver.recv_from(&mut bytes).unwrap();
    assert_eq!(&bytes[..length], b"presence");
    assert_eq!(address.ip(), std::net::IpAddr::V4(source));
}

#[test]
fn discovery_reports_transport_failure_without_including_room_code() {
    let receiver = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let service = DiscoveryService::start_loopback(receiver, Vec::new());
    service.advertise(Some(room("PRIVATE123", 1)));
    wait_for(|| service.snapshot().1.is_some());
    let error = service.snapshot().1.unwrap();
    assert!(!error.contains("PRIVATE123"));
}

#[test]
fn manual_search_queries_immediately_coalesces_clicks_and_preserves_cache() {
    let receiver = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let sink = UdpSocket::bind((Ipv4Addr::new(127, 0, 0, 2), 0)).unwrap();
    sink.set_read_timeout(Some(Duration::from_millis(700)))
        .unwrap();
    let SocketAddr::V4(target) = sink.local_addr().unwrap() else {
        unreachable!()
    };
    let service = DiscoveryService::start_loopback(receiver, vec![target]);
    let mut packet = [0; MAX_PACKET];
    let (length, _) = sink.recv_from(&mut packet).unwrap();
    assert!(matches!(decode(&packet[..length]), Some(Message::Query)));
    service.shared.lock().unwrap().cache.receive(
        *target.ip(),
        room("PRESERVED", 2),
        Instant::now(),
    );
    let seen_before = service
        .shared
        .lock()
        .unwrap()
        .cache
        .entries
        .values()
        .next()
        .unwrap()
        .1;
    // UDP delivery can precede the worker's accounting of that same send.
    wait_for(|| service.diagnostics().queries_sent >= 1);
    let before = service.diagnostics().queries_sent;
    assert!(service.request_refresh());
    assert!(!service.request_refresh());
    assert!(!service.request_refresh());
    let started = Instant::now();
    let (length, _) = sink
        .recv_from(&mut packet)
        .expect("manual query must arrive before the 2-second periodic interval");
    assert!(started.elapsed() < Duration::from_millis(700));
    assert!(matches!(decode(&packet[..length]), Some(Message::Query)));
    wait_for(|| service.diagnostics().queries_sent >= before + 1);
    assert_eq!(service.diagnostics().queries_sent, before + 1);
    assert_eq!(service.snapshot().0[0].advertisement.code, "PRESERVED");
    let shared = service.shared.lock().unwrap();
    assert_eq!(
        shared.cache.entries.values().next().unwrap().1,
        seen_before,
        "a manual query must not extend stale room expiry"
    );
    drop(shared);
    service.stopping.store(true, Ordering::Release);
    assert!(!service.is_running());
    assert!(!service.request_refresh());
}

// A provocação de WSAECONNRESET (send UDP a porta fechada seguido de recv)
// é um comportamento do Windows, onde o aplicativo roda.
#[test]
#[cfg(windows)]
fn worker_survives_connection_reset_and_keeps_discovering() {
    let receiver = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let SocketAddr::V4(address) = receiver.local_addr().unwrap() else {
        unreachable!()
    };
    // Provoca WSAECONNRESET: envio para uma porta sem destino deixa o erro
    // pendente no socket; o primeiro recv do worker recebe esse erro em vez
    // de um datagrama. Encostado no comportamento atual, isso encerraria o
    // worker imediatamente.
    let closed = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let closed_port = closed.local_addr().unwrap();
    drop(closed);
    let trigger = [0_u8; 8];
    let _ = receiver.send_to(&trigger, closed_port);

    let service = DiscoveryService::start_loopback(receiver, Vec::new());

    // Sucessor falso: envia um anúncio válido depois que o errou já ocorreu;
    // o worker precisa processá-lo normalmente. Envia de 127.0.0.2: accepts()
    // rejeita fontes no próprio IP do serviço (127.0.0.1) para evitar eco.
    let announcer = UdpSocket::bind((Ipv4Addr::new(127, 0, 0, 2), 0)).unwrap();
    let payload = encode(Message::Room {
        room: room("SALA-RESILIENTE", 2),
    });
    announcer.send_to(&payload, address).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let diagnostics = service.diagnostics();
        if diagnostics.receive_failures >= 1 && diagnostics.announcements_received >= 1 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "contadores aguardando: {diagnostics:?}"
        );
        thread::sleep(Duration::from_millis(10));
    }
    assert!(
        service.is_running(),
        "o worker precisa continuar rodando após a falha de recepção"
    );
    assert!(
        service
            .snapshot()
            .0
            .iter()
            .any(|discovered| discovered.advertisement.code == "SALA-RESILIENTE"),
        "o anúncio enviado após o erro precisa ser processado normalmente"
    );
}
