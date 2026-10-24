use super::*;

#[test]
fn legacy_vault_contents_default_to_no_deployment_environments() {
    let contents: Contents =
        serde_json::from_str(r#"{"secrets":[],"connections":[],"plans":[]}"#).unwrap();
    assert!(contents.service_environments.is_empty());
}

fn secret(name: &str, value: &str) -> Secret {
    Secret {
        summary: SecretSummary {
            id: Uuid::new_v4(),
            name: name.into(),
            description: String::new(),
            version: 1,
            created_at: now_ms(),
            updated_at: now_ms(),
            source: None,
        },
        value: value.into(),
    }
}

#[test]
fn encrypted_values_and_credentials_survive_reopen_without_plaintext_files() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("secrets");
    let vault = Vault::new(path.clone());
    vault
        .write(|contents| {
            contents
                .secrets
                .push(secret("DB_PASSWORD", "unique-password-never-on-disk"));
            contents.connections.push(Connection {
                summary: ConnectionSummary {
                    id: Uuid::new_v4(),
                    name: "production".into(),
                    provider: Provider::Infisical,
                    settings: Settings::default(),
                    version: 1,
                    updated_at: now_ms(),
                },
                credentials: BTreeMap::from([("token".into(), "private-provider-token".into())]),
            });
            Ok(())
        })
        .unwrap();
    let reopened = Vault::new(path.clone());
    reopened
        .read(|contents| {
            assert_eq!(contents.secrets[0].value, "unique-password-never-on-disk");
            assert_eq!(
                contents.connections[0].credentials["token"],
                "private-provider-token"
            );
            Ok(())
        })
        .unwrap();
    for item in fs::read_dir(&path).unwrap() {
        let file = item.unwrap().path();
        let bytes = fs::read(&file).unwrap();
        for plain in [
            "DB_PASSWORD",
            "unique-password-never-on-disk",
            "private-provider-token",
        ] {
            assert!(
                !bytes
                    .windows(plain.len())
                    .any(|window| window == plain.as_bytes()),
                "plaintext in {}",
                file.display()
            );
        }
        assert_eq!(
            fs::metadata(file).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o700
    );
}

#[test]
fn missing_key_and_modified_ciphertext_fail_closed() {
    let root = tempfile::tempdir().unwrap();
    let vault = Vault::new(root.path().join("secrets"));
    vault
        .write(|contents| {
            contents.secrets.push(secret("KEY", "value"));
            Ok(())
        })
        .unwrap();
    let key = fs::read(vault.root.join("master.key")).unwrap();
    fs::remove_file(vault.root.join("master.key")).unwrap();
    assert!(matches!(
        vault.read(|_| Ok(())),
        Err(SecretError::Encryption)
    ));
    assert!(!vault.root.join("master.key").exists());
    fs::write(vault.root.join("master.key"), key).unwrap();
    let path = vault.root.join("vault.bin");
    let mut bytes = fs::read(&path).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 1;
    fs::write(path, bytes).unwrap();
    assert!(matches!(
        vault.read(|_| Ok(())),
        Err(SecretError::Encryption)
    ));
}

#[test]
fn independent_handles_serialize_changes_and_failed_changes_are_not_saved() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("secrets");
    let threads: Vec<_> = (0..8)
        .map(|index| {
            let path = path.clone();
            std::thread::spawn(move || {
                Vault::new(path)
                    .write(|contents| {
                        contents
                            .secrets
                            .push(secret(&format!("KEY_{index}"), "value"));
                        Ok(())
                    })
                    .unwrap()
            })
        })
        .collect();
    for thread in threads {
        thread.join().unwrap();
    }
    let vault = Vault::new(path);
    let failed: Result<(), SecretError> = vault.write(|contents| {
        contents.secrets.clear();
        Err(SecretError::Conflict)
    });
    assert!(failed.is_err());
    assert_eq!(
        vault.read(|contents| Ok(contents.secrets.len())).unwrap(),
        8
    );
}

#[test]
fn refuses_symlinked_keys() {
    let root = tempfile::tempdir().unwrap();
    let vault = Vault::new(root.path().join("secrets"));
    fs::create_dir(&vault.root).unwrap();
    let target = root.path().join("other-key");
    fs::write(&target, [0; 32]).unwrap();
    std::os::unix::fs::symlink(&target, vault.root.join("master.key")).unwrap();
    assert!(vault.read(|_| Ok(())).is_err());
    assert_eq!(fs::read(target).unwrap(), [0; 32]);
}
