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
    /// Full Pipe manifests and token registration cannot safely edit an address with a claim.
    pub fn sandbox_tunnel_has_claims(&self) -> Result<bool, PersistenceError> {
        Ok(self.lock().query_row(
            "SELECT EXISTS(SELECT 1 FROM ai_sandbox_tunnels WHERE subdomain IS NOT NULL
             OR (json_extract(spec,'$.successorBindingId') IS NOT NULL
                 AND json_extract(spec,'$.handoffConfirmedAt') IS NULL))",
            [],
            |row| row.get(0),
        )?)
    }
    /// Stopped claims and their handoff ancestors still own the connector's control path.
    pub fn sandbox_tunnel_claims_connector(
        &self,
        connector: Uuid,
    ) -> Result<bool, PersistenceError> {
        Ok(self.lock().query_row(
            "SELECT EXISTS(SELECT 1 FROM ai_sandbox_tunnels b
             WHERE json_extract(b.spec,'$.connectorId')=?1 AND (
                b.subdomain IS NOT NULL
                OR EXISTS(SELECT 1 FROM ai_sandbox_tunnels owner
                    WHERE owner.subdomain=json_extract(b.spec,'$.spec.subdomain'))
                OR (json_extract(b.spec,'$.successorBindingId') IS NOT NULL
                    AND json_extract(b.spec,'$.handoffConfirmedAt') IS NULL)))",
            [connector.to_string()],
            |row| row.get(0),
        )?)
    }
    /// Unknown helper outcomes keep their VM address reserved until explicitly stopped.
    pub fn sandbox_vm_has_live_tunnels(&self, vm: Uuid) -> Result<bool, PersistenceError> {
        Ok(self.lock().query_row(
            "SELECT EXISTS(SELECT 1 FROM ai_sandbox_tunnels b
             WHERE json_extract(b.spec,'$.runtime.vmId')=?1 AND (
                (json_extract(b.spec,'$.state') NOT IN ('stopped','deleted','superseded')
                 AND (json_extract(b.spec,'$.connectorId') IS NOT NULL
                      OR json_extract(b.spec,'$.appliedTarget') IS NOT NULL))
                OR (json_extract(b.spec,'$.state')='superseded'
                    AND json_extract(b.spec,'$.successorBindingId') IS NOT NULL
                    AND json_extract(b.spec,'$.handoffConfirmedAt') IS NULL)
                OR EXISTS(SELECT 1 FROM ai_sandbox_tunnels p
                    WHERE p.id=json_extract(b.spec,'$.previousBindingId')
                    AND json_extract(p.spec,'$.successorBindingId')=b.id
                    AND json_extract(p.spec,'$.handoffConfirmedAt') IS NULL)))",
            [vm.to_string()],
            |row| row.get(0),
        )?)
    }
    pub fn sandbox_project_has_live_tunnels(
        &self,
        project: Uuid,
    ) -> Result<bool, PersistenceError> {
        Ok(self.lock().query_row("SELECT EXISTS(SELECT 1 FROM ai_sandbox_tunnels WHERE project_id=?1 AND (json_extract(spec,'$.state') NOT IN ('deleted','superseded') OR (json_extract(spec,'$.successorBindingId') IS NOT NULL AND json_extract(spec,'$.handoffConfirmedAt') IS NULL)))",[project.to_string()],|r|r.get(0))?)
    }
    /// Follow only unconfirmed predecessor links, including records written before this guard.
    pub fn sandbox_tunnel_handoff(
        &self,
        binding: &SandboxTunnelBinding,
    ) -> Result<Vec<SandboxTunnelBinding>, PersistenceError> {
        pending_predecessors(&self.lock(), binding)
    }
    /// Release predecessor VM holds only after the caller has checked actual helper evidence.
    /// Verify the persisted successor observation before confirming predecessors in one transaction.
    pub fn confirm_sandbox_tunnel_handoff(
        &self,
        binding: &SandboxTunnelBinding,
    ) -> Result<(), PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let saved: String = tx.query_row(
            "SELECT spec FROM ai_sandbox_tunnels WHERE id=?1",
            [binding.id.to_string()],
            |r| r.get(0),
        )?;
        if decode(saved)? != *binding {
            return Err(corrupt());
        }
        for mut previous in pending_predecessors(&tx, binding)? {
            previous.handoff_confirmed_at = Some(binding.updated_at);
            previous.updated_at = binding
                .updated_at
                .max(previous.updated_at.saturating_add(1));
            let spec = serde_json::to_string(&previous).map_err(|_| corrupt())?;
            tx.execute(
                "UPDATE ai_sandbox_tunnels SET spec=?2 WHERE id=?1",
                params![previous.id.to_string(), spec],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
    /// Operator-only inventory retains remote-retirement evidence after project deletion.
    pub fn sandbox_tunnel_inventory(&self) -> Result<Vec<SandboxTunnelBinding>, PersistenceError> {
        self.lock()
            .prepare("SELECT spec FROM ai_sandbox_tunnels ORDER BY rowid DESC")?
            .query_map([], |r| r.get(0))?
            .map(|r| decode(r?))
            .collect()
    }
    /// Transfer the one address claim atomically; neither attempt/runtime identity changes.
    pub fn supersede_sandbox_tunnel(
        &self,
        previous: &SandboxTunnelBinding,
        next: &SandboxTunnelBinding,
    ) -> Result<(), PersistenceError> {
        use firecrab_api_types::SandboxTunnelState;
        if previous.id == next.id
            || previous.project_id != next.project_id
            || previous.spec.subdomain != next.spec.subdomain
            || previous.state != SandboxTunnelState::Superseded
            || previous.successor_binding_id != Some(next.id)
            || previous.handoff_confirmed_at.is_some()
            || next.previous_binding_id != Some(previous.id)
            || next.task_id != next.spec.task_id
            || next.handoff_confirmed_at.is_some()
        {
            return Err(corrupt());
        }
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let current: String = tx.query_row(
            "SELECT spec FROM ai_sandbox_tunnels WHERE id=?1",
            [previous.id.to_string()],
            |r| r.get(0),
        )?;
        let current = decode(current)?;
        if matches!(
            current.state,
            SandboxTunnelState::Deleted
                | SandboxTunnelState::Superseded
                | SandboxTunnelState::Deleting
        ) || current.project_id != previous.project_id
            || current.task_id != previous.task_id
            || current.attempt_id != previous.attempt_id
            || current.runtime != previous.runtime
            || current.owner != previous.owner
            || current.spec != previous.spec
            || !pending_predecessors(&tx, &current)?.is_empty()
        {
            return Err(corrupt());
        }
        let old = serde_json::to_string(previous).map_err(|_| corrupt())?;
        let new = serde_json::to_string(next).map_err(|_| corrupt())?;
        tx.execute(
            "UPDATE ai_sandbox_tunnels SET subdomain=NULL,spec=?2 WHERE id=?1",
            params![previous.id.to_string(), old],
        )?;
        tx.execute("INSERT INTO ai_sandbox_tunnels(id,project_id,subdomain,task_id,attempt_id,spec) VALUES(?1,?2,?3,?4,?5,?6)",params![next.id.to_string(),next.project_id.to_string(),next.spec.subdomain,next.task_id.to_string(),next.attempt_id.to_string(),new])?;
        tx.commit()?;
        Ok(())
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
            "SELECT spec FROM ai_sandbox_tunnels WHERE project_id=?1 ORDER BY json_extract(spec,'$.state')='deleted',rowid DESC LIMIT 256",
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
        if binding.task_id != binding.spec.task_id {
            return Err(corrupt());
        }
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
            if matches!(
                saved.state,
                firecrab_api_types::SandboxTunnelState::Deleted
                    | firecrab_api_types::SandboxTunnelState::Superseded
            ) && saved != *binding
            {
                return Err(corrupt());
            }
            if saved.project_id != binding.project_id
                || saved.spec.subdomain != binding.spec.subdomain
                || saved.task_id != binding.task_id
                || saved.attempt_id != binding.attempt_id
                || saved.runtime != binding.runtime
                || saved.owner != binding.owner
                || saved.previous_binding_id != binding.previous_binding_id
                || saved.successor_binding_id != binding.successor_binding_id
                || saved.handoff_confirmed_at != binding.handoff_confirmed_at
            {
                return Err(corrupt());
            }
        }
        let claim = (!matches!(
            binding.state,
            firecrab_api_types::SandboxTunnelState::Deleted
                | firecrab_api_types::SandboxTunnelState::Superseded
        ))
        .then_some(&binding.spec.subdomain);
        tx.execute("INSERT INTO ai_sandbox_tunnels(id,project_id,subdomain,task_id,attempt_id,spec) VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(id) DO UPDATE SET subdomain=excluded.subdomain,spec=excluded.spec",
            params![binding.id.to_string(),binding.project_id.to_string(),claim,binding.task_id.to_string(),binding.attempt_id.to_string(),spec])?;
        tx.commit()?;
        Ok(())
    }
}

/// Validate every pending edge before using historical bindings as control authority.
fn pending_predecessors(
    conn: &rusqlite::Connection,
    binding: &SandboxTunnelBinding,
) -> Result<Vec<SandboxTunnelBinding>, PersistenceError> {
    let mut next = binding.clone();
    let mut seen = std::collections::BTreeSet::from([next.id]);
    let mut pending = Vec::new();
    while let Some(id) = next.previous_binding_id {
        if !seen.insert(id) || seen.len() > 256 {
            return Err(corrupt());
        }
        let spec: String = conn.query_row(
            "SELECT spec FROM ai_sandbox_tunnels WHERE id=?1",
            [id.to_string()],
            |r| r.get(0),
        )?;
        let previous = decode(spec)?;
        if previous.id != id
            || previous.project_id != next.project_id
            || previous.spec.subdomain != next.spec.subdomain
            || previous.successor_binding_id != Some(next.id)
            || previous.state != firecrab_api_types::SandboxTunnelState::Superseded
        {
            return Err(corrupt());
        }
        if previous.handoff_confirmed_at.is_some() {
            break;
        }
        next = previous.clone();
        pending.push(previous);
    }
    Ok(pending)
}
