use super::*;
use crate::secrets::{Secret, SecretSummary};
use firecrab_stack::ResourceRemovalPolicy;

fn input(store: &Store, manager: Manager) -> PlanResource {
    PlanResource {
        operation_id: Uuid::new_v4(),
        expected_endpoint_id: store.iac_endpoint_id().unwrap(),
        owner: ResourceOwner {
            manager,
            id: if manager == Manager::Stack {
                store.register_stack("private-plan").unwrap().id
            } else {
                Uuid::new_v4()
            },
            key: "private-value".into(),
        },
        resource: None,
        removal: None,
        desired: Some(ResourceConfiguration::Secret(SecretConfiguration {
            name: "MANAGED".into(),
            description: "private config".into(),
            value: Some(CredentialInput::WriteOnly { version: 1 }),
        })),
    }
}
fn command(plan: &ResourcePlan) -> ApplyResourcePlan {
    ApplyResourcePlan {
        expected_endpoint_id: plan.ownership.endpoint_id,
        plan_sha256: plan.plan_sha256.clone(),
    }
}
fn upload(
    store: &Store,
    vault: &Vault,
    plan: &ResourcePlan,
    version: u64,
    value: &str,
) -> Result<(), PersistenceError> {
    store.upload_resource_value(
        vault,
        plan.operation_id,
        UploadResourceValue {
            expected_endpoint_id: plan.ownership.endpoint_id,
            plan_sha256: plan.plan_sha256.clone(),
            version,
            value: value.into(),
        },
        101,
    )
}
fn apply(
    store: &Store,
    vault: &Vault,
    plan: &ResourcePlan,
    now: u64,
) -> Result<ResourcePlanReceipt, PersistenceError> {
    store.apply_vault_resource_plan(vault, plan.operation_id, &command(plan), now)
}
fn add(vault: &Vault, name: &str, value: &str) -> Uuid {
    let id = Uuid::new_v4();
    vault
        .write(|contents| {
            contents.secrets.push(Secret {
                summary: SecretSummary {
                    id,
                    name: name.into(),
                    description: String::new(),
                    version: 1,
                    created_at: 1,
                    updated_at: 1,
                    source: None,
                },
                value: value.into(),
            });
            Ok(())
        })
        .unwrap();
    id
}

