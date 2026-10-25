//! Deployment-owned domains share the Pipe transaction with manual route changes.

use super::{Config, Route, hostname, valid_route_hostname, validate_upstreams};
use crate::network::MicroNetworkSpec;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use uuid::Uuid;

/// Stable service and deployment identities prevent stale cleanup from deleting a newer route.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ServiceRouteOwner {
    /// Service configuration that requested the domain.
    pub service_id: Uuid,
    /// Deployment attempt responsible for publication and cleanup.
    pub deployment_id: Uuid,
}

/// One route mutation, applied to the latest manifest under the host Pipe lock.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum ServiceRouteRequest {
    /// Publish a private root route on the service's own MicroNetwork gateway.
    PublishInternal {
        /// Captured destination group; None retains network-level access.
        #[serde(default)]
        security_group: Option<crate::security_group::SecurityGroupPolicySpec>,
        /// Exact deployment owner, retained across lost responses and retries.
        owner: ServiceRouteOwner,
        /// Single DNS label used as `<name>.internal`.
        name: String,
        /// Listener address and permitted source subnet.
        network: MicroNetworkSpec,
        /// Healthy replicas in this network.
        upstreams: Vec<SocketAddr>,
    },
    /// Add an exclusive root route after every replica passed health checks.
    Publish {
        /// Exact owner; an existing route must have the same owner.
        owner: ServiceRouteOwner,
        /// Lowercase DNS hostname with no scheme or path.
        host: String,
        /// Healthy guest application addresses; no public host ports are required.
        upstreams: Vec<SocketAddr>,
    },
    /// Remove only this owner's route. Repeated or stale removal is harmless.
    Remove {
        /// Owner that must match the saved route.
        owner: ServiceRouteOwner,
        /// Hostname captured by the deployment.
        host: String,
    },
}

impl ServiceRouteRequest {
    /// Validate bounded identities, hostname and targets before privileged work.
    pub fn validate(&self) -> Result<(), String> {
        let (owner, host) = match self {
            Self::PublishInternal {
                security_group,
                owner,
                name,
                network,
                upstreams,
            } => {
                let route = Route {
                    security_group: security_group.clone(),
                    owner: Some(owner.clone()),
                    network: Some(network.clone()),
                    host: format!("{name}.internal"),
                    path_prefix: "/".into(),
                    upstreams: upstreams.clone(),
                };
                validate_internal_route(&route, network)?;
                return validate_owner(owner);
            }
            Self::Publish {
                owner,
                host,
                upstreams,
            } => {
                if upstreams.len() > 100 {
                    return Err("a service route supports at most 100 replicas".into());
                }
                validate_upstreams(upstreams, None, host).map_err(|error| error.to_string())?;
                (owner, host)
            }
            Self::Remove { owner, host } => (owner, host),
        };
        if !hostname(host) || (!valid_route_hostname(host) && !host.ends_with(".internal")) {
            return Err("provide a service, deployment and valid domain".into());
        }
        if matches!(self, Self::Publish { .. })
            && (!valid_route_hostname(host) || host.ends_with(".internal"))
        {
            return Err("public service domains cannot use .internal".into());
        }
        validate_owner(owner)
    }

