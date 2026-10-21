//! Root-owned stable-address registrations. The updater is independent of firecrab-api.
use crate::{
    service::env::ServiceEnv,
    shell::{CommandRunner, RealCommandRunner},
};
use firecrab_helper_protocol::{
    firecrab_tunnel::{Address, DOMAIN, ENDPOINT, MAX_ADDRESSES, Request, Target},
    pipe::{Config, QuickTunnelEntry, QuickTunnelTarget},
};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Read,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
const UNIT: &str = "firecrab-address-updater.service";
const TEMPLATE: &str = include_str!("../../../packaging/systemd/firecrab-address-updater.service");
#[derive(Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RegistryProtocol {
    #[default]
    AddressesV1,
    RegisterV1,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Binding {
    subdomain: String,
    target: Target,
    connector_id: Uuid,
    owned_connector: bool,
    token: String,
    registered: bool,
    connected: bool,
    last_success: u64,
    error: Option<String>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Store {
    endpoint: String,
    #[serde(default)]
    protocol: RegistryProtocol,
    bindings: Vec<Binding>,
}
fn seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn path() -> PathBuf {
    ServiceEnv::from_process_env()
        .confdir
        .join("pipe/firecrab-addresses.json")
}
fn endpoint(value: &str) -> Result<String> {
    let url = reqwest::Url::parse(value)?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
        || !(url.scheme() == "https"
            || (url.scheme() == "http"
                && url
                    .host_str()
                    .and_then(|h| h.parse::<std::net::IpAddr>().ok())
                    .is_some_and(|ip| ip.is_loopback())))
    {
        return Err(
            "Firecrab endpoint must be an HTTPS origin (HTTP loopback is allowed for QA)".into(),
        );
    }
    Ok(url.origin().ascii_serialization())
}
fn read() -> Result<Store> {
    if path().exists() {
        let store: Store = serde_json::from_slice(&fs::read(path())?)?;
        endpoint(&store.endpoint)?;
        return Ok(store);
    }
    Ok(Store {
        endpoint: endpoint(
            &std::env::var("FIRECRAB_TUNNEL_ENDPOINT").unwrap_or_else(|_| ENDPOINT.into()),
        )?,
        protocol: RegistryProtocol::default(),
        bindings: vec![],
    })
}
fn write(store: &Store) -> Result<()> {
    let path = path();
    write_at(&path, store)
}
fn write_at(path: &Path, store: &Store) -> Result<()> {
    let parent = path.parent().unwrap();
    fs::create_dir_all(parent)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    use std::io::Write;
    temporary
        .as_file()
        .set_permissions(fs::Permissions::from_mode(0o600))?;
    temporary.write_all(&serde_json::to_vec(store)?)?;
    temporary.as_file().sync_all()?;
    temporary.persist(path)?;
    Ok(())
}

// Remote address operations use a separate lock so a service outage does not block route saves.
fn address_lock() -> Result<fs::File> {
    use fs2::FileExt;
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open("/run/firecrab-addresses.lock")?;
    file.try_lock_exclusive()
        .map_err(|_| "Another Firecrab address change is running; retry shortly")?;
    Ok(file)
}
fn config() -> Result<Config> {
    Config::read(
        &ServiceEnv::from_process_env()
            .confdir
            .join("pipe/manifest.json"),
    )
}
fn apply(config: &Config) -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("pipe.json");
    fs::write(&path, serde_json::to_vec(config)?)?;
    super::host::apply_quick_locked(&path)
}
fn unit(enable: bool) -> Result<()> {
    let runner = RealCommandRunner;
    if enable {
        let env = ServiceEnv::from_process_env();
        let executable = std::env::current_exe()?;
        let content = TEMPLATE
            .replace("@CLI@", super::host::unit_path(&executable)?)
            .replace("@CONFDIR@", super::host::unit_path(&env.confdir)?);
        fs::create_dir_all(&env.unitdir)?;
        let path = env.unitdir.join(UNIT);
        if fs::read_to_string(&path).ok().as_deref() != Some(&content) {
            use std::io::Write;
            let mut file = fs::OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .mode(0o644)
                .open(&path)?;
            file.write_all(content.as_bytes())?;
            let output = runner.run("systemctl", &["daemon-reload"])?;
            if !output.status.success() {
                return Err("Cannot reload the address-updater unit".into());
            }
        }
    }
    let output = if enable {
        runner.run("systemctl", &["enable", UNIT])?
    } else {
        runner.run("systemctl", &["disable", "--now", UNIT])?
    };
    if !output.status.success() {
        return Err("Cannot change the Firecrab address-updater service".into());
    }
    // Load the matching CLI after upgrades, even when the unit text is unchanged.
    if enable
        && !runner
            .run("systemctl", &["restart", UNIT])?
            .status
            .success()
    {
        return Err("Cannot restart the Firecrab address-updater service".into());
    }
    Ok(())
}

