//! Versioned stack validation, ownership types and a pure read-only planner.
//!
//! Validation never reads host state, resolves images, or creates resources.
//! A successful result is a structurally valid declaration, not a deployment plan.

use std::collections::{BTreeMap, BTreeSet};
use std::net::Ipv4Addr;

use firecrab_api_types::{EgressPolicy, MicroServiceSpec};
use serde::{Deserialize, Serialize};

mod planning;
pub use planning::{
    Action, Binding, Change, Inventory, Plan, RegisterStackRequest, ResourceKind, ServiceSnapshot,
    StackRecord, VmSnapshot, plan,
};

/// Maximum UTF-8 manifest size, including comments.
pub const MAX_MANIFEST_BYTES: usize = 1024 * 1024;
/// Maximum total declared resources in one stack.
pub const MAX_RESOURCES: usize = 256;

/// An experimental stack declaration. Unknown fields are rejected at every level.
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Stack {
    /// Explicit schema version; currently only `firecrab.dev/v1alpha1`.
    pub api_version: ApiVersion,
    /// Document type; must be `Stack`.
    pub kind: StackKind,
    /// Stable logical stack name, independent of the file path.
    pub name: String,
    /// Logical network names and their IPv4 configuration.
    #[serde(default)]
    pub micro_networks: BTreeMap<String, Network>,
    /// Logical standalone VM names and their configuration.
    #[serde(default, rename = "microVMs")]
    pub micro_vms: BTreeMap<String, Vm>,
    /// Logical service names and their configuration and placement.
    #[serde(default)]
    pub micro_services: BTreeMap<String, Service>,
}

/// Supported experimental manifest versions.
#[derive(Clone, Deserialize, Serialize)]
pub enum ApiVersion {
    /// Initial validation-only schema; may change before apply support lands.
    #[serde(rename = "firecrab.dev/v1alpha1")]
    V1Alpha1,
}

/// Supported document kind.
#[derive(Clone, Deserialize, Serialize)]
pub enum StackKind {
    /// A set of resources managed together.
    Stack,
}

/// Initial IPv4 network schema. Advanced network fields are not yet accepted.
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Network {
    /// Name to use in the resource API.
    pub name: String,
    /// IPv4 CIDR with a prefix between /16 and /28.
    pub subnet_cidr: String,
    /// Whether the network permits internet access; defaults to true.
    #[serde(default = "default_internet_enabled")]
    pub internet_enabled: bool,
}

/// Standalone VM declaration; `network` refers to a logical name in this stack.
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Vm {
    /// Name to use in the resource API.
    pub name: String,
    /// Installed template alias. Availability and content identity need host validation.
    pub template: String,
    /// RAM in MiB, matching the VM API's power-of-two sizes.
    pub ram: u32,
    /// Virtual CPU count.
    pub cpu: u8,
    /// Requested disk capacity in GiB; the image-specific minimum needs host validation.
    pub disk_gb: u16,
    /// Logical key in `microNetworks`, never an implicit resource adoption by name.
    pub network: String,
    /// Outbound network policy; defaults to internet access.
    #[serde(default)]
    pub egress_policy: EgressPolicy,
    /// Registered storage ID; omission uses the host default.
    #[serde(default)]
    pub storage_root: Option<String>,
    /// Non-secret guest environment values, using the VM API's limits.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

/// Host-port service declaration, reusing the existing API configuration type.
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Service {
    /// Existing MicroService configuration shape.
    pub config: MicroServiceSpec,
    /// Logical key in `microNetworks`.
    pub network: String,
    /// First host TCP port; replicas use consecutive ports.
    pub host_port: u16,
}

/// One diagnostic without manifest values or source excerpts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Diagnostic {
    /// Schema path, or `$` for syntax and whole-document failures.
    pub path: String,
    /// Static explanation that never echoes a submitted value.
    pub message: &'static str,
    /// One-based YAML line when a parser error has a known location.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u64>,
    /// One-based YAML column when a parser error has a known location.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column: Option<u64>,
}

