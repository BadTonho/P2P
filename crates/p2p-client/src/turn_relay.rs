use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::str::FromStr;
use std::sync::mpsc as std_mpsc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;
use turn_server::config::{Auth, Config, Interface, LogLevel};
use turn_server::service::session::ports::PortRange;

const TURN_PORT: u16 = 3478;
const RELAY_PORT_START: u16 = 50_000;
const RELAY_PORT_END: u16 = 50_100;
const TURN_REALM: &str = "p2p-voz-e-tela";

#[derive(Clone, Deserialize, Serialize)]
pub struct TurnCredentials {
    pub url: String,
    pub username: String,
    pub credential: String,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct TurnRoomConfig {
    pub turn: Option<TurnCredentials>,
}

pub struct TurnRelayServer {
    shutdown: Option<oneshot::Sender<()>>,
    worker: Option<JoinHandle<()>>,
    failures: std_mpsc::Receiver<String>,
}

impl TurnRelayServer {
    pub fn start(external_ipv4: Ipv4Addr) -> Result<(Self, TurnCredentials), String> {
        let username = format!("room-{}", hex(&rand::random::<[u8; 16]>()));
        let credential = hex(&rand::random::<[u8; 32]>());
        let server = Self::start_with(
            external_ipv4,
            TURN_PORT,
            RELAY_PORT_START,
            RELAY_PORT_END,
            username.clone(),
            credential.clone(),
        )?;
        let credentials = TurnCredentials {
            url: format!("turn:{external_ipv4}:{TURN_PORT}?transport=udp"),
            username,
            credential,
        };
        Ok((server, credentials))
    }

    fn start_with(
        external_ipv4: Ipv4Addr,
        turn_port: u16,
        relay_start: u16,
        relay_end: u16,
        username: String,
        credential: String,
    ) -> Result<Self, String> {
        let listen = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, turn_port));
        let probe = UdpSocket::bind(listen).map_err(|error| {
            format!(
                "Não foi possível iniciar o TURN em UDP {turn_port}: a porta está ocupada ou indisponível ({error})."
            )
        })?;
        drop(probe);

        let config = build_config(
            external_ipv4,
            turn_port,
            relay_start,
            relay_end,
            username,
            credential,
        )?;
        let (shutdown, shutdown_rx) = oneshot::channel();
        let (failure_tx, failures) = std_mpsc::channel();
        let worker = thread::Builder::new()
            .name("p2p-turn-relay".to_owned())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        let _ = failure_tx.send(format!(
                            "Não foi possível iniciar o runtime do TURN: {error}"
                        ));
                        return;
                    }
                };

                runtime.block_on(async move {
                    tokio::select! {
                        result = turn_server::start_server(config) => {
                            let message = match result {
                                Ok(()) => "O servidor TURN encerrou inesperadamente.".to_owned(),
                                Err(error) => format!("O servidor TURN falhou: {error}"),
                            };
                            let _ = failure_tx.send(message);
                        }
                        _ = shutdown_rx => {}
                    }
                });
                // Dropping the runtime also stops the UDP receive tasks owned by turn-server.
            })
            .map_err(|error| format!("Não foi possível iniciar o serviço TURN: {error}"))?;

        let mut server = Self {
            shutdown: Some(shutdown),
            worker: Some(worker),
            failures,
        };
        let mut ready = false;
        for _ in 0..40 {
            if let Some(error) = server.try_failure() {
                return Err(error);
            }
            match UdpSocket::bind(listen) {
                Ok(socket) => drop(socket),
                Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => {
                    ready = true;
                    break;
                }
                Err(error) => {
                    return Err(format!(
                        "Não foi possível confirmar a porta UDP {turn_port} do TURN: {error}"
                    ));
                }
            }
            thread::sleep(Duration::from_millis(25));
        }
        if !ready {
            return Err(format!(
                "O servidor TURN não começou a escutar na porta UDP {turn_port} dentro do prazo."
            ));
        }

        tracing::info!(
            turn_port,
            relay_port_start = relay_start,
            relay_port_end = relay_end,
            "Servidor TURN integrado iniciado; credenciais omitidas"
        );
        Ok(server)
    }

    pub fn try_failure(&mut self) -> Option<String> {
        self.failures.try_recv().ok()
    }
}

