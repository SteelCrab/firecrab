//! A scoped API token creates a tunnel and DNS record; only tunnel credentials
//! survive setup. The root CLI owns the transaction even if the API disconnects.
use std::io::Read;
use std::time::Duration;

use base64::Engine;
use firecrab_helper_protocol::cloudflare::{ConnectRequest, Zone, identifier};
use firecrab_helper_protocol::pipe::{Config, Tunnel};
use reqwest::{Method, blocking::Client};
use serde_json::{Value, json};
use uuid::Uuid;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
const API: &str = "https://api.cloudflare.com/client/v4/";

struct Cloudflare {
    client: Client,
    base: String,
}

impl Cloudflare {
    fn new(request: &ConnectRequest) -> Result<Self> {
        let mut token = reqwest::header::HeaderValue::from_str(&format!(
            "Bearer {}",
            request.api_token.value()
        ))?;
        token.set_sensitive(true);
        let client = Client::builder()
            .default_headers(reqwest::header::HeaderMap::from_iter([(
                reqwest::header::AUTHORIZATION,
                token,
            )]))
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(10))
            .build()?;
        Ok(Self {
            client,
            base: API.into(),
        })
    }

    fn call(&self, method: Method, path: &str, body: Option<Value>) -> Result<Value> {
        let mut request = self.client.request(method, format!("{}{path}", self.base));
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().map_err(|_| "Cloudflare did not respond; check network access and the Cloudflare dashboard before retrying")?;
        let status = response.status();
        if matches!(status.as_u16(), 401 | 403) {
            return Err("Cloudflare rejected the API token. Check Cloudflare Tunnel Edit, DNS Edit, Zone Read, and the selected account/domain resources".into());
        }
        if status.as_u16() == 429 {
            return Err("Cloudflare rate limit reached; retry later".into());
        }
        if !status.is_success() {
            return Err(format!("Cloudflare request failed (HTTP {})", status.as_u16()).into());
        }
        let mut bytes = Vec::new();
        response.take(1024 * 1024 + 1).read_to_end(&mut bytes)?;
        if bytes.len() > 1024 * 1024 {
            return Err("Cloudflare response exceeded the size limit".into());
        }
        let value: Value =
            serde_json::from_slice(&bytes).map_err(|_| "Invalid Cloudflare response")?;
        if value.get("success") != Some(&Value::Bool(true)) {
            return Err(
                "Cloudflare rejected the operation; check token permissions and domain settings"
                    .into(),
            );
        }
        value
            .get("result")
            .cloned()
            .ok_or_else(|| "Cloudflare response has no result".into())
    }

    fn wait_connected(&self, account: &str, tunnel: Uuid) -> Result<()> {
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        while std::time::Instant::now() < deadline {
            let connections = self.call(
                Method::GET,
                &format!("accounts/{account}/cfd_tunnel/{tunnel}/connections"),
                None,
            )?;
            if connections
                .as_array()
                .is_some_and(|values| !values.is_empty())
            {
                return Ok(());
            }
            std::thread::sleep(Duration::from_secs(1));
        }
        Err("The connector did not connect to Cloudflare. Check outbound port 7844; previous local settings will be restored".into())
    }

    fn remove_tunnel(&self, account: &str, tunnel: Uuid) -> Result<()> {
        let path = format!("accounts/{account}/cfd_tunnel/{tunnel}");
        // A newly stopped connector may still be registered at the edge.
        let _ = self.call(Method::DELETE, &format!("{path}/connections"), None);
        self.call(Method::DELETE, &path, None)?;
        Ok(())
    }

    fn provision(
        &self,
        request: &ConnectRequest,
        apply: impl FnOnce(Config, Value, &str, Uuid) -> Result<()>,
    ) -> Result<()> {
        request.validate()?;
        let zone: Zone = serde_json::from_value(self.call(
            Method::GET,
            &format!("zones/{}", request.zone_id),
            None,
        )?)?;
        if zone.id != request.zone_id || !identifier(&zone.account.id) || zone.status != "active" {
            return Err("Select an active Cloudflare domain with access to its account".into());
        }
        if request.hostname != zone.name && !request.hostname.ends_with(&format!(".{}", zone.name))
        {
            return Err("The public hostname must belong to the selected Cloudflare domain".into());
        }
        let dns_path = format!("zones/{}/dns_records", zone.id);
        let query = format!("{dns_path}?name={}", request.hostname);
        let records = self.call(Method::GET, &query, None)?;
        if !records.as_array().is_some_and(Vec::is_empty) {
            return Err("A DNS record already uses this hostname. Choose an unused subdomain; existing records are never overwritten".into());
        }
        let secret = base64::engine::general_purpose::STANDARD
            .encode([*Uuid::new_v4().as_bytes(), *Uuid::new_v4().as_bytes()].concat());
        let name = format!(
            "firecrab-{}-{}",
            request
                .hostname
                .chars()
                .take(50)
                .collect::<String>()
                .replace('.', "-"),
            &Uuid::new_v4().simple().to_string()[..8]
        );
        let tunnel = self.call(
            Method::POST,
            &format!("accounts/{}/cfd_tunnel", zone.account.id),
            Some(json!({"name":name,"config_src":"local","tunnel_secret":secret})),
        )?;
        let tunnel = tunnel
            .get("id")
            .and_then(Value::as_str)
            .and_then(|value| Uuid::parse_str(value).ok())
            .filter(|value| !value.is_nil())
            .ok_or("Cloudflare returned an invalid tunnel identifier")?;
        let mut dns_id = None;
        let setup = (|| {
            let dns = self.call(Method::POST, &dns_path, Some(json!({"type":"CNAME","name":request.hostname,"content":format!("{tunnel}.cfargotunnel.com"),"proxied":true,"ttl":1,"comment":format!("Managed by Firecrab: {name}")})))?;
            dns_id = dns
                .get("id")
                .and_then(Value::as_str)
                .filter(|value| identifier(value))
                .map(str::to_owned);
            if dns_id.is_none() {
                return Err("Cloudflare returned an invalid DNS record identifier".into());
            }
            let credentials =
                json!({"AccountTag":zone.account.id,"TunnelID":tunnel,"TunnelSecret":secret});
            let mut config = request.config.clone();
            config.tunnel = Some(Tunnel {
                id: tunnel,
                credentials_file: "credentials.json".into(),
                hostname: Some(request.hostname.clone()),
            });
            apply(config, credentials, &zone.account.id, tunnel)
        })();
        if let Err(error) = setup {
            // On an ambiguous POST failure, find only this transaction's record.
            let mut dns_known = true;
            if dns_id.is_none() {
                match self.call(Method::GET, &query, None) {
                    Ok(records) => {
                        if let Some(records) = records.as_array() {
                            let target = format!("{tunnel}.cfargotunnel.com");
                            let marker = format!("Managed by Firecrab: {name}");
                            let owned = records.iter().find(|record| {
                                record
                                    .get("content")
                                    .and_then(Value::as_str)
                                    .map(|s| s.trim_end_matches('.'))
                                    == Some(target.as_str())
                                    && record.get("comment").and_then(Value::as_str)
                                        == Some(marker.as_str())
                            });
                            dns_id = owned
                                .and_then(|record| record.get("id"))
                                .and_then(Value::as_str)
                                .filter(|id| identifier(id))
                                .map(str::to_owned);
                            dns_known = owned.is_none() || dns_id.is_some();
                        } else {
                            dns_known = false;
                        }
                    }
                    Err(_) => dns_known = false,
                }
            }
            let dns_removed = dns_known
                && dns_id.as_ref().is_none_or(|id| {
                    self.call(Method::DELETE, &format!("{dns_path}/{id}"), None)
                        .is_ok()
                });
            let tunnel_removed = self.remove_tunnel(&zone.account.id, tunnel).is_ok();
            if !dns_removed || !tunnel_removed {
                return Err(format!("{error}; Cloudflare cleanup was incomplete. Inspect hostname {} and tunnel {tunnel} in Cloudflare before retrying", request.hostname).into());
            }
            return Err(format!("{error}; newly-created Cloudflare resources removed").into());
        }
        Ok(())
    }
}

