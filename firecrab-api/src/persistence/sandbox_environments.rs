//! Immutable environment identity and an append-only resource ownership ledger.
use super::{PersistenceError, Store};
use crate::ai_sandbox::SavedTask;
use firecrab_api_types::{SandboxEnvironment, SandboxEnvironmentState};
use rusqlite::{OptionalExtension, params};
use uuid::Uuid;

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS ai_sandbox_environments (
 id TEXT PRIMARY KEY, project_id TEXT NOT NULL, generation TEXT NOT NULL UNIQUE,
 state TEXT NOT NULL, expires_at INTEGER, spec TEXT NOT NULL
) STRICT;
CREATE INDEX IF NOT EXISTS ai_sandbox_environment_project ON ai_sandbox_environments(project_id);
CREATE TABLE IF NOT EXISTS ai_sandbox_environment_resources (
 id TEXT PRIMARY KEY, environment_id TEXT NOT NULL, generation TEXT NOT NULL,
 kind TEXT NOT NULL, role TEXT NOT NULL,
 UNIQUE(environment_id,kind,role)
) STRICT;
";

fn corrupt() -> PersistenceError {
    PersistenceError::CorruptRecord {
        id: "ai_sandbox_environment".into(),
        reason: "invalid environment or resource ownership record".into(),
    }
}

fn decode(value: String) -> Result<SandboxEnvironment, PersistenceError> {
    serde_json::from_str(&value).map_err(|_| corrupt())
}

impl Store {
    /// Managed VM UUIDs cannot be rebound through the borrowed-VM project entry point.
    pub fn sandbox_environment_owns_vm(&self, id: Uuid) -> Result<bool, PersistenceError> {
        Ok(self.lock().query_row("SELECT EXISTS(SELECT 1 FROM ai_sandbox_environment_resources WHERE id=?1 AND kind='vm')", [id.to_string()], |row| row.get(0))?)
    }
    /// Read a manifest and check every reserved resource against its ownership ledger.
    pub fn sandbox_environment(
        &self,
        id: Uuid,
    ) -> Result<Option<SandboxEnvironment>, PersistenceError> {
        let conn = self.lock();
        let row: Option<(String, String, String)> = conn
            .query_row(
                "SELECT project_id,generation,spec FROM ai_sandbox_environments WHERE id=?1",
                [id.to_string()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let Some((project, generation, json)) = row else {
            return Ok(None);
        };
        let saved = decode(json)?;
        if saved.roles.len() != saved.spec.roles.len()
            || saved
                .roles
                .iter()
                .zip(&saved.spec.roles)
                .any(|(reserved, spec)| reserved.role != spec.name)
        {
            return Err(corrupt());
        }
        if saved.id != id
            || saved.project_id.to_string() != project
            || saved.generation.to_string() != generation
        {
            return Err(corrupt());
        }
        for (kind, role, resource) in resources(&saved) {
            let owned: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM ai_sandbox_environment_resources WHERE id=?1 AND environment_id=?2 AND generation=?3 AND kind=?4 AND role=?5)",
                params![resource.to_string(),id.to_string(),generation,kind,role],|r|r.get(0),
            )?;
            if !owned {
                return Err(corrupt());
            }
        }
        Ok(Some(saved))
    }