/// Safe status projection; tokens and service configuration are never returned to the API.
pub(super) fn addresses() -> Result<Vec<Address>> {
    Ok(read()?
        .bindings
        .into_iter()
        .map(|binding| {
            let connected =
                binding.connected && seconds().saturating_sub(binding.last_success) < 150;
            Address {
                public_url: format!("https://{}.{}", binding.subdomain, DOMAIN),
                subdomain: binding.subdomain,
                target: binding.target,
                connector_id: binding.connector_id,
                connected,
                error: if binding.connected && !connected {
                    Some("Address updater has not refreshed the connection".into())
                } else {
                    binding.error
                },
            }
        })
        .collect())
}

/// Prevent direct Quick Tunnel deletion from orphaning a stable public address.
pub(super) fn require_unreferenced(id: Uuid) -> Result<()> {
    if read()?.bindings.iter().any(|b| b.connector_id == id) {
        return Err("Remove the Firecrab address before deleting its connector".into());
    }
    Ok(())
}

/// Keep managed connector targets under their stable address controller.
pub(super) fn require_editable(id: Uuid) -> Result<()> {
    if read()?
        .bindings
        .iter()
        .any(|b| b.connector_id == id && b.owned_connector)
    {
        return Err("Edit the Firecrab address to change its managed connector".into());
    }
    Ok(())
}

/// Full manifest applies must not silently remove an address's selected connector.
pub(super) fn validate_config(config: &Config) -> Result<()> {
    for binding in read()?.bindings {
        let present = config
            .quick_tunnels
            .iter()
            .any(|entry| entry.id == binding.connector_id)
            || (binding.connector_id == firecrab_helper_protocol::pipe::LEGACY_QUICK_TUNNEL_ID
                && config.quick_tunnel.is_some());
        if !present {
            return Err(format!(
                "Remove Firecrab address '{}' before removing its connector",
                binding.subdomain
            )
            .into());
        }
    }
    Ok(())
}

fn release(store: &mut Store, config: &mut Config, binding: &Binding) {
    if !binding.owned_connector {
        return;
    }
    if let Some(peer) = store
        .bindings
        .iter_mut()
        .find(|b| b.subdomain != binding.subdomain && b.connector_id == binding.connector_id)
    {
        peer.owned_connector = true;
    } else {
        config
            .quick_tunnels
            .retain(|entry| entry.id != binding.connector_id);
    }
}

/// Consume a non-secret intent and update only the selected stable address.
pub(super) fn manage() -> Result<()> {
    super::host::require_root()?;
    let mut input = Vec::new();
    std::io::stdin().take(4097).read_to_end(&mut input)?;
    if input.len() > 4096 {
        return Err("Firecrab address request exceeds 4 KiB".into());
    }
    let request: Request = serde_json::from_slice(&input)?;
    manage_request(request)
}

