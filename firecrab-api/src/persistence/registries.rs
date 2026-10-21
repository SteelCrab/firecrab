//! Registry logins share the owner-only SQLite database used for Docker Hub.
use super::*;
use crate::oci::RegistryCredential;

/// Saved authentication and its optional human-readable AWS account name.
pub struct RegistryLogin {
    /// Authentication username and secret for this exact registry.
    pub credential: RegistryCredential,
    /// Display-only account name, independent of the ECR username `AWS`.
    pub account_name: Option<String>,
}

/// Add display names while retaining credentials from older databases.
pub(super) fn migrate_account_name(conn: &Connection) -> Result<(), PersistenceError> {
    if !conn
        .prepare(
            "SELECT 1 FROM pragma_table_info('registry_credentials') WHERE name = 'account_name'",
        )?
        .exists([])?
    {
        conn.execute(
            "ALTER TABLE registry_credentials ADD COLUMN account_name TEXT",
            [],
        )?;
    }
    Ok(())
}

impl Store {
    pub fn registry_logins(&self) -> Result<Vec<RegistryLogin>, PersistenceError> {
        // Reuse the existing login so Pipe and the legacy API cannot diverge.
        let docker_hub = self.docker_hub_credential()?.map(RegistryCredential::from);
        let conn = self.lock();
        let mut statement = conn.prepare(
            "SELECT registry, username, secret, account_name FROM registry_credentials ORDER BY registry",
        )?;
        let mut credentials = statement
            .query_map([], |row| {
                Ok(RegistryLogin {
                    credential: RegistryCredential {
                        registry: row.get(0)?,
                        username: row.get(1)?,
                        secret: row.get(2)?,
                    },
                    account_name: row.get(3)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        credentials.extend(docker_hub.map(|credential| RegistryLogin {
            credential,
            account_name: None,
        }));
        credentials.sort_by(|a, b| a.credential.registry.cmp(&b.credential.registry));
        Ok(credentials)
    }

    pub fn registry_credential(
        &self,
        registry: &str,
    ) -> Result<Option<RegistryCredential>, PersistenceError> {
        if matches!(registry, "docker.io" | "registry-1.docker.io") {
            return Ok(self.docker_hub_credential()?.map(RegistryCredential::from));
        }
        self.lock()
            .query_row(
                "SELECT username, secret FROM registry_credentials WHERE registry = ?1",
                [registry],
                |row| {
                    Ok(RegistryCredential {
                        registry: registry.into(),
                        username: row.get(0)?,
                        secret: row.get(1)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn put_registry_login(
        &self,
        credential: &RegistryCredential,
        account_name: Option<&str>,
    ) -> Result<RegistryLogin, PersistenceError> {
        if matches!(
            credential.registry.as_str(),
            "docker.io" | "registry-1.docker.io"
        ) {
            self.put_docker_hub_credential(&credential.username, &credential.secret)?;
            return Ok(RegistryLogin {
                credential: credential.clone(),
                account_name: None,
            });
        }
        let conn = self.lock();
        conn.execute(
            "INSERT INTO registry_credentials (registry, username, secret, account_name)
            VALUES (?1, ?2, ?3, NULLIF(?4, ''))
            ON CONFLICT(registry) DO UPDATE SET username=excluded.username, secret=excluded.secret,
            account_name=CASE WHEN ?4 IS NULL THEN registry_credentials.account_name ELSE NULLIF(?4, '') END",
            params![credential.registry, credential.username, credential.secret, account_name],
        )?;
        let account_name = conn.query_row(
            "SELECT account_name FROM registry_credentials WHERE registry = ?1",
            [&credential.registry],
            |row| row.get(0),
        )?;
        Ok(RegistryLogin {
            credential: credential.clone(),
            account_name,
        })
    }

    pub fn delete_registry_credential(&self, registry: &str) -> Result<(), PersistenceError> {
        if matches!(registry, "docker.io" | "registry-1.docker.io") {
            return self.delete_docker_hub_credential();
        }
        self.lock().execute(
            "DELETE FROM registry_credentials WHERE registry = ?1",
            [registry],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_names_migrate_persist_and_stay_separate_from_authentication() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE registry_credentials (
            registry TEXT PRIMARY KEY, username TEXT NOT NULL, secret TEXT NOT NULL
        ) STRICT;
        INSERT INTO registry_credentials VALUES (
            '123456789012.dkr.ecr.ap-northeast-2.amazonaws.com', 'AWS', 'original-token'
        );",
        )
        .unwrap();
        drop(conn);
        let store = Store::open(&path).unwrap();
        let login = store.registry_logins().unwrap().remove(0);
        assert_eq!(login.account_name, None);
        assert_eq!(login.credential.username, "AWS");
        store
            .put_registry_login(&login.credential, Some("production"))
            .unwrap();
        drop(store);

        let store = Store::open(&path).unwrap();
        let login = store.registry_logins().unwrap().remove(0);
        assert_eq!(login.account_name.as_deref(), Some("production"));
        let mut credential = login.credential;
        credential.secret = "rotated-token".into();
        let saved = store.put_registry_login(&credential, None).unwrap();
        assert_eq!(saved.account_name.as_deref(), Some("production"));
        let auth = store
            .registry_credential(&credential.registry)
            .unwrap()
            .unwrap();
        assert_eq!(auth.username, "AWS");
        assert_eq!(auth.secret, "rotated-token");
        let saved = store.put_registry_login(&credential, Some("")).unwrap();
        assert_eq!(saved.account_name, None);
        store
            .delete_registry_credential(&credential.registry)
            .unwrap();
        assert!(store.registry_logins().unwrap().is_empty());
    }

    #[test]
    fn registry_credentials_persist_rotate_and_delete_independently() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        let store = Store::open(&path).unwrap();
        for registry in ["ghcr.io", "123456789012.dkr.ecr.us-east-1.amazonaws.com"] {
            store
                .put_registry_login(
                    &RegistryCredential {
                        registry: registry.into(),
                        username: "user".into(),
                        secret: "secret".into(),
                    },
                    None,
                )
                .unwrap();
        }
        drop(store);
        let store = Store::open(&path).unwrap();
        assert_eq!(store.registry_logins().unwrap().len(), 2);
        let mut credential = store.registry_credential("ghcr.io").unwrap().unwrap();
        credential.secret = "rotated".into();
        store.put_registry_login(&credential, None).unwrap();
        assert_eq!(
            store
                .registry_credential("ghcr.io")
                .unwrap()
                .unwrap()
                .secret,
            "rotated"
        );
        store.delete_registry_credential("ghcr.io").unwrap();
        assert!(store.registry_credential("ghcr.io").unwrap().is_none());
        assert_eq!(store.registry_logins().unwrap().len(), 1);
    }
    #[test]
    fn docker_hub_logins_share_the_legacy_row_and_aliases() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        let store = Store::open(&path).unwrap();
        store
            .put_docker_hub_credential("existing", "legacy-token")
            .unwrap();
        let listed = store.registry_logins().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].credential.username, "existing");
        for host in ["docker.io", "registry-1.docker.io"] {
            assert_eq!(
                store.registry_credential(host).unwrap().unwrap().secret,
                "legacy-token"
            );
            store
                .put_registry_login(
                    &RegistryCredential {
                        registry: host.into(),
                        username: "updated".into(),
                        secret: "new-token".into(),
                    },
                    None,
                )
                .unwrap();
            assert_eq!(
                store.docker_hub_credential().unwrap().unwrap().secret,
                "new-token"
            );
            assert_eq!(store.registry_logins().unwrap().len(), 1);
            store
                .put_docker_hub_credential("existing", "legacy-token")
                .unwrap();
        }
        drop(store);
        let store = Store::open(&path).unwrap();
        store
            .delete_registry_credential("registry-1.docker.io")
            .unwrap();
        assert!(store.docker_hub_credential().unwrap().is_none());
        assert!(store.registry_logins().unwrap().is_empty());
        store
            .put_registry_login(
                &RegistryCredential::docker_hub("from-pipe", "pipe-token"),
                None,
            )
            .unwrap();
        store.delete_docker_hub_credential().unwrap();
        assert!(store.registry_credential("docker.io").unwrap().is_none());
    }
}
