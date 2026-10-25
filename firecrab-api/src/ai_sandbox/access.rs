//! Sandbox authorization uses verified transport identity, never caller-supplied headers.
use crate::{error::AppError, server::RequestId, state::AppState, tls::AuthenticatedHost};
use axum::{
    extract::{ConnectInfo, FromRequestParts},
    http::{StatusCode, request::Parts},
};
use firecrab_api_types::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Scoped hosts cannot bypass Sandbox grants through VM keys, provider APIs or consoles.
pub async fn restrict_delegates(
    axum::extract::State(state): axum::extract::State<AppState>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    if let Some(ConnectInfo(peer)) = request.extensions().get::<ConnectInfo<AuthenticatedHost>>() {
        let controller = request
            .extensions()
            .get::<ControllerIdentity>()
            .is_some_and(|id| id.0 == peer.host_id);
        if !controller {
            let id = request
                .extensions()
                .get::<RequestId>()
                .map_or_else(Uuid::new_v4, |id| id.0);
            match state.store.sandbox_delegate(peer.host_id) {
                Err(_) => return AppError::internal(id).into_response(),
                Ok(true) => {
                    let path = request.uri().path();
                    let sandbox = path.starts_with("/api/ai-sandbox/");
                    let public_asset = matches!(
                        *request.method(),
                        axum::http::Method::GET | axum::http::Method::HEAD
                    ) && !path.starts_with("/api/")
                        && !path.starts_with("/ws/")
                        && !path.starts_with("/auth/");
                    if !sandbox && !public_asset {
                        return AppError::new(
                            StatusCode::FORBIDDEN,
                            "sandbox_scope_required",
                            "Delegated hosts can use only the Sandbox API",
                            id,
                        )
                        .into_response();
                    }
                }
                Ok(false) => {}
            }
        }
    }
    next.run(request).await
}

/// Installed only by the loopback plaintext listener, after its bind check.
#[derive(Clone, Copy)]
pub(crate) struct LocalOperator;

/// Identity of the configured controller certificate, installed by the TLS listener.
#[derive(Clone, Copy)]
pub(crate) struct ControllerIdentity(pub Uuid);

/// Private execution identity. Operator status can only originate at the listener.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Caller {
    /// Immutable caller UUID/transport origin captured at admission.
    pub principal: SandboxPrincipal,
    operator: bool,
}

impl Default for Caller {
    fn default() -> Self {
        Self {
            principal: SandboxPrincipal::Local,
            operator: true,
        }
    }
}

impl<S: Send + Sync> FromRequestParts<S> for Caller {
    type Rejection = AppError;
    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        if let Some(ConnectInfo(peer)) = parts.extensions.get::<ConnectInfo<AuthenticatedHost>>() {
            return Ok(Self {
                principal: SandboxPrincipal::Host(peer.host_id),
                operator: parts
                    .extensions
                    .get::<ControllerIdentity>()
                    .is_some_and(|id| id.0 == peer.host_id),
            });
        }
        if parts.extensions.get::<LocalOperator>().is_some() {
            return Ok(Self::default());
        }
        let id = parts
            .extensions
            .get::<RequestId>()
            .map_or_else(Uuid::new_v4, |id| id.0);
        Err(AppError::new(
            StatusCode::UNAUTHORIZED,
            "sandbox_identity_required",
            "Verified controller transport identity is required",
            id,
        ))
    }
}

impl Caller {
    /// Only controller identities can administer resource policies.
    pub fn operator(self) -> Result<(), String> {
        if self.operator {
            Ok(())
        } else {
            Err("Only the controller operator can manage Sandbox grants".into())
        }
    }

    /// Match every right against one atomic policy snapshot.
    pub fn require_all(
        self,
        state: &AppState,
        rights: &[(SandboxResource, SandboxPermission)],
    ) -> Result<(), String> {
        if self.operator {
            return Ok(());
        }
        let SandboxPrincipal::Host(principal) = self.principal else {
            return Err("Sandbox permission denied".into());
        };
        let policy = state
            .store
            .sandbox_access(principal)
            .map_err(|_| "Cannot read Sandbox permissions")?;
        if rights.iter().all(|(resource, permission)| {
            policy
                .grants
                .iter()
                .any(|grant| grant.resource == *resource && grant.permissions.contains(permission))
        }) {
            Ok(())
        } else {
            Err("Sandbox project or resource permission denied".into())
        }
    }

    /// Test one exact resource grant without implicit inheritance.
    pub fn allowed(
        self,
        state: &AppState,
        resource: &SandboxResource,
        permission: SandboxPermission,
    ) -> Result<bool, String> {
        if self.operator {
            return Ok(true);
        }
        let SandboxPrincipal::Host(principal) = self.principal else {
            return Ok(false);
        };
        let policy = state
            .store
            .sandbox_access(principal)
            .map_err(|_| "Cannot read Sandbox permissions")?;
        Ok(policy
            .grants
            .iter()
            .any(|grant| grant.resource == *resource && grant.permissions.contains(&permission)))
    }