/// Apply a CLI or helper address intent through the same privileged transaction.
pub(super) fn manage_request(request: Request) -> Result<()> {
    super::host::require_root()?;
    request.validate()?;
    let _address_lock = address_lock()?;
    let mut store = read()?;
    match request {
        Request::Connect { token, target } => {
            let (subdomain, protocol) = resolve_token(&store.endpoint, &token.0)?;
            store.protocol = protocol;
            save(&mut store, subdomain, target, Some(token.0))?;
        }
        Request::Save { subdomain, target } => save(&mut store, subdomain, target, None)?,
        Request::Delete { subdomain } => {
            let binding = store
                .bindings
                .iter()
                .find(|b| b.subdomain == subdomain)
                .cloned()
                .ok_or("Firecrab address was not found")?;
            // Dashboard-owned reservations are revoked only in the dashboard.
            // Local removal stops their updater and exclusively owned connector.
            if store.protocol == RegistryProtocol::AddressesV1 {
                let response = client()?
                    .delete(format!("{}/v1/addresses/{subdomain}", store.endpoint))
                    .header("host", "tunnel.firecrab.dev")
                    .bearer_auth(&binding.token)
                    .send()
                    .map_err(|_| {
                        "Cannot reach the Firecrab connection service; address ownership is retained"
                    })?;
                if !(response.status().is_success()
                    || response.status().as_u16() == 410
                    || (response.status().as_u16() == 409 && !binding.registered))
                {
                    return Err(
                        "Connection service rejected address removal; ownership is retained".into(),
                    );
                }
            }
            let _pipe_lock = super::host::lock()?;
            let mut config = config()?;
            release(&mut store, &mut config, &binding);
            apply(&config)?;
            store.bindings.retain(|b| b.subdomain != subdomain);
            write(&store)?;
            if store.bindings.is_empty() {
                unit(false)?;
            }
        }
    }
    Ok(())
}
/// Change one local target after resolving its dashboard-issued ownership token.
fn save(store: &mut Store, subdomain: String, target: Target, token: Option<String>) -> Result<()> {
    let pipe_lock = super::host::lock()?;
    if store.bindings.len() >= MAX_ADDRESSES
        && !store.bindings.iter().any(|b| b.subdomain == subdomain)
    {
        return Err("At most 16 Firecrab addresses can be saved on one host".into());
    }
    let mut config = config()?;
    let old = store
        .bindings
        .iter()
        .find(|b| b.subdomain == subdomain)
        .cloned();
    let token = token
        .or_else(|| old.as_ref().map(|binding| binding.token.clone()))
        .ok_or("Use a dashboard connection token to add a new Firecrab address")?;
    let managed_id = || {
        old.as_ref()
            .filter(|b| {
                b.owned_connector
                    && !store.bindings.iter().any(|peer| {
                        peer.subdomain != subdomain && peer.connector_id == b.connector_id
                    })
            })
            .map(|b| b.connector_id)
            .unwrap_or_else(Uuid::new_v4)
    };
    let (connector_id, connector_target) = match &target {
        Target::Apps { upstreams } => (
            managed_id(),
            Some(QuickTunnelTarget::Apps {
                upstreams: upstreams.clone(),
            }),
        ),
        Target::QuickTunnel { id } => {
            let exists = config.quick_tunnels.iter().any(|e| e.id == *id)
                || (*id == firecrab_helper_protocol::pipe::LEGACY_QUICK_TUNNEL_ID
                    && config.quick_tunnel.is_some());
            if !exists {
                return Err("Selected Quick Tunnel no longer exists".into());
            }
            (*id, None)
        }
        Target::Nginx { host } => {
            if !config.routes.iter().any(|r| r.host == *host) {
                return Err("Selected Nginx route no longer exists".into());
            }
            (
                managed_id(),
                Some(QuickTunnelTarget::Routes { host: host.clone() }),
            )
        }
    };
    let owned = connector_target.is_some();
    if let Some(target) = connector_target {
        config
            .quick_tunnels
            .retain(|entry| entry.id != connector_id);
        config.quick_tunnels.push(QuickTunnelEntry {
            id: connector_id,
            name: format!("Firecrab: {subdomain}"),
            target,
        });
    }
    if let Some(old) = &old
        && old.connector_id != connector_id
    {
        release(store, &mut config, old);
    }
    config.validate()?;
    if owned {
        super::engines::install()?;
    }
    apply(&config)?;
    let owned = owned
        || old
            .as_ref()
            .is_some_and(|b| b.owned_connector && b.connector_id == connector_id);
    let mut binding = old.unwrap_or_else(|| Binding {
        subdomain: subdomain.clone(),
        target: target.clone(),
        connector_id,
        owned_connector: owned,
        token: token.clone(),
        registered: false,
        connected: false,
        last_success: 0,
        error: None,
    });
    binding.token = token;
    binding.target = target;
    binding.connector_id = connector_id;
    binding.owned_connector = owned;
    binding.connected = false;
    store.bindings.retain(|b| b.subdomain != subdomain);
    store.bindings.push(binding);
    // Persist ownership before the network request, including a lost response.
    write(store)?;
    drop(pipe_lock);
    unit(true)?;
    // Ownership is committed; the updater retries if this first refresh fails.
    let _ = sync_locked(store);
    Ok(())
}

