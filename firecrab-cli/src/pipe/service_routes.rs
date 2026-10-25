//! Service domains mutate the latest manifest while holding the shared Pipe lock.

use firecrab_helper_protocol::pipe::{Config, ServiceRouteRequest};
use std::io::Read;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

pub(super) fn manage() -> Result<()> {
    super::host::require_root()?;
    let mut bytes = Vec::new();
    std::io::stdin()
        .take(48 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 48 * 1024 {
        return Err("service route request exceeds 48 KiB".into());
    }
    let request: ServiceRouteRequest = serde_json::from_slice(&bytes)?;
    request.validate()?;
    let _lock = super::host::lock()?;
    let manifest = crate::service::env::ServiceEnv::from_process_env()
        .confdir
        .join("pipe/manifest.json");
    let config = if manifest.is_file() {
        Config::read(&manifest)?
    } else {
        serde_json::from_value(serde_json::json!({"routes":[],"tunnel":null}))?
    };
    let next = request.updated_config(&config)?;
    // A retry must reconcile Nginx even if the manifest already changed before
    // an interrupted reload. Only a host with no saved Pipe state is a no-op.
    if !manifest.is_file() && matches!(request, ServiceRouteRequest::Remove { .. }) {
        return Ok(());
    }
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("pipe.json");
    std::fs::write(&path, serde_json::to_vec(&next)?)?;
    match request {
        ServiceRouteRequest::Publish { .. } | ServiceRouteRequest::Replace { .. } => {
            super::host::apply_service_locked(&path)
        }
        ServiceRouteRequest::Remove { .. } => super::host::apply_quick_locked(&path),
    }
}
