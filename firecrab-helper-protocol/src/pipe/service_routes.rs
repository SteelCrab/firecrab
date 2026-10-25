//! Deployment-owned domains share the Pipe transaction with manual route changes.

use super::{Config, Route, valid_route_hostname, validate_upstreams};
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
    /// Replace a route only if its exact owner and upstreams still match.
    Replace {
        /// Owner observed before accepting the transition.
        expected_owner: ServiceRouteOwner,
        /// Exact upstream list observed before the transition.
        expected_upstreams: Vec<SocketAddr>,
        /// Owner of the new serving generation.
        owner: ServiceRouteOwner,
        /// Stable service hostname.
        host: String,
        /// Healthy targets for the new generation.
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
            Self::Publish {
                owner,
                host,
                upstreams,
            }
            | Self::Replace {
                owner,
                host,
                upstreams,
                ..
            } => {
                if upstreams.len() > 100 {
                    return Err("a service route supports at most 100 replicas".into());
                }
                validate_upstreams(upstreams, None, host).map_err(|error| error.to_string())?;
                (owner, host)
            }
            Self::Remove { owner, host } => (owner, host),
        };
        if owner.service_id.is_nil() || owner.deployment_id.is_nil() || !valid_route_hostname(host)
        {
            return Err("provide a service, deployment and valid domain".into());
        }
        if let Self::Replace {
            expected_owner,
            expected_upstreams,
            ..
        } = self
        {
            if expected_owner.service_id != owner.service_id
                || expected_owner.deployment_id.is_nil()
            {
                return Err("route transitions must retain the service owner".into());
            }
            validate_upstreams(expected_upstreams, None, host)
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    }

    /// Return a validated next manifest without changing the caller's saved state.
    pub fn updated_config(&self, config: &Config) -> Result<Config, String> {
        self.validate()?;
        let mut next = config.clone();
        match self {
            Self::Replace {
                expected_owner,
                expected_upstreams,
                owner,
                host,
                upstreams,
            } => {
                let route = next
                    .routes
                    .iter_mut()
                    .find(|route| route.host == *host)
                    .ok_or("service route is missing; inspect Deploy before retrying")?;
                // A lost acknowledgement may be retried, but an older transition
                // must never restore an earlier upstream list within the same deploy.
                if route.owner.as_ref() == Some(owner) && route.upstreams == *upstreams {
                    return Ok(next);
                }
                if route.owner.as_ref() != Some(expected_owner)
                    || route.upstreams != *expected_upstreams
                {
                    return Err("service route changed since Deploy accepted the transition".into());
                }
                route.owner = Some(owner.clone());
                route.upstreams = upstreams.clone();
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
                next.routes
                    .retain(|route| route.owner.as_ref() != Some(owner));
                next.routes.push(Route {
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
                || !services.insert(owner.service_id)
                || self
                    .routes
                    .iter()
                    .filter(|other| other.host == route.host)
                    .count()
                    != 1
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
    fn replacement_is_idempotent_but_rejects_stale_same_deployment_transitions() {
        let saved = publish().updated_config(&config()).unwrap();
        let mut next_owner = owner();
        next_owner.deployment_id = Uuid::from_u128(3);
        let replace = ServiceRouteRequest::Replace {
            expected_owner: owner(),
            expected_upstreams: saved.routes[1].upstreams.clone(),
            owner: next_owner.clone(),
            host: "web.example.com".into(),
            upstreams: vec!["172.30.0.4:8080".parse().unwrap()],
        };
        let next = replace.updated_config(&saved).unwrap();
        assert_eq!(replace.updated_config(&next).unwrap(), next);
        assert_eq!(next.routes[0], saved.routes[0]);
        let second = ServiceRouteRequest::Replace {
            expected_owner: next_owner.clone(),
            expected_upstreams: next.routes[1].upstreams.clone(),
            owner: next_owner,
            host: "web.example.com".into(),
            upstreams: vec!["172.30.0.5:8080".parse().unwrap()],
        };
        let latest = second.updated_config(&next).unwrap();
        assert!(replace.updated_config(&latest).is_err());
        let stale_remove = ServiceRouteRequest::Remove {
            owner: owner(),
            host: "web.example.com".into(),
        };
        assert_eq!(stale_remove.updated_config(&latest).unwrap(), latest);
    }

    #[test]
    fn replacement_cannot_take_another_services_or_a_manual_route() {
        let saved = publish().updated_config(&config()).unwrap();
        let mut foreign = owner();
        foreign.service_id = Uuid::from_u128(99);
        let replace = ServiceRouteRequest::Replace {
            expected_owner: owner(),
            expected_upstreams: saved.routes[1].upstreams.clone(),
            owner: foreign,
            host: "web.example.com".into(),
            upstreams: vec!["172.30.0.4:8080".parse().unwrap()],
        };
        assert!(replace.updated_config(&saved).is_err());
        let mut manual = saved;
        manual.routes[1].owner = None;
        assert!(replace.updated_config(&manual).is_err());
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
