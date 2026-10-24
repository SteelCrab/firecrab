//! Auth shares the existing owner-only credential database.
use super::*;
use crate::pipe_auth::{Provider, SavedAuth};

/// Provider credentials remain in the existing private database.
pub(super) const CREATE_TABLE_SQL: &str = "CREATE TABLE IF NOT EXISTS pipe_auth_credentials (
    provider TEXT PRIMARY KEY CHECK (provider IN ('aws', 'github', 'firecrab')),
    credential TEXT NOT NULL
) STRICT";

/// Extend the provider constraint and remove credentials from the earlier Tunnel-token prototype.
pub(super) fn migrate(conn: &Connection) -> Result<(), PersistenceError> {
    let schema: String = conn.query_row(
        "SELECT sql FROM sqlite_master WHERE type='table' AND name='pipe_auth_credentials'",
        [],
        |row| row.get(0),
    )?;
    let tx = conn.unchecked_transaction()?;
    if !schema.contains("'firecrab'") {
        tx.execute(
            "ALTER TABLE pipe_auth_credentials RENAME TO pipe_auth_credentials_legacy",
            [],
        )?;
        tx.execute(CREATE_TABLE_SQL, [])?;
        tx.execute(
            "INSERT INTO pipe_auth_credentials(provider, credential)
             SELECT provider, credential FROM pipe_auth_credentials_legacy",
            [],
        )?;
        tx.execute("DROP TABLE pipe_auth_credentials_legacy", [])?;
    }
    // A Tunnel address cannot establish an account identity. Keep other providers and
    // independently saved Tunnel bindings, which live outside this credential table.
    tx.execute(
        "DELETE FROM pipe_auth_credentials WHERE provider='firecrab'
         AND CASE WHEN json_valid(credential) THEN
             json_type(credential, '$.subdomain')='text'
             AND json_type(credential, '$.accountId') IS NULL
         ELSE 0 END",
        [],
    )?;
    tx.commit()?;
    Ok(())
}

impl Store {
    /// Decode saved provider credentials, rejecting malformed documents and provider mismatches.
    pub fn pipe_auths(&self) -> Result<Vec<SavedAuth>, PersistenceError> {
        let conn = self.lock();
        let mut statement = conn
            .prepare("SELECT provider, credential FROM pipe_auth_credentials ORDER BY provider")?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        rows.map(|row| {
            let (provider, raw) = row?;
            let auth: SavedAuth =
                serde_json::from_str(&raw).map_err(|_| PersistenceError::CorruptRecord {
                    id: provider.clone(),
                    reason: "invalid Auth credential".into(),
                })?;
            if auth.connection().provider.as_str() != provider {
                return Err(PersistenceError::CorruptRecord {
                    id: provider,
                    reason: "Auth provider mismatch".into(),
                });
            }
            Ok(auth)
        })
        .collect()
    }

    /// Atomically replace one provider credential without modifying other providers.
    pub fn put_pipe_auth(&self, auth: &SavedAuth) -> Result<(), PersistenceError> {
        let provider = auth.connection().provider;
        let raw = serde_json::to_string(auth).map_err(|_| PersistenceError::CorruptRecord {
            id: provider.as_str().into(),
            reason: "could not encode Auth credential".into(),
        })?;
        self.lock().execute(
            "INSERT INTO pipe_auth_credentials (provider, credential) VALUES (?1, ?2)
             ON CONFLICT(provider) DO UPDATE SET credential=excluded.credential",
            params![provider.as_str(), raw],
        )?;
        Ok(())
    }

    /// Remove the selected provider credential; an already absent connection also succeeds.
    pub fn delete_pipe_auth(&self, provider: Provider) -> Result<(), PersistenceError> {
        self.lock().execute(
            "DELETE FROM pipe_auth_credentials WHERE provider = ?1",
            [provider.as_str()],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn legacy_provider_constraint_is_migrated_without_losing_existing_credentials() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("state.db");
        let conn = Connection::open(&file).unwrap();
        conn.execute(
            "CREATE TABLE pipe_auth_credentials (
            provider TEXT PRIMARY KEY CHECK (provider IN ('aws', 'github')),
            credential TEXT NOT NULL
        ) STRICT",
            [],
        )
        .unwrap();
        let github = SavedAuth::Github {
            username: "octocat".into(),
            token: "github-secret".into(),
        };
        conn.execute(
            "INSERT INTO pipe_auth_credentials VALUES ('github', ?1)",
            [serde_json::to_string(&github).unwrap()],
        )
        .unwrap();
        drop(conn);
        let store = Store::open(&file).unwrap();
        let auths = store.pipe_auths().unwrap();
        assert!(matches!(&auths[0], SavedAuth::Github { token, .. } if token == "github-secret"));
        store
            .put_pipe_auth(&SavedAuth::Firecrab {
                account_id: "11111111-1111-4111-8111-111111111111".into(),
                email: "owner@example.com".into(),
                expires_at: 4102444800000,
                token: format!("fca_{}", "a".repeat(64)),
            })
            .unwrap();
        drop(store);
        let store = Store::open(&file).unwrap();
        assert_eq!(store.pipe_auths().unwrap().len(), 2);
    }