    /// Return a validated next manifest without changing the caller's saved state.
    pub fn updated_config(&self, config: &Config) -> Result<Config, String> {
        self.validate()?;
        let mut next = config.clone();
        match self {
            Self::PublishInternal {
                security_group,
                owner,
                name,
                network,
                upstreams,
            } => {
                let host = format!("{name}.internal");
                if next
                    .routes
                    .iter()
                    .any(|route| route.host == host && route.owner.as_ref() != Some(owner))
                {
                    return Err("Internal address is already used by another service".into());
                }
                next.routes.retain(|route| {
                    !(route.owner.as_ref() == Some(owner) && route.network.is_some())
                });
                next.routes.push(Route {
                    security_group: security_group.clone(),
                    owner: Some(owner.clone()),
                    network: Some(network.clone()),
                    host,
                    path_prefix: "/".into(),
                    upstreams: upstreams.clone(),
                });
            }
            Self::Publish {
                owner,
                host,
                upstreams,
            } => {
                if next
                    .routes
                    .iter()
                    .any(|route| route.host == *host && route.owner.as_ref() != Some(owner))
                {
                    return Err("Domain is already used by another Pipe route or service".into());
                }
                next.routes.retain(|route| {
                    !(route.owner.as_ref() == Some(owner) && route.network.is_none())
                });
                next.routes.push(Route {
                    security_group: None,
                    network: None,
                    host: host.clone(),
                    path_prefix: "/".into(),
                    upstreams: upstreams.clone(),
                    owner: Some(owner.clone()),
                });
            }
            Self::Remove { owner, host } => {
                next.routes
                    .retain(|route| !(route.host == *host && route.owner.as_ref() == Some(owner)));
                if next == *config {
                    return Ok(next);
                }
                // The connector and its DNS stay configured. Only the onboarding label
                // is cleared when its final route is explicitly withdrawn by its owner.
                if let Some(tunnel) = &mut next.tunnel
                    && tunnel.hostname.as_ref() == Some(host)
                    && !next.routes.iter().any(|route| route.host == *host)
                {
                    tunnel.hostname = None;
                }
            }
        }
        next.validate().map_err(|error| error.to_string())?;
        Ok(next)
    }
}

impl Config {
    /// A service owns its whole hostname so manual paths cannot intercept its traffic.
    pub(super) fn validate_service_routes(&self) -> super::Result<()> {
        let mut services = std::collections::BTreeSet::new();
        for route in &self.routes {
            let Some(owner) = &route.owner else {
                continue;
            };
            if owner.service_id.is_nil()
                || owner.deployment_id.is_nil()
                || route.path_prefix != "/"
                || !services.insert((owner.service_id, route.network.is_some()))
                || self
                    .routes
                    .iter()
                    .filter(|other| other.host == route.host)
                    .count()
                    != 1
                || self.routes.iter().any(|other| {
                    other.owner.as_ref().is_some_and(|other_owner| {
                        other_owner.service_id == owner.service_id && other_owner != owner
                    })
                })
            {
                return Err(
                    "MicroService domains must have one exclusive deployment-owned root route"
                        .into(),
                );
            }
        }
        Ok(())
    }

    /// Preserve authoritative service routes when a dashboard or CLI applies a stale draft.
    /// Submitted ownership cannot create, retarget or remove a service's route.
    pub fn preserve_service_routes(&mut self, saved: &Config) -> super::Result<()> {
        if self.routes.iter().any(|route| {
            route.owner.is_some() && !saved.routes.iter().any(|old| old.owner == route.owner)
        }) {
            return Err("MicroService routes must be managed through their service".into());
        }
        self.routes.retain(|route| route.owner.is_none());
        self.routes.extend(
            saved
                .routes
                .iter()
                .filter(|route| route.owner.is_some())
                .cloned(),
        );
        self.validate()
    }
}

fn validate_owner(owner: &ServiceRouteOwner) -> Result<(), String> {
    if owner.service_id.is_nil() || owner.deployment_id.is_nil() {
        return Err("provide a service and deployment identity".into());
    }
    Ok(())
}