#[test]
fn iac_resource_secret_both_managers_create_noop_metadata_update_rotate_and_retain() {
    for manager in [Manager::Stack, Manager::Terraform] {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(&root.path().join("state.db")).unwrap();
        let vault = Vault::new(root.path().join("vault"));
        let mut request = input(&store, manager);
        let plan = store
            .plan_vault_resource(&vault, request.clone(), 100)
            .unwrap();
        assert!(matches!(
            apply(&store, &vault, &plan, 101),
            Err(PersistenceError::IacCredentialVersionRequired)
        ));
        upload(&store, &vault, &plan, 1, "private-canary-never-public").unwrap();
        upload(&store, &vault, &plan, 1, "private-canary-never-public").unwrap();
        assert!(matches!(
            upload(&store, &vault, &plan, 1, "different-value"),
            Err(PersistenceError::IacOperationIdentity)
        ));
        assert!(matches!(
            upload(&store, &vault, &plan, 2, "wrong-marker"),
            Err(PersistenceError::IacOperationIdentity)
        ));
        vault
            .read(|contents| {
                assert!(contents.secrets.is_empty());
                Ok(())
            })
            .unwrap();
        let receipt = apply(&store, &vault, &plan, 102).unwrap();
        assert_eq!(apply(&store, &vault, &plan, u64::MAX).unwrap(), receipt);
        request.resource = Some(receipt.ownership.resource.clone());
        request.operation_id = Uuid::new_v4();
        let noop = store
            .plan_vault_resource(&vault, request.clone(), 103)
            .unwrap();
        assert_eq!(noop.action, ResourcePlanAction::Noop);
        apply(&store, &vault, &noop, 104).unwrap();
        request.operation_id = Uuid::new_v4();
        let Some(ResourceConfiguration::Secret(config)) = request.desired.as_mut() else {
            unreachable!()
        };
        config.description = "changed metadata".into();
        let update = store
            .plan_vault_resource(&vault, request.clone(), 105)
            .unwrap();
        assert_eq!(update.changed_fields, vec!["description"]);
        apply(&store, &vault, &update, 106).unwrap();
        vault
            .read(|contents| {
                assert_eq!(contents.secrets.len(), 1);
                assert_eq!(contents.secrets[0].summary.version, 2);
                assert_eq!(contents.secrets[0].value, "private-canary-never-public");
                assert!(contents.iac_plan_inputs.is_empty());
                assert!(contents.iac_plan_executions.is_empty());
                Ok(())
            })
            .unwrap();
        request.operation_id = Uuid::new_v4();
        let Some(ResourceConfiguration::Secret(config)) = request.desired.as_mut() else {
            unreachable!()
        };
        config.value = Some(CredentialInput::WriteOnly { version: 2 });
        let rotation = store
            .plan_vault_resource(&vault, request.clone(), 107)
            .unwrap();
        assert_eq!(rotation.changed_fields, vec!["valueVersion"]);
        upload(&store, &vault, &rotation, 2, "second-private-canary").unwrap();
        apply(&store, &vault, &rotation, 108).unwrap();
        request.operation_id = Uuid::new_v4();
        request.desired = None;
        let deletion = store.plan_vault_resource(&vault, request, 109).unwrap();
        assert!(!deletion.removal.unwrap().delete_data);
        let deleted = apply(&store, &vault, &deletion, 110).unwrap();
        assert!(deleted.ownership.owner.is_none());
        vault
            .read(|contents| {
                assert_eq!(contents.secrets[0].summary.version, 3);
                assert_eq!(contents.secrets[0].value, "second-private-canary");
                Ok(())
            })
            .unwrap();
        let conn = store.lock();
        let mut stmt = conn
            .prepare("SELECT definition,COALESCE(receipt,'') FROM iac_resource_plans")
            .unwrap();
        for row in stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .unwrap()
        {
            let (definition, receipt) = row.unwrap();
            for canary in ["private-canary-never-public", "second-private-canary"] {
                assert!(!definition.contains(canary));
                assert!(!receipt.contains(canary));
            }
        }
        drop(stmt);
        drop(conn);
        let ciphertext = std::fs::read(root.path().join("vault/vault.bin")).unwrap();
        assert!(!String::from_utf8_lossy(&ciphertext).contains("second-private-canary"));
    }
}

#[test]
fn iac_resource_secret_lost_sql_ack_recovers_original_value_after_restart_and_expiry() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("state.db");
    let vault_path = root.path().join("vault");
    let store = Store::open(&path).unwrap();
    let vault = Vault::new(vault_path.clone());
    let source = add(&vault, "SOURCE", "original-private-source");
    let mut request = input(&store, Manager::Stack);
    let Some(ResourceConfiguration::Secret(config)) = request.desired.as_mut() else {
        unreachable!()
    };
    config.value = Some(CredentialInput::Secret {
        id: source,
        version: 1,
    });
    let plan = store
        .plan_vault_resource(&vault, request.clone(), 100)
        .unwrap();
    store.lock().execute_batch("CREATE TRIGGER reject_secret_ack BEFORE UPDATE OF receipt ON iac_resource_plans BEGIN SELECT RAISE(ABORT,'injected_lost_ack'); END").unwrap();
    assert!(apply(&store, &vault, &plan, 101).is_err());
    assert_eq!(
        store
            .iac_native_operation(plan.operation_id)
            .unwrap()
            .unwrap()
            .state,
        NativeOperationState::NeedsRecovery
    );
    assert!(
        store
            .resource_plan_receipt(plan.operation_id)
            .unwrap()
            .is_none()
    );
    assert!(matches!(
        store.delete_empty_stack("private-plan", request.owner.id),
        Err(PersistenceError::IacResourceBusy)
    ));
    vault
        .write(|contents| {
            let target = contents
                .secrets
                .iter()
                .find(|secret| secret.summary.id.to_string() == plan.ownership.resource.id)
                .unwrap();
            assert_eq!(target.summary.version, 1);
            assert_eq!(target.value, "original-private-source");
            assert_eq!(contents.iac_plan_executions.len(), 1);
            contents
                .secrets
                .retain(|secret| secret.summary.id != source);
            Ok(())
        })
        .unwrap();
    store
        .lock()
        .execute_batch("DROP TRIGGER reject_secret_ack")
        .unwrap();
    drop(store);
    drop(vault);
    let reopened = Store::open(&path).unwrap();
    let vault = Vault::new(vault_path.clone());
    let receipt = apply(&reopened, &vault, &plan, u64::MAX).unwrap();
    assert_eq!(receipt.plan, plan);
    vault
        .read(|contents| {
            assert_eq!(contents.secrets.len(), 1);
            assert_eq!(contents.secrets[0].summary.version, 1);
            assert_eq!(contents.secrets[0].value, "original-private-source");
            assert!(contents.iac_plan_executions.is_empty());
            Ok(())
        })
        .unwrap();
    std::fs::write(vault_path.join("vault.bin"), b"corrupt-current-vault").unwrap();
    assert_eq!(apply(&reopened, &vault, &plan, u64::MAX).unwrap(), receipt);
}

