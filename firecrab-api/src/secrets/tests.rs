use super::*;

#[test]
fn legacy_vault_contents_default_to_no_deployment_environments() {
    let contents: Contents =
        serde_json::from_str(r#"{"secrets":[],"connections":[],"plans":[]}"#).unwrap();
    assert!(contents.service_environments.is_empty());
}

#[test]
fn retired_connections_are_removed_without_losing_imported_values_or_other_providers() {
    use serde_json::json;

    let root = tempfile::tempdir().unwrap();
    let vault = Vault::new(root.path().join("secrets"));
    let retired_id = Uuid::new_v4();
    let active_id = Uuid::new_v4();
    let aws_id = Uuid::new_v4();
    let account_id = Uuid::new_v4();
    let service_id = Uuid::new_v4();
    let mut imported = secret("IMPORTED", "retained-imported-value");
    imported.summary.source = Some(SecretSource {
        connection_id: retired_id,
        connection_name: "old-source".into(),
        provider: "infisical".into(),
        remote_key: "IMPORTED".into(),
        imported_at: 123,
    });
    let connection = |id: Uuid, provider: &str| {
        json!({
            "summary": {"id":id,"name":provider,"provider":provider,"version":1,"updatedAt":1,
                "settings":{"baseUrl":"https://secrets.example.com","region":"us-east-1",
                    "projectId":"old-project","environment":"qa","secretPath":"/qa"}},
            "credentials":{"token":format!("{provider}-private-token")}
        })
    };
    let plan = |id: Uuid| {
        json!({"id":Uuid::new_v4(),"connection_id":id,"connection_version":1,
            "direction":"import","created_at":now_ms(),"values":[]})
    };
    let mut account_plan = plan(account_id);
    account_plan["account"] = json!({"account_id":account_id,"token_hash":"saved-hash"});
    let legacy = json!({
        "secrets":[imported],
        "connections":[connection(retired_id,"infisical"),connection(active_id,"firecrab"),connection(aws_id,"awsSecretsManager")],
        "plans":[plan(retired_id),plan(active_id),account_plan],
        "service_environments":{service_id.to_string():{"TOKEN":"retained-deployment-value"}}
    });
    let (_lock, key) = vault.open().unwrap();
    let mut ciphertext = serde_json::to_vec(&legacy).unwrap();
    let nonce = [7; 12];
    key.seal_in_place_append_tag(
        Nonce::assume_unique_for_key(nonce),
        Aad::from(MAGIC),
        &mut ciphertext,
    )
    .unwrap();
    let mut file = MAGIC.to_vec();
    file.extend_from_slice(&nonce);
    file.extend(ciphertext);
    atomic_write(&vault.root.join("vault.bin"), &file).unwrap();
    drop(_lock);

    vault
        .read(|contents| {
            assert_eq!(contents.secrets[0].value, "retained-imported-value");
            let source = contents.secrets[0].summary.source.as_ref().unwrap();
            assert_eq!(source.provider, "infisical");
            assert_eq!(source.connection_id, retired_id);
            assert_eq!(source.imported_at, 123);
            assert_eq!(contents.connections.len(), 2);
            assert_eq!(contents.connections[0].summary.id, active_id);
            assert_eq!(contents.connections[1].summary.id, aws_id);
            assert_eq!(
                contents.connections[0].credentials["token"],
                "firecrab-private-token"
            );
            assert_eq!(contents.plans.len(), 2);
            assert_eq!(contents.plans[0].connection_id, active_id);
            assert!(contents.plans[1].account.is_some());
            assert_eq!(
                contents.service_environments[&service_id]["TOKEN"],
                "retained-deployment-value"
            );
            Ok(())
        })
        .unwrap();
    // Reads leave the old encrypted file intact; the next successful write persists cleanup.
    assert_eq!(fs::read(vault.root.join("vault.bin")).unwrap(), file);
    vault.write(|_| Ok(())).unwrap();
    let mut saved = fs::read(vault.root.join("vault.bin")).unwrap();
    let nonce = saved[MAGIC.len()..MAGIC.len() + 12].try_into().unwrap();
    let plain = key
        .open_in_place(
            Nonce::assume_unique_for_key(nonce),
            Aad::from(MAGIC),
            &mut saved[MAGIC.len() + 12..],
        )
        .unwrap();
    let saved: serde_json::Value = serde_json::from_slice(plain).unwrap();
    assert!(!saved.to_string().contains("infisical-private-token"));
    assert!(
        saved["connections"][0]["summary"]["settings"]
            .get("projectId")
            .is_none()
    );
    assert_eq!(
        saved["secrets"][0]["summary"]["source"]["provider"],
        "infisical"
    );
    Vault::new(vault.root.clone())
        .read(|contents| {
            assert_eq!(contents.connections.len(), 2);
            assert_eq!(contents.secrets[0].value, "retained-imported-value");
            Ok(())
        })
        .unwrap();
}

#[test]
fn vault_migration_rejects_unknown_providers_and_malformed_contents() {
    for plain in [
        "null",
        "[]",
        "1",
        r#"{"secrets":[],"connections":[null],"plans":[]}"#,
        r#"{"secrets":[],"connections":[{"summary":{"provider":"unknown"}}],"plans":[]}"#,
        r#"{"secrets":[],"connections":[{"summary":{"provider":"infisical","id":"invalid"}}],"plans":[]}"#,
    ] {
        assert!(matches!(
            decode_contents(plain.as_bytes()),
            Err(SecretError::Storage)
        ));
    }
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
                    provider: Provider::Firecrab,
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
