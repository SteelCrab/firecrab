//! Registry logins share the owner-only SQLite database used for Docker Hub.
use super::*;
use crate::oci::RegistryCredential;

impl Store {
    pub fn registry_credentials(&self) -> Result<Vec<RegistryCredential>, PersistenceError> {
        // Reuse the existing login so Pipe and the legacy API cannot diverge.
        let docker_hub = self.docker_hub_credential()?.map(RegistryCredential::from);
        let conn = self.lock();
        let mut statement = conn.prepare(
            "SELECT registry, username, secret FROM registry_credentials ORDER BY registry",
        )?;
        let mut credentials = statement
            .query_map([], |row| {
                Ok(RegistryCredential {
                    registry: row.get(0)?,
                    username: row.get(1)?,
                    secret: row.get(2)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        credentials.extend(docker_hub);
        credentials.sort_by(|a, b| a.registry.cmp(&b.registry));
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

    pub fn put_registry_credential(
        &self,
        credential: &RegistryCredential,
    ) -> Result<(), PersistenceError> {
        if matches!(
            credential.registry.as_str(),
            "docker.io" | "registry-1.docker.io"
        ) {
            return self.put_docker_hub_credential(&credential.username, &credential.secret);
        }
        self.lock().execute(
            "INSERT INTO registry_credentials (registry, username, secret) VALUES (?1, ?2, ?3)
            ON CONFLICT(registry) DO UPDATE SET username=excluded.username, secret=excluded.secret",
            params![credential.registry, credential.username, credential.secret],
        )?;
        Ok(())
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
    fn registry_credentials_persist_rotate_and_delete_independently() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        let store = Store::open(&path).unwrap();
        for registry in ["ghcr.io", "123456789012.dkr.ecr.us-east-1.amazonaws.com"] {
            store
                .put_registry_credential(&RegistryCredential {
                    registry: registry.into(),
                    username: "user".into(),
                    secret: "secret".into(),
                })
                .unwrap();
        }
        drop(store);
        let store = Store::open(&path).unwrap();
        assert_eq!(store.registry_credentials().unwrap().len(), 2);
        let mut credential = store.registry_credential("ghcr.io").unwrap().unwrap();
        credential.secret = "rotated".into();
        store.put_registry_credential(&credential).unwrap();
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
        assert_eq!(store.registry_credentials().unwrap().len(), 1);
    }
    #[test]
    fn docker_hub_logins_share_the_legacy_row_and_aliases() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        let store = Store::open(&path).unwrap();
        store
            .put_docker_hub_credential("existing", "legacy-token")
            .unwrap();
        let listed = store.registry_credentials().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].username, "existing");
        for host in ["docker.io", "registry-1.docker.io"] {
            assert_eq!(
                store.registry_credential(host).unwrap().unwrap().secret,
                "legacy-token"
            );
            store
                .put_registry_credential(&RegistryCredential {
                    registry: host.into(),
                    username: "updated".into(),
                    secret: "new-token".into(),
                })
                .unwrap();
            assert_eq!(
                store.docker_hub_credential().unwrap().unwrap().secret,
                "new-token"
            );
            assert_eq!(store.registry_credentials().unwrap().len(), 1);
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
        assert!(store.registry_credentials().unwrap().is_empty());
        store
            .put_registry_credential(&RegistryCredential::docker_hub("from-pipe", "pipe-token"))
            .unwrap();
        store.delete_docker_hub_credential().unwrap();
        assert!(store.registry_credential("docker.io").unwrap().is_none());
    }
}
