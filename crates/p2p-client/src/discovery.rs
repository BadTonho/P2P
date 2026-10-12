//! Local room presence only. Received room codes are never persisted or logged.
use std::collections::HashMap;
use std::io;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

pub(crate) const DISCOVERY_PORT: u16 = 8999;
const PROTOCOL: &str = "p2p-voz-tela-discovery";
const VERSION: u8 = 1;
const INTERVAL: Duration = Duration::from_secs(2);
const EXPIRY: Duration = Duration::from_secs(6);
const MAX_PACKET: usize = 1024;
const MAX_ROOMS: usize = 128;

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct DiscoveryNetwork {
    pub adapter: String,
    pub ipv4: Ipv4Addr,
    pub prefix_len: u32,
}

impl DiscoveryNetwork {
    fn broadcast(&self) -> Option<Ipv4Addr> {
        (1..=30).contains(&self.prefix_len).then(|| {
            let mask = u32::MAX << (32 - self.prefix_len);
            Ipv4Addr::from(u32::from(self.ipv4) | !mask)
        })
    }

    fn accepts(&self, ip: Ipv4Addr, allow_loopback: bool) -> bool {
        if ip == self.ipv4
            || ip.is_unspecified()
            || ip.is_multicast()
            || ip.is_broadcast()
            || ip.is_loopback() && !allow_loopback
            || self.prefix_len == 0
            || self.prefix_len > 32
        {
            return false;
        }
        let mask = u32::MAX << (32 - self.prefix_len);
        u32::from(ip) & mask == u32::from(self.ipv4) & mask && self.broadcast() != Some(ip)
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RoomAdvertisement {
    pub code: String,
    pub host_name: String,
    pub participants: u8,
}

impl RoomAdvertisement {
    pub(crate) fn valid(&self) -> bool {
        self.code == self.code.trim()
            && crate::app::is_valid_room_code(&self.code)
            && !self.host_name.trim().is_empty()
            && self.host_name.chars().count() <= 64
            && !self.host_name.chars().any(char::is_control)
            && (1..=8).contains(&self.participants)
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct DiscoveredRoom {
    pub ipv4: Ipv4Addr,
    pub advertisement: RoomAdvertisement,
}

impl DiscoveredRoom {
    pub(crate) fn endpoint(&self) -> String {
        format!("ws://{}:9000", self.ipv4)
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Packet {
    protocol: String,
    version: u8,
    message: Message,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum Message {
    Query,
    Room { room: RoomAdvertisement },
}

fn encode(message: Message) -> Vec<u8> {
    serde_json::to_vec(&Packet {
        protocol: PROTOCOL.to_owned(),
        version: VERSION,
        message,
    })
    .expect("discovery messages contain only serializable scalar fields")
}

fn decode(bytes: &[u8]) -> Option<Message> {
    if bytes.len() > MAX_PACKET {
        return None;
    }
    let packet: Packet = serde_json::from_slice(bytes).ok()?;
    if packet.protocol != PROTOCOL || packet.version != VERSION {
        return None;
    }
    match packet.message {
        Message::Room { ref room } if !room.valid() => None,
        message => Some(message),
    }
}

#[derive(Default)]
struct RoomCache {
    entries: HashMap<(Ipv4Addr, String), (DiscoveredRoom, Instant)>,
}

impl RoomCache {
    fn receive(&mut self, ip: Ipv4Addr, room: RoomAdvertisement, now: Instant) {
        self.expire(now);
        let key = (ip, room.code.clone());
        if self.entries.len() < MAX_ROOMS || self.entries.contains_key(&key) {
            self.entries.insert(
                key,
                (
                    DiscoveredRoom {
                        ipv4: ip,
                        advertisement: room,
                    },
                    now,
                ),
            );
        }
    }

    fn expire(&mut self, now: Instant) {
        self.entries
            .retain(|_, (_, seen)| now.saturating_duration_since(*seen) < EXPIRY);
    }

    fn snapshot(&mut self, now: Instant) -> Vec<DiscoveredRoom> {
        self.expire(now);
        let mut rooms: Vec<_> = self
            .entries
            .values()
            .map(|(room, _)| room.clone())
            .collect();
        rooms.sort_by(|left, right| {
            left.advertisement
                .host_name
                .cmp(&right.advertisement.host_name)
                .then_with(|| left.ipv4.cmp(&right.ipv4))
                .then_with(|| left.advertisement.code.cmp(&right.advertisement.code))
        });
        rooms
    }
}

#[derive(Default)]
struct Shared {
    advertisement: Option<RoomAdvertisement>,
    cache: RoomCache,
    error: Option<String>,
    diagnostics: DiscoveryDiagnostics,
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct DiscoveryDiagnostics {
    pub queries_sent: u64,
    pub announcements_sent: u64,
    pub queries_received: u64,
    pub announcements_received: u64,
    pub invalid_datagrams: u64,
    pub ignored_sources: u64,
}

pub(crate) struct DiscoveryService {
    pub network: DiscoveryNetwork,
    shared: Arc<Mutex<Shared>>,
    stopping: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl DiscoveryService {
    #[cfg(test)]
    pub(crate) fn start_loopback(receiver: UdpSocket, targets: Vec<SocketAddrV4>) -> Self {
        let SocketAddr::V4(address) = receiver.local_addr().unwrap() else {
            unreachable!()
        };
        let sender = UdpSocket::bind((*address.ip(), 0)).unwrap();
        Self::with_transport(
            DiscoveryNetwork {
                adapter: "test loopback".to_owned(),
                ipv4: *address.ip(),
                prefix_len: 8,
            },
            receiver,
            sender,
            targets,
            true,
        )
        .unwrap()
    }

    pub(crate) fn start(network: DiscoveryNetwork) -> Result<Self, String> {
        let receiver =
            UdpSocket::bind((Ipv4Addr::UNSPECIFIED, DISCOVERY_PORT)).map_err(|error| {
                format!("Não foi possível abrir a descoberta UDP {DISCOVERY_PORT}: {error}")
            })?;
        // Separate sender fixes the source address even on adapters with multiple IPv4s.
        let sender = UdpSocket::bind((network.ipv4, 0)).map_err(|error| {
            format!("Não foi possível usar o adaptador para descoberta: {error}")
        })?;
        restrict_interface(&receiver, &sender, &network.adapter).map_err(|error| {
            format!("Não foi possível restringir a descoberta à rede escolhida: {error}")
        })?;
        let mut targets = vec![SocketAddrV4::new(Ipv4Addr::BROADCAST, DISCOVERY_PORT)];
        if let Some(broadcast) = network.broadcast() {
            let target = SocketAddrV4::new(broadcast, DISCOVERY_PORT);
            if !targets.contains(&target) {
                targets.push(target);
            }
        }
        Self::with_transport(network, receiver, sender, targets, false)
            .map_err(|error| format!("Não foi possível iniciar a descoberta: {error}"))
    }

    fn with_transport(
        network: DiscoveryNetwork,
        receiver: UdpSocket,
        sender: UdpSocket,
        targets: Vec<SocketAddrV4>,
        allow_loopback: bool,
    ) -> io::Result<Self> {
        receiver.set_read_timeout(Some(Duration::from_millis(100)))?;
        sender.set_write_timeout(Some(Duration::from_millis(100)))?;
        sender.set_broadcast(true)?;
        tracing::info!(adapter = %network.adapter, ipv4 = %network.ipv4, prefix_len = network.prefix_len, udp_port = DISCOVERY_PORT, targets = targets.len(), "Descoberta de salas iniciada na interface escolhida; códigos omitidos");
        let shared = Arc::new(Mutex::new(Shared::default()));
        let stopping = Arc::new(AtomicBool::new(false));
        let worker_shared = shared.clone();
        let worker_stopping = stopping.clone();
        let worker_network = network.clone();
        let worker = thread::Builder::new()
            .name("room-discovery".to_owned())
            .spawn(move || {
                run_worker(
                    worker_network,
                    receiver,
                    sender,
                    targets,
                    allow_loopback,
                    worker_shared,
                    worker_stopping,
                );
            })?;
        Ok(Self {
            network,
            shared,
            stopping,
            worker: Some(worker),
        })
    }

    pub(crate) fn advertise(&self, advertisement: Option<RoomAdvertisement>) {
        let advertisement = advertisement.filter(RoomAdvertisement::valid);
        let mut shared = self.shared.lock().expect("discovery state poisoned");
        if shared.advertisement != advertisement {
            tracing::info!(
                announcing = advertisement.is_some(),
                participants = advertisement.as_ref().map(|room| room.participants),
                "Estado do anúncio local alterado; nome e código omitidos"
            );
            shared.advertisement = advertisement;
        }
    }

    pub(crate) fn diagnostics(&self) -> DiscoveryDiagnostics {
        self.shared
            .lock()
            .expect("discovery state poisoned")
            .diagnostics
    }

    pub(crate) fn snapshot(&self) -> (Vec<DiscoveredRoom>, Option<String>) {
        let mut shared = self.shared.lock().expect("discovery state poisoned");
        (shared.cache.snapshot(Instant::now()), shared.error.clone())
    }
}

impl Drop for DiscoveryService {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            // Socket reads and writes have short timeouts, including when the network goes away.
            let _ = worker.join();
        }
        tracing::info!(ipv4 = %self.network.ipv4, "Descoberta encerrada; porta UDP liberada");
    }
}

fn run_worker(
    network: DiscoveryNetwork,
    receiver: UdpSocket,
    sender: UdpSocket,
    targets: Vec<SocketAddrV4>,
    allow_loopback: bool,
    shared: Arc<Mutex<Shared>>,
    stopping: Arc<AtomicBool>,
) {
    let mut buffer = [0_u8; MAX_PACKET + 1];
    let mut last_tick = Instant::now() - INTERVAL;
    let mut last_advertisement = None;
    let mut last_reply = Instant::now() - INTERVAL;
    let mut last_report = Instant::now();
    while !stopping.load(Ordering::Acquire) {
        let now = Instant::now();
        let advertisement = shared
            .lock()
            .expect("discovery state poisoned")
            .advertisement
            .clone();
        let periodic = now.duration_since(last_tick) >= INTERVAL;
        let changed = advertisement != last_advertisement;
        if periodic {
            send(&sender, &targets, Message::Query, &shared);
            last_tick = now;
        }
        if (periodic || changed)
            && let Some(room) = &advertisement
        {
            send(
                &sender,
                &targets,
                Message::Room { room: room.clone() },
                &shared,
            );
            last_reply = now;
        }
        last_advertisement = advertisement.clone();
        match receiver.recv_from(&mut buffer) {
            Ok((length, SocketAddr::V4(source)))
                if network.accepts(*source.ip(), allow_loopback) =>
            {
                match decode(&buffer[..length]) {
                    Some(Message::Room { room }) => {
                        let mut shared = shared.lock().expect("discovery state poisoned");
                        shared.diagnostics.announcements_received += 1;
                        shared.cache.receive(*source.ip(), room, Instant::now());
                    }
                    Some(Message::Query) => {
                        shared
                            .lock()
                            .expect("discovery state poisoned")
                            .diagnostics
                            .queries_received += 1;
                        if last_reply.elapsed() >= INTERVAL
                            && let Some(room) = advertisement
                        {
                            // Replies use the same local broadcasts, never an attacker-supplied endpoint.
                            send(&sender, &targets, Message::Room { room }, &shared);
                            last_reply = Instant::now();
                        }
                    }
                    None => {
                        shared
                            .lock()
                            .expect("discovery state poisoned")
                            .diagnostics
                            .invalid_datagrams += 1
                    }
                }
            }
            Ok(_) => {
                shared
                    .lock()
                    .expect("discovery state poisoned")
                    .diagnostics
                    .ignored_sources += 1
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) => {}
            Err(error) => {
                tracing::warn!(error = %error, "Descoberta falhou ao receber; entrada manual continua disponível");
                shared.lock().expect("discovery state poisoned").error =
                    Some(format!("Falha ao receber descoberta: {error}"));
                break;
            }
        }
        if last_report.elapsed() >= Duration::from_secs(10) {
            let mut shared = shared.lock().expect("discovery state poisoned");
            shared.cache.expire(Instant::now());
            tracing::info!(ipv4 = %network.ipv4, rooms = shared.cache.entries.len(), counters = ?shared.diagnostics, failed = shared.error.is_some(), "Resumo da descoberta; códigos e nomes omitidos");
            last_report = Instant::now();
        }
    }
}

fn send(sender: &UdpSocket, targets: &[SocketAddrV4], message: Message, shared: &Mutex<Shared>) {
    let is_query = matches!(message, Message::Query);
    let bytes = encode(message);
    let mut sent = false;
    let mut last_error = None;
    for target in targets {
        match sender.send_to(&bytes, target) {
            Ok(_) => sent = true,
            Err(error) => last_error = Some(error),
        }
    }
    let mut shared = shared.lock().expect("discovery state poisoned");
    if sent {
        if is_query {
            shared.diagnostics.queries_sent += 1;
        } else {
            shared.diagnostics.announcements_sent += 1;
        }
    }
    let error = if sent {
        None
    } else {
        Some(format!(
            "Falha ao enviar descoberta: {}",
            last_error.map_or_else(
                || "nenhum destino disponível".to_owned(),
                |error| error.to_string()
            )
        ))
    };
    if shared.error != error {
        if let Some(error) = &error {
            tracing::warn!(error = %error, "Falha da descoberta; entrada manual continua disponível");
        } else {
            tracing::info!("Envio da descoberta restabelecido");
        }
        shared.error = error;
    }
}

#[cfg(windows)]
fn restrict_interface(receiver: &UdpSocket, sender: &UdpSocket, adapter: &str) -> io::Result<()> {
    use windows::Win32::NetworkManagement::IpHelper::{
        ConvertInterfaceAliasToLuid, ConvertInterfaceLuidToIndex,
    };
    use windows::Win32::NetworkManagement::Ndis::NET_LUID_LH;
    use windows::core::PCWSTR;

    let alias: Vec<u16> = adapter.encode_utf16().chain(Some(0)).collect();
    let mut luid = NET_LUID_LH::default();
    let mut index = 0;
    // Valid null-terminated alias and writable stack output values.
    unsafe {
        ConvertInterfaceAliasToLuid(PCWSTR(alias.as_ptr()), &mut luid)
            .ok()
            .map_err(io::Error::other)?;
        ConvertInterfaceLuidToIndex(&luid, &mut index)
            .ok()
            .map_err(io::Error::other)?;
    }
    restrict_socket_interfaces(receiver, sender, index)
}

#[cfg(windows)]
fn restrict_socket_interfaces(
    receiver: &UdpSocket,
    sender: &UdpSocket,
    index: u32,
) -> io::Result<()> {
    use std::os::windows::io::AsRawSocket;
    use windows::Win32::Networking::WinSock::{
        IP_ADD_IFLIST, IP_IFLIST, IP_UNICAST_IF, IPPROTO_IP, SOCKET, SOCKET_ERROR, WSAGetLastError,
        setsockopt,
    };
    let set = |socket: &UdpSocket, option, value: [u8; 4]| -> io::Result<()> {
        // Socket remains owned by UdpSocket; setsockopt copies the four-byte value.
        if unsafe {
            setsockopt(
                SOCKET(socket.as_raw_socket() as usize),
                IPPROTO_IP.0,
                option,
                Some(&value),
            )
        } == SOCKET_ERROR
        {
            return Err(io::Error::from_raw_os_error(unsafe { WSAGetLastError() }.0));
        }
        Ok(())
    };
    set(sender, IP_UNICAST_IF, index.to_be_bytes()).map_err(|error| {
        io::Error::new(error.kind(), format!("Seleção da rede de envio: {error}"))
    })?;
    // Windows requires enabling the initially empty list before adding interfaces.
    set(receiver, IP_IFLIST, 1_u32.to_ne_bytes()).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("Restrição da rede de recepção: {error}"),
        )
    })?;
    set(receiver, IP_ADD_IFLIST, index.to_ne_bytes()).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("Seleção da rede de recepção: {error}"),
        )
    })
}

#[cfg(not(windows))]
fn restrict_interface(_: &UdpSocket, _: &UdpSocket, _: &str) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "Descoberta disponível no Windows.",
    ))
}

#[cfg(test)]
#[path = "discovery_tests.rs"]
mod tests;