#[test]
fn iac_resource_secret_native_value_drift_needs_new_marker_and_metadata_edits_keep_proof() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(&root.path().join("state.db")).unwrap();
    let vault = Vault::new(root.path().join("vault"));
    let mut request = input(&store, Manager::Terraform);
    let plan = store
        .plan_vault_resource(&vault, request.clone(), 100)
        .unwrap();
    upload(&store, &vault, &plan, 1, "initial-value").unwrap();
    request.resource = Some(
        apply(&store, &vault, &plan, 102)
            .unwrap()
            .ownership
            .resource,
    );
    let id = request.resource.as_ref().unwrap().id.parse().unwrap();
    vault
        .write(|contents| {
            store.note_native_secret_write(id, 1, false)?;
            contents.secrets[0].summary.version += 1;
            Ok(())
        })
        .unwrap();
    request.operation_id = Uuid::new_v4();
    assert_eq!(
        store
            .plan_vault_resource(&vault, request.clone(), 103)
            .unwrap()
            .action,
        ResourcePlanAction::Noop
    );
    vault
        .write(|contents| {
            store.note_native_secret_write(id, 2, true)?;
            contents.secrets[0].summary.version += 1;
            contents.secrets[0].value = "native-rotated".into();
            Ok(())
        })
        .unwrap();
    request.operation_id = Uuid::new_v4();
    assert!(matches!(
        store.plan_vault_resource(&vault, request.clone(), 104),
        Err(PersistenceError::IacCredentialVersionRequired)
    ));
    let Some(ResourceConfiguration::Secret(config)) = request.desired.as_mut() else {
        unreachable!()
    };
    config.value = None;
    assert_eq!(
        store
            .plan_vault_resource(&vault, request.clone(), 105)
            .unwrap()
            .action,
        ResourcePlanAction::Noop
    );
    request.operation_id = Uuid::new_v4();
    let Some(ResourceConfiguration::Secret(config)) = request.desired.as_mut() else {
        unreachable!()
    };
    config.value = Some(CredentialInput::WriteOnly { version: 2 });
    let changed = store.plan_vault_resource(&vault, request, 106).unwrap();
    upload(&store, &vault, &changed, 2, "explicit-new-value").unwrap();
    apply(&store, &vault, &changed, 107).unwrap();
    vault
        .read(|contents| {
            assert_eq!(contents.secrets[0].summary.version, 4);
            assert_eq!(contents.secrets[0].value, "explicit-new-value");
            Ok(())
        })
        .unwrap();
}