/// Resource counts; safe to show without returning configuration values.
#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceCounts {
    /// Number of declared networks.
    pub micro_networks: usize,
    /// Number of declared standalone VMs.
    #[serde(rename = "microVMs")]
    pub micro_vms: usize,
    /// Number of declared services, excluding their future replica VMs.
    pub micro_services: usize,
}

impl Stack {
    /// Count declarations, not future runtime instances.
    pub fn counts(&self) -> ResourceCounts {
        ResourceCounts {
            micro_networks: self.micro_networks.len(),
            micro_vms: self.micro_vms.len(),
            micro_services: self.micro_services.len(),
        }
    }
}

/// Parse and validate one bounded YAML document without accessing files or the network.
///
/// Duplicate keys, extra documents, unsupported fields and invalid references fail closed.
/// Parser messages are reduced to static categories and coordinates to protect env values.
pub fn parse(source: &str) -> Result<Stack, Vec<Diagnostic>> {
    if source.len() > MAX_MANIFEST_BYTES {
        return Err(vec![diagnostic("$", "manifest exceeds 1 MiB")]);
    }
    let mut options = serde_saphyr::Options::default();
    options.strict_booleans = true;
    options.reject_unsupported_tags = true;
    options.with_snippet = false;
    options.duplicate_keys = serde_saphyr::DuplicateKeyPolicy::Error;
    options.merge_keys = serde_saphyr::MergeKeyPolicy::Error;
    options.budget = serde_saphyr::budget! {
        max_reader_input_bytes: Some(MAX_MANIFEST_BYTES),
        max_documents: 1,
        max_depth: 16,
        max_nodes: 100_000,
        max_events: 200_000,
        max_total_scalar_bytes: MAX_MANIFEST_BYTES,
        max_aliases: 0,
        max_anchors: 0,
    };
    let stack: Stack = serde_saphyr::from_str_with_options(source, options).map_err(|error| {
        let message = match &error {
            serde_saphyr::Error::SerdeUnknownField { .. } => "unknown field",
            serde_saphyr::Error::SerdeMissingField { .. } => "missing required field",
            serde_saphyr::Error::DuplicateMappingKey { .. } => "duplicate mapping key",
            serde_saphyr::Error::MultipleDocuments { .. } => "only one YAML document is allowed",
            _ => "invalid YAML or unsupported stack schema",
        };
        let mut item = diagnostic("$", message);
        if let Some(location) = error.location() {
            item.line = Some(location.line());
            item.column = Some(location.column());
        }
        vec![item]
    })?;
    validate(&stack)?;
    Ok(stack)
}

