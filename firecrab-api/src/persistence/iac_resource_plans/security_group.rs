//! Shared ordered policy configuration. Runtime consumers keep their own identities.

use super::*;
use firecrab_stack::SecurityGroupConfiguration;

#[cfg(test)]
mod tests;

pub(super) fn validate(config: &mut SecurityGroupConfiguration) -> Result<(), PersistenceError> {
    if config.name.trim().is_empty()
        || config.name.len() > 64
        || config.name.chars().any(char::is_control)
        || config.micro_network_id.is_nil()
        || config.rules.len() > firecrab_api_types::MAX_SECURITY_GROUP_RULES
    {
        return Err(PersistenceError::InvalidIacOwner);
    }
    for rule in &mut config.rules {
        let parsed =
            firecrab_helper_protocol::security_group::SecurityGroupRuleSpec::from_api(rule)
                .map_err(|_| PersistenceError::InvalidIacOwner)?;
        rule.peer_cidr = format!("{}/{}", parsed.peer_address, parsed.peer_prefix);
    }
    Ok(())
}

pub(super) fn observe(
    conn: &Connection,
    target: &ManagedResource,
) -> Result<Option<SecurityGroupConfiguration>, PersistenceError> {
    let id = target
        .id
        .parse()
        .map_err(|_| PersistenceError::InvalidIacOwner)?;
    super::super::security_groups::read(conn, id).map(|group| {
        group.map(|group| SecurityGroupConfiguration {
            name: group.name,
            micro_network_id: group.micro_network_id,
            rules: group.rules,
        })
    })
}

pub(super) fn dependencies(
    conn: &Connection,
    config: &SecurityGroupConfiguration,
) -> Result<Vec<ResourcePlanDependency>, PersistenceError> {
    let valid: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM micro_networks WHERE id=?1 AND ipv6_cidr IS NULL)",
        [config.micro_network_id.to_string()],
        |row| row.get(0),
    )?;
    if !valid {
        return Err(PersistenceError::InvalidStackResource);
    }
    let target = ManagedResource {
        kind: "network".into(),
        id: config.micro_network_id.to_string(),
    };
    iac_native::ensure_idle(conn, &target)?;
    super::super::iac_host::ensure_resource_idle(conn, &target)?;
    if super::super::stacks::resource_apply_busy(
        conn,
        firecrab_stack::ResourceKind::Network,
        config.micro_network_id,
    )? {
        return Err(PersistenceError::StackApplyBusy);
    }
    // Network UUID, subnet and address family are immutable in the native API.
    Ok(vec![ResourcePlanDependency {
        ownership: iac_owners::read(conn, &target)?,
        configuration_generation: 0,
    }])
}

pub(super) fn deletable(
    conn: &Connection,
    target: &ManagedResource,
) -> Result<(), PersistenceError> {
    let used: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM vms WHERE security_group_id=?1)
        OR EXISTS(SELECT 1 FROM micro_services WHERE json_extract(spec,'$.securityGroupId')=?1)",
        [&target.id],
        |row| row.get(0),
    )?;
    if used {
        return Err(PersistenceError::IacManagedDependent);
    }
    super::super::security_groups::ensure_rules_idle(conn, &target.id)?;
    Ok(())
}

pub(super) fn differences(
    before: &SecurityGroupConfiguration,
    desired: &SecurityGroupConfiguration,
) -> Result<Vec<String>, PersistenceError> {
    if before.micro_network_id != desired.micro_network_id {
        return Err(PersistenceError::InvalidStackResource);
    }
    let mut fields = Vec::new();
    if before.name != desired.name {
        fields.push("name".into());
    }
    if before.rules != desired.rules {
        fields.push("rules".into());
    }
    Ok(fields)
}

pub(super) fn write(
    conn: &Connection,
    target: &ManagedResource,
    before: Option<&Observation>,
    desired: &SecurityGroupConfiguration,
) -> Result<(), PersistenceError> {
    if let Some(Observation::SecurityGroup(before)) = before
        && before.rules != desired.rules
    {
        super::super::security_groups::ensure_rules_idle(conn, &target.id)?;
    }
    conn.execute(
        "INSERT INTO security_groups(id,micro_network_id,name) VALUES(?1,?2,?3)
        ON CONFLICT(id) DO UPDATE SET name=excluded.name",
        params![
            target.id,
            desired.micro_network_id.to_string(),
            desired.name
        ],
    )?;
    conn.execute(
        "DELETE FROM security_group_rules WHERE group_id=?1",
        [&target.id],
    )?;
    for (position, rule) in desired.rules.iter().enumerate() {
        conn.execute(
            "INSERT INTO security_group_rules(group_id,position,rule) VALUES(?1,?2,?3)",
            params![target.id, position as i64, encode(rule)?],
        )?;
    }
    Ok(())
}

pub(super) fn remove(conn: &Connection, target: &ManagedResource) -> Result<(), PersistenceError> {
    deletable(conn, target)?;
    conn.execute(
        "DELETE FROM security_group_rules WHERE group_id=?1",
        [&target.id],
    )?;
    conn.execute("DELETE FROM security_groups WHERE id=?1", [&target.id])?;
    Ok(())
}
