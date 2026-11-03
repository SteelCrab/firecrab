//! Models the VM's own Codex offers in its model picker.
use super::*;
use std::{collections::HashMap, sync::Mutex};

#[cfg(all(test, unix))]
mod tests;

impl Runtime {
    pub fn codex_models(&self) -> Result<Value, Error> {
        type Cached = (Instant, Value);
        static CACHE: std::sync::OnceLock<Mutex<HashMap<PathBuf, Cached>>> =
            std::sync::OnceLock::new();
        let guest = self
            .guest
            .as_ref()
            .ok_or_else(|| invalid("Select a MicroVM first."))?;
        if !guest.available.contains("codex") {
            return Err(invalid("Install Codex in this MicroVM first."));
        }
        let cache = CACHE.get_or_init(Default::default);
        if let Some((at, models)) = cache
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&guest.home)
            && at.elapsed() < Duration::from_secs(600)
        {
            return Ok(json!({"models": models}));
        }
        // Without `--bundled`, Codex reads its cached account catalog and goes
        // online only when it has none. The catalog carries model instructions,
        // so it is far larger than a plain shell reply.
        let output = guest.shell_capture(
            "timeout 10 codex debug models",
            Duration::from_secs(15),
            8 << 20,
        )?;
        let models = parse_codex_models(&output)?;
        cache
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(guest.home.clone(), (Instant::now(), models.clone()));
        Ok(json!({"models": models}))
    }
}

/// Codex shows a model in its picker when its visibility is `list`, ordered by priority.
pub(super) fn parse_codex_models(text: &str) -> Result<Value, Error> {
    let catalog: Value = serde_json::from_str(text)?;
    let entries = catalog["models"]
        .as_array()
        .ok_or_else(|| invalid("Codex returned no model catalog."))?;
    fn usable(value: &Value) -> Option<&str> {
        value
            .as_str()
            .filter(|v| !v.is_empty() && v.len() <= 128 && !v.contains(char::is_control))
    }
    let mut listed: Vec<_> = entries
        .iter()
        .filter(|entry| entry["visibility"] == "list")
        .filter_map(|entry| {
            let id = usable(&entry["slug"])?;
            let name = usable(&entry["display_name"]).unwrap_or(id);
            Some((entry["priority"].as_i64().unwrap_or(i64::MAX), id, name))
        })
        .collect();
    listed.sort_by_key(|entry| entry.0);
    let mut seen = std::collections::HashSet::new();
    listed.retain(|entry| seen.insert(entry.1));
    Ok(listed
        .into_iter()
        .map(|(_, id, name)| json!({"id": id, "name": name}))
        .collect())
}