#[test]
fn iac_resource_secret_explicit_delete_removes_only_exact_owned_data() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(&root.path().join("state.db")).unwrap();
    let vault = Vault::new(root.path().join("vault"));
    let peer = add(&vault, "BORROWED", "preserved-peer");
    let mut request = input(&store, Manager::Terraform);
    let plan = store
        .plan_vault_resource(&vault, request.clone(), 100)
        .unwrap();
    upload(&store, &vault, &plan, 1, "delete-this-value").unwrap();
    request.resource = Some(
        apply(&store, &vault, &plan, 102)
            .unwrap()
            .ownership
            .resource,
    );
    request.operation_id = Uuid::new_v4();
    request.desired = None;
    request.removal = Some(ResourceRemovalPolicy { delete_data: true });
    let plan = store.plan_vault_resource(&vault, request, 103).unwrap();
    assert!(plan.removal.unwrap().delete_data);
    apply(&store, &vault, &plan, 104).unwrap();
    vault
        .read(|contents| {
            assert_eq!(contents.secrets.len(), 1);
            assert_eq!(contents.secrets[0].summary.id, peer);
            Ok(())
        })
        .unwrap();
}

#[test]
fn iac_resource_secret_parallel_vault_callers_return_one_original_receipt() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("state.db");
    let vault_path = root.path().join("vault");
    let store = Store::open(&path).unwrap();
    let vault = Vault::new(vault_path.clone());
    let plan = store
        .plan_vault_resource(&vault, input(&store, Manager::Terraform), 100)
        .unwrap();
    upload(&store, &vault, &plan, 1, "single-private-value").unwrap();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
    let handles: Vec<_> = (0..2)
        .map(|_| {
            let path = path.clone();
            let vault_path = vault_path.clone();
            let barrier = barrier.clone();
            let plan = plan.clone();
            std::thread::spawn(move || {
                let store = Store::open(&path).unwrap();
                let vault = Vault::new(vault_path);
                barrier.wait();
                apply(&store, &vault, &plan, 102).unwrap()
            })
        })
        .collect();
    barrier.wait();
    let receipts: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    assert_eq!(receipts[0], receipts[1]);
    vault
        .read(|contents| {
            assert_eq!(contents.secrets.len(), 1);
            assert_eq!(contents.secrets[0].summary.version, 1);
            Ok(())
        })
        .unwrap();
}

#[test]
fn iac_resource_secret_reference_rotation_and_expiry_reject_before_any_effect() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(&root.path().join("state.db")).unwrap();
    let vault = Vault::new(root.path().join("vault"));
    let source = add(&vault, "SOURCE", "original");
    let mut request = input(&store, Manager::Terraform);
    let Some(ResourceConfiguration::Secret(config)) = request.desired.as_mut() else {
        unreachable!()
    };
    config.value = Some(CredentialInput::Secret {
        id: source,
        version: 1,
    });
    let plan = store.plan_vault_resource(&vault, request, 100).unwrap();
    assert_eq!(plan.dependencies[0].configuration_generation, 1);
    vault
        .write(|contents| {
            contents.secrets[0].summary.version = 2;
            contents.secrets[0].value = "newer".into();
            Ok(())
        })
        .unwrap();
    assert!(apply(&store, &vault, &plan, 101).is_err());
    assert!(
        store
            .iac_native_operations(&plan.ownership.resource)
            .unwrap()
            .is_empty()
    );
    let plan = store
        .plan_vault_resource(&vault, input(&store, Manager::Terraform), 100)
        .unwrap();
    upload(&store, &vault, &plan, 1, "expired-private-input").unwrap();
    assert!(matches!(
        apply(&store, &vault, &plan, 600100),
        Err(PersistenceError::StaleIacExecution)
    ));
    vault
        .read(|contents| {
            assert_eq!(contents.secrets.len(), 1);
            assert!(contents.iac_plan_executions.is_empty());
            Ok(())
        })
        .unwrap();
}

