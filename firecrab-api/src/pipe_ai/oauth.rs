//! Official public-client ChatGPT plan flow. No shared Codex client or backend-api.
use super::{Failure, MAX_RESPONSE_BYTES, OpenAi, SavedConnection, transport_failure};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use firecrab_api_types::{AiLoginRequest, AiLoginStart, AiLoginStatus};
use ring::{
    rand::{SecureRandom, SystemRandom},
    signature,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

const ISSUER: &str = "https://auth.openai.com";
const RESOURCE: &str = "https://api.openai.com/v1";
const SCOPES: &str =
    "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct";
const LOGIN_TTL: Duration = Duration::from_secs(600);

#[derive(Clone, Deserialize, Serialize)]
pub struct OAuthSession {
    pub client_id: String,
    pub subject: String,
    pub refresh_token: String,
    pub expires_at: u64,
    pub earliest_refresh_at: u64,
    pub scope: String,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct OAuthRegistration {
    pub client_id: String,
    pub subject: String,
}

pub struct PendingLogin {
    pub connection_id: Uuid,
    pub input: AiLoginRequest,
    pub redirect_uri: String,
    verifier: String,
    nonce: String,
    client_id: Option<String>,
    subject: Option<String>,
}

struct Attempt {
    connection_id: Uuid,
    expires: Instant,
    pending: Option<PendingLogin>,
    status: AiLoginStatus,
}

#[derive(Default)]
pub struct LoginRegistry(Mutex<HashMap<Uuid, Attempt>>);

// Never print PKCE verifiers, identity tokens, or authorization query parameters.
impl std::fmt::Debug for LoginRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("LoginRegistry([REDACTED])")
    }
}

fn random() -> Result<String, Failure> {
    let mut bytes = [0u8; 32];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| Failure::Login)?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

impl LoginRegistry {
    pub fn begin(
        &self,
        connection_id: Uuid,
        input: AiLoginRequest,
        host: String,
        port: u16,
        existing: Option<&OAuthRegistration>,
    ) -> Result<AiLoginStart, Failure> {
        if let Some(model) = &input.model {
            super::validate_model(model)?;
        }
        if input.name.trim().is_empty()
            || input.name.len() > 100
            || input.name.chars().any(char::is_control)
        {
            return Err(Failure::InvalidInput);
        }
        let mut attempts = self.0.lock().map_err(|_| Failure::Login)?;
        attempts.retain(|_, value| value.expires > Instant::now());
        if attempts.len() >= 16
            || attempts
                .values()
                .any(|a| a.connection_id == connection_id && a.status.status == "pending")
        {
            return Err(Failure::Login);
        }
        let login_id = Uuid::new_v4();
        let verifier = random()?;
        let nonce = random()?;
        let redirect_uri = format!("http://127.0.0.1:{port}/auth/callback");
        let mut url = reqwest::Url::parse(&format!("{ISSUER}/api/accounts/authorize"))
            .map_err(|_| Failure::Login)?;
        let client = existing
            .map(|s| s.client_id.as_str())
            .unwrap_or("dynamic_agent_client");
        url.query_pairs_mut().extend_pairs([
            ("client_id", client),
            ("ext_agent_host_id", &host),
            ("response_type", "code"),
            ("redirect_uri", &redirect_uri),
            ("scope", SCOPES),
            ("resource", RESOURCE),
            ("state", &login_id.to_string()),
            ("nonce", &nonce),
            ("code_challenge_method", "S256"),
            (
                "code_challenge",
                &URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes())),
            ),
        ]);
        if existing.is_none() {
            url.query_pairs_mut()
                .append_pair("agent_name_hint", "Firecrab");
        }
        let pending = PendingLogin {
            connection_id,
            input,
            redirect_uri,
            verifier,
            nonce,
            client_id: existing.map(|s| s.client_id.clone()),
            subject: existing.map(|s| s.subject.clone()),
        };
        attempts.insert(
            login_id,
            Attempt {
                connection_id,
                expires: Instant::now() + LOGIN_TTL,
                pending: Some(pending),
                status: AiLoginStatus {
                    status: "pending".into(),
                    message: None,
                },
            },
        );
        Ok(AiLoginStart {
            login_id,
            authorization_url: url.into(),
            expires_in: LOGIN_TTL.as_secs(),
        })
    }

    pub fn take(&self, state: Uuid) -> Result<PendingLogin, Failure> {
        let mut attempts = self.0.lock().map_err(|_| Failure::Login)?;
        let attempt = attempts
            .get_mut(&state)
            .filter(|a| a.expires > Instant::now())
            .ok_or(Failure::Login)?;
        let pending = attempt.pending.take().ok_or(Failure::Login)?;
        attempt.status.status = "working".into();
        Ok(pending)
    }

    pub fn finish(&self, state: Uuid, result: Result<(), Failure>) {
        if let Ok(mut attempts) = self.0.lock()
            && let Some(attempt) = attempts.get_mut(&state)
        {
            attempt.pending = None;
            attempt.status = match result {
                Ok(()) => AiLoginStatus {
                    status: "connected".into(),
                    message: None,
                },
                Err(error) => AiLoginStatus {
                    status: "failed".into(),
                    message: Some(error.details().2.into()),
                },
            };
        }
    }

    pub fn status(&self, state: Uuid) -> Result<AiLoginStatus, Failure> {
        let attempts = self.0.lock().map_err(|_| Failure::Login)?;
        attempts
            .get(&state)
            .filter(|a| a.expires > Instant::now())
            .map(|a| a.status.clone())
            .ok_or(Failure::Login)
    }

    pub fn cancel_connection(&self, id: Uuid) {
        if let Ok(mut attempts) = self.0.lock() {
            attempts.retain(|_, a| a.connection_id != id);
        }
    }
}

