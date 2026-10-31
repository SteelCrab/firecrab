use firecrab_stack::{
    ChangeResourceOwner, ManagedResource, Manager, ResourceOwner, resource_kinds,
};
use uuid::Uuid;

#[test]
fn inventory_resource_identities_normalize_uuid_aliases_and_reject_unknown_kinds() {
    assert_eq!(resource_kinds().len(), 43);
    let id = Uuid::new_v4();
    let mut resource = ManagedResource {
        kind: "network".into(),
        id: id.simple().to_string().to_uppercase(),
    };
    resource.normalize().unwrap();
    assert_eq!(resource.id, id.to_string());
    let mut named = ManagedResource {
        kind: "registry".into(),
        id: "ghcr.io".into(),
    };
    named.normalize().unwrap();
    for (kind, id) in [
        ("unknown", "ghcr.io"),
        ("network", "named"),
        ("volume", "named"),
        ("storage_root", "named"),
        ("security_group", "named"),
        ("shell", "named"),
        ("schedule", "named"),
        ("pool", "named"),
        ("queue", "named"),
        ("database_connection", "named"),
        ("storage_connection", "named"),
        ("secret", "named"),
        ("secret_connection", "named"),
        ("ai_connection", "named"),
        ("ai_model", "named"),
        ("sandbox_environment", "named"),
        ("auth", "GitHub"),
        ("auth", "unknown"),
        ("registry", "https://registry.example.com"),
        ("registry", "user@registry.example.com"),
        ("vm", "00000000-0000-0000-0000-000000000000"),
        ("registry", "bad\nvalue"),
    ] {
        assert!(
            ManagedResource {
                kind: kind.into(),
                id: id.into()
            }
            .normalize()
            .is_err()
        );
    }
}

#[test]
fn ownership_requests_reject_invalid_owner_keys_and_preserve_exact_expected_owner() {
    let owner = ResourceOwner {
        manager: Manager::Stack,
        id: Uuid::new_v4(),
        key: "app".into(),
    };
    let mut request = ChangeResourceOwner {
        expected_endpoint_id: Uuid::new_v4(),
        operation_id: Uuid::new_v4(),
        resource: ManagedResource {
            kind: "vm".into(),
            id: Uuid::new_v4().to_string(),
        },
        expected_generation: 3,
        expected_owner: Some(owner.clone()),
        owner: None,
    };
    request.validate().unwrap();
    assert_eq!(request.expected_owner, Some(owner));
    request.expected_owner.as_mut().unwrap().key = "invalid key".into();
    assert!(request.validate().is_err());
    request.expected_owner = None;
    request.operation_id = Uuid::nil();
    assert!(request.validate().is_err());
}
