//! Policy CRUD serialized with VM starts and service deployment claims.

use super::{micro_services::database, vms::parse_id};

#[cfg(test)]
mod tests;
use crate::{
    error::AppError, extract::ValidatedJson, network_policy, persistence::PersistenceError,
    server::RequestId, state::AppState,
};
use axum::{
    Json,
    extract::{Extension, Path, State},
    http::StatusCode,
};
use firecrab_api_types::{
    CreateSecurityGroupRequest, MAX_SECURITY_GROUP_RULES, ReplaceSecurityGroupRulesRequest,
    SecurityGroupResponse, UpdateSecurityGroupRequest,
};
use firecrab_helper_protocol::security_group::SecurityGroupRuleSpec;
use std::collections::BTreeMap;
use uuid::Uuid;

fn field(request: Uuid, key: &str, message: &str) -> AppError {
    AppError::validation(BTreeMap::from([(key.into(), message.into())]), request)
}

fn validate_name(name: &str, request: Uuid) -> Result<(), AppError> {
    if name.trim().is_empty() || name.len() > 64 || name.chars().any(char::is_control) {
        return Err(field(
            request,
            "name",
            "must contain 1–64 bytes without control characters",
        ));
    }
    Ok(())
}

async fn group(
    state: &AppState,
    id: Uuid,
    request: Uuid,
) -> Result<SecurityGroupResponse, AppError> {
    let store = state.store.clone();
    database(request, move || store.security_group(id))
        .await?
        .ok_or_else(|| AppError::not_found(request))
}

pub async fn list(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
) -> Result<Json<Vec<SecurityGroupResponse>>, AppError> {
    database(request.0, move || state.store.list_security_groups())
        .await
        .map(Json)
}

pub async fn get(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    Path(id): Path<String>,
) -> Result<Json<SecurityGroupResponse>, AppError> {
    group(&state, parse_id(&id, request.0)?, request.0)
        .await
        .map(Json)
}

pub async fn create(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    ValidatedJson(body): ValidatedJson<CreateSecurityGroupRequest>,
) -> Result<(StatusCode, Json<SecurityGroupResponse>), AppError> {
    validate_name(&body.name, request.0)?;
    let _guard = state.network_mutations.lock().await;
    let store = state.store.clone();
    let network = database(request.0, move || {
        store.micro_network(body.micro_network_id)
    })
    .await?
    .ok_or_else(|| field(request.0, "microNetworkId", "network does not exist"))?;
    if network.ipv6_cidr.is_some() {
        return Err(field(
            request.0,
            "microNetworkId",
            "requires an IPv4-only MicroNetwork",
        ));
    }
    let group = SecurityGroupResponse {
        id: Uuid::new_v4(),
        name: body.name,
        micro_network_id: network.id,
        rules: vec![],
    };
    let record = group.clone();
    database(request.0, move || {
        state.store.insert_security_group(&record)
    })
    .await?;
    Ok((StatusCode::CREATED, Json(group)))
}

pub async fn update(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    Path(id): Path<String>,
    ValidatedJson(body): ValidatedJson<UpdateSecurityGroupRequest>,
) -> Result<Json<SecurityGroupResponse>, AppError> {
    let id = parse_id(&id, request.0)?;
    validate_name(&body.name, request.0)?;
    let _guard = state.network_mutations.lock().await;
    let store = state.store.clone();
    if !database(request.0, move || {
        store.rename_security_group(id, &body.name)
    })
    .await?
    {
        return Err(AppError::not_found(request.0));
    }
    group(&state, id, request.0).await.map(Json)
}

pub async fn replace_rules(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    Path(id): Path<String>,
    ValidatedJson(mut body): ValidatedJson<ReplaceSecurityGroupRulesRequest>,
) -> Result<Json<SecurityGroupResponse>, AppError> {
    let id = parse_id(&id, request.0)?;
    if body.rules.len() > MAX_SECURITY_GROUP_RULES {
        return Err(field(request.0, "rules", "at most 256 rules are allowed"));
    }
    for rule in &mut body.rules {
        let parsed = SecurityGroupRuleSpec::from_api(rule)
            .map_err(|message| field(request.0, "rules", &message))?;
        rule.peer_cidr = format!("{}/{}", parsed.peer_address, parsed.peer_prefix);
    }
    let _services = state.service_mutations.lock().await;
    let _network = state.network_mutations.lock().await;
    group(&state, id, request.0).await?;
    ensure_idle(&state, id, request.0).await?;
    let store = state.store.clone();
    if !database(request.0, move || {
        store.replace_security_group_rules(id, &body.rules)
    })
    .await?
    {
        return Err(AppError::not_found(request.0));
    }
    group(&state, id, request.0).await.map(Json)
}

pub async fn delete(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    Path(id): Path<String>,
) -> Result<StatusCode, AppError> {
    let id = parse_id(&id, request.0)?;
    let _services = state.service_mutations.lock().await;
    let _network = state.network_mutations.lock().await;
    let store = state.store.clone();
    let deleted = tokio::task::spawn_blocking(move || store.delete_security_group(id))
        .await
        .map_err(|_| AppError::internal(request.0))?
        .map_err(|error| match error {
            PersistenceError::SecurityGroupInUse => AppError::conflict(
                "security_group_in_use",
                "security group is referenced by a service or VM",
                request.0,
            ),
            _ => AppError::internal(request.0),
        })?;
    if !deleted {
        return Err(AppError::not_found(request.0));
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn ensure_idle(state: &AppState, id: Uuid, request: Uuid) -> Result<(), AppError> {
    let vms = state
        .vms
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .values()
        .filter(|vm| vm.security_group_id == Some(id))
        .cloned()
        .collect::<Vec<_>>();
    for vm in vms {
        if network_policy::vm_is_active(state, &vm).map_err(|_| AppError::internal(request))? {
            return Err(AppError::conflict(
                "security_group_in_use",
                "stop affected VMs before changing rules",
                request,
            ));
        }
    }
    let store = state.store.clone();
    let busy = database(request, move || {
        for service in store.list_micro_services()? {
            if let Some(deployment) = store.service_deployment(service.id)?
                && deployment.spec.security_group_id == Some(id)
                && (!matches!(
                    deployment.phase,
                    firecrab_api_types::ServicePhase::Stopped
                        | firecrab_api_types::ServicePhase::Failed
                ) || !deployment.instances.is_empty())
            {
                return Ok(true);
            }
        }
        Ok(false)
    })
    .await?;
    if busy {
        return Err(AppError::conflict(
            "security_group_in_use",
            "stop the service deployment before changing rules",
            request,
        ));
    }
    Ok(())
}
