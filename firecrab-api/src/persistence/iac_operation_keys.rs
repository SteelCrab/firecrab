//! One durable UUID namespace across shared IaC operation families.

use super::PersistenceError;
use rusqlite::{Connection, Transaction, TransactionBehavior};

const CONFLICT: &str = "iac_operation_identity_conflict";
const TABLES: &[(&str, &str)] = &[
    ("iac_native_operations", "native"),
    ("iac_owner_changes", "ownership"),
    ("iac_host_owner_intents", "ownership"),
    ("stack_applies", "stack"),
];

/// These static trigger failures map to the public identity-conflict category.
pub(super) fn identity_conflict(error: &rusqlite::Error) -> bool {
    matches!(error, rusqlite::Error::SqliteFailure(_,Some(message)) if message == CONFLICT)
}

/// Preserve historical receipts, including known rejections, before admitting new UUIDs.
pub(super) fn initialize(conn: &Connection) -> Result<(), PersistenceError> {
    // Acquire the writer before reading history. Concurrent startup must not
    // race a deferred read transaction's later lock upgrade.
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS iac_operation_keys (
        operation_id TEXT PRIMARY KEY,
        family TEXT NOT NULL CHECK(family IN ('native','ownership','stack'))
    ) STRICT;",
    )?;
    for (table, family) in TABLES {
        let conflict: bool = tx.query_row(
            &format!(
                "SELECT EXISTS(SELECT 1 FROM {table} source JOIN iac_operation_keys reserved
             ON reserved.operation_id=source.operation_id WHERE reserved.family<>?1)"
            ),
            [family],
            |row| row.get(0),
        )?;
        if conflict {
            return Err(PersistenceError::IacOperationIdentity);
        }
        tx.execute(
            &format!(
                "INSERT INTO iac_operation_keys(operation_id,family)
            SELECT operation_id,?1 FROM {table} WHERE true ON CONFLICT(operation_id) DO NOTHING"
            ),
            [family],
        )?;
        // Table/family names are compile-time constants. No request can select SQL identifiers.
        tx.execute_batch(&format!("CREATE TRIGGER IF NOT EXISTS {table}_operation_key
            BEFORE INSERT ON {table} BEGIN
                SELECT CASE WHEN EXISTS(SELECT 1 FROM iac_operation_keys
                    WHERE operation_id=NEW.operation_id AND family<>'{family}')
                    THEN RAISE(ABORT,'{CONFLICT}') END;
                INSERT INTO iac_operation_keys(operation_id,family) VALUES(NEW.operation_id,'{family}')
                    ON CONFLICT(operation_id) DO NOTHING;
            END;
            CREATE TRIGGER IF NOT EXISTS {table}_immutable_operation_id
            BEFORE UPDATE OF operation_id ON {table} WHEN OLD.operation_id<>NEW.operation_id
            BEGIN SELECT RAISE(ABORT,'{CONFLICT}'); END;"))?;
    }
    tx.execute_batch(&format!(
        "CREATE TRIGGER IF NOT EXISTS iac_operation_keys_immutable_update
        BEFORE UPDATE ON iac_operation_keys BEGIN SELECT RAISE(ABORT,'{CONFLICT}'); END;
        CREATE TRIGGER IF NOT EXISTS iac_operation_keys_immutable_delete
        BEFORE DELETE ON iac_operation_keys BEGIN SELECT RAISE(ABORT,'{CONFLICT}'); END;"
    ))?;
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::{Connection, params};
    use std::{
        sync::{Arc, Barrier},
        time::Duration,
    };
    use uuid::Uuid;

    fn schema(conn: &Connection) {
        for (table, _) in TABLES {
            conn.execute_batch(&format!("CREATE TABLE IF NOT EXISTS {table} (operation_id TEXT PRIMARY KEY,payload TEXT NOT NULL DEFAULT '') STRICT;")).unwrap();
        }
    }
    fn insert(conn: &Connection, table: &str, id: Uuid) -> Result<usize, rusqlite::Error> {
        conn.execute(
            &format!("INSERT INTO {table}(operation_id,payload) VALUES(?1,'original receipt')"),
            [id.to_string()],
        )
    }

    #[test]
    fn iac_operation_keys_share_owner_intent_receipt_and_reject_cross_family_reuse() {
        for (first, family) in TABLES {
            for (second, other_family) in TABLES {
                if family == other_family {
                    continue;
                }
                let conn = Connection::open_in_memory().unwrap();
                schema(&conn);
                initialize(&conn).unwrap();
                let id = Uuid::new_v4();
                insert(&conn, first, id).unwrap();
                let error = insert(&conn, second, id).unwrap_err();
                assert!(identity_conflict(&error), "{first} -> {second}: {error}");
                assert!(matches!(
                    PersistenceError::from(error),
                    PersistenceError::IacOperationIdentity
                ));
                let payload: String = conn
                    .query_row(
                        &format!("SELECT payload FROM {first} WHERE operation_id=?1"),
                        [id.to_string()],
                        |row| row.get(0),
                    )
                    .unwrap();
                assert_eq!(payload, "original receipt");
            }
        }
        let conn = Connection::open_in_memory().unwrap();
        schema(&conn);
        initialize(&conn).unwrap();
        let id = Uuid::new_v4();
        insert(&conn, "iac_host_owner_intents", id).unwrap();
        insert(&conn, "iac_owner_changes", id).unwrap();
        let count: i64 = conn
            .query_row("SELECT count(*) FROM iac_operation_keys", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn iac_operation_keys_migration_rejects_ambiguous_history_without_changing_receipts() {
        let conn = Connection::open_in_memory().unwrap();
        schema(&conn);
        let id = Uuid::new_v4();
        insert(&conn, "iac_native_operations", id).unwrap();
        insert(&conn, "iac_owner_changes", id).unwrap();
        assert!(matches!(
            initialize(&conn),
            Err(PersistenceError::IacOperationIdentity)
        ));
        for table in ["iac_native_operations", "iac_owner_changes"] {
            let payload: String = conn
                .query_row(
                    &format!("SELECT payload FROM {table} WHERE operation_id=?1"),
                    [id.to_string()],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(payload, "original receipt");
        }
        assert!(
            !conn
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='iac_operation_keys')",
                    [],
                    |row| row.get::<_, bool>(0)
                )
                .unwrap()
        );
    }

    #[test]
    fn iac_operation_keys_migration_preserves_compatible_history_and_survives_reopen() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("keys.sqlite");
        let conn = Connection::open(&path).unwrap();
        schema(&conn);
        let id = Uuid::new_v4();
        insert(&conn, "iac_host_owner_intents", id).unwrap();
        insert(&conn, "iac_owner_changes", id).unwrap();
        initialize(&conn).unwrap();
        drop(conn);
        let conn = Connection::open(&path).unwrap();
        initialize(&conn).unwrap();
        assert!(identity_conflict(
            &insert(&conn, "stack_applies", id).unwrap_err()
        ));
        conn.execute(
            "DELETE FROM iac_owner_changes WHERE operation_id=?1",
            [id.to_string()],
        )
        .unwrap();
        conn.execute(
            "DELETE FROM iac_host_owner_intents WHERE operation_id=?1",
            [id.to_string()],
        )
        .unwrap();
        assert!(identity_conflict(
            &insert(&conn, "iac_native_operations", id).unwrap_err()
        ));
        assert!(identity_conflict(
            &conn
                .execute(
                    "DELETE FROM iac_operation_keys WHERE operation_id=?1",
                    [id.to_string()]
                )
                .unwrap_err()
        ));
        assert!(identity_conflict(
            &conn
                .execute(
                    "UPDATE iac_operation_keys SET family='native' WHERE operation_id=?1",
                    [id.to_string()]
                )
                .unwrap_err()
        ));
        let fresh = Uuid::new_v4();
        insert(&conn, "stack_applies", fresh).unwrap();
        assert!(identity_conflict(
            &conn
                .execute(
                    "UPDATE stack_applies SET operation_id=?2 WHERE operation_id=?1",
                    params![fresh.to_string(), Uuid::new_v4().to_string()]
                )
                .unwrap_err()
        ));
    }

    #[test]
    fn iac_operation_keys_concurrent_database_connections_admit_only_one_family() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("race.sqlite");
        let conn = Connection::open(&path).unwrap();
        schema(&conn);
        initialize(&conn).unwrap();
        drop(conn);
        let id = Uuid::new_v4();
        let start = Arc::new(Barrier::new(2));
        let workers = ["iac_native_operations", "iac_owner_changes"].map(|table| {
            let path = path.clone();
            let start = start.clone();
            std::thread::spawn(move || {
                let conn = Connection::open(path).unwrap();
                conn.busy_timeout(Duration::from_secs(5)).unwrap();
                start.wait();
                insert(&conn, table, id)
            })
        });
        let results = workers.map(|worker| worker.join().unwrap());
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert!(identity_conflict(
            results
                .iter()
                .find_map(|result| result.as_ref().err())
                .unwrap()
        ));
        let conn = Connection::open(&path).unwrap();
        let count: i64 = conn
            .query_row("SELECT count(*) FROM iac_operation_keys", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn iac_operation_keys_concurrent_store_startup_preserves_the_existing_namespace() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("startup.sqlite");
        let store = super::super::Store::open(&path).unwrap();
        let expected = store.iac_endpoint_id().unwrap();
        let start = Arc::new(Barrier::new(8));
        let workers: Vec<_> = (0..8)
            .map(|_| {
                let path = path.clone();
                let start = start.clone();
                std::thread::spawn(move || {
                    start.wait();
                    super::super::Store::open(&path).and_then(|store| store.iac_endpoint_id())
                })
            })
            .collect();
        for worker in workers {
            assert_eq!(worker.join().unwrap().unwrap(), expected);
        }
    }
}
