//! Security groups and ordered rules. Stored parse failures remain errors.

use firecrab_api_types::{SecurityGroupResponse, SecurityGroupRule};
use rusqlite::{Connection, OptionalExtension, params};
use uuid::Uuid;

use super::{PersistenceError, Store};

pub(super) fn migrate(conn: &Connection) -> Result<(), PersistenceError> {
    if !conn
        .prepare("SELECT 1 FROM pragma_table_info('vms') WHERE name = 'security_group_id'")?
        .exists([])?
    {
        conn.execute("ALTER TABLE vms ADD COLUMN security_group_id TEXT", [])?;
    }
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS security_groups (
            id TEXT PRIMARY KEY, micro_network_id TEXT NOT NULL, name TEXT NOT NULL
        ) STRICT;
        CREATE TABLE IF NOT EXISTS security_group_rules (
            group_id TEXT NOT NULL, position INTEGER NOT NULL, rule TEXT NOT NULL,
            PRIMARY KEY (group_id, position)
        ) STRICT;
        CREATE INDEX IF NOT EXISTS vms_security_group ON vms(security_group_id);",
    )?;
    Ok(())
}

impl Store {
    pub fn security_group(
        &self,
        id: Uuid,
    ) -> Result<Option<SecurityGroupResponse>, PersistenceError> {
        read(&self.lock(), id)
    }

    pub fn list_security_groups(&self) -> Result<Vec<SecurityGroupResponse>, PersistenceError> {
        let conn = self.lock();
        let mut statement = conn.prepare("SELECT id FROM security_groups ORDER BY name, id")?;
        let ids = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        ids.into_iter()
            .map(|id| {
                let id = Uuid::parse_str(&id).map_err(|_| corrupt(&id, "invalid group id"))?;
                read(&conn, id)?.ok_or_else(|| corrupt(&id.to_string(), "missing group"))
            })
            .collect()
    }

    pub fn insert_security_group(
        &self,
        group: &SecurityGroupResponse,
    ) -> Result<(), PersistenceError> {
        self.insert_security_group_authority(group, None)
    }

    pub(crate) fn insert_environment_security_group(
        &self,
        environment: &firecrab_api_types::SandboxEnvironment,
        group: &SecurityGroupResponse,
    ) -> Result<(), PersistenceError> {
        self.insert_security_group_authority(group, Some(environment))
    }

    fn insert_security_group_authority(
        &self,
        group: &SecurityGroupResponse,
        parent: Option<&firecrab_api_types::SandboxEnvironment>,
    ) -> Result<(), PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        ensure_writable(&tx, group.id, parent)?;
        tx.execute(
            "INSERT INTO security_groups (id, micro_network_id, name) VALUES (?1, ?2, ?3)",
            params![
                group.id.to_string(),
                group.micro_network_id.to_string(),
                group.name
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn rename_security_group(&self, id: Uuid, name: &str) -> Result<bool, PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        ensure_writable(&tx, id, None)?;
        let changed = tx.execute(
            "UPDATE security_groups SET name = ?2 WHERE id = ?1",
            params![id.to_string(), name],
        )? != 0;
        tx.commit()?;
        Ok(changed)
    }

    pub fn replace_security_group_rules(
        &self,
        id: Uuid,
        rules: &[SecurityGroupRule],
    ) -> Result<bool, PersistenceError> {
        self.replace_security_group_rules_authority(id, rules, None)
    }

    pub(crate) fn replace_environment_security_group_rules(
        &self,
        parent: &firecrab_api_types::SandboxEnvironment,
        id: Uuid,
        rules: &[SecurityGroupRule],
    ) -> Result<bool, PersistenceError> {
        self.replace_security_group_rules_authority(id, rules, Some(parent))
    }

    fn replace_security_group_rules_authority(
        &self,
        id: Uuid,
        rules: &[SecurityGroupRule],
        parent: Option<&firecrab_api_types::SandboxEnvironment>,
    ) -> Result<bool, PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        ensure_writable(&tx, id, parent)?;
        if read(&tx, id)?.is_none() {
            return Ok(false);
        }
        ensure_rules_idle(&tx, &id.to_string())?;
        tx.execute(
            "DELETE FROM security_group_rules WHERE group_id = ?1",
            [id.to_string()],
        )?;
        for (position, rule) in rules.iter().enumerate() {
            let json = serde_json::to_string(rule)
                .map_err(|error| corrupt(&id.to_string(), &error.to_string()))?;
            tx.execute(
                "INSERT INTO security_group_rules (group_id, position, rule) VALUES (?1, ?2, ?3)",
                params![id.to_string(), position as i64, json],
            )?;
        }
        tx.commit()?;
        Ok(true)
    }

    pub fn delete_security_group(&self, id: Uuid) -> Result<bool, PersistenceError> {
        self.delete_security_group_authority(id, None)
    }