/// Check declarations supplied by another typed caller, without host-state checks.
pub fn validate(stack: &Stack) -> Result<(), Vec<Diagnostic>> {
    let mut errors = Vec::new();
    if !logical_name(&stack.name) {
        errors.push(diagnostic(
            "name",
            "must be a lowercase DNS label of 1-63 bytes",
        ));
    }
    let counts = stack.counts();
    let total = counts.micro_networks + counts.micro_vms + counts.micro_services;
    if total == 0 || total > MAX_RESOURCES {
        errors.push(diagnostic("$", "must declare between 1 and 256 resources"));
        return Err(errors);
    }
    let mut subnets: Vec<(u32, u32)> = Vec::new();
    let mut names = BTreeSet::new();
    for (key, network) in &stack.micro_networks {
        let Some(path) = resource_path("microNetworks", key, &mut errors) else {
            continue;
        };
        check_name(&path, &network.name, &mut names, &mut errors);
        match subnet(&network.subnet_cidr) {
            Some((first, last)) => {
                if subnets.iter().any(|(a, b)| first <= *b && *a <= last) {
                    errors.push(diagnostic(
                        &format!("{path}.subnetCidr"),
                        "overlaps another declared network",
                    ));
                }
                subnets.push((first, last));
            }
            None => errors.push(diagnostic(
                &format!("{path}.subnetCidr"),
                "must be an IPv4 CIDR with prefix /16 through /28",
            )),
        }
    }
    names.clear();
    for (key, vm) in &stack.micro_vms {
        let Some(path) = resource_path("microVMs", key, &mut errors) else {
            continue;
        };
        check_name(&path, &vm.name, &mut names, &mut errors);
        check_capacity(&path, vm.cpu, vm.ram, &mut errors);
        check_network(&path, &vm.network, stack, &mut errors);
        check_env(&format!("{path}.env"), &vm.env, &mut errors);
        if !resource_name(&vm.template) {
            errors.push(diagnostic(
                &format!("{path}.template"),
                "must be an installed template alias of 1-64 safe ASCII characters",
            ));
        }
        if !(1..=500).contains(&vm.disk_gb) {
            errors.push(diagnostic(
                &format!("{path}.diskGb"),
                "must be between 1 and 500 GiB; image minimum is checked on the host",
            ));
        }
        if vm
            .storage_root
            .as_ref()
            .is_some_and(|id| !resource_name(id))
        {
            errors.push(diagnostic(
                &format!("{path}.storageRoot"),
                "must be a registered storage ID of 1-64 safe ASCII characters",
            ));
        }
    }
    names.clear();
    let mut host_ports = BTreeSet::new();
    for (key, service) in &stack.micro_services {
        let Some(path) = resource_path("microServices", key, &mut errors) else {
            continue;
        };
        let config = &service.config;
        let config_path = format!("{path}.config");
        check_name(&config_path, &config.name, &mut names, &mut errors);
        if !logical_name(&config.name) {
            errors.push(diagnostic(
                &format!("{config_path}.name"),
                "must be a lowercase DNS label of 1-63 bytes",
            ));
        }
        check_capacity(&config_path, config.cpu, config.ram, &mut errors);
        check_network(&path, &service.network, stack, &mut errors);
        check_env(&format!("{config_path}.env"), &config.env, &mut errors);
        if !pinned_image(&config.image) {
            errors.push(diagnostic(
                &format!("{config_path}.image"),
                "must use an OCI reference pinned with @sha256: and 64 hexadecimal digits",
            ));
        }
        if config.port == 0 {
            errors.push(diagnostic(
                &format!("{config_path}.port"),
                "must be between 1 and 65535",
            ));
        }
        if !(1..=100).contains(&config.replicas) {
            errors.push(diagnostic(
                &format!("{config_path}.replicas"),
                "must be between 1 and 100",
            ));
        }
        if !(1..=3600).contains(&config.startup_timeout) || config.drain_timeout > 3600 {
            errors.push(diagnostic(
                &config_path,
                "startupTimeout must be 1-3600 and drainTimeout must be 0-3600 seconds",
            ));
        }
        if !health_path(&config.health_path) {
            errors.push(diagnostic(
                &format!("{config_path}.healthPath"),
                "must be an absolute ASCII HTTP path without query or fragment, at most 2048 bytes",
            ));
        }
        if config.domain.is_some() || !config.secrets.is_empty() {
            errors.push(diagnostic(
                &config_path,
                "domain and secrets are not supported by the current host-port deployment",
            ));
        }
        let end = u32::from(service.host_port) + u32::from(config.replicas);
        if service.host_port == 0 || end > 65536 {
            errors.push(diagnostic(
                &format!("{path}.hostPort"),
                "replica host ports must fit between 1 and 65535",
            ));
        } else if (u32::from(service.host_port)..end).any(|port| !host_ports.insert(port)) {
            errors.push(diagnostic(
                &format!("{path}.hostPort"),
                "replica host ports overlap another declared service",
            ));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

fn diagnostic(path: &str, message: &'static str) -> Diagnostic {
    Diagnostic {
        path: path.to_owned(),
        message,
        line: None,
        column: None,
    }
}

fn default_internet_enabled() -> bool {
    true
}

/// Whether a stack name or logical resource key is a safe lowercase DNS label.
pub fn logical_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    (1..=63).contains(&bytes.len())
        && bytes[0].is_ascii_alphanumeric()
        && bytes[bytes.len() - 1].is_ascii_alphanumeric()
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
}

fn resource_name(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

fn resource_path(group: &str, key: &str, errors: &mut Vec<Diagnostic>) -> Option<String> {
    if !logical_name(key) {
        errors.push(diagnostic(
            group,
            "resource keys must be lowercase DNS labels of 1-63 bytes",
        ));
        return None;
    }
    Some(format!("{group}.{key}"))
}

fn check_name(path: &str, name: &str, seen: &mut BTreeSet<String>, errors: &mut Vec<Diagnostic>) {
    if !resource_name(name) {
        errors.push(diagnostic(
            &format!("{path}.name"),
            "must be 1-64 ASCII letters, digits, '.', '_' or '-'",
        ));
    } else if !seen.insert(name.to_owned()) {
        errors.push(diagnostic(
            &format!("{path}.name"),
            "duplicates another resource name in this group",
        ));
    }
}

fn check_capacity(path: &str, cpu: u8, ram: u32, errors: &mut Vec<Diagnostic>) {
    if !(1..=32).contains(&cpu) {
        errors.push(diagnostic(
            &format!("{path}.cpu"),
            "must be between 1 and 32",
        ));
    }
    if !(128..=32768).contains(&ram) || !ram.is_power_of_two() {
        errors.push(diagnostic(
            &format!("{path}.ram"),
            "must be a power of two between 128 and 32768 MiB",
        ));
    }
}

fn check_network(path: &str, reference: &str, stack: &Stack, errors: &mut Vec<Diagnostic>) {
    if !stack.micro_networks.contains_key(reference) {
        errors.push(diagnostic(
            &format!("{path}.network"),
            "must reference a declared microNetworks key",
        ));
    }
}

fn check_env(path: &str, env: &BTreeMap<String, String>, errors: &mut Vec<Diagnostic>) {
    if env.len() > 64
        || env.iter().any(|(key, value)| {
            let mut bytes = key.bytes();
            !matches!(bytes.next(), Some(b) if b.is_ascii_alphabetic() || b == b'_')
                || !bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_')
                || key.len() > 256
                || value.len() > 4096
                || value.contains('\0')
        })
    {
        errors.push(diagnostic(
            path,
            "allows up to 64 POSIX names (256 bytes each) and values up to 4096 bytes without NUL",
        ));
    }
}

fn subnet(cidr: &str) -> Option<(u32, u32)> {
    let (address, prefix) = cidr.split_once('/')?;
    let address = u32::from(address.parse::<Ipv4Addr>().ok()?);
    let prefix = prefix.parse::<u8>().ok()?;
    if !(16..=28).contains(&prefix) {
        return None;
    }
    let mask = u32::MAX << (32 - prefix);
    let first = address & mask;
    Some((first, first | !mask))
}

fn pinned_image(image: &str) -> bool {
    let Some((repository, digest)) = image.split_once("@sha256:") else {
        return false;
    };
    !repository.is_empty()
        && !repository.contains('@')
        && image.len() <= 2048
        && repository
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'/' | b':'))
        && digest.len() == 64
        && digest.bytes().all(|b| b.is_ascii_hexdigit())
}

fn health_path(path: &str) -> bool {
    path.len() <= 2048
        && path.starts_with('/')
        && !path.starts_with("//")
        && path.bytes().all(|b| {
            b.is_ascii()
                && !b.is_ascii_control()
                && !b.is_ascii_whitespace()
                && !matches!(b, b'?' | b'#' | b'\\')
        })
}
