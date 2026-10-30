use crate::{
    ai_sandbox::guest,
    persistence::{code_runtime::Record, code_verification::Snapshot},
    pipe_auth::SavedAuth,
    state::AppState,
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use firecrab_api_types::{CodeRepositoryVerification, CodeRuntimeFailure as Failure};
use serde::Deserialize;
use serde_json::json;
use std::{path::Path, time::Duration};
use uuid::Uuid;

/// Read-only preflight is repeated immediately before transferring a credential.
pub(super) async fn repository(
    state: &AppState,
    record: &Record,
) -> Result<(Snapshot, String, String), Failure> {
    let snapshot = state
        .store
        .code_verification_snapshot(record.runtime.workspace_id)
        .map_err(|_| Failure::RepositoryDenied)?
        .ok_or(Failure::RepositoryDenied)?;
    let result = tokio::time::timeout(
        Duration::from_secs(20),
        state
            .code_github
            .verify(&snapshot.workspace.spec.github, snapshot.github.as_ref()),
    )
    .await
    .map_err(|_| Failure::RepositoryDenied)?;
    let CodeRepositoryVerification::Verified { commit_sha } = result else {
        return Err(Failure::RepositoryDenied);
    };
    let Some(SavedAuth::Github { token, .. }) = &snapshot.github else {
        return Err(Failure::RepositoryDenied);
    };
    let token = token.clone();
    Ok((snapshot, commit_sha, token))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Prepared {
    initial_commit: Option<String>,
    workspace: Uuid,
    #[serde(default)]
    needs_credentials: bool,
}

async fn invoke(
    state: &AppState,
    binding: &guest::Binding,
    directory: &Path,
    payload: &serde_json::Value,
) -> Result<Prepared, Failure> {
    let encoded =
        STANDARD.encode(serde_json::to_vec(payload).map_err(|_| Failure::PreparationFailed)?);
    let input = format!(
        "timeout -k 5 840 /bin/sh -s <<'FIRECRAB_CODE_INNER'\n{}\npython3 - <<'FIRECRAB_CODE_PY'\n{}\nimport base64\nmain(base64.b64decode('{}'))\nFIRECRAB_CODE_PY\nFIRECRAB_CODE_INNER\n",
        include_str!("bootstrap.sh"),
        include_str!("guest.py"),
        encoded
    );
    let output = guest::code_bootstrap(state, binding, directory, input.into_bytes())
        .await
        .map_err(|_| Failure::GuestUnavailable)?;
    if output.code != 0 {
        return Err(Failure::PreparationFailed);
    }
    serde_json::from_slice(&output.stdout).map_err(|_| Failure::PreparationFailed)
}

pub(super) async fn prepare(
    state: &AppState,
    record: &Record,
    binding: &guest::Binding,
    directory: &Path,
) -> Result<(Uuid, String), Failure> {
    let mut payload = json!({
        "workspace":record.runtime.workspace_id,"owner":record.spec.github.owner,"repository":record.spec.github.repository,
        "branch":record.spec.github.branch,"runtime":record.spec.runtime,"sha":record.runtime.initial_commit.as_deref().unwrap_or(""),
        "token":"","account":"","resume":record.runtime.initial_commit.is_some(),"stageOnly":record.runtime.initial_commit.is_none()
    });
    // Install trusted tools first, or recover a completed guest-side publication without
    // requiring a new account connection. No credential is transferred in this stage.
    let mut prepared = invoke(state, binding, directory, &payload).await?;
    let mut snapshot = None;
    if prepared.needs_credentials {
        if record.runtime.initial_commit.is_some()
            || prepared.workspace != record.runtime.workspace_id
        {
            return Err(Failure::PreparationFailed);
        }
        let (current, sha, token) = repository(state, record).await?;
        if state.store.code_verification_current(&current).ok() != Some(true) {
            return Err(Failure::AccountChanged);
        }
        payload["token"] = json!(token);
        payload["account"] = json!(current.workspace.spec.github.account);
        payload["sha"] = json!(sha);
        payload["stageOnly"] = json!(false);
        prepared = invoke(state, binding, directory, &payload).await?;
        snapshot = Some(current);
    }
    let commit = prepared.initial_commit.ok_or(Failure::PreparationFailed)?;
    if prepared.needs_credentials
        || prepared.workspace != record.runtime.workspace_id
        || commit.len() != 40
        || !commit.bytes().all(|c| c.is_ascii_hexdigit())
        || record
            .runtime
            .initial_commit
            .as_ref()
            .is_some_and(|sha| sha != &commit)
    {
        return Err(Failure::PreparationFailed);
    }
    if guest::binding(state, binding.vm_id).map_err(|_| Failure::GuestUnavailable)? != *binding {
        return Err(Failure::GuestUnavailable);
    }
    if snapshot
        .as_ref()
        .is_some_and(|snapshot| state.store.code_verification_current(snapshot).ok() != Some(true))
    {
        return Err(Failure::AccountChanged);
    }
    Ok((binding.runtime_id, commit))
}