    pub(crate) fn delete_environment_security_group(
        &self,
        parent: &firecrab_api_types::SandboxEnvironment,
        id: Uuid,
    ) -> Result<bool, PersistenceError> {
        self.delete_security_group_authority(id, Some(parent))
    }

    fn delete_security_group_authority(
        &self,
        id: Uuid,
        parent: Option<&firecrab_api_types::SandboxEnvironment>,
    ) -> Result<bool, PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        ensure_writable(&tx, id, parent)?;
        let referenced: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM vms WHERE security_group_id = ?1)
             OR EXISTS(SELECT 1 FROM micro_services WHERE json_extract(spec, '$.securityGroupId') = ?1)",
            [id.to_string()], |row| row.get(0))?;
        if referenced {
            return Err(PersistenceError::SecurityGroupInUse);
        }
        ensure_rules_idle(&tx, &id.to_string())?;
        tx.execute(
            "DELETE FROM security_group_rules WHERE group_id = ?1",
            [id.to_string()],
        )?;
        let deleted = tx.execute(
            "DELETE FROM security_groups WHERE id = ?1",
            [id.to_string()],
        )? != 0;
        tx.commit()?;
        Ok(deleted)
    }

    pub fn network_has_security_groups(&self, id: Uuid) -> Result<bool, PersistenceError> {
        Ok(self.lock().query_row(
            "SELECT EXISTS(SELECT 1 FROM security_groups WHERE micro_network_id = ?1)",
            [id.to_string()],
            |row| row.get(0),
        )?)
    }
}

pub(super) fn read(
    conn: &Connection,
    id: Uuid,
) -> Result<Option<SecurityGroupResponse>, PersistenceError> {
    let row: Option<(String, String)> = conn
        .query_row(
            "SELECT micro_network_id, name FROM security_groups WHERE id = ?1",
            [id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((network, name)) = row else {
        return Ok(None);
    };
    let micro_network_id =
        Uuid::parse_str(&network).map_err(|_| corrupt(&id.to_string(), "invalid network id"))?;
    let mut statement = conn
        .prepare("SELECT rule FROM security_group_rules WHERE group_id = ?1 ORDER BY position")?;
    let json = statement
        .query_map([id.to_string()], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    let rules = json
        .into_iter()
        .map(|text| {
            serde_json::from_str(&text)
                .map_err(|error| corrupt(&id.to_string(), &error.to_string()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Some(SecurityGroupResponse {
        id,
        name,
        micro_network_id,
        rules,
    }))
}

fn ensure_writable(
    conn: &Connection,
    id: Uuid,
    parent: Option<&firecrab_api_types::SandboxEnvironment>,
) -> Result<(), PersistenceError> {
    if let Some(parent) = parent {
        let original:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM ai_sandbox_environment_resources r
            JOIN ai_sandbox_environments e ON e.id=r.environment_id AND e.generation=r.generation
            WHERE r.id=?1 AND r.kind='security_group' AND r.environment_id=?2 AND r.generation=?3 AND e.state<>'deleted')",
            params![id.to_string(),parent.id.to_string(),parent.generation.to_string()],|row|row.get(0))?;
        if !original {
            return Err(PersistenceError::IacParentManaged);
        }
        let target = firecrab_stack::ManagedResource {
            kind: "security_group".into(),
            id: id.to_string(),
        };
        super::iac_native::ensure_idle(conn, &target)?;
        super::iac_host::ensure_resource_idle(conn, &target)?;
        return super::iac_owners::ensure_unowned(conn, &target);
    }
    if !super::iac_owners::public_write_allowed(
        conn,
        &firecrab_stack::ManagedResource {
            kind: "security_group".into(),
            id: id.to_string(),
        },
    )? {
        return Err(PersistenceError::IacOwnershipConflict);
    }
    Ok(())
}

pub(super) fn ensure_rules_idle(conn: &Connection, id: &str) -> Result<(), PersistenceError> {
    let busy:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM vms WHERE security_group_id=?1 AND state NOT IN ('created','stopped','error'))
        OR EXISTS(SELECT 1 FROM micro_service_deployments WHERE json_extract(deployment,'$.spec.securityGroupId')=?1
        AND (COALESCE(json_extract(deployment,'$.phase'),'') NOT IN ('stopped','failed')
        OR CASE WHEN json_type(deployment,'$.instances')='array' THEN json_array_length(deployment,'$.instances') ELSE 1 END>0))",
        [id],|row|row.get(0))?;
    if busy {
        return Err(PersistenceError::IacResourceBusy);
    }
    Ok(())
}

fn corrupt(id: &str, reason: &str) -> PersistenceError {
    PersistenceError::CorruptRecord {
        id: id.to_owned(),
        reason: reason.to_owned(),
    }
}