/// Verify a token remotely before changing local routes or saving its secret.
fn resolve_token(endpoint: &str, token: &str) -> Result<(String, RegistryProtocol)> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Address {
        subdomain: String,
        public_url: String,
    }
    #[derive(Deserialize)]
    struct Registration {
        name: String,
        url: String,
        heartbeat_url: String,
        suspended: bool,
    }
    let client = client()?;
    // The deployed dashboard resolves ownership through registration. An empty
    // body verifies its token without publishing or changing a tunnel URL.
    let response = client
        .post(format!("{endpoint}/v1/register"))
        .header("host", "tunnel.firecrab.dev")
        .bearer_auth(token)
        .json(&serde_json::json!({}))
        .send()
        .map_err(|_| "Cannot verify the token with the Firecrab connection service")?;
    let (response, protocol) = if matches!(response.status().as_u16(), 404 | 405) {
        // Retain compatibility with the standalone Rust registry and saved hosts.
        (
            client
                .get(format!("{endpoint}/v1/token"))
                .header("host", "tunnel.firecrab.dev")
                .bearer_auth(token)
                .send()
                .map_err(|_| "Cannot verify the token with the Firecrab connection service")?,
            RegistryProtocol::AddressesV1,
        )
    } else {
        (response, RegistryProtocol::RegisterV1)
    };
    if !response.status().is_success() {
        return Err(match response.status().as_u16() {
            401 | 410 => "The connection token is invalid or revoked",
            404 | 405 => "Firecrab connection API is unavailable; check the service deployment",
            429 => "Token verification is rate limited; retry later",
            _ => "The Firecrab connection service could not verify the token",
        }
        .into());
    }
    let address = match protocol {
        RegistryProtocol::AddressesV1 => response
            .json::<Address>()
            .map_err(|_| "Invalid token verification response")?,
        RegistryProtocol::RegisterV1 => {
            let registration: Registration = response
                .json()
                .map_err(|_| "Invalid token verification response")?;
            if registration.heartbeat_url != format!("{}/__firecrab/heartbeat", registration.url) {
                return Err("Invalid token verification response".into());
            }
            if registration.suspended {
                return Err("The Firecrab address is suspended; check the dashboard".into());
            }
            Address {
                subdomain: registration.name,
                public_url: registration.url,
            }
        }
    };
    if !firecrab_helper_protocol::firecrab_tunnel::subdomain(&address.subdomain)
        || address.public_url != format!("https://{}.{}", address.subdomain, DOMAIN)
    {
        return Err("Invalid token verification response".into());
    }
    Ok((address.subdomain, protocol))
}

fn client() -> Result<reqwest::blocking::Client> {
    Ok(reqwest::blocking::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(2))
        .timeout(Duration::from_secs(5))
        .build()?)
}
fn sync_locked(store: &mut Store) -> Result<()> {
    let snapshot = {
        let _lock = super::host::lock()?;
        super::host::snapshot(true)?
    };
    refresh_bindings(store, &snapshot)?;
    write(store)
}

fn connector_error(
    connector: Option<&firecrab_helper_protocol::pipe::QuickTunnelSnapshot>,
) -> String {
    let Some(connector) = connector else {
        return "Selected Quick Tunnel no longer exists".into();
    };
    if let Some(error) = &connector.error {
        return error.clone();
    }
    match connector.unit.active_state.as_str() {
        "inactive" => "The Cloudflare connector is stopped; start the tunnel",
        "failed" => "The Cloudflare connector failed; check its service logs or restart the tunnel",
        _ => "Waiting for the selected connector URL",
    }
    .into()
}