/// Keep private listeners, DNS names and targets confined to one IPv4 MicroNetwork.
pub(super) fn validate_internal_route(
    route: &Route,
    network: &MicroNetworkSpec,
) -> Result<(), String> {
    if let Some(group) = &route.security_group {
        group.validate()?;
        if group.micro_network_id != network.micro_network_id || network.ipv6.is_some() {
            return Err(
                "internal service security group must belong to its IPv4-only MicroNetwork".into(),
            );
        }
    }
    let name = route
        .host
        .strip_suffix(".internal")
        .ok_or("internal routes need a .internal hostname")?;
    if !hostname(name)
        || name.contains('.')
        || route.owner.is_none()
        || route.path_prefix != "/"
        || network.micro_network_id.is_nil()
        || !(8..=30).contains(&network.prefix)
        || !network.gateway.is_private()
        || network.gateway == network.network_address()
        || u32::from(network.gateway)
            == (u32::from(network.network_address()) | (u32::MAX >> network.prefix))
    {
        return Err(
            "internal routes need a service DNS label and a private MicroNetwork gateway".into(),
        );
    }
    if route.upstreams.len() > 100 || route.upstreams.iter().any(|address| {
        if route.upstreams.first().is_some_and(|first| first.port() != address.port()) {
            return true;
        }
        !matches!(address.ip(), std::net::IpAddr::V4(ip) if network.contains(ip) && ip != network.gateway
            && ip != network.network_address()
            && u32::from(ip) != (u32::from(network.network_address()) | (u32::MAX >> network.prefix)))
    }) {
        return Err("internal upstreams must be replicas in the service MicroNetwork".into());
    }
    validate_upstreams(&route.upstreams, None, &route.host).map_err(|error| error.to_string())
}

