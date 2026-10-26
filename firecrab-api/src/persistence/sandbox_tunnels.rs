//! Durable preview ownership claims; tombstones retain retirement evidence.
use super::{PersistenceError, Store};
use firecrab_api_types::SandboxTunnelBinding;
use rusqlite::{OptionalExtension, params};
use uuid::Uuid;

pub(super) const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS ai_sandbox_tunnels (
    id TEXT PRIMARY KEY, project_id TEXT NOT NULL, subdomain TEXT UNIQUE,
    task_id TEXT NOT NULL, attempt_id TEXT NOT NULL, spec TEXT NOT NULL
) STRICT;";

fn corrupt() -> PersistenceError {
    PersistenceError::CorruptRecord {
        id: "sandbox_tunnel".into(),
        reason: "invalid preview binding".into(),
    }
}
fn decode(text: String) -> Result<SandboxTunnelBinding, PersistenceError> {
    serde_json::from_str(&text).map_err(|_| corrupt())
}

impl Store {
    pub fn sandbox_project_has_live_tunnels(
        &self,
        project: Uuid,
    ) -> Result<bool, PersistenceError> {
        Ok(self
            .sandbox_tunnels(project)?
            .iter()
            .any(|binding| binding.state != firecrab_api_types::SandboxTunnelState::Deleted))
    }
    pub fn sandbox_tunnel(
        &self,
        id: Uuid,
    ) -> Result<Option<SandboxTunnelBinding>, PersistenceError> {
        self.lock()
            .query_row(
                "SELECT spec FROM ai_sandbox_tunnels WHERE id=?1",
                [id.to_string()],
                |r| r.get(0),
            )
            .optional()?
            .map(decode)
            .transpose()
    }
    pub fn sandbox_tunnels(
        &self,
        project: Uuid,
    ) -> Result<Vec<SandboxTunnelBinding>, PersistenceError> {
        let conn = self.lock();
        let mut query = conn.prepare(
            "SELECT spec FROM ai_sandbox_tunnels WHERE project_id=?1 ORDER BY id LIMIT 256",
        )?;
        query
            .query_map([project.to_string()], |r| r.get(0))?
            .map(|r| decode(r?))
            .collect()
    }
    pub fn sandbox_tunnel_claim(&self, subdomain: &str) -> Result<Option<Uuid>, PersistenceError> {
        let value: Option<String> = self
            .lock()
            .query_row(
                "SELECT id FROM ai_sandbox_tunnels WHERE subdomain=?1",
                [subdomain],
                |r| r.get(0),
            )
            .optional()?;
        value
            .map(|v| Uuid::parse_str(&v).map_err(|_| corrupt()))
            .transpose()
    }
    /// Identity columns never change when the binding's observable state changes.
    pub fn save_sandbox_tunnel(
        &self,
        binding: &SandboxTunnelBinding,
    ) -> Result<(), PersistenceError> {
        let spec = serde_json::to_string(binding).map_err(|_| corrupt())?;
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let existing: Option<String> = tx
            .query_row(
                "SELECT spec FROM ai_sandbox_tunnels WHERE id=?1",
                [binding.id.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(existing) = existing {
            let saved = decode(existing)?;
            if saved.project_id != binding.project_id
                || saved.spec.subdomain != binding.spec.subdomain
                || saved.task_id != binding.task_id
                || saved.attempt_id != binding.attempt_id
                || saved.runtime != binding.runtime
                || saved.owner != binding.owner
            {
                return Err(corrupt());
            }
        }
        let claim = (binding.state != firecrab_api_types::SandboxTunnelState::Deleted)
            .then_some(&binding.spec.subdomain);
        tx.execute("INSERT INTO ai_sandbox_tunnels(id,project_id,subdomain,task_id,attempt_id,spec) VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(id) DO UPDATE SET subdomain=excluded.subdomain,spec=excluded.spec",
            params![binding.id.to_string(),binding.project_id.to_string(),claim,binding.task_id.to_string(),binding.attempt_id.to_string(),spec])?;
        tx.commit()?;
        Ok(())
    }
}
