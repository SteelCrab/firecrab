use firecrab_stack::{MAX_MANIFEST_BYTES, ResourceCounts, parse};

const EXAMPLE: &str = include_str!("../../examples/stacks/basic.yaml");

fn paths(source: &str) -> Vec<String> {
    parse(source)
        .err()
        .expect("invalid manifest")
        .into_iter()
        .map(|error| error.path)
        .collect()
}

#[test]
fn example_reuses_service_defaults_and_resolves_network_references() {
    let stack = parse(EXAMPLE).expect("valid example");
    assert_eq!(stack.name, "demo");
    assert_eq!(
        stack.counts(),
        ResourceCounts {
            micro_networks: 1,
            micro_vms: 1,
            micro_services: 1
        }
    );
    assert_eq!(stack.micro_services["web"].config.startup_timeout, 300);
    assert_eq!(stack.micro_services["web"].config.drain_timeout, 30);
    assert!(stack.micro_networks["app"].internet_enabled);
}

#[test]
fn rejects_schema_mistakes_instead_of_silently_dropping_them() {
    for invalid in [
        EXAMPLE.replace("v1alpha1", "v2"),
        EXAMPLE.replace("kind: Stack", "kind: Deployment"),
        EXAMPLE.replace("cpu: 1", "cpus: 1"),
        EXAMPLE.replace("healthPath: /health", "healthPaths: /health"),
        EXAMPLE.replace("subnetCidr:", "subnetCIDR:"),
        EXAMPLE.replace("internetEnabled: true", "internetEnabled: yes"),
        EXAMPLE.replace("name: demo\n", "name: &name demo\n"),
        EXAMPLE.replace("name: demo\n", "name: *missing\n"),
        EXAMPLE.replace("name: demo\n", "name: demo\n<<: {name: other}\n"),
        EXAMPLE.replace("name: demo\n", "name: demo\nname: other\n"),
        EXAMPLE.replace("    env:", "    env:\n      APP_MODE: first"),
        format!("{EXAMPLE}\n---\n{EXAMPLE}"),
        "".to_owned(),
        "[]".to_owned(),
    ] {
        assert!(parse(&invalid).is_err());
    }
}

#[test]
fn rejects_duplicate_resource_keys() {
    let duplicate = EXAMPLE.replace(
        "microVMs:",
        "  app:\n    name: duplicate\n    subnetCidr: 172.31.21.0/24\n\nmicroVMs:",
    );
    assert!(parse(&duplicate).is_err());
}

#[test]
fn collects_independent_resource_errors_in_stable_order() {
    let invalid = EXAMPLE
        .replace("cpu: 1", "cpu: 0")
        .replace("ram: 256", "ram: 255")
        .replace("network: app", "network: missing");
    let first = paths(&invalid);
    assert_eq!(first, paths(&invalid));
    for path in [
        "microVMs.worker.cpu",
        "microVMs.worker.ram",
        "microVMs.worker.network",
        "microServices.web.config.cpu",
        "microServices.web.network",
    ] {
        assert!(first.iter().any(|item| item == path), "{path}");
    }
}

#[test]
fn rejects_invalid_resource_identity_without_echoing_it() {
    let invalid = EXAMPLE.replace("worker:", "\"private-token-DO-NOT-PRINT\\n\":");
    let errors = parse(&invalid).err().expect("invalid key");
    let output = serde_json::to_string(&errors).unwrap();
    assert!(output.contains("resource keys"));
    assert!(!output.contains("DO-NOT-PRINT"));
    assert!(parse(&EXAMPLE.replace("name: demo\n", "name: Uppercase\n")).is_err());
}

#[test]
fn rejects_empty_stacks_and_resource_limits() {
    let header = "apiVersion: firecrab.dev/v1alpha1\nkind: Stack\nname: demo\n";
    assert!(parse(header).is_err());
    let mut large = format!("{header}microNetworks:\n");
    for i in 0..257 {
        large.push_str(&format!(
            "  net-{i}:\n    name: net-{i}\n    subnetCidr: 172.31.20.0/24\n"
        ));
    }
    let errors = parse(&large).err().expect("too many resources");
    assert_eq!(
        errors[0].message,
        "must declare between 1 and 256 resources"
    );
    let errors = parse(&" ".repeat(MAX_MANIFEST_BYTES + 1))
        .err()
        .expect("too large");
    assert_eq!(errors[0].message, "manifest exceeds 1 MiB");
}

