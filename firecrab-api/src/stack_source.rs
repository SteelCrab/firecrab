//! Git polling and explicit native synchronization. Repository content is never executed.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use firecrab_stack::{
    Action, ApplyRequest, GitCheck, GitCheckState, GitCredential, GitSourceStatus, GitSyncMode,
    MAX_MANIFEST_BYTES, Stack,
};
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use serde::{Deserialize, de::DeserializeOwned};

use crate::error::AppError;
use crate::handlers::stacks::{database, inventory};
use crate::persistence::{PersistenceError, SourceTrigger};
use crate::pipe_auth::SavedAuth;
use crate::state::AppState;

/// Wall-clock time is persisted so scheduling survives API restarts.
pub(crate) fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// Run after the existing single-process ownership lock is acquired at API startup.
pub(crate) fn spawn_reconciler(state: AppState) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(5));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tick.tick().await;
            let store = state.store.clone();
            let sources = database(uuid::Uuid::new_v4(), move || store.stack_sources()).await;
            let Ok(sources) = sources else {
                tracing::error!("could not read Git source schedule");
                continue;
            };
            for mut source in sources {
                if source
                    .latest
                    .as_ref()
                    .is_some_and(|c| c.state == GitCheckState::Applying)
                {
                    let store = state.store.clone();
                    let name = source.name.clone();
                    let Ok(updated) = database(uuid::Uuid::new_v4(), move || {
                        store.refresh_stack_sync(&name, now())
                    })
                    .await
                    else {
                        continue;
                    };
                    source = updated;
                }
                if source.source.paused
                    || (source.source.sync_mode == GitSyncMode::Automatic
                        && source.recovery_operation_id.is_some())
                    || source.next_check_at > now()
                    || source.latest.as_ref().is_some_and(|c| c.state.is_active())
                {
                    continue;
                }
                // Admission checks the generation and pause flag again under a write transaction.
                let _ = start_check(state.clone(), source.name, source.generation, true).await;
            }
        }
    })
}

/// Durable admission is independent of the HTTP request lifetime.
pub(crate) async fn start_check(
    state: AppState,
    name: String,
    generation: u64,
    periodic: bool,
) -> Result<Option<GitSourceStatus>, AppError> {
    start_observation(
        state,
        name,
        generation,
        if periodic {
            SourceTrigger::Poll
        } else {
            SourceTrigger::Check
        },
    )
    .await
}

pub(crate) async fn start_sync(
    state: AppState,
    name: String,
    generation: u64,
    recovery: Option<uuid::Uuid>,
) -> Result<Option<GitSourceStatus>, AppError> {
    start_observation(state, name, generation, SourceTrigger::Sync(recovery)).await
}

async fn start_observation(
    state: AppState,
    name: String,
    generation: u64,
    trigger: SourceTrigger,
) -> Result<Option<GitSourceStatus>, AppError> {
    let store = state.store.clone();
    let admitted = database(uuid::Uuid::new_v4(), move || {
        store.refresh_stack_sync(&name, now())?;
        store.begin_stack_check(&name, generation, now(), trigger)
    })
    .await?;
    let Some((source, fresh)) = admitted else {
        return Ok(None);
    };
    if fresh {
        let worker_state = state.clone();
        let worker_source = source.clone();
        let task = tokio::spawn(async move {
            let _permit = worker_state
                .stack_source_permits
                .clone()
                .acquire_owned()
                .await
                .expect("Git source semaphore stays open");
            let check = worker_source
                .latest
                .clone()
                .expect("admission created a check");
            match Github::new() {
                Ok(github) => reconcile(&worker_state, &worker_source, check, &github).await,
                Err(error) => {
                    complete(&worker_state, worker_source.stack_id, check, Err(error)).await
                }
            }
        });
        let failed_source = source.clone();
        tokio::spawn(async move {
            if task.await.is_err() {
                let check = failed_source
                    .latest
                    .clone()
                    .expect("admission created a check");
                complete(
                    &state,
                    failed_source.stack_id,
                    check,
                    Err("check_worker_failed"),
                )
                .await;
            }
        });
    }
    Ok(Some(source))
}

async fn reconcile(
    state: &AppState,
    source: &GitSourceStatus,
    mut check: GitCheck,
    github: &Github,
) {
    let result = tokio::time::timeout(
        Duration::from_secs(90),
        inspect(state, source, &mut check, github),
    )
    .await
    .unwrap_or(Err("source_timeout"));
    match result {
        Ok(stack) if check.sync && check.state == GitCheckState::Drifted => {
            let request = ApplyRequest {
                stack,
                expected_fingerprint: check
                    .plan
                    .as_ref()
                    .expect("inspection produced a plan")
                    .fingerprint
                    .clone(),
                operation_id: check.id,
                allow_stop: source.source.allow_stop,
            };
            // Successful admission atomically links the source and apply. The scheduler/status
            // endpoint follows that journal; a timeout must never start a replacement operation.
            if let Err(error) =
                crate::handlers::stack_applies::admit(state.clone(), request, Some(check.clone()))
                    .await
            {
                check.state = GitCheckState::Failed;
                check.error = Some(error.code().into());
                complete(state, source.stack_id, check, Ok(())).await;
            }
        }
        Ok(_) => complete(state, source.stack_id, check, Ok(())).await,
        Err(error) => complete(state, source.stack_id, check, Err(error)).await,
    }
}