fn refresh_bindings(
    store: &mut Store,
    snapshot: &firecrab_helper_protocol::pipe::Snapshot,
) -> Result<()> {
    let client = client()?;
    for binding in &mut store.bindings {
        let connector = snapshot
            .quick_tunnels
            .iter()
            .find(|q| q.id == binding.connector_id);
        let url = connector.and_then(|q| q.public_url.as_ref());
        let kind = if matches!(binding.target, Target::Nginx { .. }) {
            "nginx"
        } else {
            "quick_tunnel"
        };
        binding.connected = false;
        let request = match store.protocol {
            RegistryProtocol::AddressesV1 => client
                .put(format!(
                    "{}/v1/addresses/{}",
                    store.endpoint, binding.subdomain
                ))
                .header("host", "tunnel.firecrab.dev")
                .json(&serde_json::json!({"target":url.map(|url|serde_json::json!({"kind":kind,"url":url})),"enabled":url.is_some()})),
            RegistryProtocol::RegisterV1 => {
                let Some(url) = url else {
                    binding.error = Some(connector_error(connector));
                    continue;
                };
                let origin = if store.endpoint == ENDPOINT {
                    format!("https://{}.{}", binding.subdomain, DOMAIN)
                } else {
                    store.endpoint.clone()
                };
                client
                    .post(format!("{origin}/__firecrab/heartbeat"))
                    .header("host", format!("{}.{}", binding.subdomain, DOMAIN))
                    .json(&serde_json::json!({"tunnel_url":url}))
            }
        };
        let result = request.bearer_auth(&binding.token).send();
        match result {
            Ok(response) if response.status().is_success() => {
                if store.protocol == RegistryProtocol::RegisterV1 {
                    #[derive(Deserialize)]
                    struct Heartbeat {
                        ok: bool,
                        url: String,
                        suspended: bool,
                    }
                    match response.json::<Heartbeat>() {
                        Ok(heartbeat)
                            if heartbeat.ok
                                && heartbeat.url
                                    == format!("https://{}.{}", binding.subdomain, DOMAIN) =>
                        {
                            if heartbeat.suspended {
                                binding.error = Some(
                                    "The Firecrab address is suspended; check the dashboard".into(),
                                );
                                continue;
                            }
                        }
                        _ => {
                            binding.error = Some("Invalid Firecrab heartbeat response".into());
                            continue;
                        }
                    }
                }
                binding.registered = true;
                binding.connected = url.is_some();
                binding.last_success = seconds();
                binding.error = if url.is_none() {
                    Some(connector_error(connector))
                } else {
                    None
                };
            }
            Ok(response) => {
                binding.error = Some(
                    match response.status().as_u16() {
                        401 | 410 => "The connection token is invalid or revoked",
                        403 => "The Firecrab address is suspended; check the dashboard",
                        404 | 405 => {
                            "Firecrab connection API is unavailable; check the service deployment"
                        }
                        409 => "This subdomain belongs to another host",
                        429 => "Address registration limit reached; retry later",
                        400 => "Connection service rejected the target",
                        _ => "Connection service is unavailable",
                    }
                    .into(),
                );
            }
            Err(_) => {
                binding.error = Some("Cannot reach the Firecrab connection service".into());
            }
        }
    }
    Ok(())
}