#[test]
fn iac_resource_secret_reference_guards_survive_other_sqlite_connections_and_deleted_targets() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("state.db");
    let store = Store::open(&path).unwrap();
    let vault = Vault::new(root.path().join("vault"));
    let mut request = input(&store, Manager::Terraform);
    let created = store
        .plan_vault_resource(&vault, request.clone(), 100)
        .unwrap();
    upload(&store, &vault, &created, 1, "guarded").unwrap();
    let receipt = apply(&store, &vault, &created, 102).unwrap();
    let id = &receipt.ownership.resource.id;
    let service = Uuid::new_v4().to_string();
    let spec = serde_json::json!({"secrets":{"TOKEN":id}}).to_string();
    // Exercise the same persistence boundary used by every service writer, including rollback.
    store
        .lock()
        .execute(
            "INSERT INTO micro_services(id,name,spec) VALUES(?1,'referencing',?2)",
            params![service, spec],
        )
        .unwrap();
    request.resource = Some(receipt.ownership.resource);
    request.desired = None;
    request.removal = Some(ResourceRemovalPolicy { delete_data: true });
    request.operation_id = Uuid::new_v4();
    assert!(matches!(
        store.plan_vault_resource(&vault, request.clone(), 103),
        Err(PersistenceError::IacManagedDependent)
    ));
    request.removal = None;
    assert!(
        !store
            .plan_vault_resource(&vault, request.clone(), 103)
            .unwrap()
            .removal
            .unwrap()
            .delete_data
    );
    store
        .lock()
        .execute("DELETE FROM micro_services WHERE id=?1", [&service])
        .unwrap();
    request.operation_id = Uuid::new_v4();
    request.removal = Some(ResourceRemovalPolicy { delete_data: true });
    let deletion = store.plan_vault_resource(&vault, request, 104).unwrap();
    store.lock().execute_batch("CREATE TRIGGER reject_secret_ack BEFORE UPDATE OF receipt ON iac_resource_plans BEGIN SELECT RAISE(ABORT,'injected_lost_ack'); END").unwrap();
    assert!(apply(&store, &vault, &deletion, 105).is_err());
    let peer = rusqlite::Connection::open(&path).unwrap();
    assert!(
        peer.execute(
            "INSERT INTO micro_services(id,name,spec) VALUES(?1,'late',?2)",
            params![service, spec]
        )
        .is_err()
    );
    peer.execute_batch("DROP TRIGGER reject_secret_ack")
        .unwrap();
    apply(&store, &vault, &deletion, u64::MAX).unwrap();
    assert!(
        peer.execute(
            "INSERT INTO micro_services(id,name,spec) VALUES(?1,'late',?2)",
            params![service, spec]
        )
        .is_err()
    );
    assert_eq!(
        store
            .iac_native_operation(deletion.operation_id)
            .unwrap()
            .unwrap()
            .state,
        NativeOperationState::Succeeded
    );
}

#[test]
fn iac_resource_secret_pending_creation_reserves_terraform_address_across_kinds() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("state.db");
    let store = Store::open(&path).unwrap();
    let vault = Vault::new(root.path().join("vault"));
    let request = input(&store, Manager::Terraform);
    let plan = store
        .plan_vault_resource(&vault, request.clone(), 100)
        .unwrap();
    upload(&store, &vault, &plan, 1, "pending-private-input").unwrap();
    store.lock().execute_batch("CREATE TRIGGER reject_secret_ack BEFORE UPDATE OF receipt ON iac_resource_plans BEGIN SELECT RAISE(ABORT,'injected_lost_ack'); END").unwrap();
    assert!(apply(&store, &vault, &plan, 102).is_err());
    let other = Store::open(&path).unwrap();
    let mut conflicting = request;
    conflicting.operation_id = Uuid::new_v4();
    conflicting.desired = Some(ResourceConfiguration::Shell(
        firecrab_api_types::CreateShellRequest {
            name: "same-address".into(),
            description: None,
            content: "echo safe".into(),
        },
    ));
    assert!(matches!(
        other.plan_resource(conflicting.clone(), 103),
        Err(PersistenceError::IacResourceBusy)
    ));
    other
        .lock()
        .execute_batch("DROP TRIGGER reject_secret_ack")
        .unwrap();
    apply(&other, &vault, &plan, u64::MAX).unwrap();
    assert!(matches!(
        other.plan_resource(conflicting, 104),
        Err(PersistenceError::IacOwnershipConflict)
    ));
}