/// Intersect allowed ingress peers with the service's own MicroNetwork.
pub(super) fn allowed_sources(route: &Route, network: &MicroNetworkSpec) -> Vec<String> {
    use firecrab_api_types::{PortProtocol, SecurityGroupDirection};
    let Some(group) = &route.security_group else {
        return vec![network.subnet_cidr()];
    };
    let port = route.upstreams[0].port();
    group
        .rules
        .iter()
        .filter(|rule| {
            rule.direction == SecurityGroupDirection::Ingress
                && rule.protocol == PortProtocol::Tcp
                && (rule.port_from..=rule.port_to).contains(&port)
        })
        .filter_map(|rule| {
            let mask = u32::MAX
                .checked_shl(u32::from(32 - rule.peer_prefix))
                .unwrap_or(0);
            if rule.peer_prefix >= network.prefix && network.contains(rule.peer_address) {
                Some(format!("{}/{}", rule.peer_address, rule.peer_prefix))
            } else if rule.peer_prefix < network.prefix
                && (u32::from(network.gateway) & mask) == u32::from(rule.peer_address)
            {
                Some(network.subnet_cidr())
            } else {
                None
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn config() -> Config {
        serde_json::from_value(serde_json::json!({"routes":[{"host":"other.example.com","upstreams":["127.0.0.1:9000"]}]})).unwrap()
    }
    fn owner() -> ServiceRouteOwner {
        ServiceRouteOwner {
            service_id: Uuid::from_u128(1),
            deployment_id: Uuid::from_u128(2),
        }
    }
    fn publish() -> ServiceRouteRequest {
        ServiceRouteRequest::Publish {
            owner: owner(),
            host: "web.example.com".into(),
            upstreams: vec![
                "172.30.0.2:8080".parse().unwrap(),
                "172.30.0.3:8080".parse().unwrap(),
            ],
        }
    }
    fn internal() -> ServiceRouteRequest {
        serde_json::from_value(serde_json::json!({
            "action":"publish_internal", "owner":owner(), "name":"web",
            "network":{"micro_network_id":Uuid::from_u128(8),"gateway":"172.30.0.1","prefix":24},
            "upstreams":["172.30.0.2:8080","172.30.0.3:8080"]
        }))
        .unwrap()
    }

    #[test]
    fn preexisting_manual_internal_hostnames_keep_their_manifest_shape() {
        let mut saved = config();
        saved.routes[0].host = "manual.internal".into();
        saved.validate().unwrap();
        assert!(saved.internal_dns_hosts().is_empty());
        assert!(
            saved
                .nginx()
                .contains("listen 127.0.0.1:8080; server_name manual.internal;")
        );
    }

    #[test]
    fn internal_dns_and_routing_share_ownership_without_exposing_private_hosts() {
        let public = publish().updated_config(&config()).unwrap();
        let next = internal().updated_config(&public).unwrap();
        assert_eq!(next.routes.len(), 3);
        assert_eq!(next.internal_dns_hosts(), "172.30.0.1 web.internal\n");
        assert_eq!(publish().updated_config(&next).unwrap().routes.len(), 3);
        let nginx = next.nginx();
        assert!(nginx.contains("listen 172.30.0.1:80; server_name web.internal;"));
        assert!(!nginx.contains("listen 127.0.0.1:8080; server_name web.internal;"));
        assert!(nginx.contains("allow 172.30.0.0/24;\n    deny all;"));
        assert!(nginx.contains("listen 172.30.0.1:80 default_server; http2 on;"));
        let mut tunneled = next.clone();
        tunneled.tunnel = Some(super::super::Tunnel {
            id: Uuid::from_u128(4),
            credentials_file: "credentials.json".into(),
            hostname: None,
        });
        assert!(
            !tunneled
                .tunnel_config()
                .unwrap()
                .to_string()
                .contains("web.internal")
        );
        tunneled.quick_tunnels.push(serde_json::from_value(serde_json::json!({
            "id":Uuid::from_u128(5),"name":"private","target":{"type":"routes","host":"web.internal"}
        })).unwrap());
        assert!(tunneled.validate().is_err());
        let stale = ServiceRouteOwner {
            deployment_id: Uuid::from_u128(7),
            ..owner()
        };
        let remove = |owner| ServiceRouteRequest::Remove {
            owner,
            host: "web.internal".into(),
        };
        assert_eq!(remove(stale).updated_config(&next).unwrap(), next);
        assert_eq!(remove(owner()).updated_config(&next).unwrap(), public);
        let mut draft = config();
        draft.preserve_service_routes(&next).unwrap();
        assert_eq!(draft, next);
    }

    #[test]
    fn internal_routes_reject_unsafe_names_foreign_upstreams_and_forged_ownership() {
        for name in ["bad.name", "web;return", "-web", "", "WEB"] {
            let mut request = internal();
            if let ServiceRouteRequest::PublishInternal { name: label, .. } = &mut request {
                *label = name.into();
            }
            assert!(request.validate().is_err(), "{name}");
        }
        for target in [
            "127.0.0.1:5523",
            "172.31.0.2:80",
            "172.30.0.1:80",
            "172.30.0.0:80",
            "172.30.0.255:80",
            "[::1]:80",
        ] {
            let mut request = internal();
            if let ServiceRouteRequest::PublishInternal { upstreams, .. } = &mut request {
                *upstreams = vec![target.parse().unwrap()];
            }
            assert!(request.validate().is_err(), "{target}");
        }
        let next = internal().updated_config(&config()).unwrap();
        let mut forged = next.clone();
        forged.routes.last_mut().unwrap().owner = None;
        assert!(forged.validate().is_err());
        let mut forged = next.clone();
        assert!(forged.preserve_service_routes(&config()).is_err());
        let mut public = publish();
        if let ServiceRouteRequest::Publish { host, .. } = &mut public {
            *host = "web.internal".into();
        }
        assert!(public.validate().is_err());
    }

    #[test]
    fn internal_ingress_intersects_group_peers_and_app_ports_with_the_network() {
        let mut request = internal();
        let group = serde_json::from_value(serde_json::json!({
            "id":Uuid::from_u128(9),"micro_network_id":Uuid::from_u128(8),
            "rules":[
                {"direction":"ingress","protocol":"tcp","port_from":8080,"port_to":8080,"peer_address":"172.30.0.10","peer_prefix":32},
                {"direction":"ingress","protocol":"tcp","port_from":8080,"port_to":8080,"peer_address":"172.31.0.0","peer_prefix":24},
                {"direction":"ingress","protocol":"udp","port_from":8080,"port_to":8080,"peer_address":"172.30.0.0","peer_prefix":24},
                {"direction":"ingress","protocol":"tcp","port_from":80,"port_to":80,"peer_address":"172.30.0.0","peer_prefix":24}
            ]
        })).unwrap();
        if let ServiceRouteRequest::PublishInternal { security_group, .. } = &mut request {
            *security_group = Some(group);
        }
        let next = request.updated_config(&config()).unwrap();
        let route = next.routes.last().unwrap();
        assert_eq!(
            allowed_sources(route, route.network.as_ref().unwrap()),
            vec!["172.30.0.10/32"]
        );
        let mut empty = next.clone();
        empty
            .routes
            .last_mut()
            .unwrap()
            .security_group
            .as_mut()
            .unwrap()
            .rules
            .clear();
        let route = empty.routes.last().unwrap();
        assert!(allowed_sources(route, route.network.as_ref().unwrap()).is_empty());
        let mut wide = next.clone();
        let group = wide
            .routes
            .last_mut()
            .unwrap()
            .security_group
            .as_mut()
            .unwrap();
        group.rules.truncate(1);
        group.rules[0].peer_address = std::net::Ipv4Addr::UNSPECIFIED;
        group.rules[0].peer_prefix = 0;
        let route = wide.routes.last().unwrap();
        assert_eq!(
            allowed_sources(route, route.network.as_ref().unwrap()),
            vec!["172.30.0.0/24"]
        );
        wide.routes
            .last_mut()
            .unwrap()
            .security_group
            .as_mut()
            .unwrap()
            .micro_network_id = Uuid::from_u128(99);
        assert!(wide.validate().is_err());
    }
    #[test]
    fn publishes_all_replicas_and_removes_only_the_exact_owner() {
        let saved = config();
        let next = publish().updated_config(&saved).unwrap();
        assert_eq!(next.routes[0], saved.routes[0]);
        assert_eq!(next.routes[1].upstreams.len(), 2);
        assert_eq!(publish().updated_config(&next).unwrap(), next);
        let mut stale = owner();
        stale.deployment_id = Uuid::from_u128(3);
        let remove = |owner| ServiceRouteRequest::Remove {
            owner,
            host: "web.example.com".into(),
        };
        assert_eq!(remove(stale).updated_config(&next).unwrap(), next);
        assert_eq!(remove(owner()).updated_config(&next).unwrap(), saved);
        assert_eq!(remove(owner()).updated_config(&saved).unwrap(), saved);
    }
    #[test]
    fn conflicts_and_stale_manual_applies_do_not_overwrite_service_domains() {
        let next = publish().updated_config(&config()).unwrap();
        let mut manual = config();
        manual.routes[0].host = "web.example.com".into();
        assert!(publish().updated_config(&manual).is_err());
        let mut stale = config();
        stale.preserve_service_routes(&next).unwrap();
        assert_eq!(stale, next);
        manual.routes[0].path_prefix = "/api".into();
        assert!(manual.preserve_service_routes(&next).is_err());
        let mut forged = next.clone();
        assert!(forged.preserve_service_routes(&config()).is_err());
    }
    #[test]
    fn domain_withdrawal_keeps_named_connector_and_removes_its_onboarding_label() {
        let mut next = publish().updated_config(&config()).unwrap();
        next.tunnel = Some(super::super::Tunnel {
            id: Uuid::from_u128(9),
            credentials_file: "credentials.json".into(),
            hostname: Some("web.example.com".into()),
        });
        let removed = ServiceRouteRequest::Remove {
            owner: owner(),
            host: "web.example.com".into(),
        }
        .updated_config(&next)
        .unwrap();
        assert_eq!(removed.tunnel.as_ref().unwrap().id, Uuid::from_u128(9));
        assert!(removed.tunnel.unwrap().hostname.is_none());
    }
}