impl Drop for TurnRelayServer {
    fn drop(&mut self) {
        tracing::info!("Encerrando servidor TURN integrado; credenciais omitidas");
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn build_config(
    external_ipv4: Ipv4Addr,
    turn_port: u16,
    relay_start: u16,
    relay_end: u16,
    username: String,
    credential: String,
) -> Result<Config, String> {
    if relay_start < 49_152 || relay_start > relay_end {
        return Err("A faixa de portas UDP do TURN precisa ficar entre 49152 e 65535.".to_owned());
    }
    let port_range = PortRange::from_str(&format!("{relay_start}..{relay_end}"))
        .map_err(|error| format!("Faixa de portas TURN inválida: {error}"))?;
    let external = SocketAddr::V4(SocketAddrV4::new(external_ipv4, turn_port));
    let mut config = Config::default();
    config.server.realm = TURN_REALM.to_owned();
    config.server.max_threads = 2;
    config.server.port_range = port_range;
    config.server.interfaces = vec![Interface::Udp {
        listen: SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, turn_port)),
        external,
        idle_timeout: 120,
        mtu: 1_200,
    }];
    config.auth = Auth {
        static_credentials: HashMap::from([(username, credential)]),
        static_auth_secret: None,
        enable_hooks_auth: false,
    };
    config.log.stdout = false;
    config.log.level = LogLevel::Warn;
    Ok(config)
}

fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

#[cfg(test)]
mod tests {
    use super::{build_config, hex};
    use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
    use turn_server::config::Interface;

    #[test]
    fn turn_configuration_uses_the_expected_udp_ports_and_credentials() {
        let config = build_config(
            Ipv4Addr::new(203, 0, 113, 9),
            3478,
            50_000,
            50_100,
            "temporary-user".to_owned(),
            "temporary-password".to_owned(),
        )
        .unwrap();

        assert_eq!(config.server.port_range.start(), 50_000);
        assert_eq!(config.server.port_range.end(), 50_100);
        assert_eq!(config.server.interfaces.len(), 1);
        assert!(matches!(
            config.server.interfaces[0],
            Interface::Udp { listen, external, .. }
                if listen.port() == 3478
                    && listen.ip().is_unspecified()
                    && external.to_string() == "203.0.113.9:3478"
        ));
        assert_eq!(
            config.auth.static_credentials.get("temporary-user"),
            Some(&"temporary-password".to_owned())
        );
    }

    #[test]
    fn hex_encoding_is_lowercase_and_preserves_leading_zeroes() {
        assert_eq!(hex(&[0, 0xab, 0xff]), "00abff");
    }

    #[test]
    fn integrated_turn_server_releases_its_listener_when_dropped() {
        let address = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0));
        let probe = UdpSocket::bind(address).unwrap();
        let port = probe.local_addr().unwrap().port();
        drop(probe);

        let server = super::TurnRelayServer::start_with(
            Ipv4Addr::LOCALHOST,
            port,
            50_000,
            50_100,
            "test-user".to_owned(),
            "test-password".to_owned(),
        )
        .unwrap();
        assert_eq!(
            UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, port))
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::AddrInUse
        );

        drop(server);
        UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, port)).unwrap();
    }

    #[test]
    fn integrated_turn_server_rejects_an_occupied_listener_port() {
        let listener = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let result = super::TurnRelayServer::start_with(
            Ipv4Addr::LOCALHOST,
            port,
            50_000,
            50_100,
            "test-user".to_owned(),
            "test-password".to_owned(),
        );
        let error = match result {
            Err(error) => error,
            Ok(server) => {
                drop(server);
                panic!("TURN iniciou mesmo com a porta UDP ocupada");
            }
        };
        assert!(error.contains(&format!("UDP {port}")));
    }
}