    #[test]
    fn firecrab_token_survives_reopen_and_rotation_without_changing_other_credentials() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("state.db");
        let store = Store::open(&file).unwrap();
        store
            .put_pipe_auth(&SavedAuth::Github {
                username: "octocat".into(),
                token: "github-secret".into(),
            })
            .unwrap();
        store
            .put_pipe_auth(&SavedAuth::Firecrab {
                account_id: "11111111-1111-4111-8111-111111111111".into(),
                email: "old@example.com".into(),
                expires_at: 4102444800000,
                token: format!("fca_{}", "a".repeat(64)),
            })
            .unwrap();
        drop(store);
        let store = Store::open(&file).unwrap();
        store
            .put_pipe_auth(&SavedAuth::Firecrab {
                account_id: "22222222-2222-4222-8222-222222222222".into(),
                email: "new@example.com".into(),
                expires_at: 4102444800000,
                token: format!("fca_{}", "b".repeat(64)),
            })
            .unwrap();
        let auths = store.pipe_auths().unwrap();
        assert_eq!(auths.len(), 2);
        let firecrab = auths
            .iter()
            .find(|auth| auth.connection().provider == Provider::Firecrab)
            .unwrap();
        assert_eq!(firecrab.connection().account, "new@example.com");
        assert!(
            matches!(firecrab, SavedAuth::Firecrab { token, .. } if token == &format!("fca_{}", "b".repeat(64)))
        );
        store.delete_pipe_auth(Provider::Firecrab).unwrap();
        store.delete_pipe_auth(Provider::Firecrab).unwrap();
        let auths = store.pipe_auths().unwrap();
        assert_eq!(auths.len(), 1);
        assert_eq!(auths[0].connection().provider, Provider::Github);
    }

    #[test]
    fn prototype_tunnel_credential_does_not_become_an_account_connection() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("state.db");
        let store = Store::open(&file).unwrap();
        store
            .put_pipe_auth(&SavedAuth::Github {
                username: "octocat".into(),
                token: "github-secret".into(),
            })
            .unwrap();
        store.lock().execute(
            "INSERT INTO pipe_auth_credentials VALUES ('firecrab', ?1)",
            [serde_json::json!({"provider":"firecrab", "subdomain":"old-app", "token":"a".repeat(64)}).to_string()],
        ).unwrap();
        drop(store);
        let store = Store::open(&file).unwrap();
        let auths = store.pipe_auths().unwrap();
        assert_eq!(auths.len(), 1);
        assert!(matches!(&auths[0], SavedAuth::Github { token, .. } if token == "github-secret"));
    }

    #[test]
    fn credentials_survive_reopen_rotate_and_delete_without_affecting_other_providers() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("state.db");
        let store = Store::open(&file).unwrap();
        let github = SavedAuth::Github {
            username: "octocat".into(),
            token: "old-secret".into(),
        };
        let aws = SavedAuth::Aws {
            account_id: "123456789012".into(),
            credentials: crate::pipe_auth::AwsCredentials {
                access_key_id: "AKIAIOSFODNN7EXAMPLE".into(),
                secret_access_key: "aws-secret".into(),
                session_token: "session".into(),
                region: "ap-northeast-2".into(),
            },
        };
        store.put_pipe_auth(&github).unwrap();
        store.put_pipe_auth(&aws).unwrap();
        drop(store);
        let store = Store::open(&file).unwrap();
        assert_eq!(store.pipe_auths().unwrap().len(), 2);
        store
            .put_pipe_auth(&SavedAuth::Github {
                username: "new-user".into(),
                token: "new-secret".into(),
            })
            .unwrap();
        let rows = store.pipe_auths().unwrap();
        assert_eq!(rows.len(), 2);
        let github = rows
            .iter()
            .find(|auth| auth.connection().provider == Provider::Github)
            .unwrap();
        assert_eq!(github.connection().account, "new-user");
        store.delete_pipe_auth(Provider::Github).unwrap();
        store.delete_pipe_auth(Provider::Github).unwrap();
        assert_eq!(
            store.pipe_auths().unwrap()[0].connection().provider,
            Provider::Aws
        );
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
