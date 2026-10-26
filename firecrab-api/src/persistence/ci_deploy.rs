//! Token hashes and CI request identity commit with the existing Deploy journal.
use super::{PersistenceError, Store};
use firecrab_api_types::{CiDeployRequest, CiDeployStatus, DeployToken, MicroServiceDeployment};
use rusqlite::{Connection, OptionalExtension, params};
use uuid::Uuid;

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS service_deploy_tokens(
 id TEXT PRIMARY KEY, service_id TEXT NOT NULL, metadata TEXT NOT NULL, secret_hash BLOB NOT NULL
) STRICT;
CREATE INDEX IF NOT EXISTS service_deploy_token_owner ON service_deploy_tokens(service_id);
CREATE TABLE IF NOT EXISTS service_ci_requests(
 operation_id TEXT PRIMARY KEY, service_id TEXT NOT NULL, token_id TEXT NOT NULL, request TEXT NOT NULL, outcome TEXT
) STRICT;
";

pub(crate) struct SavedToken {
    pub metadata: DeployToken,
    pub hash: Vec<u8>,
}

#[derive(Clone)]
pub(crate) struct Admission {
    pub token_id: Uuid,
    pub request: CiDeployRequest,
}

fn decode<T: serde::de::DeserializeOwned>(id: Uuid, raw: &str) -> Result<T, PersistenceError> {
    serde_json::from_str(raw).map_err(|_| PersistenceError::CorruptRecord {
        id: id.to_string(),
        reason: "invalid CI Deploy record".into(),
    })
}

fn token(conn: &Connection, id: Uuid) -> Result<Option<SavedToken>, PersistenceError> {
    let row: Option<(String, Vec<u8>)> = conn
        .query_row(
            "SELECT metadata,secret_hash FROM service_deploy_tokens WHERE id=?1",
            [id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    row.map(|(raw, hash)| {
        Ok(SavedToken {
            metadata: decode(id, &raw)?,
            hash,
        })
    })
    .transpose()
}

/// Runs before journal writes in the same transaction. Expiry/revocation cannot race acceptance.
pub(super) fn admit(
    conn: &Connection,
    deployment: &MicroServiceDeployment,
    admission: &Admission,
) -> Result<(), PersistenceError> {
    let token = token(conn, admission.token_id)?
        .ok_or(PersistenceError::CiDeployUnauthorized)?
        .metadata;
    let previous = deployment
        .update
        .as_ref()
        .ok_or(PersistenceError::CiDeployUnauthorized)?;
    let mut expected = previous.previous.spec.clone();
    expected.image = admission.request.image.clone();
    let blocked:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM cluster_service_workers WHERE id=?1) OR EXISTS(SELECT 1 FROM stack_bindings b WHERE b.kind='service' AND b.resource_id=?1 AND (EXISTS(SELECT 1 FROM stack_git_sources g WHERE g.stack_id=b.stack_id) OR EXISTS(SELECT 1 FROM stack_applies a WHERE a.stack_id=b.stack_id AND a.state IN ('queued','running'))))",[deployment.service_id.to_string()],|r|r.get(0))?;
    let current: Option<String> = conn
        .query_row(
            "SELECT deployment FROM micro_service_deployments WHERE service_id=?1",
            [deployment.service_id.to_string()],
            |r| r.get(0),
        )
        .optional()?;
    let current = current
        .map(|raw| decode::<MicroServiceDeployment>(deployment.service_id, &raw))
        .transpose()?
        .ok_or(PersistenceError::CiDeployUnauthorized)?;
    if token.expires_at <= chrono::Utc::now().timestamp()
        || blocked
        || current.id != admission.request.expected_release_id
        || !crate::ci_deploy::pinned(&expected.image)
        || token.service_id != deployment.service_id
        || deployment.id != admission.request.operation_id
        || previous.previous.id != admission.request.expected_release_id
        || previous.strategy != admission.request.strategy
        || deployment.spec != expected
        || crate::ci_deploy::repository(&expected.image).as_deref()
            != Some(token.repository.as_str())
        || crate::ci_deploy::repository(&previous.previous.spec.image).as_deref()
            != Some(token.repository.as_str())
    {
        return Err(PersistenceError::CiDeployUnauthorized);
    }
    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM micro_service_attempts WHERE id=?1)",
        [deployment.id.to_string()],
        |row| row.get(0),
    )?;
    if exists {
        return Err(PersistenceError::DeployQueueIdentity);
    }
    let request = serde_json::to_string(&admission.request).expect("CI request serialization");
    conn.execute(
        "INSERT INTO service_ci_requests (operation_id,service_id,token_id,request) VALUES (?1,?2,?3,?4)",
        params![
            deployment.id.to_string(),
            deployment.service_id.to_string(),
            admission.token_id.to_string(),
            request
        ],
    )
    .map_err(|_| PersistenceError::DeployQueueIdentity)?;
    Ok(())
}

