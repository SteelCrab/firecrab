//! Operator view of deployed service security coverage.

mod activity;
mod analysis;
#[allow(dead_code)]
mod cve;
use axum::{Extension, Json, extract::State, http::StatusCode};
use serde::Serialize;
use uuid::Uuid;

use crate::{
    ai_sandbox::access::Caller, error::AppError, handlers::micro_services::database,
    server::RequestId, state::AppState,
};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Service {
    id: Uuid,
    name: String,
    deployment_id: Uuid,
    digest: Option<String>,
    replicas: usize,
    signals: Vec<activity::Signal>,
    sampled_records: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Coverage {
    services: Vec<Service>,
    cve_scan: &'static str,
    threat_detection: &'static str,
    ai_analysis: &'static str,
}

/// Report coverage explicitly; missing scanners must never appear as a clean scan.
pub(crate) async fn get(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    caller: Caller,
) -> Result<Json<Coverage>, AppError> {
    caller.operator().map_err(|_| {
        AppError::new(
            StatusCode::FORBIDDEN,
            "service_operator_required",
            "Only the operator can access service security",
            request.0,
        )
    })?;
    let store = state.store.clone();
    let services = database(request.0, move || {
        let mut services = Vec::new();
        for service in store.list_micro_services()? {
            if store.service_is_controller_owned(service.id)? {
                continue;
            }
            if let Some(deployment) = store.service_deployment(service.id)? {
                let instances = deployment
                    .instances
                    .iter()
                    .map(|instance| instance.vm_id.to_string())
                    .collect();
                let logs = store.service_logs(service.id)?;
                let (signals, sampled_records) =
                    activity::detect(&logs, &instances, chrono::Utc::now());
                services.push(Service {
                    id: service.id,
                    name: deployment.spec.name,
                    deployment_id: deployment.id,
                    digest: deployment.digest,
                    replicas: deployment.instances.len(),
                    signals,
                    sampled_records,
                });
            }
        }
        Ok(services)
    })
    .await?;
    Ok(Json(Coverage {
        services,
        cve_scan: "unavailable",
        threat_detection: "limited",
        ai_analysis: "on_demand",
    }))
}

/// Explicit, operator-requested explanation of aggregate activity.
pub(crate) use analysis::explain;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::templates::TemplateRegistry;
    use axum::response::IntoResponse;

    #[tokio::test]
    async fn coverage_requires_operator_and_reports_unavailable_scanners() {
        let root = tempfile::tempdir().unwrap();
        let state = AppState::with_db_file(
            TemplateRegistry::from_specs(root.path(), std::iter::empty()).unwrap(),
            root.path().join("state.db"),
        )
        .await
        .unwrap();
        let delegate: Caller =
            serde_json::from_value(serde_json::json!({"principal":"local", "operator":false}))
                .unwrap();
        let request = || Extension(RequestId(Uuid::new_v4()));
        let denied = match get(State(state.clone()), request(), delegate).await {
            Err(error) => error,
            Ok(_) => panic!("delegate must not access service security"),
        };
        assert_eq!(denied.into_response().status(), StatusCode::FORBIDDEN);
        let result = get(State(state), request(), Caller::default())
            .await
            .unwrap()
            .0;
        assert!(result.services.is_empty());
        assert_eq!(result.cve_scan, "unavailable");
        assert_eq!(result.threat_detection, "limited");
        assert_eq!(result.ai_analysis, "on_demand");
    }
    #[tokio::test]
    async fn coverage_lists_deployment_identity_without_sensitive_configuration() {
        use firecrab_api_types::{MicroServiceDeployment, MicroServiceResponse, MicroServiceSpec};
        use serde_json::json;

        let root = tempfile::tempdir().unwrap();
        let state = AppState::with_db_file(
            TemplateRegistry::from_specs(root.path(), std::iter::empty()).unwrap(),
            root.path().join("state.db"),
        )
        .await
        .unwrap();
        let service_id = Uuid::new_v4();
        let deployment_id = Uuid::new_v4();
        let spec: MicroServiceSpec = serde_json::from_value(json!({
            "name": "orders", "image": "example.test/orders:v1", "port": 8080,
            "ram": 128, "cpu": 1, "env": {"PRIVATE_VALUE": "secret-value"}
        }))
        .unwrap();
        state
            .store
            .insert_micro_service(&MicroServiceResponse {
                id: service_id,
                spec: spec.clone(),
            })
            .unwrap();
        let undeployed: MicroServiceSpec = serde_json::from_value(json!({
            "name": "not-deployed", "image": "example.test/other:v1", "port": 8080,
            "ram": 128, "cpu": 1
        }))
        .unwrap();
        state
            .store
            .insert_micro_service(&MicroServiceResponse {
                id: Uuid::new_v4(),
                spec: undeployed,
            })
            .unwrap();
        let deployment: MicroServiceDeployment = serde_json::from_value(json!({
            "id": deployment_id, "serviceId": service_id, "phase": "running", "spec": spec,
            "request": {"hostPort": 0}, "digest": "sha256:1234", "instances": [], "log": [], "error": null
        })).unwrap();
        state.store.save_service_deployment(&deployment).unwrap();
        let coverage = get(
            State(state),
            Extension(RequestId(Uuid::new_v4())),
            Caller::default(),
        )
        .await
        .unwrap()
        .0;
        let result = serde_json::to_value(coverage).unwrap();
        assert_eq!(result["services"].as_array().unwrap().len(), 1);
        assert_eq!(result["services"][0]["id"], service_id.to_string());
        assert_eq!(
            result["services"][0]["deploymentId"],
            deployment_id.to_string()
        );
        assert_eq!(result["services"][0]["name"], "orders");
        assert_eq!(result["services"][0]["digest"], "sha256:1234");
        assert_eq!(result["services"][0]["replicas"], 0);
        assert!(!result.to_string().contains("secret-value"));
        assert_eq!(result["cveScan"], "unavailable");
    }
}