async fn complete(
    state: &AppState,
    stack_id: uuid::Uuid,
    mut check: GitCheck,
    result: Result<(), &'static str>,
) {
    if let Err(code) = result {
        check.state = GitCheckState::Failed;
        check.error = Some(code.into());
    }
    check.finished_at = Some(now());
    let store = state.store.clone();
    if database(check.id, move || {
        store.finish_stack_check(stack_id, &check, now())
    })
    .await
    .is_err()
    {
        tracing::error!(%stack_id, "could not finish Git source observation");
    }
}

async fn inspect(
    state: &AppState,
    source: &GitSourceStatus,
    check: &mut GitCheck,
    github: &Github,
) -> Result<Stack, &'static str> {
    source.source.validate().map_err(|_| "invalid_source")?;
    let token = if source.source.credential_ref == Some(GitCredential::Github) {
        let store = state.store.clone();
        let auths = database(check.id, move || store.pipe_auths())
            .await
            .map_err(|_| "credential_unavailable")?;
        Some(
            auths
                .into_iter()
                .find_map(|auth| match auth {
                    SavedAuth::Github { token, .. } => Some(token),
                    _ => None,
                })
                .ok_or("credential_unavailable")?,
        )
    } else {
        None
    };
    let stack = github.manifest(source, check, token.as_deref()).await?;
    if stack.name != source.name {
        return Err("manifest_stack_name_mismatch");
    }
    check.desired_hash = Some(firecrab_stack::desired_hash(&stack));
    let store = state.store.clone();
    let templates = state.templates.clone();
    let name = source.name.clone();
    let expected_id = source.stack_id;
    let (record, observed) = database(check.id, move || {
        let record = store
            .stack(&name)?
            .filter(|r| r.id == expected_id)
            .ok_or(PersistenceError::MissingStack)?;
        Ok((record, inventory(&store, &templates)?))
    })
    .await
    .map_err(|_| "inventory_unavailable")?;
    let mut plan =
        firecrab_stack::plan(&stack, &record, &observed).map_err(|_| "planning_failed")?;
    // Git declarations must survive host alias updates and controller restarts.
    for change in &mut plan.changes {
        if change.kind == firecrab_stack::ResourceKind::Vm
            && stack
                .micro_vms
                .get(&change.key)
                .is_some_and(|vm| vm.template_pin.is_none())
        {
            change.action = Action::Blocked;
            change.reason = Some("Git-managed VMs require templatePin; copy the approved plan input identity into the manifest".into());
            plan.blocked = true;
        }
    }
    check.state = if plan.blocked
        || plan.changes.iter().any(|c| {
            c.action == Action::Blocked || (c.action == Action::Replace && !stack.lifecycle.replace)
        }) {
        GitCheckState::Blocked
    } else if plan
        .changes
        .iter()
        .any(|c| !matches!(c.action, Action::Noop | Action::Retain))
    {
        GitCheckState::Drifted
    } else {
        GitCheckState::InSync
    };
    check.plan = Some(plan);
    Ok(stack)
}

/// Fixed GitHub API origin. Redirects, repository scripts, submodules and symlinks are excluded.
struct Github {
    client: reqwest::Client,
    origin: String,
}

