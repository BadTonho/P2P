use std::net::{IpAddr, Ipv4Addr, ToSocketAddrs};

use eframe::egui;

use super::*;
use crate::settings::{MAX_SAVED_HOSTS, SavedHostProfile};

#[derive(Clone)]
pub(crate) struct HostAddress {
    pub(crate) adapter: String,
    pub(crate) ipv4: Ipv4Addr,
}

impl ClientUi {
    pub(crate) fn join_address(&self) -> &str {
        self.selected_host_index
            .and_then(|index| self.saved_hosts.get(index))
            .map(|profile| profile.address.as_str())
            .unwrap_or(&self.server_url)
    }

    pub(crate) fn refresh_host_addresses(&mut self) {
        self.addresses_loaded = true;
        match enumerate_host_addresses() {
            Ok(addresses) => {
                tracing::info!(
                    adapter_count = addresses.len(),
                    "Lista de adaptadores IPv4 atualizada"
                );
                for address in &addresses {
                    tracing::info!(adapter = %address.adapter, ipv4 = %address.ipv4, "Adaptador IPv4 disponível");
                }
                self.host_addresses = addresses;
                self.host_addresses_error = None;
                if !self.host_addresses.is_empty() {
                    self.selected_host_address = self
                        .preferred_control_ipv4
                        .and_then(|preferred| {
                            self.host_addresses
                                .iter()
                                .position(|address| address.ipv4 == preferred)
                        })
                        .unwrap_or(0);
                    let selected_ipv4 = self.host_addresses[self.selected_host_address].ipv4;
                    if self.preferred_control_ipv4 != Some(selected_ipv4) {
                        self.preferred_control_ipv4 = Some(selected_ipv4);
                        tracing::info!(ipv4 = %selected_ipv4, "Adaptador de controle padrão selecionado porque o IPv4 salvo não está ativo");
                    }
                }
            }
            Err(error) => {
                tracing::error!(error = %error, "Falha ao enumerar adaptadores IPv4");
                self.host_addresses.clear();
                self.host_addresses_error = Some(error);
            }
        }
    }

    pub(crate) fn show_control_address_picker(&mut self, ui: &mut egui::Ui) {
        if !self.host_addresses.is_empty() {
            let previous_selection = self.selected_host_address;
            self.selected_host_address = self
                .selected_host_address
                .min(self.host_addresses.len().saturating_sub(1));
            let selected = &self.host_addresses[self.selected_host_address];
            egui::ComboBox::from_id_salt("control-ip-address")
                .selected_text(format!("{} — {}", selected.adapter, selected.ipv4))
                .show_ui(ui, |ui| {
                    for (index, address) in self.host_addresses.iter().enumerate() {
                        ui.selectable_value(
                            &mut self.selected_host_address,
                            index,
                            format!("{} — {}", address.adapter, address.ipv4),
                        );
                    }
                });
            if previous_selection != self.selected_host_address {
                let selected = &self.host_addresses[self.selected_host_address];
                self.preferred_control_ipv4 = Some(selected.ipv4);
                tracing::info!(adapter = %selected.adapter, ipv4 = %selected.ipv4, "Adaptador escolhido para a malha de controle");
            }
            ui.monospace(format!(
                "ws://{}:9001",
                self.host_addresses[self.selected_host_address].ipv4
            ));
            ui.small("Este IPv4 também será usado para vincular o vídeo WebRTC nas portas UDP 9002–9009.");
        } else if let Some(error) = &self.host_addresses_error {
            Self::show_notice(ui, "Erro de adaptador:", error);
        } else {
            ui.label("Nenhum IPv4 ativo disponível para a conexão direta de controle.");
        }
        if ui.button("Atualizar adaptadores de controle").clicked() {
            self.refresh_host_addresses();
        }
    }

    pub(crate) fn show_host_address_picker(&mut self, ui: &mut egui::Ui) {
        if !self.host_addresses.is_empty() {
            let previous_selection = self.selected_host_address;
            self.selected_host_address = self
                .selected_host_address
                .min(self.host_addresses.len().saturating_sub(1));
            let selected = &self.host_addresses[self.selected_host_address];
            egui::ComboBox::from_id_salt("host-ip-address")
                .selected_text(format!("{} — {}", selected.adapter, selected.ipv4))
                .show_ui(ui, |ui| {
                    for (index, address) in self.host_addresses.iter().enumerate() {
                        ui.selectable_value(
                            &mut self.selected_host_address,
                            index,
                            format!("{} — {}", address.adapter, address.ipv4),
                        );
                    }
                });
            if previous_selection != self.selected_host_address {
                let selected = &self.host_addresses[self.selected_host_address];
                self.preferred_control_ipv4 = Some(selected.ipv4);
                tracing::info!(adapter = %selected.adapter, ipv4 = %selected.ipv4, "Adaptador escolhido para anunciar a sala");
            }
            let url = format!(
                "ws://{}:9000",
                self.host_addresses[self.selected_host_address].ipv4
            );
            ui.horizontal(|ui| {
                ui.monospace(&url);
                if ui.button("Copiar endereço").clicked() {
                    ui.ctx().copy_text(url.clone());
                }
            });
        } else if let Some(error) = &self.host_addresses_error {
            Self::show_notice(ui, "Erro de adaptador:", error);
        } else {
            ui.label("Nenhum IPv4 ativo foi encontrado. O servidor ainda pode funcionar em redes acessíveis por outro endereço.");
        }
        if ui.button("Atualizar adaptadores").clicked() {
            self.refresh_host_addresses();
        }
    }