#[test]
fn iac_resource_secret_explicit_adoption_and_transfer_preserve_private_value() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(&root.path().join("state.db")).unwrap();
    let vault = Vault::new(root.path().join("vault"));
    let id = add(&vault, "IMPORTED", "retained-import-value");
    let mut request = input(&store, Manager::Terraform);
    request.resource = Some(ManagedResource {
        kind: "secret".into(),
        id: id.to_string(),
    });
    request.desired = Some(ResourceConfiguration::Secret(SecretConfiguration {
        name: "IMPORTED".into(),
        description: String::new(),
        value: None,
    }));
    assert!(matches!(
        store.plan_vault_resource(&vault, request.clone(), 100),
        Err(PersistenceError::IacOwnershipConflict)
    ));
    let change = ChangeResourceOwner {
        operation_id: Uuid::new_v4(),
        expected_endpoint_id: request.expected_endpoint_id,
        resource: request.resource.clone().unwrap(),
        expected_generation: 0,
        expected_owner: None,
        owner: Some(request.owner.clone()),
    };
    let adopted = store.change_iac_vault_owner(&change, &vault).unwrap();
    let plan = store
        .plan_vault_resource(&vault, request.clone(), 101)
        .unwrap();
    assert_eq!(plan.action, ResourcePlanAction::Noop);
    let receipt = apply(&store, &vault, &plan, 102).unwrap();
    let mut transfer = change;
    transfer.operation_id = Uuid::new_v4();
    transfer.expected_generation = adopted.generation;
    transfer.expected_owner = adopted.owner;
    transfer.owner = Some(ResourceOwner {
        manager: Manager::Stack,
        id: store.register_stack("adopted").unwrap().id,
        key: "imported".into(),
    });
    store.change_iac_vault_owner(&transfer, &vault).unwrap();
    assert_eq!(apply(&store, &vault, &plan, u64::MAX).unwrap(), receipt);
    request.operation_id = Uuid::new_v4();
    request.owner = transfer.owner.unwrap();
    let transferred = store.plan_vault_resource(&vault, request, 103).unwrap();
    assert_eq!(transferred.action, ResourcePlanAction::Noop);
    apply(&store, &vault, &transferred, 104).unwrap();
    vault
        .read(|contents| {
            assert_eq!(contents.secrets.len(), 1);
            assert_eq!(contents.secrets[0].summary.version, 1);
            assert_eq!(contents.secrets[0].value, "retained-import-value");
            Ok(())
        })
        .unwrap();
}

#[test]
fn iac_resource_secret_cleanup_expires_only_unadmitted_private_inputs_and_keeps_public_plans() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(&root.path().join("state.db")).unwrap();
    let vault_path = root.path().join("vault");
    let vault = Vault::new(vault_path.clone());
    assert_eq!(
        store.prune_resource_plan_inputs(&vault, u64::MAX).unwrap(),
        0
    );
    assert!(!vault_path.exists());
    let peer = add(&vault, "ORDINARY", "ordinary-retained-value");
    let plan = store
        .plan_vault_resource(&vault, input(&store, Manager::Terraform), 100)
        .unwrap();
    upload(&store, &vault, &plan, 1, "unadmitted-private-input").unwrap();
    assert_eq!(store.prune_resource_plan_inputs(&vault, 600099).unwrap(), 0);
    assert_eq!(store.prune_resource_plan_inputs(&vault, 600100).unwrap(), 1);
    assert_eq!(
        store.prune_resource_plan_inputs(&vault, u64::MAX).unwrap(),
        0
    );
    assert_eq!(
        store.resource_plan(plan.operation_id).unwrap().unwrap(),
        plan
    );
    vault
        .read(|contents| {
            assert!(contents.iac_plan_inputs.is_empty());
            assert!(contents.iac_plan_executions.is_empty());
            assert_eq!(contents.secrets.len(), 1);
            assert_eq!(contents.secrets[0].summary.id, peer);
            assert_eq!(contents.secrets[0].value, "ordinary-retained-value");
            Ok(())
        })
        .unwrap();
}