    /// List only one project's durable manifests.
    pub fn sandbox_environments(
        &self,
        project: Uuid,
    ) -> Result<Vec<SandboxEnvironment>, PersistenceError> {
        let ids = self
            .lock()
            .prepare("SELECT id FROM ai_sandbox_environments WHERE project_id=?1 ORDER BY rowid")?
            .query_map([project.to_string()], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        ids.into_iter()
            .map(|id| {
                let id = Uuid::parse_str(&id).map_err(|_| corrupt())?;
                self.sandbox_environment(id)?.ok_or_else(corrupt)
            })
            .collect()
    }

    /// Reserve all identities atomically before provisioning any host resource.
    pub fn reserve_sandbox_environment(
        &self,
        saved: &SandboxEnvironment,
    ) -> Result<(), PersistenceError> {
        let json = serde_json::to_string(saved).map_err(|_| corrupt())?;
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        tx.execute("INSERT INTO ai_sandbox_environments(id,project_id,generation,state,expires_at,spec) VALUES(?1,?2,?3,?4,?5,?6)",
            params![saved.id.to_string(),saved.project_id.to_string(),saved.generation.to_string(),state(saved.state),saved.expires_at.map(|v|v as i64),json])?;
        for (kind, role, id) in resources(saved) {
            tx.execute("INSERT INTO ai_sandbox_environment_resources(id,environment_id,generation,kind,role) VALUES(?1,?2,?3,?4,?5)",
                params![id.to_string(),saved.id.to_string(),saved.generation.to_string(),kind,role])?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Only observations/lifecycle change. Resource IDs, owner and manifest stay immutable.
    pub fn save_sandbox_environment(
        &self,
        saved: &SandboxEnvironment,
    ) -> Result<(), PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let previous: String = tx.query_row(
            "SELECT spec FROM ai_sandbox_environments WHERE id=?1",
            [saved.id.to_string()],
            |r| r.get(0),
        )?;
        let previous = decode(previous)?;
        if previous.project_id != saved.project_id
            || previous.generation != saved.generation
            || previous.owner != saved.owner
            || previous.spec != saved.spec
            || previous.network_id != saved.network_id
            || resources(&previous) != resources(saved)
            || previous.created_at != saved.created_at
            || previous.expires_at != saved.expires_at
            || (previous.state == SandboxEnvironmentState::Deleted && saved != &previous)
        {
            return Err(corrupt());
        }
        let json = serde_json::to_string(saved).map_err(|_| corrupt())?;
        tx.execute(
            "UPDATE ai_sandbox_environments SET state=?2,spec=?3 WHERE id=?1",
            params![saved.id.to_string(), state(saved.state), json],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Resolve one exact ownership grant before a destructive lifecycle operation.
    pub fn sandbox_environment_owns(
        &self,
        environment: &SandboxEnvironment,
        kind: &str,
        id: Uuid,
    ) -> Result<bool, PersistenceError> {
        Ok(self.lock().query_row("SELECT EXISTS(SELECT 1 FROM ai_sandbox_environment_resources WHERE id=?1 AND environment_id=?2 AND generation=?3 AND kind=?4)",
            params![id.to_string(),environment.id.to_string(),environment.generation.to_string(),kind],|r|r.get(0))?)
    }

    /// Inspect attempts individually so large retained files are not loaded as one aggregate.
    pub fn sandbox_environment_task_ids(
        &self,
        environment: Uuid,
    ) -> Result<Vec<Uuid>, PersistenceError> {
        self.lock().prepare("SELECT id FROM ai_sandbox_tasks WHERE json_extract(spec,'$.task.environment.id')=?1 ORDER BY rowid")?
            .query_map([environment.to_string()],|r|r.get::<_,String>(0))?
            .map(|r|Uuid::parse_str(&r?).map_err(|_|corrupt())).collect()
    }

    /// Pending cleanup is resumed; TTL only selects non-retained manifests.
    pub fn sandbox_environment_cleanup_ids(&self, now: u64) -> Result<Vec<Uuid>, PersistenceError> {
        self.lock().prepare("SELECT id FROM ai_sandbox_environments WHERE state='cleanup_pending' OR (expires_at IS NOT NULL AND expires_at<=?1 AND state!='deleted') ORDER BY rowid")?
            .query_map([now as i64],|r|r.get::<_,String>(0))?
            .map(|r|Uuid::parse_str(&r?).map_err(|_|corrupt())).collect()
    }

    /// Read one complete task for export verification.
    pub fn sandbox_environment_task(
        &self,
        id: Uuid,
    ) -> Result<Option<SavedTask>, PersistenceError> {
        self.sandbox_task(id)
    }

    /// Desired-running environments refresh readiness; failures need explicit reconciliation.
    pub fn sandbox_environment_provisioning_ids(&self) -> Result<Vec<Uuid>, PersistenceError> {
        self.lock()
            .prepare(
                "SELECT id FROM ai_sandbox_environments WHERE state IN ('provisioning','ready') ORDER BY rowid",
            )?
            .query_map([], |r| r.get::<_, String>(0))?
            .map(|r| Uuid::parse_str(&r?).map_err(|_| corrupt()))
            .collect()
    }
}

fn resources(saved: &SandboxEnvironment) -> Vec<(&'static str, &str, Uuid)> {
    let mut rows = vec![("network", "", saved.network_id)];
    for role in &saved.roles {
        rows.push(("vm", &role.role, role.vm_id));
        rows.push(("security_group", &role.role, role.security_group_id));
    }
    rows
}

fn state(value: SandboxEnvironmentState) -> &'static str {
    match value {
        SandboxEnvironmentState::Provisioning => "provisioning",
        SandboxEnvironmentState::Ready => "ready",
        SandboxEnvironmentState::Stopped => "stopped",
        SandboxEnvironmentState::CleanupPending => "cleanup_pending",
        SandboxEnvironmentState::Deleted => "deleted",
        SandboxEnvironmentState::Failed => "failed",
    }
}
