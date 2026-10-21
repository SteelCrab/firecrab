//! Verify provider credentials before replacing the saved Pipe Auth connection.
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

pub(crate) fn auth_error(message: &'static str, id: RequestId) -> AppError {
    AppError::new(StatusCode::BAD_REQUEST, "pipe_auth_failed", message, id.0)
}

pub(crate) async fn saved(state: &AppState, id: RequestId) -> Result<Vec<SavedAuth>, AppError> {
    let store = state.store.clone();
    tokio::task::spawn_blocking(move || store.pipe_auths())
        .await
        .map_err(|_| AppError::internal(id.0))?
        .map_err(|_| AppError::internal(id.0))
}

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

pub async fn save(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    Path(provider): Path<Provider>,
    ValidatedJson(login): ValidatedJson<Login>,
) -> Result<Json<Connection>, AppError> {
    if provider != login.provider() {
        return Err(auth_error("Provider does not match the credential", id));
    }
    let auth = pipe_auth::connect(login)
        .await
        .map_err(|message| auth_error(message, id))?;
    let connection = auth.connection();
    tokio::task::spawn_blocking(move || state.store.put_pipe_auth(&auth))
        .await
        .map_err(|_| AppError::internal(id.0))?
        .map_err(|_| AppError::internal(id.0))?;
    Ok(Json(connection))
}

pub async fn delete(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    Path(provider): Path<Provider>,
) -> Result<Json<serde_json::Value>, AppError> {
    tokio::task::spawn_blocking(move || state.store.delete_pipe_auth(provider))
        .await
        .map_err(|_| AppError::internal(id.0))?
        .map_err(|_| AppError::internal(id.0))?;
    Ok(Json(serde_json::json!({ "deleted": true })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{oci::RegistryCredential, templates::TemplateRegistry};

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
            .put_registry_credential(&RegistryCredential {
                registry: "ghcr.io".into(),
                username: "legacy".into(),
                secret: "legacy-token".into(),
            })
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
