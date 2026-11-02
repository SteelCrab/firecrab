//! Verify provider credentials before replacing the saved Auth connection.
use crate::{
    error::AppError,
    extract::ValidatedJson,
    pipe_auth::{self, Connection, Login, Provider, SavedAuth},
    server::RequestId,
    state::AppState,
};
use axum::{
    Extension, Json,
    extract::{Path, State},
    http::StatusCode,
};

/// Report a provider authentication failure without exposing credential values or upstream bodies.
pub(crate) fn auth_error(message: &'static str, id: RequestId) -> AppError {
    AppError::new(StatusCode::BAD_REQUEST, "pipe_auth_failed", message, id.0)
}

/// Read private provider credentials off the async runtime and hide database error details.
pub(crate) async fn saved(state: &AppState, id: RequestId) -> Result<Vec<SavedAuth>, AppError> {
    let store = state.store.clone();
    tokio::task::spawn_blocking(move || store.pipe_auths())
        .await
        .map_err(|_| AppError::internal(id.0))?
        .map_err(|_| AppError::internal(id.0))
}

/// List connected providers as metadata without serializing their stored secrets.
pub async fn list(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
) -> Result<Json<Vec<Connection>>, AppError> {
    Ok(Json(
        saved(&state, id)
            .await?
            .iter()
            .map(SavedAuth::connection)
            .collect(),
    ))
}

/// Verify a matching provider credential before atomically replacing its stored connection.
pub async fn save(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    Path(provider): Path<Provider>,
    ValidatedJson(login): ValidatedJson<Login>,
) -> Result<Json<Connection>, AppError> {
    if provider != login.provider() {
        return Err(auth_error("Provider does not match the credential", id));
    }
    let _claim = state.service_mutations.lock().await;
    let auth = pipe_auth::connect(login)
        .await
        .map_err(|message| auth_error(message, id))?;
    let connection = auth.connection();
    if let SavedAuth::Aws { credentials, .. } = &auth {
        super::pipe_storage::validate_aws_replacement(&state, credentials, id.0).await?;
    }
    let store = state.store.clone();
    super::stacks::database(id.0, move || store.put_pipe_auth(&auth)).await?;
    Ok(Json(connection))
}

