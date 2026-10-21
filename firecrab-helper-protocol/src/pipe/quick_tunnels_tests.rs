use super::*;
use serde_json::json;

fn config() -> Config {
    serde_json::from_value(json!({
        "routes": [
            { "host": "app.example.com", "upstreams": ["172.30.1.2:80"] },
            { "host": "app.example.com", "pathPrefix": "/api", "upstreams": ["172.30.1.3:80"] },
            { "host": "other.example.com", "upstreams": ["172.30.1.4:80"] }
        ]
    }))
    .unwrap()
}

fn app(id: u128) -> QuickTunnelEntry {
    QuickTunnelEntry {
        id: Uuid::from_u128(id),
        name: format!("App {id}"),
        target: QuickTunnelTarget::Apps {
            upstreams: vec!["172.30.1.5:80".parse().unwrap()],
        },
    }
}

#[test]
fn identities_have_stable_distinct_private_hosts_and_systemd_units() {
    let id = Uuid::parse_str("12345678-1234-4234-8234-123456789012").unwrap();
    assert_eq!(
        quick_tunnel_host(id),
        "quick-12345678123442348234123456789012.firecrab.internal"
    );
    assert_eq!(
        quick_tunnel_unit(id),
        "firecrab-quick-tunnel-12345678123442348234123456789012.service"
    );
    assert_eq!(LEGACY_QUICK_TUNNEL_ID, Uuid::from_u128(1));
    assert_ne!(
        quick_tunnel_unit(id),
        quick_tunnel_unit(LEGACY_QUICK_TUNNEL_ID)
    );
}

#[test]
fn intents_round_trip_without_accepting_full_configuration_or_arbitrary_service_input() {
    let id = Uuid::from_u128(2);
    let requests = [
        json!({ "action": "save", "name": "Web", "target": { "type": "apps", "upstreams": ["172.30.1.5:80"] } }),
        json!({ "action": "save", "id": id, "name": "Routes", "target": { "type": "routes", "host": "app.example.com" } }),
        json!({ "action": "delete", "id": id }),
        json!({ "action": "control", "id": id, "serviceAction": "restart" }),
    ];
    for value in requests {
        let request: QuickTunnelRequest = serde_json::from_value(value).unwrap();
        request.validate().unwrap();
        assert_eq!(
            serde_json::from_value::<QuickTunnelRequest>(serde_json::to_value(&request).unwrap())
                .unwrap(),
            request
        );
    }
    for value in [
        json!({ "action": "delete", "id": id, "config": {} }),
        json!({ "action": "control", "id": id, "serviceAction": "reload" }),
        json!({ "action": "control", "id": id, "serviceAction": "stop", "unit": "sshd.service" }),
        json!({ "action": "save", "name": "Bad", "target": { "type": "apps", "upstreams": ["172.30.1.5:80"], "host": "example.com" } }),
        json!({ "action": "save", "name": "Bad", "target": { "type": "unknown" } }),
    ] {
        assert!(serde_json::from_value::<QuickTunnelRequest>(value).is_err());
    }
    assert!(
        serde_json::from_value::<QuickTunnelSaveRequest>(json!({
            "name": "Bad", "target": { "type": "routes", "host": "app.example.com" }, "config": {}
        }))
        .is_err()
    );
}

#[test]
fn names_ids_addresses_and_intent_size_are_bounded_before_privileged_calls() {
    for name in ["", "   ", "bad\nname", &"x".repeat(65)] {
        let request = QuickTunnelRequest::Save {
            id: None,
            name: name.into(),
            target: app(2).target,
        };
        assert!(request.validate().is_err(), "{name:?}");
    }
    let unicode = QuickTunnelRequest::Save {
        id: None,
        name: "웹".repeat(64),
        target: app(2).target,
    };
    unicode.validate().unwrap();
    for request in [
        QuickTunnelRequest::Delete { id: Uuid::nil() },
        QuickTunnelRequest::Control {
            id: Uuid::nil(),
            service_action: ServiceAction::Stop,
        },
        QuickTunnelRequest::Save {
            id: Some(Uuid::nil()),
            name: "Web".into(),
            target: app(2).target,
        },
    ] {
        assert!(request.validate().is_err());
    }
    for upstreams in [
        vec![],
        vec!["0.0.0.0:80"],
        vec!["224.0.0.1:80"],
        vec!["172.30.1.2:0"],
        vec!["172.30.1.2:80", "172.30.1.2:80"],
    ] {
        let request = QuickTunnelRequest::Save {
            id: None,
            name: "Web".into(),
            target: QuickTunnelTarget::Apps {
                upstreams: upstreams
                    .iter()
                    .map(|value| value.parse().unwrap())
                    .collect(),
            },
        };
        assert!(request.validate().is_err());
    }
    let oversized = QuickTunnelRequest::Save {
        id: None,
        name: "x".repeat(48 * 1024),
        target: app(2).target,
    };
    assert!(
        oversized
            .validate()
            .unwrap_err()
            .to_string()
            .contains("48 KiB")
    );
}

