//! Expand operation outcomes without discarding receipts or dependency fences.

use super::PersistenceError;
use rusqlite::{Connection, OptionalExtension};

const COLUMNS: &str = "(
 operation_id TEXT PRIMARY KEY, kind TEXT NOT NULL, resource_id TEXT NOT NULL,
 state TEXT NOT NULL CHECK(state IN ('running','needs_recovery','succeeded','failed')),
 status TEXT NOT NULL
) STRICT";

pub(super) fn initialize(conn: &Connection) -> Result<(), PersistenceError> {
    conn.execute_batch(&format!(
        "CREATE TABLE IF NOT EXISTS iac_native_operations {COLUMNS};"
    ))?;
    let schema: String = conn.query_row(
        "SELECT sql FROM sqlite_schema WHERE name='iac_native_operations'",
        [],
        |row| row.get(0),
    )?;
    // Older operation databases may predate dependency fences entirely.
    conn.execute_batch(super::iac_native::SCHEMA)?;
    if !schema.contains("'failed'") {
        upgrade(conn)?;
    }
    conn.execute_batch(super::iac_native::SCHEMA)?;
    Ok(())
}

fn upgrade(conn: &Connection) -> Result<(), PersistenceError> {
    let foreign_keys: bool = conn.pragma_query_value(None, "foreign_keys", |row| row.get(0))?;
    conn.pragma_update(None, "foreign_keys", false)?;
    let result = (|| {
        let tx = conn.unchecked_transaction()?;
        // The plan table survives this rebuild. Its forward-reference trigger
        // must not point at a temporarily missing native-operation table.
        tx.execute_batch(
            "DROP TRIGGER IF EXISTS iac_plan_native_identity;
            DROP TRIGGER IF EXISTS iac_native_plan_identity;",
        )?;
        tx.execute_batch(&format!("CREATE TABLE iac_native_operations_upgrade {COLUMNS};
            INSERT INTO iac_native_operations_upgrade SELECT operation_id,kind,resource_id,state,status FROM iac_native_operations;
            DROP TABLE iac_native_operations;
            ALTER TABLE iac_native_operations_upgrade RENAME TO iac_native_operations;"))?;
        // Every dependency still refers to the original operation UUID and table name.
        let violation: Option<String> = tx
            .query_row(
                "PRAGMA foreign_key_check(iac_native_dependencies)",
                [],
                |row| row.get(0),
            )
            .optional()?;
        if violation.is_some() {
            return Err(PersistenceError::IacOperationIdentity);
        }
        tx.commit()?;
        Ok(())
    })();
    let restore = conn
        .pragma_update(None, "foreign_keys", foreign_keys)
        .map_err(PersistenceError::from);
    result.and(restore)
}

#[cfg(test)]
mod tests {
    use super::*;
    use firecrab_stack::{
        ManagedResource, NativeAction, NativeOperation, NativeOperationState, ResourceOwnership,
    };
    use uuid::Uuid;

    fn operation(endpoint: Uuid, resource: Uuid, state: NativeOperationState) -> NativeOperation {
        NativeOperation {
            operation_id: Uuid::new_v4(),
            ownership: ResourceOwnership {
                endpoint_id: endpoint,
                resource: ManagedResource {
                    kind: "sandbox_project".into(),
                    id: resource.to_string(),
                },
                generation: 0,
                owner: None,
            },
            action: NativeAction::DeleteSandboxProject,
            state,
            error_code: None,
            execution_generation: 0,
            configuration_generation: None,
            retain_disks: None,
        }
    }

    #[test]
    fn iac_native_schema_upgrade_retains_receipts_dependency_fences_and_foreign_keys() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;
            CREATE TABLE iac_native_operations(operation_id TEXT PRIMARY KEY,kind TEXT NOT NULL,resource_id TEXT NOT NULL,state TEXT NOT NULL CHECK(state IN ('running','needs_recovery','succeeded')),status TEXT NOT NULL) STRICT;").unwrap();
        conn.execute_batch(super::super::iac_native::SCHEMA)
            .unwrap();
        let original = operation(
            Uuid::new_v4(),
            Uuid::new_v4(),
            NativeOperationState::NeedsRecovery,
        );
        super::super::iac_native::save(&conn, &original).unwrap();
        conn.execute(
            "INSERT INTO iac_native_dependencies VALUES (?1,'service',?2,?3)",
            rusqlite::params![
                original.operation_id.to_string(),
                Uuid::new_v4().to_string(),
                serde_json::to_string(&original.ownership).unwrap()
            ],
        )
        .unwrap();
        initialize(&conn).unwrap();
        assert!(
            conn.pragma_query_value::<bool, _>(None, "foreign_keys", |row| row.get(0))
                .unwrap()
        );
        assert_eq!(
            super::super::iac_native::read(&conn, original.operation_id).unwrap(),
            Some(original.clone())
        );
        assert_eq!(
            conn.query_row("SELECT count(*) FROM iac_native_dependencies", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
            1
        );
        let mut failed = operation(
            original.ownership.endpoint_id,
            Uuid::new_v4(),
            NativeOperationState::Failed,
        );
        failed.error_code = Some("verification_failed".into());
        super::super::iac_native::save(&conn, &failed).unwrap();
        super::super::iac_native::ensure_idle(&conn, &failed.ownership.resource).unwrap();
        assert!(matches!(
            super::super::iac_native::ensure_idle(&conn, &original.ownership.resource),
            Err(PersistenceError::IacResourceBusy)
        ));
        let broken = conn.execute(
            "INSERT INTO iac_native_dependencies VALUES (?1,'service',?2,'{}')",
            rusqlite::params![Uuid::new_v4().to_string(), Uuid::new_v4().to_string()],
        );
        assert!(broken.is_err());
        initialize(&conn).unwrap();
        assert_eq!(
            super::super::iac_native::read(&conn, failed.operation_id).unwrap(),
            Some(failed)
        );
    }
    #[test]
    fn iac_native_schema_upgrade_accepts_history_created_before_dependency_tables() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE iac_native_operations(operation_id TEXT PRIMARY KEY,kind TEXT NOT NULL,resource_id TEXT NOT NULL,state TEXT NOT NULL CHECK(state IN ('running','needs_recovery','succeeded')),status TEXT NOT NULL) STRICT;").unwrap();
        let original = operation(
            Uuid::new_v4(),
            Uuid::new_v4(),
            NativeOperationState::Succeeded,
        );
        super::super::iac_native::save(&conn, &original).unwrap();
        initialize(&conn).unwrap();
        assert_eq!(
            super::super::iac_native::read(&conn, original.operation_id).unwrap(),
            Some(original)
        );
    }

    #[test]
    fn iac_native_schema_upgrade_rolls_back_when_existing_fences_are_corrupt() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE iac_native_operations(operation_id TEXT PRIMARY KEY,kind TEXT NOT NULL,resource_id TEXT NOT NULL,state TEXT NOT NULL CHECK(state IN ('running','needs_recovery','succeeded')),status TEXT NOT NULL) STRICT;").unwrap();
        conn.execute_batch(super::super::iac_native::SCHEMA)
            .unwrap();
        conn.pragma_update(None, "foreign_keys", false).unwrap();
        conn.execute(
            "INSERT INTO iac_native_dependencies VALUES (?1,'service',?2,'{}')",
            rusqlite::params![Uuid::new_v4().to_string(), Uuid::new_v4().to_string()],
        )
        .unwrap();
        assert!(matches!(
            initialize(&conn),
            Err(PersistenceError::IacOperationIdentity)
        ));
        let schema: String = conn
            .query_row(
                "SELECT sql FROM sqlite_schema WHERE name='iac_native_operations'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(!schema.contains("'failed'"));
        assert_eq!(
            conn.query_row("SELECT count(*) FROM iac_native_dependencies", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
            1
        );
        assert!(
            !conn
                .pragma_query_value::<bool, _>(None, "foreign_keys", |row| row.get(0))
                .unwrap()
        );
        assert_eq!(
            conn.query_row(
                "SELECT count(*) FROM sqlite_schema WHERE name='iac_native_operations_upgrade'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
    }
}