#[test]
fn checks_ipv4_subnets_and_overlap_using_normalized_ranges() {
    for cidr in [
        "not-a-subnet",
        "172.31.20.0/15",
        "172.31.20.0/29",
        "fd00::/64",
    ] {
        assert!(
            paths(&EXAMPLE.replace("172.31.20.0/24", cidr))
                .contains(&"microNetworks.app.subnetCidr".to_owned())
        );
    }
    let overlapping = EXAMPLE.replace(
        "microVMs:",
        "  other:\n    name: other\n    subnetCidr: 172.31.20.16/28\n\nmicroVMs:",
    );
    assert!(paths(&overlapping).contains(&"microNetworks.other.subnetCidr".to_owned()));
    assert!(parse(&overlapping.replace("172.31.20.16/28", "172.31.21.16/28")).is_ok());
}

#[test]
fn checks_duplicate_names_ports_and_replica_port_overflow() {
    let mut stack = parse(EXAMPLE).unwrap();
    let web = stack.micro_services["web"].clone();
    stack.micro_services.insert("other".to_owned(), web);
    let errors = firecrab_stack::validate(&stack).unwrap_err();
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("duplicates another"))
    );
    assert!(errors.iter().any(|e| e.message.contains("ports overlap")));
    assert!(
        paths(&EXAMPLE.replace("hostPort: 30100", "hostPort: 65535"))
            .contains(&"microServices.web.hostPort".to_owned())
    );
    assert!(parse(&EXAMPLE.replace("hostPort: 30100", "hostPort: 65534")).is_ok());
}

#[test]
fn checks_image_pins_health_paths_and_current_deployment_limits() {
    for image in ["nginx:latest", "nginx@sha256:123", "https://x@sha256:abc"] {
        let source = EXAMPLE.replace("ghcr.io/example/web@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef", image);
        assert!(paths(&source).contains(&"microServices.web.config.image".to_owned()));
    }
    for path in ["//host", "/health?query", "/health#fragment"] {
        assert!(
            paths(&EXAMPLE.replace("healthPath: /health", &format!("healthPath: '{path}'")))
                .contains(&"microServices.web.config.healthPath".to_owned())
        );
    }
    assert!(parse(&EXAMPLE.replace("port: 8080", "port: 0")).is_err());
    assert!(parse(&EXAMPLE.replace("replicas: 2", "replicas: 101")).is_err());
    assert!(
        parse(&EXAMPLE.replace(
            "healthPath: /health",
            "healthPath: /health\n      domain: example.com"
        ))
        .is_err()
    );
}

#[test]
fn never_reports_environment_values_in_parse_or_validation_errors() {
    for value in [
        "[private-token-DO-NOT-PRINT]",
        "\"private-token-DO-NOT-PRINT\\0\"",
    ] {
        let source = EXAMPLE.replace("APP_MODE: demo", &format!("APP_MODE: {value}"));
        let errors = parse(&source).err().expect("invalid env");
        assert!(
            !serde_json::to_string(&errors)
                .unwrap()
                .contains("DO-NOT-PRINT")
        );
    }
    assert!(parse(&EXAMPLE.replace("APP_MODE: demo", "1INVALID: demo")).is_err());
}

#[test]
fn does_not_resolve_environment_properties_or_file_include_tags() {
    let stack = parse(&EXAMPLE.replace("APP_MODE: demo", "APP_MODE: '${HOME}'")).unwrap();
    assert_eq!(stack.micro_vms["worker"].env["APP_MODE"], "${HOME}");
    assert!(parse(&EXAMPLE.replace("APP_MODE: demo", "APP_MODE: !include /etc/passwd")).is_err());
}