    pub(crate) fn selected_signaling_address(&self) -> Option<String> {
        self.host_addresses
            .get(self.selected_host_address)
            .map(|address| format!("ws://{}:9000", address.ipv4))
    }

    pub(crate) fn selected_media_ipv4(&self) -> Result<Ipv4Addr, String> {
        self.host_addresses
            .get(self.selected_host_address)
            .map(|address| address.ipv4)
            .ok_or_else(|| {
                "Nenhum adaptador IPv4 está selecionado. Atualize os adaptadores em Configurações > Conexão antes de compartilhar a tela.".to_owned()
            })
    }
}

pub(crate) fn enumerate_host_addresses() -> Result<Vec<HostAddress>, String> {
    #[cfg(target_os = "windows")]
    {
        let adapters = ipconfig::get_adapters()
            .map_err(|error| format!("Não foi possível listar adaptadores de rede: {error}"))?;
        let mut addresses = Vec::new();
        for adapter in adapters {
            if adapter.oper_status() != ipconfig::OperStatus::IfOperStatusUp {
                continue;
            }
            for address in adapter.ip_addresses() {
                let IpAddr::V4(ipv4) = address else {
                    continue;
                };
                if ipv4.is_unspecified() || ipv4.is_loopback() || ipv4.is_link_local() {
                    continue;
                }
                addresses.push(HostAddress {
                    adapter: adapter.friendly_name().to_owned(),
                    ipv4: *ipv4,
                });
            }
        }
        addresses.sort_by(|left, right| {
            left.adapter
                .cmp(&right.adapter)
                .then_with(|| left.ipv4.octets().cmp(&right.ipv4.octets()))
        });
        addresses.dedup_by(|left, right| left.ipv4 == right.ipv4);
        Ok(addresses)
    }
    #[cfg(not(target_os = "windows"))]
    {
        Ok(Vec::new())
    }
}

pub(crate) fn signaling_ws_url(input: &str) -> Result<String, String> {
    let value = input.trim();
    if value.is_empty() {
        return Err("O endereço do anfitrião está vazio.".to_owned());
    }
    let address = if let Some(address) = value.strip_prefix("ws://") {
        address
    } else if value.contains("://") {
        return Err("Use ws://; wss:// ainda não está disponível nesta etapa.".to_owned());
    } else {
        value
    };

    let address = address.trim_end_matches('/');
    if address.contains('/') || address.contains('?') || address.contains('#') {
        return Err("Informe somente o IPv4 ou nome do anfitrião, sem caminho.".to_owned());
    }

    let host = match address.rsplit_once(':') {
        Some((host, port)) if !host.contains(':') => {
            if port != "9000" {
                return Err("A porta do servidor é fixa em 9000.".to_owned());
            }
            host
        }
        Some(_) => {
            return Err(
                "IPv6 não está disponível nesta etapa; informe um IPv4 ou nome DDNS.".to_owned(),
            );
        }
        None => address,
    };

    if host.parse::<Ipv4Addr>().is_err() {
        if host.len() > 253 || host.is_empty() || !host.is_ascii() {
            return Err("Informe um IPv4 ou nome DDNS válido.".to_owned());
        }
        let valid_name = host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        });
        if !valid_name {
            return Err("Informe um IPv4 ou nome DDNS válido.".to_owned());
        }
    }

    Ok(format!("ws://{host}:9000"))
}

pub(crate) fn validate_saved_host_profile(profile: &SavedHostProfile) -> Result<(), String> {
    validate_host_name(&profile.name)?;
    let address = profile.address.trim();
    if address.len() > 512 {
        return Err("O endereço excede 512 caracteres.".to_owned());
    }
    signaling_ws_url(address).map(|_| ())
}

pub(crate) fn validate_new_host_profile(
    name: &str,
    address: &str,
    current_count: usize,
) -> Result<(), String> {
    if current_count >= MAX_SAVED_HOSTS {
        return Err(format!(
            "A lista já atingiu o limite de {MAX_SAVED_HOSTS} anfitriões."
        ));
    }
    validate_host_name(name)?;
    validate_saved_host_profile(&SavedHostProfile {
        name: name.trim().to_owned(),
        address: address.trim().to_owned(),
    })
}