/// Continuously refresh current target URLs and leases without depending on the API process.
pub(super) fn sync() -> Result<()> {
    super::host::require_root()?;
    loop {
        if let Ok(_lock) = address_lock() {
            let result = read().and_then(|mut store| sync_locked(&mut store));
            if result.is_err() {
                eprintln!("Firecrab address refresh failed; will retry");
            }
        }
        std::thread::sleep(Duration::from_secs(30));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry(
        responses: Vec<(u16, serde_json::Value)>,
    ) -> (String, std::thread::JoinHandle<Vec<String>>) {
        use std::io::{BufRead, BufReader, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let mut requests = Vec::new();
            for (status, body) in responses {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut reader = BufReader::new(&mut stream);
                let mut request = String::new();
                let mut length = 0;
                loop {
                    let mut line = String::new();
                    assert!(reader.read_line(&mut line).unwrap() > 0);
                    if line == "\r\n" {
                        break;
                    }
                    if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        length = value.trim().parse().unwrap();
                    }
                    request.push_str(&line);
                    assert!(request.len() < 8192);
                }
                let mut payload = vec![0; length];
                reader.read_exact(&mut payload).unwrap();
                request.push_str(&String::from_utf8(payload).unwrap());
                requests.push(request);
                let body = body.to_string();
                write!(stream, "HTTP/1.1 {status} fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            }
            requests
        });
        (endpoint, server)
    }

    #[test]
    fn dashboard_token_resolution_does_not_publish_a_target() {
        let (endpoint, server) = registry(vec![(
            200,
            serde_json::json!({
                "name":"pipe", "url":"https://pipe.firecrab.dev",
                "heartbeat_url":"https://pipe.firecrab.dev/__firecrab/heartbeat", "suspended":false
            }),
        )]);
        let (name, protocol) = resolve_token(&endpoint, &"a".repeat(64)).unwrap();
        assert_eq!(name, "pipe");
        assert!(protocol == RegistryProtocol::RegisterV1);
        let requests = server.join().unwrap();
        assert!(requests[0].starts_with("POST /v1/register HTTP/1.1\r\n"));
        assert!(requests[0].ends_with("{}"));
    }

    #[test]
    fn standalone_registry_falls_back_to_its_token_lookup() {
        let (endpoint, server) = registry(vec![
            (404, serde_json::json!({"error":"not_found"})),
            (
                200,
                serde_json::json!({"subdomain":"pipe", "publicUrl":"https://pipe.firecrab.dev"}),
            ),
        ]);
        let (name, protocol) = resolve_token(&endpoint, &"a".repeat(64)).unwrap();
        assert_eq!(name, "pipe");
        assert!(protocol == RegistryProtocol::AddressesV1);
        assert!(server.join().unwrap()[1].starts_with("GET /v1/token HTTP/1.1\r\n"));
    }

    #[test]
    fn missing_apis_are_not_reported_as_invalid_tokens() {
        let (endpoint, server) = registry(vec![
            (404, serde_json::json!({"error":"not_found"})),
            (404, serde_json::json!({"error":"not_found"})),
        ]);
        let error = resolve_token(&endpoint, &"a".repeat(64)).err().unwrap();
        assert_eq!(
            error.to_string(),
            "Firecrab connection API is unavailable; check the service deployment"
        );
        server.join().unwrap();
    }

    #[test]
    fn token_lookup_rejects_an_unexpected_heartbeat_destination() {
        let (endpoint, server) = registry(vec![(
            200,
            serde_json::json!({
                "name":"pipe", "url":"https://pipe.firecrab.dev",
                "heartbeat_url":"https://another.example/heartbeat", "suspended":false
            }),
        )]);
        assert_eq!(
            resolve_token(&endpoint, &"a".repeat(64))
                .err()
                .unwrap()
                .to_string(),
            "Invalid token verification response"
        );
        server.join().unwrap();
    }

    fn binding(name: &str, id: Uuid, owned: bool) -> Binding {
        Binding {
            subdomain: name.into(),
            target: Target::QuickTunnel { id },
            connector_id: id,
            owned_connector: owned,
            token: "secret-owned-on-host".into(),
            registered: true,
            connected: true,
            last_success: seconds(),
            error: None,
        }
    }

    #[test]
    fn connector_failure_reaches_the_address_and_a_later_url_refresh_connects_it() {
        let id = Uuid::new_v4();
        let mut snapshot: firecrab_helper_protocol::pipe::Snapshot = serde_json::from_value(
            serde_json::json!({
                "config":null, "units":[], "nginxAvailable":true, "cloudflaredAvailable":true,
                "quickTunnels":[{
                    "id":id, "name":"managed", "target":{"type":"apps","upstreams":["127.0.0.1:9000"]},
                    "unit":{"unit":firecrab_helper_protocol::pipe::quick_tunnel_unit(id),"loadState":"loaded","activeState":"activating","subState":"auto-restart","mainPid":0},
                    "publicUrl":null, "error":"Cloudflare rate limited URL creation (HTTP 429)"
                }]
            }),
        ).unwrap();
        let (endpoint, server) = registry(vec![(
            200,
            serde_json::json!({
                "ok":true, "url":"https://first.firecrab.dev", "suspended":false
            }),
        )]);
        let mut store = Store {
            endpoint,
            protocol: RegistryProtocol::RegisterV1,
            bindings: vec![binding("first", id, true)],
        };
        refresh_bindings(&mut store, &snapshot).unwrap();
        assert!(!store.bindings[0].connected);
        assert_eq!(store.bindings[0].error, snapshot.quick_tunnels[0].error);

        snapshot.quick_tunnels[0].error = None;
        snapshot.quick_tunnels[0].unit.active_state = "active".into();
        snapshot.quick_tunnels[0].public_url =
            Some("https://recovered-crab.trycloudflare.com".into());
        refresh_bindings(&mut store, &snapshot).unwrap();
        assert!(store.bindings[0].connected);
        assert!(store.bindings[0].error.is_none());
        let requests = server.join().unwrap();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].starts_with("POST /__firecrab/heartbeat HTTP/1.1"));
        assert!(
            requests[0].ends_with(r#"{"tunnel_url":"https://recovered-crab.trycloudflare.com"}"#)
        );
    }

    #[test]
    fn standalone_refresh_disables_a_failed_target_and_preserves_its_error() {
        let id = Uuid::new_v4();
        let snapshot: firecrab_helper_protocol::pipe::Snapshot = serde_json::from_value(
            serde_json::json!({
                "config":null,"units":[],"nginxAvailable":true,"cloudflaredAvailable":true,
                "quickTunnels":[{
                    "id":id,"name":"managed","target":{"type":"apps","upstreams":["127.0.0.1:9000"]},
                    "unit":{"unit":firecrab_helper_protocol::pipe::quick_tunnel_unit(id),"loadState":"loaded","activeState":"failed","subState":"failed","mainPid":0},
                    "publicUrl":null,"error":"Cannot reach the Cloudflare URL service"
                }]
            }),
        ).unwrap();
        let (endpoint, server) = registry(vec![(200, serde_json::json!({}))]);
        let mut store = Store {
            endpoint,
            protocol: RegistryProtocol::AddressesV1,
            bindings: vec![binding("first", id, true)],
        };
        refresh_bindings(&mut store, &snapshot).unwrap();
        assert!(!store.bindings[0].connected);
        assert_eq!(store.bindings[0].error, snapshot.quick_tunnels[0].error);
        assert!(server.join().unwrap()[0].ends_with(r#"{"enabled":false,"target":null}"#));
    }
    fn config(id: Uuid) -> Config {
        serde_json::from_value(serde_json::json!({"routes":[],"quickTunnels":[{"id":id,"name":"managed","target":{"type":"apps","upstreams":["127.0.0.1:9000"]}}]})).unwrap()
    }
    #[test]
    fn deleting_an_owned_binding_transfers_connector_ownership_to_its_remaining_peer() {
        let id = Uuid::new_v4();
        let original = binding("first", id, true);
        let mut store = Store {
            endpoint: ENDPOINT.into(),
            protocol: RegistryProtocol::default(),
            bindings: vec![original.clone(), binding("second", id, false)],
        };
        let mut config = config(id);
        release(&mut store, &mut config, &original);
        assert!(store.bindings[1].owned_connector);
        assert_eq!(config.quick_tunnels.len(), 1);
        store.bindings.remove(0);
        release(&mut store, &mut config, &binding("second", id, true));
        assert!(config.quick_tunnels.is_empty());
    }
    #[test]
    fn deleting_a_reused_connector_binding_keeps_the_user_connector() {
        let id = Uuid::new_v4();
        let original = binding("first", id, false);
        let mut store = Store {
            endpoint: ENDPOINT.into(),
            protocol: RegistryProtocol::default(),
            bindings: vec![original.clone()],
        };
        let mut config = config(id);
        release(&mut store, &mut config, &original);
        assert_eq!(config.quick_tunnels.len(), 1);
    }
    #[test]
    fn ownership_is_written_atomically_with_private_permissions() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("addresses.json");
        let store = Store {
            endpoint: ENDPOINT.into(),
            protocol: RegistryProtocol::default(),
            bindings: vec![binding("first", Uuid::new_v4(), false)],
        };
        write_at(&path, &store).unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let loaded: Store = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(loaded.bindings[0].token, "secret-owned-on-host");
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
    }
    #[test]
    fn production_endpoints_never_accept_credentials_redirect_paths_or_insecure_remote_servers() {
        for invalid in [
            "http://remote.test",
            "https://user@service.test",
            "https://service.test/api",
            "https://service.test?secret=token",
        ] {
            assert!(endpoint(invalid).is_err());
        }
        assert_eq!(endpoint(ENDPOINT).unwrap(), ENDPOINT);
        assert!(endpoint("http://127.0.0.1:8790").is_ok());
    }
}
