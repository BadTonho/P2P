use super::*;
use crate::discovery::DiscoveryNetwork;

impl ClientUi {
    pub(super) fn discovery_diagnostic_summary(&self) -> String {
        let mode = match self.network_selection {
            NetworkSelection::Automatic => "Automática",
            NetworkSelection::Manual => "Manual",
        };
        let state = match &self.discovery {
            Some(service) => format!(
                "interface={}, UDP 8999, salas={}, contadores={:?}",
                service.network.ipv4,
                self.discovered_rooms.len(),
                service.diagnostics()
            ),
            None => "serviço inativo".to_owned(),
        };
        format!(
            "rede={mode}, anúncio habilitado={}, {state}, falha={}",
            self.announce_local_room,
            self.discovery_error.as_deref().unwrap_or("nenhuma")
        )
    }

    fn local_room_advertisement(&self) -> Option<RoomAdvertisement> {
        if !self.announce_local_room
            || !self.hosting_locally
            || self.signaling.is_none()
            || self.room_mode != RoomMode::Local
            || (!self.current_leader_id.is_empty() && self.current_leader_id != self.participant_id)
        {
            return None;
        }
        let name = self.profile_display_name.trim();
        let advertisement = RoomAdvertisement {
            code: self.room_code.clone()?,
            host_name: if name.is_empty() {
                "Anfitrião".to_owned()
            } else {
                name.chars().take(64).collect()
            },
            participants: self.participants.len().clamp(1, 8) as u8,
        };
        advertisement.valid().then_some(advertisement)
    }

    fn discovery_network(&self) -> Option<DiscoveryNetwork> {
        let mode = if self.network_is_locked() {
            self.room_mode
        } else {
            self.create_room_mode
        };
        if mode != RoomMode::Local {
            return None;
        }
        self.host_addresses
            .get(self.selected_host_address)
            .map(|address| DiscoveryNetwork {
                adapter: address.adapter.clone(),
                ipv4: address.ipv4,
                prefix_len: address.prefix_len,
            })
    }

    pub(super) fn refresh_discovery(&mut self, context: &egui::Context) {
        let now = Instant::now();
        if !self.network_is_locked()
            && self
                .last_adapter_refresh
                .is_none_or(|last| now.duration_since(last) >= Duration::from_secs(5))
        {
            self.refresh_host_addresses();
        }
        let network = self.discovery_network();
        if self
            .discovery
            .as_ref()
            .is_some_and(|service| Some(&service.network) != network.as_ref())
        {
            self.discovery = None;
            self.discovered_rooms.clear();
            self.discovery_attempted_at = None;
        }
        if let Some(network) = network {
            context.request_repaint_after(Duration::from_millis(250));
            if self.discovery.is_none()
                && self
                    .discovery_attempted_at
                    .is_none_or(|last| now.duration_since(last) >= Duration::from_secs(5))
            {
                self.discovery_attempted_at = Some(now);
                match DiscoveryService::start(network) {
                    Ok(service) => {
                        self.discovery = Some(service);
                        self.discovery_error = None;
                    }
                    Err(error) => {
                        if self.discovery_error.as_ref() != Some(&error) {
                            tracing::warn!(error = %error, "Não foi possível iniciar descoberta; entrada manual continua disponível");
                        }
                        self.discovery_error = Some(error);
                    }
                }
            }
        } else {
            // Retain the actionable manual-search error until an adapter appears.
            if self.manual_room_search_at.is_none() {
                self.discovery_error = None;
            }
            self.discovered_rooms.clear();
            // Allows detecting Radmin or Ethernet/Wi-Fi becoming active while idle.
            context.request_repaint_after(Duration::from_secs(2));
        }
        if let Some(service) = &self.discovery {
            service.advertise(self.local_room_advertisement());
            let (rooms, error) = service.snapshot();
            self.discovered_rooms = rooms;
            self.discovery_error = error;
        }
    }

    pub(super) fn room_search_in_progress(&self) -> bool {
        let now = Instant::now();
        self.manual_room_search_at
            .is_some_and(|start| now.duration_since(start) < Duration::from_secs(2))
            || (self.discovery.is_some()
                && self
                    .discovery_attempted_at
                    .is_some_and(|start| now.duration_since(start) < Duration::from_secs(2)))
    }

    pub(super) fn can_search_rooms(&self) -> bool {
        self.create_room_mode == RoomMode::Local
            && !self.network_is_locked()
            && !self.update_blocks_room_actions()
            && !self.room_search_in_progress()
    }

    pub(super) fn request_room_search(&mut self, context: &egui::Context) {
        if !self.can_search_rooms() {
            return;
        }
        self.manual_room_search_at = Some(Instant::now());
        self.refresh_host_addresses();
        let network = self.discovery_network();
        self.start_room_search(context, network, DiscoveryService::start);
    }

    // The transport factory makes recovery testable without physical adapters.
    fn start_room_search(
        &mut self,
        context: &egui::Context,
        network: Option<DiscoveryNetwork>,
        start: impl FnOnce(DiscoveryNetwork) -> Result<DiscoveryService, String>,
    ) {
        context.request_repaint_after(Duration::from_millis(100));
        let Some(network) = network else {
            self.discovery = None;
            self.discovered_rooms.clear();
            let error = self.host_addresses_error.clone().unwrap_or_else(|| {
                "Nenhum adaptador IPv4 disponível para buscar salas nesta rede.".to_owned()
            });
            tracing::warn!(operation = "manual_refresh", cause = %error, "Busca de salas indisponível; entrada manual continua disponível");
            self.discovery_error = Some(error);
            return;
        };
        if self
            .discovery
            .as_ref()
            .is_some_and(|service| service.network != network || !service.is_running())
        {
            self.discovery = None;
            self.discovery_attempted_at = None;
            self.discovered_rooms.clear();
        }
        if self.discovery.is_none() {
            self.discovery_attempted_at = Some(Instant::now());
            match start(network.clone()) {
                Ok(service) => {
                    self.discovery = Some(service);
                    self.discovery_error = None;
                }
                Err(error) => {
                    tracing::warn!(operation = "manual_refresh", ipv4 = %network.ipv4, cause = %error, "Não foi possível reiniciar descoberta; entrada manual continua disponível");
                    self.discovery_error = Some(error);
                    return;
                }
            }
        }
        if let Some(service) = &self.discovery {
            service.request_refresh();
            let (rooms, error) = service.snapshot();
            self.discovered_rooms = rooms;
            self.discovery_error = error;
        }
    }

    pub(super) fn join_discovered_room(&mut self, room: DiscoveredRoom) {
        // The code goes directly to signaling, never through set_join_code/preferences.
        if self.discovered_rooms.contains(&room) {
            tracing::info!(ipv4 = %room.ipv4, signaling_port = 9000, "Entrada escolhida pela lista de salas descobertas; código omitido");
            self.start_join_at(room.endpoint(), room.advertisement.code);
        } else {
            self.connection_error = Some(
                "Esta sala não está mais disponível. Aguarde a atualização da lista.".to_owned(),
            );
        }
    }
}

#[cfg(test)]
#[path = "discovery_tests.rs"]
mod tests;