#[cfg(target_os = "linux")]
pub(super) fn connect() -> Result<()> {
    use super::host;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    host::require_root()?;
    let _lock = host::lock()?;
    let mut bytes = Vec::new();
    std::io::stdin()
        .take(48 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 48 * 1024 {
        return Err("Cloudflare request exceeds 48 KiB".into());
    }
    let mut request: ConnectRequest = serde_json::from_slice(&bytes)?;
    request.validate()?;
    let snapshot = host::snapshot(true)?;
    if snapshot
        .config
        .as_ref()
        .is_some_and(|config| config.tunnel.is_some())
    {
        return Err("A tunnel is already configured. Keep it, or remove its local configuration before creating another".into());
    }
    if !snapshot.nginx_available {
        return Err("Install Nginx before connecting Cloudflare".into());
    }
    // The lock covers setup. Use the latest saved routes and connector peers.
    if let Some(mut current) = snapshot.config {
        if !current
            .routes
            .iter()
            .any(|route| route.host == request.hostname)
        {
            current.routes.extend(
                request
                    .config
                    .routes
                    .iter()
                    .filter(|route| route.host == request.hostname)
                    .cloned(),
            );
        }
        request.config = current;
        request.validate()?;
    }
    let client = Cloudflare::new(&request)?;
    if !snapshot.cloudflared_available {
        super::engines::install()?;
    }
    client.provision(&request, |mut config, credentials, account, tunnel| {
        let directory = tempfile::tempdir()?;
        let file = directory.path().join("credentials.json");
        fs::write(&file, serde_json::to_vec(&credentials)?)?;
        fs::set_permissions(&file, fs::Permissions::from_mode(0o600))?;
        config.tunnel.as_mut().unwrap().credentials_file = file;
        let file = directory.path().join("pipe.json");
        fs::write(&file, serde_json::to_vec(&config)?)?;
        host::apply_locked(&file, None, None, || client.wait_connected(account, tunnel))
    })?;
    println!("Cloudflare connected: https://{}", request.hostname);
    Ok(())
}

#[cfg(test)]
#[path = "cloudflare_tests.rs"]
mod tests;

#[cfg(target_os = "linux")]
pub(super) fn disconnect() -> Result<()> {
    use super::host;
    host::require_root()?;
    let _lock = host::lock()?;
    let manifest = crate::service::env::ServiceEnv::from_process_env()
        .confdir
        .join("pipe/manifest.json");
    let mut config = Config::read(&manifest)?;
    if config.tunnel.is_none() {
        return Ok(());
    }
    config.tunnel = None;
    let directory = tempfile::tempdir()?;
    let file = directory.path().join("pipe.json");
    std::fs::write(&file, serde_json::to_vec(&config)?)?;
    host::apply_quick_locked(&file)
}