    /// Project listing uses one policy snapshot and filters sources before the SQL page limit.
    pub fn sources(self, state: &AppState, project: Uuid) -> Result<Option<Vec<String>>, String> {
        if self.operator {
            return Ok(None);
        }
        let SandboxPrincipal::Host(principal) = self.principal else {
            return Err("Sandbox permission denied".into());
        };
        let policy = state
            .store
            .sandbox_access(principal)
            .map_err(|_| "Cannot read Sandbox permissions")?;
        if !policy.grants.iter().any(|g| {
            g.resource == SandboxResource::Project { id: project }
                && g.permissions.contains(&SandboxPermission::Read)
        }) {
            return Err("Sandbox permission denied".into());
        }
        Ok(Some(
            policy
                .grants
                .into_iter()
                .filter_map(|g| match g.resource {
                    SandboxResource::Source { path }
                        if g.permissions.contains(&SandboxPermission::Read) =>
                    {
                        Some(path)
                    }
                    _ => None,
                })
                .collect(),
        ))
    }

    /// Reject a missing exact resource right before accessing it.
    pub fn require(
        self,
        state: &AppState,
        resource: &SandboxResource,
        permission: SandboxPermission,
    ) -> Result<(), String> {
        self.require_all(state, &[(resource.clone(), permission)])
    }

    /// Authorize access to one explicit project identity.
    pub fn project(
        self,
        state: &AppState,
        project: Uuid,
        permission: SandboxPermission,
    ) -> Result<(), String> {
        self.require(state, &SandboxResource::Project { id: project }, permission)
    }

    /// Check every resource selected by a project configuration together.
    pub fn settings(self, state: &AppState, spec: &SandboxProjectSpec) -> Result<(), String> {
        let mut rights = Vec::new();
        if let Some(path) = &spec.source_path {
            rights.push((
                SandboxResource::Source { path: path.clone() },
                SandboxPermission::Read,
            ));
        }
        if let Some(id) = spec.vm_id {
            rights.push((SandboxResource::Vm { id }, SandboxPermission::Execute));
        }
        if let Some(selection) = &spec.selection {
            rights.push((
                SandboxResource::Connection {
                    id: selection.connection_id,
                },
                SandboxPermission::Execute,
            ));
        }
        self.require_all(state, &rights)
    }

    /// Authorize the captured resources, including the original source during apply.
    pub fn task(
        self,
        state: &AppState,
        saved: &super::SavedTask,
        permission: SandboxPermission,
    ) -> Result<(), String> {
        self.require_all(state, &task_rights(saved, permission))
    }
}

/// Re-read current grants and owning project before provider/file/guest side effects.
pub fn attempt(state: &AppState, saved: &super::SavedTask) -> Result<(), String> {
    let mut rights = task_rights(saved, SandboxPermission::Execute);
    rights.push((
        SandboxResource::Connection {
            id: saved.task.selection.connection_id,
        },
        SandboxPermission::Execute,
    ));
    saved.caller.require_all(state, &rights)?;
    let project = state
        .store
        .sandbox_project(saved.task.project_id)
        .map_err(|_| "Cannot read owning Sandbox project")?
        .ok_or("Owning Sandbox project is unavailable")?;
    if project.spec.source_path != saved.project.source_path
        || project.spec.vm_id != saved.project.vm_id
    {
        return Err("Sandbox project resources changed after admission".into());
    }
    if saved.task.request.kind == SandboxTaskKind::Code {
        let runtime = saved
            .task
            .runtime
            .ok_or("Code attempt has no admitted runtime reference")?;
        if saved.project.vm_id != Some(runtime.vm_id) || saved.guest != Some(runtime) {
            return Err("Sandbox runtime reference changed after admission".into());
        }
        if super::guest::binding(state, runtime.vm_id)
            .map_err(|_| "Admitted MicroVM runtime is unavailable")?
            != runtime
        {
            return Err("MicroVM restarted after admission; create a new attempt".into());
        }
    }
    Ok(())
}

/// Assemble the full operation boundary before reading a policy.
fn task_rights(
    saved: &super::SavedTask,
    permission: SandboxPermission,
) -> Vec<(SandboxResource, SandboxPermission)> {
    let project = SandboxResource::Project {
        id: saved.task.project_id,
    };
    let mut rights = vec![
        (project.clone(), SandboxPermission::Read),
        (project, permission),
    ];
    if let Some(path) = &saved.project.source_path {
        rights.push((
            SandboxResource::Source { path: path.clone() },
            SandboxPermission::Read,
        ));
        if permission == SandboxPermission::Apply {
            rights.push((
                SandboxResource::Source { path: path.clone() },
                SandboxPermission::Write,
            ));
        }
    }
    if permission == SandboxPermission::Execute
        && saved.task.request.kind == SandboxTaskKind::Code
        && let Some(id) = saved.project.vm_id
    {
        rights.push((SandboxResource::Vm { id }, SandboxPermission::Execute));
    }
    rights
}