/// Disconnect one provider while retaining other accounts and independently saved registry logins.
pub async fn delete(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    Path(provider): Path<Provider>,
) -> Result<Json<serde_json::Value>, AppError> {
    let _claim = state.service_mutations.lock().await;
    if provider == Provider::Aws {
        let store = state.store.clone();
        let records =
            super::micro_services::database(id.0, move || store.list_pipe_storage()).await?;
        if records.iter().any(|record| {
            record.spec.credential_ref == firecrab_api_types::StorageCredentialRef::PipeAuthAws
        }) {
            return Err(AppError::conflict(
                "storage_auth_in_use",
                "Disconnect or change the storage connections using Auth AWS first",
                id.0,
            ));
        }
    }
    let store = state.store.clone();
    super::stacks::database(id.0, move || store.delete_pipe_auth(provider)).await?;
    Ok(Json(serde_json::json!({ "deleted": true })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{oci::RegistryCredential, templates::TemplateRegistry};

    #[tokio::test]
    async fn shared_aws_auth_preserves_the_saved_registry_account_name() {
        let dir = tempfile::tempdir().unwrap();
        let templates = TemplateRegistry::from_specs(dir.path(), std::iter::empty()).unwrap();
        let state = AppState::with_db_file(templates, dir.path().join("state.db"))
            .await
            .unwrap();
        let id = RequestId(uuid::Uuid::nil());
        let registry = "123456789012.dkr.ecr.ap-northeast-2.amazonaws.com";
        state
            .store
            .put_registry_login(
                &RegistryCredential {
                    registry: registry.into(),
                    username: "AWS".into(),
                    secret: "direct-password".into(),
                },
                Some("production"),
            )
            .unwrap();
        state
            .store
            .put_pipe_auth(&SavedAuth::Aws {
                account_id: "123456789012".into(),
                credentials: pipe_auth::AwsCredentials {
                    access_key_id: "example-key".into(),
                    secret_access_key: "shared-secret".into(),
                    session_token: String::new(),
                    region: "ap-northeast-2".into(),
                },
            })
            .unwrap();

        for shared in [true, false] {
            let Json(rows) = super::super::registries::list(State(state.clone()), Extension(id))
                .await
                .unwrap();
            let rows = serde_json::to_value(rows).unwrap();
            assert_eq!(rows.as_array().unwrap().len(), 1);
            assert_eq!(rows[0]["registry"], registry);
            assert_eq!(rows[0]["accountName"], "production");
            assert_eq!(rows[0]["username"], "AWS");
            assert_eq!(rows[0].get("authProvider").is_some(), shared);
            assert!(!rows.to_string().contains("direct-password"));
            assert!(!rows.to_string().contains("shared-secret"));
            if shared {
                let _ = delete(State(state.clone()), Extension(id), Path(Provider::Aws))
                    .await
                    .unwrap();
            }
        }
    }

    #[tokio::test]
    async fn shared_auth_lists_a_single_registry_scopes_secrets_and_disconnects() {
        let dir = tempfile::tempdir().unwrap();
        let templates = TemplateRegistry::from_specs(dir.path(), std::iter::empty()).unwrap();
        let state = AppState::with_db_file(templates, dir.path().join("state.db"))
            .await
            .unwrap();
        let id = RequestId(uuid::Uuid::nil());
        state
            .store
            .put_registry_login(
                &RegistryCredential {
                    registry: "ghcr.io".into(),
                    username: "legacy".into(),
                    secret: "legacy-token".into(),
                },
                None,
            )
            .unwrap();
        state
            .store
            .put_pipe_auth(&SavedAuth::Github {
                username: "octocat".into(),
                token: "shared-token".into(),
            })
            .unwrap();
        let Json(auths) = list(State(state.clone()), Extension(id)).await.unwrap();
        let json = serde_json::to_string(&auths).unwrap();
        assert!(json.contains("octocat"));
        assert!(!json.contains("shared-token"));
        let Json(registries) = super::super::registries::list(State(state.clone()), Extension(id))
            .await
            .unwrap();
        let registries = serde_json::to_value(registries).unwrap();
        assert_eq!(registries.as_array().unwrap().len(), 1);
        assert_eq!(registries[0]["authProvider"], "github");
        assert_eq!(registries[0]["username"], "octocat");
        assert!(!registries.to_string().contains("shared-token"));
        assert!(
            super::super::registries::delete(
                State(state.clone()),
                Extension(id),
                Path("ghcr.io".into())
            )
            .await
            .is_err()
        );
        let credential = super::super::oci::stored_credential_for(&state, "GHCR.IO", id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(credential.secret, "shared-token");
        for host in [
            "ghcr.io.evil.test",
            "evil.test",
            "123456789012.dkr.ecr.us-east-1.amazonaws.com",
        ] {
            assert!(
                super::super::oci::stored_credential_for(&state, host, id)
                    .await
                    .unwrap()
                    .is_none()
            );
        }
        // Invalid replacement input must preserve the previously verified connection.
        assert!(
            save(
                State(state.clone()),
                Extension(id),
                Path(Provider::Github),
                ValidatedJson(Login::Github {
                    token: "bad token".into()
                })
            )
            .await
            .is_err()
        );
        assert_eq!(
            state.store.pipe_auths().unwrap()[0].connection().account,
            "octocat"
        );
        let _ = delete(State(state.clone()), Extension(id), Path(Provider::Github))
            .await
            .unwrap();
        assert!(state.store.pipe_auths().unwrap().is_empty());
        // A separately saved registry login remains available after shared auth is removed.
        assert_eq!(
            super::super::oci::stored_credential_for(&state, "ghcr.io", id)
                .await
                .unwrap()
                .unwrap()
                .secret,
            "legacy-token"
        );
    }
}