pub(crate) fn validate_host_name(name: &str) -> Result<(), String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("Informe um apelido para este anfitrião.".to_owned());
    }
    if name.chars().count() > 64 {
        return Err("O apelido pode ter no máximo 64 caracteres.".to_owned());
    }
    Ok(())
}

pub(crate) fn resolve_public_ipv4(input: &str) -> Result<Ipv4Addr, String> {
    let endpoint = signaling_ws_url(input)?;
    let authority = endpoint
        .strip_prefix("ws://")
        .and_then(|value| value.strip_suffix(":9000"))
        .ok_or_else(|| {
            "Informe o IPv4 público ou nome DDNS usado para acessar o anfitrião.".to_owned()
        })?;

    let addresses = (authority, 3478)
        .to_socket_addrs()
        .map_err(|error| format!("Não foi possível resolver o endereço do anfitrião: {error}"))?;
    let address = addresses
        .filter_map(|address| match address.ip() {
            IpAddr::V4(ipv4) if is_public_ipv4_candidate(ipv4) => Some(ipv4),
            _ => None,
        })
        .next()
        .ok_or_else(|| {
            "O endereço não resolveu para um IPv4 público. TURN precisa de um endereço público alcançável; CGNAT não permite essa conexão de entrada.".to_owned()
        })?;
    Ok(address)
}

pub(crate) fn is_public_ipv4_candidate(address: Ipv4Addr) -> bool {
    let octets = address.octets();
    let is_shared_address_space = octets[0] == 100 && (64..=127).contains(&octets[1]);
    let is_documentation_range = matches!(
        octets,
        [192, 0, 2, _] | [198, 51, 100, _] | [203, 0, 113, _]
    );
    !address.is_private()
        && !address.is_loopback()
        && !address.is_link_local()
        && !address.is_unspecified()
        && !address.is_multicast()
        && !address.is_broadcast()
        && !is_shared_address_space
        && !is_documentation_range
}

pub(crate) fn signaling_address_for_control(control_address: &str) -> String {
    control_address
        .strip_suffix(":9001")
        .map(|ip| format!("ws://{ip}:9000"))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signaling_address_accepts_ipv4_or_ddns_with_fixed_port() {
        assert_eq!(
            signaling_ws_url("203.0.113.10").unwrap(),
            "ws://203.0.113.10:9000"
        );
        assert_eq!(
            signaling_ws_url("ws://my-room.ddns.net:9000").unwrap(),
            "ws://my-room.ddns.net:9000"
        );
    }

    #[test]
    fn signaling_address_rejects_tls_ipv6_wrong_port_and_paths() {
        assert!(signaling_ws_url("wss://my-room.ddns.net").is_err());
        assert!(signaling_ws_url("2001:db8::1").is_err());
        assert!(signaling_ws_url("192.0.2.10:9001").is_err());
        assert!(signaling_ws_url("192.0.2.10/room").is_err());
    }

    #[test]
    fn saved_host_profiles_validate_name_address_and_limit() {
        assert!(validate_new_host_profile("Friend", "192.168.1.20", 0).is_ok());
        assert!(validate_new_host_profile("  ", "192.168.1.20", 0).is_err());
        assert!(validate_new_host_profile("Friend", "not an address", 0).is_err());
        assert!(validate_new_host_profile("Friend", "192.168.1.20", MAX_SAVED_HOSTS).is_err());
        assert!(validate_new_host_profile("Friend", "192.168.1.20", MAX_SAVED_HOSTS - 1).is_ok());
        assert!(
            validate_saved_host_profile(&SavedHostProfile {
                name: "Friend".to_owned(),
                address: "ws://friend.example.net:9000".to_owned(),
            })
            .is_ok()
        );
    }

    #[test]
    fn selected_saved_host_is_used_for_join_without_changing_own_invite_address() {
        let mut ui = super::ClientUi::default();
        ui.server_url = "manual.example.net".to_owned();
        ui.public_server_url = "my-room.example.net".to_owned();
        ui.saved_hosts = vec![SavedHostProfile {
            name: "Friend".to_owned(),
            address: "friend.example.net".to_owned(),
        }];
        ui.selected_host_index = Some(0);

        assert_eq!(ui.join_address(), "friend.example.net");
        assert_eq!(ui.public_server_url, "my-room.example.net");
        ui.selected_host_index = None;
        assert_eq!(ui.join_address(), "manual.example.net");
    }

    #[test]
    fn turn_host_address_requires_a_public_ipv4_candidate() {
        assert!(is_public_ipv4_candidate(Ipv4Addr::new(8, 8, 8, 8)));
        assert!(!is_public_ipv4_candidate(Ipv4Addr::new(192, 168, 1, 5)));
        assert!(!is_public_ipv4_candidate(Ipv4Addr::new(100, 80, 2, 3)));
        assert!(!is_public_ipv4_candidate(Ipv4Addr::new(203, 0, 113, 5)));
    }
}