#[test]
fn route_targets_require_an_existing_non_reserved_literal_hostname() {
    for host in [
        "*.example.com",
        "App.example.com",
        "app.example.com:80",
        "quick.firecrab.internal",
        "quick-abc.firecrab.internal",
        "missing.example.com",
    ] {
        let mut config = config();
        config.quick_tunnels = vec![QuickTunnelEntry {
            id: Uuid::from_u128(2),
            name: "Routes".into(),
            target: QuickTunnelTarget::Routes { host: host.into() },
        }];
        assert!(config.validate().is_err(), "{host}");
    }
    let mut config = config();
    config.routes[0].host = quick_tunnel_host(Uuid::from_u128(2));
    assert!(
        config
            .validate()
            .unwrap_err()
            .to_string()
            .contains("reserved")
    );
}

#[test]
fn linked_host_can_lose_one_path_but_not_its_last_saved_route() {
    let mut config = config();
    config.quick_tunnels = vec![QuickTunnelEntry {
        id: Uuid::from_u128(2),
        name: "Public web".into(),
        target: QuickTunnelTarget::Routes {
            host: "app.example.com".into(),
        },
    }];
    config.validate().unwrap();
    config.routes.remove(0);
    config.validate().unwrap();
    config
        .routes
        .retain(|route| route.host != "app.example.com");
    let error = config.validate().unwrap_err().to_string();
    assert!(error.contains("edit or delete"));
    assert!(error.contains("Public web"));
    assert!(error.contains("app.example.com"));
}

#[test]
fn connector_ids_are_unique_and_total_count_includes_the_legacy_singleton() {
    let mut config = config();
    config.quick_tunnels = vec![app(2), app(2)];
    assert!(
        config
            .validate()
            .unwrap_err()
            .to_string()
            .contains("duplicate")
    );
    config.quick_tunnels = (2..18).map(app).collect();
    config.validate().unwrap();
    config.quick_tunnels.push(app(18));
    assert!(
        config
            .validate()
            .unwrap_err()
            .to_string()
            .contains("at most 16")
    );
    config.quick_tunnels.pop();
    config.quick_tunnel = Some(super::super::QuickTunnel {
        upstreams: vec!["172.30.1.2:80".parse().unwrap()],
    });
    assert!(
        config
            .validate()
            .unwrap_err()
            .to_string()
            .contains("at most 16")
    );
    config.quick_tunnels = vec![app(1)];
    assert!(
        config
            .validate()
            .unwrap_err()
            .to_string()
            .contains("duplicate")
    );
    config.quick_tunnel = None;
    config.validate().unwrap();
    config.tunnel = Some(super::super::Tunnel {
        id: Uuid::from_u128(9),
        credentials_file: "credentials.json".into(),
        hostname: None,
    });
    config.validate().unwrap();
}

#[test]
fn independent_apps_and_host_groups_keep_all_paths_and_restore_only_public_hosts() {
    let mut config = config();
    let saved_routes = config.routes.clone();
    let original = config.nginx();
    let old_generation = config.generation();
    config.quick_tunnels = vec![
        app(2),
        QuickTunnelEntry {
            id: Uuid::from_u128(3),
            name: "Public routes".into(),
            target: QuickTunnelTarget::Routes {
                host: "app.example.com".into(),
            },
        },
    ];
    config.validate().unwrap();
    let nginx = config.nginx();
    assert_eq!(config.routes, saved_routes);
    assert_ne!(config.generation(), old_generation);
    assert!(nginx.contains(&format!(
        "server_name {};",
        quick_tunnel_host(Uuid::from_u128(2))
    )));
    let group_host = format!("server_name {};", quick_tunnel_host(Uuid::from_u128(3)));
    let group = nginx.split(&group_host).nth(1).unwrap();
    assert!(group.contains("location / {"));
    assert!(group.contains("location = \"/api\""));
    assert!(group.contains("location ^~ \"/api/\""));
    assert!(group.contains("proxy_pass http://fc_route_4;"));
    assert!(group.contains("proxy_pass http://fc_route_5;"));
    assert_eq!(
        nginx.matches("proxy_set_header Host $host;").count(),
        original.matches("proxy_set_header Host $host;").count()
    );
    assert_eq!(
        nginx
            .matches("proxy_set_header Host $fc_quick_host;")
            .count(),
        4
    );
    let before_target_change = config.generation();
    config.quick_tunnels[0].target = QuickTunnelTarget::Apps {
        upstreams: vec!["172.30.1.6:80".parse().unwrap()],
    };
    assert_ne!(config.generation(), before_target_change);
}

#[test]
fn empty_lists_preserve_legacy_json_shape() {
    let config = config();
    assert!(
        !serde_json::to_value(config)
            .unwrap()
            .as_object()
            .unwrap()
            .contains_key("quickTunnels")
    );
    let snapshot: super::super::Snapshot = serde_json::from_value(
        json!({ "config":null,"units":[],"nginxAvailable":true,"cloudflaredAvailable":false }),
    )
    .unwrap();
    assert!(snapshot.quick_tunnels.is_empty());
    assert!(
        serde_json::to_value(snapshot)
            .unwrap()
            .get("quickTunnels")
            .is_none()
    );
}
