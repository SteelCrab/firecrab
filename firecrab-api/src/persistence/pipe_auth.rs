//! Pipe Auth shares the existing owner-only credential database.
use super::*;
use crate::pipe_auth::{Provider, SavedAuth};

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
                    reason: "invalid Pipe Auth credential".into(),
                })?;
            if auth.connection().provider.as_str() != provider {
                return Err(PersistenceError::CorruptRecord {
                    id: provider,
                    reason: "Pipe Auth provider mismatch".into(),
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
            reason: "could not encode Pipe Auth credential".into(),
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