/// Keep the first terminal outcome after Stop, history pruning and operator recovery.
pub(super) fn observe(
    conn: &Connection,
    record: &MicroServiceDeployment,
) -> Result<(), PersistenceError> {
    let raw:Option<String>=conn.query_row("SELECT request FROM service_ci_requests WHERE operation_id=?1 AND service_id=?2 AND outcome IS NULL",params![record.id.to_string(),record.service_id.to_string()],|r|r.get(0)).optional()?;
    let Some(raw) = raw else {
        return Ok(());
    };
    let request: CiDeployRequest = decode(record.id, &raw)?;
    let outcome = crate::ci_deploy::outcome(record, &request);
    if outcome.completed {
        let text = serde_json::to_string(&outcome).expect("redacted CI outcome serialization");
        conn.execute(
            "UPDATE service_ci_requests SET outcome=?2 WHERE operation_id=?1 AND outcome IS NULL",
            params![record.id.to_string(), text],
        )?;
    }
    Ok(())
}

impl Store {
    pub(crate) fn ci_result(
        &self,
        service: Uuid,
        token: Uuid,
        operation: Uuid,
    ) -> Result<Option<CiDeployStatus>, PersistenceError> {
        let raw:Option<String>=self.lock().query_row("SELECT outcome FROM service_ci_requests WHERE operation_id=?1 AND service_id=?2 AND token_id=?3 AND outcome IS NOT NULL",params![operation.to_string(),service.to_string(),token.to_string()],|r|r.get(0)).optional()?;
        raw.map(|raw| decode(operation, &raw)).transpose()
    }
    pub(crate) fn deploy_token(&self, id: Uuid) -> Result<Option<SavedToken>, PersistenceError> {
        token(&self.lock(), id)
    }

    pub(crate) fn deploy_tokens(
        &self,
        service: Uuid,
    ) -> Result<Vec<DeployToken>, PersistenceError> {
        let conn = self.lock();
        let mut query = conn.prepare(
            "SELECT metadata FROM service_deploy_tokens WHERE service_id=?1 ORDER BY rowid DESC",
        )?;
        query
            .query_map([service.to_string()], |row| row.get::<_, String>(0))?
            .map(|row| decode(service, &row?))
            .collect()
    }

    pub(crate) fn insert_deploy_token(
        &self,
        metadata: &DeployToken,
        hash: &[u8],
    ) -> Result<(), PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let count: u32 = tx.query_row(
            "SELECT count(*) FROM service_deploy_tokens WHERE service_id=?1",
            [metadata.service_id.to_string()],
            |r| r.get(0),
        )?;
        if count >= 20 {
            return Err(PersistenceError::CiTokenLimit);
        }
        let raw = serde_json::to_string(metadata).expect("token metadata serialization");
        tx.execute(
            "INSERT INTO service_deploy_tokens VALUES (?1,?2,?3,?4)",
            params![
                metadata.id.to_string(),
                metadata.service_id.to_string(),
                raw,
                hash
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn revoke_deploy_token(
        &self,
        service: Uuid,
        token: Uuid,
    ) -> Result<(), PersistenceError> {
        self.lock().execute(
            "DELETE FROM service_deploy_tokens WHERE id=?1 AND service_id=?2",
            params![token.to_string(), service.to_string()],
        )?;
        Ok(())
    }

    pub(crate) fn ci_request(
        &self,
        service: Uuid,
        token: Uuid,
        operation: Uuid,
    ) -> Result<Option<CiDeployRequest>, PersistenceError> {
        let raw: Option<String> = self.lock().query_row("SELECT request FROM service_ci_requests WHERE operation_id=?1 AND service_id=?2 AND token_id=?3", params![operation.to_string(),service.to_string(),token.to_string()], |r| r.get(0)).optional()?;
        raw.map(|raw| decode(operation, &raw)).transpose()
    }
}