impl Github {
    fn new() -> Result<Self, &'static str> {
        let client = reqwest::Client::builder()
            .https_only(true)
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(15))
            .user_agent("firecrab-stack-source")
            .build()
            .map_err(|_| "source_client_failed")?;
        Ok(Self {
            client,
            origin: "https://api.github.com".into(),
        })
    }

    async fn request(
        &self,
        path: &str,
        token: Option<&str>,
        accept: &str,
        limit: usize,
    ) -> Result<Vec<u8>, &'static str> {
        let mut request = self
            .client
            .get(format!("{}{path}", self.origin))
            .header("Accept", accept)
            .header("X-GitHub-Api-Version", "2022-11-28");
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
        let mut response = request.send().await.map_err(|_| "source_unreachable")?;
        match response.status().as_u16() {
            200 => {}
            401 | 403 => return Err("source_access_denied_or_rate_limited"),
            404 => return Err("source_not_found"),
            429 => return Err("source_rate_limited"),
            _ => return Err("source_response_failed"),
        }
        if response.content_length().is_some_and(|n| n > limit as u64) {
            return Err("source_response_too_large");
        }
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| "source_response_failed")?
        {
            if chunk.len() > limit.saturating_sub(body.len()) {
                return Err("source_response_too_large");
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    }

    async fn json<T: DeserializeOwned>(
        &self,
        path: &str,
        token: Option<&str>,
        limit: usize,
    ) -> Result<T, &'static str> {
        serde_json::from_slice(
            &self
                .request(path, token, "application/vnd.github+json", limit)
                .await?,
        )
        .map_err(|_| "invalid_source_response")
    }

    async fn manifest(
        &self,
        source: &GitSourceStatus,
        check: &mut GitCheck,
        token: Option<&str>,
    ) -> Result<Stack, &'static str> {
        let repo = source
            .source
            .repository
            .strip_prefix("https://github.com/")
            .ok_or("invalid_source")?;
        let base = format!("/repos/{repo}");
        let reference = utf8_percent_encode(&source.source.git_ref, NON_ALPHANUMERIC);
        let raw = self
            .request(
                &format!("{base}/commits/{reference}"),
                token,
                "application/vnd.github.sha",
                128,
            )
            .await?;
        let commit = std::str::from_utf8(&raw)
            .map_err(|_| "invalid_commit")?
            .trim();
        if !sha(commit) {
            return Err("invalid_commit");
        }
        check.commit = Some(commit.into());
        let object: Commit = self
            .json(&format!("{base}/git/commits/{commit}"), token, 128 * 1024)
            .await?;
        if object.sha != commit || !sha(&object.tree.sha) {
            return Err("invalid_commit_tree");
        }
        let mut tree_sha = object.tree.sha;
        let components = source.source.manifest_path.split('/').collect::<Vec<_>>();
        let mut blob_sha = None;
        for (index, part) in components.iter().enumerate() {
            let tree: Tree = self
                .json(
                    &format!("{base}/git/trees/{tree_sha}"),
                    token,
                    2 * MAX_MANIFEST_BYTES,
                )
                .await?;
            if tree.sha != tree_sha || tree.truncated {
                return Err("invalid_or_truncated_tree");
            }
            let entry = tree
                .tree
                .into_iter()
                .find(|e| e.path == *part)
                .ok_or("manifest_path_not_found")?;
            if !sha(&entry.sha) {
                return Err("invalid_tree_entry");
            }
            if index + 1 < components.len() {
                if entry.kind != "tree" || entry.mode != "040000" {
                    return Err("manifest_path_not_regular");
                }
                tree_sha = entry.sha;
            } else {
                if entry.kind != "blob" || !matches!(entry.mode.as_str(), "100644" | "100755") {
                    return Err("manifest_path_not_regular");
                }
                blob_sha = Some(entry.sha);
            }
        }
        let blob_sha = blob_sha.ok_or("manifest_path_not_found")?;
        check.blob = Some(blob_sha.clone());
        let blob: Blob = self
            .json(
                &format!("{base}/git/blobs/{blob_sha}"),
                token,
                2 * MAX_MANIFEST_BYTES,
            )
            .await?;
        if blob.sha != blob_sha || blob.encoding != "base64" || blob.size > MAX_MANIFEST_BYTES {
            return Err("invalid_manifest_blob");
        }
        let encoded: String = blob
            .content
            .chars()
            .filter(|c| !c.is_ascii_whitespace())
            .collect();
        let bytes = STANDARD
            .decode(encoded)
            .map_err(|_| "invalid_manifest_blob")?;
        if bytes.len() != blob.size || bytes.len() > MAX_MANIFEST_BYTES {
            return Err("invalid_manifest_blob");
        }
        let mut hash = ring::digest::Context::new(&ring::digest::SHA1_FOR_LEGACY_USE_ONLY);
        hash.update(format!("blob {}\0", bytes.len()).as_bytes());
        hash.update(&bytes);
        let actual = hash
            .finish()
            .as_ref()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        if actual != blob_sha {
            return Err("manifest_blob_hash_mismatch");
        }
        let text = String::from_utf8(bytes).map_err(|_| "manifest_not_utf8")?;
        firecrab_stack::parse(&text).map_err(|_| "invalid_manifest")
    }
}

fn sha(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[derive(Deserialize)]
struct Object {
    sha: String,
}
#[derive(Deserialize)]
struct Commit {
    sha: String,
    tree: Object,
}
#[derive(Deserialize)]
struct Tree {
    sha: String,
    truncated: bool,
    tree: Vec<Entry>,
}
#[derive(Deserialize)]
struct Entry {
    path: String,
    mode: String,
    #[serde(rename = "type")]
    kind: String,
    sha: String,
}
#[derive(Deserialize)]
struct Blob {
    sha: String,
    encoding: String,
    size: usize,
    content: String,
}

#[cfg(test)]
mod tests;
