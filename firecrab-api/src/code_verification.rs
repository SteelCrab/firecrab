//! Fixed-origin, read-only repository checks. No clone, inference, or agent execution.
use crate::pipe_auth::SavedAuth;
use firecrab_api_types::{
    CodeGithubRepository, CodeRepositoryVerification as Repository,
    CodeVerificationFailure as Failure,
};
use reqwest::{Client, Url};
use serde::{Deserialize, de::DeserializeOwned};
use std::time::Duration;

const MAX_BYTES: usize = 128 * 1024;

/// No configurable origin is accepted from a workspace or HTTP caller.
#[derive(Debug, Clone)]
pub(crate) struct Github {
    client: Client,
    origin: Url,
}

impl Github {
    pub(crate) fn new() -> Self {
        Self {
            client: Client::builder()
                .https_only(true)
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(Duration::from_secs(3))
                .timeout(Duration::from_secs(6))
                .user_agent("firecrab-code")
                .build()
                .expect("CODE GitHub HTTP client"),
            origin: Url::parse("https://api.github.com").expect("GitHub API origin"),
        }
    }

    #[cfg(test)]
    pub(crate) fn at(origin: String) -> Self {
        Self {
            client: Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(6))
                .build()
                .unwrap(),
            origin: Url::parse(&origin).unwrap(),
        }
    }

    async fn get<T: DeserializeOwned>(&self, segments: &[&str], token: &str) -> Result<T, Failure> {
        let mut url = self.origin.clone();
        url.path_segments_mut()
            .map_err(|_| Failure::InvalidResponse)?
            .extend(segments);
        let mut response = self
            .client
            .get(url)
            .bearer_auth(token)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2026-03-10")
            .send()
            .await
            .map_err(|error| {
                if error.is_timeout() {
                    Failure::TimedOut
                } else {
                    Failure::Unavailable
                }
            })?;
        match response.status().as_u16() {
            200 => {}
            401 => return Err(Failure::AuthenticationRejected),
            403 if response
                .headers()
                .get("x-ratelimit-remaining")
                .is_some_and(|n| n == "0")
                || response.headers().contains_key("retry-after") =>
            {
                return Err(Failure::RateLimited);
            }
            403 => return Err(Failure::AccessDenied),
            404 | 409 => return Err(Failure::NotFound),
            429 => return Err(Failure::RateLimited),
            300..=399 => return Err(Failure::Redirected),
            500..=599 => return Err(Failure::Unavailable),
            _ => return Err(Failure::InvalidResponse),
        }
        if response
            .content_length()
            .is_some_and(|length| length > MAX_BYTES as u64)
        {
            return Err(Failure::InvalidResponse);
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|error| {
            if error.is_timeout() {
                Failure::TimedOut
            } else {
                Failure::Unavailable
            }
        })? {
            if chunk.len() > MAX_BYTES.saturating_sub(bytes.len()) {
                return Err(Failure::InvalidResponse);
            }
            bytes.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&bytes).map_err(|_| Failure::InvalidResponse)
    }

    async fn branch(
        &self,
        spec: &CodeGithubRepository,
        account: Option<&SavedAuth>,
    ) -> Result<String, Failure> {
        let Some(SavedAuth::Github { username, token }) = account else {
            return Err(Failure::Missing);
        };
        if !username.eq_ignore_ascii_case(&spec.account) {
            return Err(Failure::IdentityChanged);
        }
        if token.is_empty() || token.len() > 16384 || !token.bytes().all(|c| c.is_ascii_graphic()) {
            return Err(Failure::AuthenticationRejected);
        }
        #[derive(Deserialize)]
        struct User {
            login: String,
        }
        let user: User = self.get(&["user"], token).await?;
        if !user.login.eq_ignore_ascii_case(&spec.account) {
            return Err(Failure::IdentityChanged);
        }
        #[derive(Deserialize)]
        struct Repo {
            full_name: String,
            disabled: bool,
        }
        let repo: Repo = self
            .get(&["repos", &spec.owner, &spec.repository], token)
            .await?;
        if !repo
            .full_name
            .eq_ignore_ascii_case(&format!("{}/{}", spec.owner, spec.repository))
        {
            return Err(Failure::InvalidResponse);
        }
        if repo.disabled {
            return Err(Failure::AccessDenied);
        }
        #[derive(Deserialize)]
        struct Object {
            sha: String,
            r#type: String,
        }
        #[derive(Deserialize)]
        struct Ref {
            r#ref: String,
            object: Object,
        }
        let reference: Ref = self
            .get(
                &[
                    "repos",
                    &spec.owner,
                    &spec.repository,
                    "git",
                    "ref",
                    "heads",
                    &spec.branch,
                ],
                token,
            )
            .await?;
        if reference.r#ref != format!("refs/heads/{}", spec.branch)
            || reference.object.r#type != "commit"
            || reference.object.sha.len() != 40
            || !reference.object.sha.bytes().all(|c| c.is_ascii_hexdigit())
        {
            return Err(Failure::InvalidResponse);
        }
        Ok(reference.object.sha)
    }

    /// Authenticate the selected login and read the exact heads ref without following redirects.
    pub(crate) async fn verify(
        &self,
        spec: &CodeGithubRepository,
        account: Option<&SavedAuth>,
    ) -> Repository {
        match self.branch(spec, account).await {
            Ok(commit_sha) => Repository::Verified { commit_sha },
            Err(reason) => Repository::Failed { reason },
        }
    }
}