#[test]
fn iac_resource_secret_cleanup_preserves_admitted_effect_after_expiry_and_prunes_acknowledged_metadata()
 {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(&root.path().join("state.db")).unwrap();
    let vault = Vault::new(root.path().join("vault"));
    let plan = store
        .plan_vault_resource(&vault, input(&store, Manager::Terraform), 100)
        .unwrap();
    upload(&store, &vault, &plan, 1, "original-private-input").unwrap();
    store.lock().execute_batch("CREATE TRIGGER reject_secret_ack BEFORE UPDATE OF receipt ON iac_resource_plans BEGIN SELECT RAISE(ABORT,'injected_lost_ack'); END").unwrap();
    assert!(apply(&store, &vault, &plan, 102).is_err());
    let applied = vault
        .read(|contents| Ok(contents.iac_plan_executions[&plan.operation_id].clone()))
        .unwrap();
    assert_eq!(
        store.prune_resource_plan_inputs(&vault, u64::MAX).unwrap(),
        0
    );
    store
        .lock()
        .execute_batch("DROP TRIGGER reject_secret_ack")
        .unwrap();
    let receipt = apply(&store, &vault, &plan, u64::MAX).unwrap();
    // Recreate metadata left by an unsuccessful post-acknowledgement prune, without replaying the effect.
    vault
        .write(|contents| {
            contents
                .iac_plan_executions
                .insert(plan.operation_id, applied);
            Ok(())
        })
        .unwrap();
    assert_eq!(store.prune_resource_plan_inputs(&vault, 103).unwrap(), 1);
    assert_eq!(apply(&store, &vault, &plan, u64::MAX).unwrap(), receipt);
    vault
        .read(|contents| {
            assert!(contents.iac_plan_executions.is_empty());
            assert_eq!(contents.secrets.len(), 1);
            assert_eq!(contents.secrets[0].summary.version, 1);
            assert_eq!(contents.secrets[0].value, "original-private-input");
            Ok(())
        })
        .unwrap();
}

#[test]
fn iac_resource_secret_cleanup_removes_expired_preparation_only_after_failed_sql_admission() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(&root.path().join("state.db")).unwrap();
    let vault = Vault::new(root.path().join("vault"));
    let plan = store
        .plan_vault_resource(&vault, input(&store, Manager::Terraform), 100)
        .unwrap();
    upload(&store, &vault, &plan, 1, "never-admitted-input").unwrap();
    store.lock().execute_batch("CREATE TRIGGER reject_secret_lease BEFORE INSERT ON iac_native_operations BEGIN SELECT RAISE(ABORT,'injected_admission_failure'); END").unwrap();
    assert!(apply(&store, &vault, &plan, 102).is_err());
    assert!(
        store
            .iac_native_operations(&plan.ownership.resource)
            .unwrap()
            .is_empty()
    );
    vault
        .read(|contents| {
            assert!(contents.secrets.is_empty());
            assert!(
                contents
                    .iac_plan_executions
                    .contains_key(&plan.operation_id)
            );
            Ok(())
        })
        .unwrap();
    assert_eq!(store.prune_resource_plan_inputs(&vault, 600099).unwrap(), 0);
    assert_eq!(store.prune_resource_plan_inputs(&vault, 600100).unwrap(), 1);
    vault
        .read(|contents| {
            assert!(contents.secrets.is_empty());
            assert!(contents.iac_plan_executions.is_empty());
            assert!(contents.iac_plan_inputs.is_empty());
            Ok(())
        })
        .unwrap();
}

#[test]
fn iac_resource_secret_cleanup_never_expires_material_from_another_or_unproven_database() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(&root.path().join("state.db")).unwrap();
    let vault = Vault::new(root.path().join("vault"));
    let plan = store
        .plan_vault_resource(&vault, input(&store, Manager::Terraform), 100)
        .unwrap();
    upload(&store, &vault, &plan, 1, "preserved-unknown-input").unwrap();
    let other = Store::open(&root.path().join("other.db")).unwrap();
    assert_eq!(
        other.prune_resource_plan_inputs(&vault, u64::MAX).unwrap(),
        0
    );
    other
        .lock()
        .execute(
            "UPDATE iac_endpoint SET endpoint_id=?1",
            [plan.ownership.endpoint_id.to_string()],
        )
        .unwrap();
    assert_eq!(
        other.prune_resource_plan_inputs(&vault, u64::MAX).unwrap(),
        0
    );
    vault
        .read(|contents| {
            assert_eq!(
                contents.iac_plan_inputs[&plan.operation_id].value,
                "preserved-unknown-input"
            );
            Ok(())
        })
        .unwrap();
}
