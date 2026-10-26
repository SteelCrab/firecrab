//! Explicit public shutdown checks keep local ownership and gateway evidence separate.
use super::*;
use std::future::Future;

#[derive(Default)]
pub(super) struct Observation {
    status: Option<u16>,
    error_code: Option<&'static str>,
}

/// Registration gateway errors have exactly one JSON member.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct GatewayError {
    /// Untrusted until matched against both the status and fixed allowlist.
    error: String,
}

/// Only the registration gateway's exact status/code pairs prove unavailability.
pub(super) fn classify(status: u16, body: &[u8]) -> Observation {
    let code = serde_json::from_slice::<GatewayError>(body)
        .ok()
        .and_then(|value| match (status, value.error.as_str()) {
            (404, "unknown_address") => Some("unknown_address"),
            (503, "target_offline") => Some("target_offline"),
            (503, "target_no_longer_allowed") => Some("target_no_longer_allowed"),
            _ => None,
        });
    Observation {
        status: Some(status),
        error_code: code,
    }
}

/// Read only the saved fixed origin with verified TLS, no redirects, and finite bounds.
pub(super) async fn probe(url: String) -> Observation {
    let Ok(client) = reqwest::Client::builder()
        .http1_only()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(10))
        .build()
    else {
        return Observation::default();
    };
    let Ok(mut response) = client.get(url).send().await else {
        return Observation::default();
    };
    let status = response.status().as_u16();
    let incomplete = || Observation {
        status: Some(status),
        error_code: None,
    };
    let mut body = Vec::new();
    loop {
        match response.chunk().await {
            Ok(Some(chunk)) if body.len() + chunk.len() <= 65_536 => body.extend_from_slice(&chunk),
            Ok(None) => return classify(status, &body),
            _ => return incomplete(),
        }
    }
}

/// Check both the durable claim and fresh helper evidence before and after the probe.
fn require_retired(
    state: &AppState,
    id: RequestId,
    binding: &SandboxTunnelBinding,
    snapshot: &Snapshot,
) -> Result<(), AppError> {
    let saved = state
        .store
        .sandbox_tunnel(binding.id)
        .map_err(store_error(id))?;
    let claim = state
        .store
        .sandbox_tunnel_claim(&binding.spec.subdomain)
        .map_err(store_error(id))?;
    if saved.as_ref() != Some(binding)
        || binding.successor_binding_id.is_some()
        || claim.is_some_and(|owner| owner != binding.id)
        || !state
            .store
            .sandbox_tunnel_handoff(binding)
            .map_err(store_error(id))?
            .is_empty()
    {
        return Err(failure(id, "sandbox_preview_shutdown_ownership_changed"));
    }
    let connector = binding
        .connector_id
        .ok_or_else(|| failure(id, "sandbox_preview_shutdown_unconfirmed"))?;
    let retired = match binding.state {
        SandboxTunnelState::Stopped => {
            require_applied(snapshot, binding, id)?;
            claim == Some(binding.id)
                && connector_stopped(snapshot, connector)
                && address(snapshot, binding).is_some_and(|a| !a.connected)
        }
        SandboxTunnelState::Deleted => {
            binding.local_removed
                && claim.is_none()
                && address(snapshot, binding).is_none()
                && !snapshot
                    .firecrab_tunnels
                    .iter()
                    .any(|a| a.connector_id == connector)
                && (connector_stopped(snapshot, connector)
                    || !snapshot.quick_tunnels.iter().any(|c| c.id == connector))
        }
        _ => false,
    };
    if !retired {
        return Err(failure(id, "sandbox_preview_shutdown_unconfirmed"));
    }
    Ok(())
}

/// A private probe callback lets fixtures exercise lifecycle fences without public requests.
/// The REST handler holds MUTATIONS throughout this operation.
pub(super) async fn verify<F, P>(
    state: &AppState,
    id: RequestId,
    caller: Caller,
    key: Uuid,
    request: &SandboxTunnelVerify,
    probe: F,
) -> Result<Json<SandboxTunnelBinding>, AppError>
where
    F: FnOnce(String) -> P,
    P: Future<Output = Observation>,
{
    caller.operator().map_err(|_| denied(id))?;
    let mut binding = state
        .store
        .sandbox_tunnel(key)
        .map_err(store_error(id))?
        .ok_or_else(|| AppError::not_found(id.0))?;
    // Only this evidence-only operation may update an operator's deleted tombstone.
    authorize(
        state,
        id,
        caller,
        &binding,
        binding.state != SandboxTunnelState::Deleted,
    )?;
    if request.web_socket || request.body_sha256.is_some() {
        return Err(failure(id, "sandbox_preview_invalid_verification"));
    }
    let url = verification_url(&binding, &request.path, id)?;
    require_retired(state, id, &binding, &inspect(state, id).await?)?;
    let observation = probe(url).await;
    require_retired(state, id, &binding, &inspect(state, id).await?)?;
    let previous = binding.clone();
    binding.public_shutdown = if observation.error_code.is_some() {
        SandboxTunnelVerification::Verified
    } else {
        SandboxTunnelVerification::Unverified
    };
    binding.shutdown_proof = Some(SandboxTunnelShutdownProof {
        path: request.path.clone(),
        status: observation.status,
        error_code: observation.error_code.map(str::to_owned),
        checked_at: ai_sandbox::now(),
    });
    binding.error_code = observation
        .error_code
        .is_none()
        .then(|| "sandbox_preview_shutdown_unverified".into());
    binding.updated_at = ai_sandbox::now().max(binding.updated_at.saturating_add(1));
    state
        .store
        .save_sandbox_tunnel_shutdown(&previous, &binding)
        .map_err(store_error(id))?;
    Ok(Json(binding))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_exact_gateway_status_and_single_error_field_prove_shutdown() {
        for (status, body, expected) in [
            (
                404,
                r#"{"error":"unknown_address"}"#,
                Some("unknown_address"),
            ),
            (503, r#"{"error":"target_offline"}"#, Some("target_offline")),
            (
                503,
                r#"{"error":"target_no_longer_allowed"}"#,
                Some("target_no_longer_allowed"),
            ),
            (200, r#"{"error":"target_offline"}"#, None),
            (404, r#"{"error":"target_offline"}"#, None),
            (503, r#"{"error":"unknown_address"}"#, None),
            (503, r#"{"error":"registry_unavailable"}"#, None),
            (503, r#"{"error":"private-token-value"}"#, None),
            (
                503,
                r#"{"error":"target_offline","extra":"untrusted"}"#,
                None,
            ),
            (503, r#"{"error":"wrong","error":"target_offline"}"#, None),
            (404, "Not found", None),
            (503, "Service unavailable", None),
            (503, r#"{"error":"target_offline"}" trailing"#, None),
        ] {
            let observed = classify(status, body.as_bytes());
            assert_eq!(observed.status, Some(status));
            assert_eq!(observed.error_code, expected, "{status}: {body}");
        }
        let failed_transport = Observation::default();
        assert!(failed_transport.status.is_none() && failed_transport.error_code.is_none());
    }
}