#[derive(Deserialize)]
struct Tokens {
    access_token: String,
    refresh_token: String,
    id_token: Option<String>,
    token_type: String,
    expires_in: u64,
    earliest_refresh_at: Option<u64>,
    scope: Option<String>,
}

impl OpenAi {
    async fn oauth_json(&self, response: reqwest::Response) -> Result<Value, Failure> {
        let mut response = response;
        match response.status().as_u16() {
            200..=299 => {}
            401 => return Err(Failure::Authentication),
            429 => return Err(Failure::RateLimited),
            300..=399 | 500..=599 => return Err(Failure::Provider),
            _ => return Err(Failure::Login),
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(transport_failure)? {
            if bytes.len() + chunk.len() > MAX_RESPONSE_BYTES {
                return Err(Failure::Provider);
            }
            bytes.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&bytes).map_err(|_| Failure::Login)
    }

    async fn token(&self, form: &[(&str, &str)]) -> Result<Tokens, Failure> {
        let response = self
            .client
            .post(format!("{}/api/accounts/oauth/token", self.auth_base))
            .form(form)
            .send()
            .await
            .map_err(transport_failure)?;
        let tokens: Tokens =
            serde_json::from_value(self.oauth_json(response).await?).map_err(|_| Failure::Login)?;
        if !tokens.token_type.eq_ignore_ascii_case("bearer")
            || tokens.access_token.is_empty()
            || tokens.refresh_token.is_empty()
            || tokens.expires_in == 0
            || tokens.expires_in > 86_400
            || !tokens.access_token.bytes().all(|c| c.is_ascii_graphic())
        {
            return Err(Failure::Login);
        }
        Ok(tokens)
    }

    async fn identity(
        &self,
        token: &str,
        client_id: &str,
        nonce: Option<&str>,
    ) -> Result<String, Failure> {
        let response = self
            .client
            .get(format!("{}/.well-known/jwks.json", self.auth_base))
            .send()
            .await
            .map_err(transport_failure)?;
        let jwks = self.oauth_json(response).await?;
        verify_identity(token, &jwks, client_id, nonce, now())
    }

    pub async fn exchange(
        &self,
        pending: &PendingLogin,
        code: &str,
        callback_client: Option<&str>,
    ) -> Result<(String, OAuthSession), Failure> {
        let client_id = match (&pending.client_id, callback_client) {
            (Some(expected), Some(actual)) if expected != actual => return Err(Failure::Login),
            (Some(expected), _) => expected.as_str(),
            (None, Some(issued)) if issued.starts_with("oaiapp_") && issued.len() <= 256 => issued,
            _ => return Err(Failure::Login),
        };
        let tokens = self
            .token(&[
                ("grant_type", "authorization_code"),
                ("client_id", client_id),
                ("code", code),
                ("code_verifier", &pending.verifier),
                ("redirect_uri", &pending.redirect_uri),
                ("resource", RESOURCE),
            ])
            .await?;
        let subject = self
            .identity(
                tokens.id_token.as_deref().ok_or(Failure::Login)?,
                client_id,
                Some(&pending.nonce),
            )
            .await?;
        if pending
            .subject
            .as_ref()
            .is_some_and(|expected| expected != &subject)
        {
            return Err(Failure::Login);
        }
        let scope = tokens.scope.as_deref().ok_or(Failure::Consent)?;
        require_scope(scope)?;
        let session = OAuthSession {
            client_id: client_id.into(),
            subject,
            refresh_token: tokens.refresh_token,
            expires_at: now() + tokens.expires_in,
            earliest_refresh_at: tokens.earliest_refresh_at.unwrap_or(0),
            scope: scope.into(),
        };
        Ok((tokens.access_token, session))
    }

    pub async fn refresh(&self, saved: &mut SavedConnection) -> Result<bool, Failure> {
        let Some(session) = &saved.oauth else {
            return Ok(false);
        };
        if session.expires_at > now() + 60 || session.earliest_refresh_at > now() {
            return Ok(false);
        }
        let tokens = self
            .token(&[
                ("grant_type", "refresh_token"),
                ("client_id", &session.client_id),
                ("refresh_token", &session.refresh_token),
                ("resource", RESOURCE),
            ])
            .await?;
        if let Some(token) = &tokens.id_token
            && self.identity(token, &session.client_id, None).await? != session.subject
        {
            return Err(Failure::Login);
        }
        let scope = tokens.scope.unwrap_or_else(|| session.scope.clone());
        require_scope(&scope)?;
        let session = OAuthSession {
            client_id: session.client_id.clone(),
            subject: session.subject.clone(),
            refresh_token: tokens.refresh_token,
            expires_at: now() + tokens.expires_in,
            earliest_refresh_at: tokens.earliest_refresh_at.unwrap_or(0),
            scope,
        };
        saved.api_key = tokens.access_token;
        saved.oauth = Some(session);
        Ok(true)
    }

    /// Clear locally even if remote revocation cannot be confirmed; report that fact.
    pub async fn revoke(&self, session: &OAuthSession) -> bool {
        let result = async {
            let response = self
                .client
                .get(format!(
                    "{}/.well-known/openid-configuration",
                    self.auth_base
                ))
                .send()
                .await
                .ok()?;
            let document = self.oauth_json(response).await.ok()?;
            let endpoint = document["revocation_endpoint"].as_str()?;
            let url = reqwest::Url::parse(endpoint).ok()?;
            if url.scheme() != "https"
                || url.host_str() != Some("auth.openai.com")
                || !url.username().is_empty()
                || url.password().is_some()
                || url.port_or_known_default() != Some(443)
            {
                return None;
            }
            let response = self
                .client
                .post(url)
                .timeout(Duration::from_secs(5))
                .form(&[
                    ("token", session.refresh_token.as_str()),
                    ("token_type_hint", "refresh_token"),
                    ("client_id", session.client_id.as_str()),
                ])
                .send()
                .await
                .ok()?;
            Some(response.status().as_u16() == 200)
        }
        .await;
        result.unwrap_or(false)
    }
}

fn require_scope(scope: &str) -> Result<(), Failure> {
    if !scope
        .split_whitespace()
        .any(|s| s == "chatgpt.tokens.use.direct")
    {
        return Err(Failure::Consent);
    }
    Ok(())
}

/// RS256 only. Reject algorithm substitution and verify signed OIDC claims.
fn verify_identity(
    token: &str,
    jwks: &Value,
    client_id: &str,
    nonce: Option<&str>,
    timestamp: u64,
) -> Result<String, Failure> {
    let parts: Vec<_> = token.split('.').collect();
    if parts.len() != 3 || token.len() > 65_536 {
        return Err(Failure::Login);
    }
    let decode = |text: &str| URL_SAFE_NO_PAD.decode(text).map_err(|_| Failure::Login);
    let header: Value = serde_json::from_slice(&decode(parts[0])?).map_err(|_| Failure::Login)?;
    if header["alg"] != "RS256" || header.get("crit").is_some() {
        return Err(Failure::Login);
    }
    let kid = header["kid"].as_str().ok_or(Failure::Login)?;
    let key = jwks["keys"]
        .as_array()
        .ok_or(Failure::Login)?
        .iter()
        .find(|k| {
            k["kid"] == kid
                && k["kty"] == "RSA"
                && k.get("use").is_none_or(|v| v == "sig")
                && k.get("alg").is_none_or(|v| v == "RS256")
        })
        .ok_or(Failure::Login)?;
    let n = decode(key["n"].as_str().ok_or(Failure::Login)?)?;
    let e = decode(key["e"].as_str().ok_or(Failure::Login)?)?;
    signature::RsaPublicKeyComponents { n: &n, e: &e }
        .verify(
            &signature::RSA_PKCS1_2048_8192_SHA256,
            format!("{}.{}", parts[0], parts[1]).as_bytes(),
            &decode(parts[2])?,
        )
        .map_err(|_| Failure::Login)?;
    let claims: Value = serde_json::from_slice(&decode(parts[1])?).map_err(|_| Failure::Login)?;
    let audience = &claims["aud"];
    let audience_matches = audience == client_id
        || audience
            .as_array()
            .is_some_and(|a| a.iter().any(|v| v == client_id));
    if claims["iss"] != ISSUER
        || !audience_matches
        || claims["exp"].as_u64().is_none_or(|exp| exp <= timestamp)
        || claims["iat"]
            .as_u64()
            .is_none_or(|iat| iat > timestamp + 60)
        || claims
            .get("nbf")
            .is_some_and(|nbf| nbf.as_u64().is_none_or(|v| v > timestamp + 60))
        || claims.get("azp").is_some_and(|value| value != client_id)
        || (audience.as_array().is_some_and(|a| a.len() > 1) && claims["azp"] != client_id)
        || nonce.is_some_and(|expected| claims["nonce"] != expected)
    {
        return Err(Failure::Login);
    }
    claims["sub"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .ok_or(Failure::Login)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // Public test-only RSA key, generated for these tests. Never used for authentication.
    fn signed(claims: &Value) -> String {
        let key = signature::RsaKeyPair::from_pkcs8(include_bytes!("fixtures/oidc-test-key.pk8"))
            .unwrap();
        let data = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(br#"{"alg":"RS256","kid":"test-only"}"#),
            URL_SAFE_NO_PAD.encode(claims.to_string())
        );
        let mut sig = vec![0; key.public().modulus_len()];
        key.sign(
            &signature::RSA_PKCS1_SHA256,
            &SystemRandom::new(),
            data.as_bytes(),
            &mut sig,
        )
        .unwrap();
        format!("{data}.{}", URL_SAFE_NO_PAD.encode(sig))
    }

    fn jwks() -> Value {
        serde_json::from_str(include_str!("fixtures/oidc-test-jwks.json")).unwrap()
    }
    fn claims() -> Value {
        json!({"iss":ISSUER,"sub":"test-account","aud":"oaiapp_test","exp":2000,"iat":1000,"nonce":"expected-nonce"})
    }

    #[test]
    fn signed_oidc_tokens_require_correct_signature_issuer_audience_expiry_and_nonce() {
        assert_eq!(
            verify_identity(
                &signed(&claims()),
                &jwks(),
                "oaiapp_test",
                Some("expected-nonce"),
                1001
            )
            .unwrap(),
            "test-account"
        );
        for (field, value) in [
            ("iss", json!("https://attacker.invalid")),
            ("aud", json!("another-client")),
            ("exp", json!(999)),
            ("iat", json!(3000)),
            ("nonce", json!("wrong")),
            ("sub", json!("")),
            ("azp", json!("another-client")),
            ("nbf", json!(3000)),
        ] {
            let mut doc = claims();
            doc[field] = value;
            assert!(
                verify_identity(
                    &signed(&doc),
                    &jwks(),
                    "oaiapp_test",
                    Some("expected-nonce"),
                    1001
                )
                .is_err(),
                "{field}"
            );
        }
        let valid = signed(&claims());
        let mut parts: Vec<_> = valid.split('.').map(str::to_owned).collect();
        let mut doc = claims();
        doc["sub"] = json!("tampered");
        parts[1] = URL_SAFE_NO_PAD.encode(doc.to_string());
        assert!(
            verify_identity(
                &parts.join("."),
                &jwks(),
                "oaiapp_test",
                Some("expected-nonce"),
                1001
            )
            .is_err()
        );
        assert!(
            verify_identity(
                "eyJhbGciOiJub25lIn0.e30.",
                &jwks(),
                "oaiapp_test",
                None,
                1001
            )
            .is_err()
        );
    }

    #[test]
    fn login_state_is_single_use_expiring_and_reuses_registration() {
        let registry = LoginRegistry::default();
        let connection = Uuid::new_v4();
        let registration = OAuthRegistration {
            client_id: "oaiapp_saved".into(),
            subject: "same-account".into(),
        };
        let start = registry
            .begin(
                connection,
                AiLoginRequest {
                    name: "Personal account".into(),
                    model: None,
                },
                "urn:uuid:test-host".into(),
                5523,
                Some(&registration),
            )
            .unwrap();
        let url = reqwest::Url::parse(&start.authorization_url).unwrap();
        let args: HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(args["client_id"], "oaiapp_saved");
        assert!(!args.contains_key("agent_name_hint"));
        let pending = registry.take(start.login_id).unwrap();
        assert_eq!(
            args["code_challenge"],
            URL_SAFE_NO_PAD.encode(Sha256::digest(pending.verifier.as_bytes()))
        );
        assert_ne!(pending.nonce, pending.verifier);
        assert!(registry.take(start.login_id).is_err());
        registry.finish(start.login_id, Err(Failure::Consent));
        assert_eq!(registry.status(start.login_id).unwrap().status, "failed");
        registry
            .0
            .lock()
            .unwrap()
            .get_mut(&start.login_id)
            .unwrap()
            .expires = Instant::now() - Duration::from_secs(1);
        assert!(registry.status(start.login_id).is_err());
        assert!(require_scope("openid profile email").is_err());
        assert!(require_scope(SCOPES).is_ok());
    }
}
