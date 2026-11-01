//! Fixed CLI recipes use isolated, disposable guest credentials and produce no execution grant.
use crate::{
    ai_sandbox::guest, persistence::code_runtime::Record, pipe_ai::SavedConnection, state::AppState,
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use firecrab_api_types::*;
use serde::Deserialize;
use serde_json::json;
use std::{future::Future, time::Duration};
use uuid::Uuid;

pub(crate) const MANIFEST: &str = include_str!("code_agent_auth/adapters.json");

pub(crate) fn catalog() -> Vec<CodeAgentAuthAdapter> {
    serde_json::from_str(MANIFEST).expect("compiled CODE authentication recipes")
}
pub(crate) fn resolve(
    binding: &CodeAgentBinding,
    saved: &SavedConnection,
) -> Option<CodeAgentAuthAdapter> {
    catalog().into_iter().find(|p| {
        p.agent == binding.agent
            && p.provider == saved.connection.provider
            && p.authentication == saved.connection.authentication
    })
}
pub(crate) trait Probe: Sync {
    fn check(
        &self,
        state: &AppState,
        record: &Record,
        profile: &CodeAgentAuthAdapter,
        saved: &SavedConnection,
    ) -> impl Future<Output = Result<(), CodeAgentAuthFailure>> + Send;
}
pub(crate) struct Native;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn code_agent_auth_receipts_require_the_nonce_recipe_and_exact_version() {
        let profiles = catalog();
        assert_eq!(profiles.iter().filter(|p| p.supported).count(), 4);
        assert!(
            !profiles
                .iter()
                .find(|p| p.authentication == AiAuthentication::Chatgpt)
                .unwrap()
                .supported
        );
        let p = &profiles[0];
        let nonce = Uuid::new_v4();
        let good = json!({"nonce":nonce,"adapter":p.id,"cliVersion":p.cli_version,"status":"configuration_verified"});
        assert!(receipt(&serde_json::to_vec(&good).unwrap(), nonce, p).is_ok());
        for (field, bad) in [
            ("nonce", json!(Uuid::new_v4())),
            ("adapter", json!("other")),
            ("cliVersion", json!("latest")),
            ("status", json!("authenticated")),
            ("token", json!("private")),
        ] {
            let mut forged = good.clone();
            forged[field] = bad;
            assert_eq!(
                receipt(&serde_json::to_vec(&forged).unwrap(), nonce, p),
                Err(CodeAgentAuthFailure::CliRejected)
            );
        }
        assert_eq!(
            receipt(br#"{"status":"authenticated"}"#, nonce, p),
            Err(CodeAgentAuthFailure::CliRejected)
        );
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Receipt {
    nonce: Uuid,
    adapter: String,
    cli_version: String,
    status: String,
}
pub(crate) fn receipt(
    bytes: &[u8],
    nonce: Uuid,
    profile: &CodeAgentAuthAdapter,
) -> Result<(), CodeAgentAuthFailure> {
    use CodeAgentAuthFailure as F;
    if let Ok(r) = serde_json::from_slice::<Receipt>(bytes) {
        return if r.nonce == nonce
            && r.adapter == profile.id
            && r.cli_version == profile.cli_version
            && r.status == "configuration_verified"
        {
            Ok(())
        } else {
            Err(F::CliRejected)
        };
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Failed {
        status: CodeAgentAuthFailure,
    }
    match serde_json::from_slice::<Failed>(bytes).map(|r| r.status) {
        Ok(
            reason @ (F::CliUnavailable
            | F::CliUntrusted
            | F::VersionMismatch
            | F::CliRejected
            | F::TimedOut
            | F::RuntimeUnavailable
            | F::AdapterUnavailable),
        ) => Err(reason),
        _ => Err(F::CliRejected),
    }
}
impl Probe for Native {
    async fn check(
        &self,
        state: &AppState,
        record: &Record,
        profile: &CodeAgentAuthAdapter,
        _saved: &SavedConnection,
    ) -> Result<(), CodeAgentAuthFailure> {
        use CodeAgentAuthFailure as F;
        let binding =
            crate::code_runtime::auth_binding(state, record).map_err(|_| F::RuntimeUnavailable)?;
        let nonce = Uuid::new_v4();
        let encoded = STANDARD.encode(serde_json::to_vec(&json!({"workspace":record.runtime.workspace_id,"nonce":nonce,"adapter":profile.id,"key":format!("fc-probe-{}",Uuid::new_v4().simple())})).map_err(|_| F::CliRejected)?);
        let manifest = STANDARD.encode(MANIFEST);
        // Provider proof stays on the controller; the guest checks only disposable broker-token mode.
        let input = format!(
            "timeout -k 2 35 python3 - <<'FIRECRAB_AGENT_AUTH'\n{}\nimport base64\nmain(base64.b64decode('{}'), json.loads(base64.b64decode('{}')))\nFIRECRAB_AGENT_AUTH\n",
            include_str!("code_agent_auth/guest.py"),
            encoded,
            manifest
        );
        let directory = crate::code_runtime::directory(state, record.runtime.workspace_id);
        let output = tokio::time::timeout(
            Duration::from_secs(40),
            guest::code_bootstrap(state, &binding, &directory, input.into_bytes()),
        )
        .await
        .map_err(|_| F::TimedOut)?
        .map_err(|_| F::RuntimeUnavailable)?;
        if output.code != 0 {
            return Err(F::CliRejected);
        }
        if crate::code_runtime::auth_binding(state, record)
            .ok()
            .as_ref()
            != Some(&binding)
        {
            return Err(F::RuntimeUnavailable);
        }
        receipt(&output.stdout, nonce, profile)
    }
}
