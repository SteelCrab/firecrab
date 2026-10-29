//! Operator view of deployed service security coverage.
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
                services.push(Service {
                    id: service.id,
                    name: deployment.spec.name,
                    deployment_id: deployment.id,
                    digest: deployment.digest,
                    replicas: deployment.instances.len(),
                });
            }
        }
        Ok(services)
    })
    .await?;
    Ok(Json(Coverage {
        services,
        cve_scan: "unavailable",
        threat_detection: "unavailable",
        ai_analysis: "unavailable",
    }))
}

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
        assert_eq!(result.threat_detection, "unavailable");
        assert_eq!(result.ai_analysis, "unavailable");
    }
}
