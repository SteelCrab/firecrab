//! File-agent transport over existing retained-volume APIs; no host filesystem execution.
use super::*;
use crate::ai_sandbox::files as model_files;
use files_db::Operation;

/// Server-selected authority. Model arguments cannot replace these identities.
#[derive(Clone)]
pub(crate) struct Target {
    pub project: Uuid,
    pub database: Uuid,
    pub volume: Uuid,
    pub generation: u64,
    pub runtime: guest::Binding,
    pub caller: Caller,
    pub directory: String,
}
/// Immutable per-call data, with an operation id chosen by the task controller.
#[derive(Clone)]
pub(crate) struct WriteRequest {
    pub path: String,
    pub operation: Uuid,
    pub expected: u64,
    pub content: Vec<u8>,
}
impl Target {
    fn expected(&self) -> Binding {
        Binding {
            project_id: self.project,
            database_id: self.database,
            volume_id: self.volume,
            vm_id: self.runtime.vm_id,
            generation: self.generation,
            provisioning: false,
            verified_runtime_id: Some(self.runtime.runtime_id),
        }
    }
    fn check(&self, b: &Binding) -> Result<(), Error> {
        if b.project_id != self.project
            || b.database_id != self.database
            || b.volume_id != self.volume
            || b.generation != self.generation
            || b.vm_id != self.runtime.vm_id
            || b.provisioning
        {
            return Err(Error::Conflict("The admitted folder binding changed"));
        }
        Ok(())
    }
    fn pinned(&self, context: &mut files::Files) -> Result<(), Error> {
        self.check(&context.binding)?;
        context.require_execution = true;
        context.permission()?;
        if context.runtime != self.runtime {
            return Err(Error::Conflict("The admitted folder runtime changed"));
        }
        Ok(())
    }
    fn permission(&self, state: &AppState, caller: Caller, id: Uuid) -> Result<(), AppError> {
        authorize(state, caller, self.project, SandboxPermission::Read, id)?;
        authorize(state, caller, self.project, SandboxPermission::Write, id)?;
        self.check(&binding(state, self.project).map_err(|e| e.response(id))?)
            .map_err(|e| e.response(id))
    }
}
/// Keep the Sandbox credential/reserved-file exclusions in addition to native fd boundaries.
pub(crate) fn path(directory: &str, relative: &str, root: bool) -> Result<String, String> {
    files_guest::path(directory, true).map_err(|_| "Invalid project folder")?;
    files_guest::path(relative, root).map_err(|_| "Invalid folder-relative path")?;
    let full = if directory.is_empty() {
        relative.into()
    } else if relative.is_empty() {
        directory.into()
    } else {
        format!("{directory}/{relative}")
    };
    files_guest::path(&full, root).map_err(|_| "Invalid project path")?;
    if !full.is_empty() && !model_files::allowed(&full) {
        return Err("Credential or excluded project paths are unavailable to agents".into());
    }
    Ok(full)
}
/// Admission proves the actual selected directory on the already verified project volume.
pub(crate) async fn admit(state: AppState, target: Target, id: Uuid) -> Result<(), AppError> {
    target.permission(&state, target.caller, id)?;
    files::run(state.workspaces.file_slots.clone(), id, async move {
        let mut ctx = files::Files::open(state, target.project, target.caller, true, id).await?;
        target.pinned(&mut ctx).map_err(|e| e.response(id))?;
        ctx.directory(&target.directory)
            .await
            .map_err(|e| e.response(id))
    })
    .await
}
pub(crate) async fn list(
    state: AppState,
    target: Target,
    relative: String,
    id: Uuid,
) -> Result<Vec<ProjectFileEntry>, AppError> {
    let selected = path(&target.directory, &relative, true)
        .map_err(|_| Error::Invalid("Invalid folder path").response(id))?;
    target.permission(&state, target.caller, id)?;
    files::run(state.workspaces.file_slots.clone(), id, async move {
        let mut ctx = files::Files::open(state, target.project, target.caller, false, id).await?;
        target.pinned(&mut ctx).map_err(|e| e.response(id))?;
        let entries = ctx.entries(&selected).await.map_err(|e| e.response(id))?;
        Ok(entries
            .into_iter()
            .filter(|entry| {
                let child = if relative.is_empty() {
                    entry.name.clone()
                } else {
                    format!("{relative}/{}", entry.name)
                };
                path(&target.directory, &child, false).is_ok()
            })
            .collect())
    })
    .await
}
pub(crate) async fn read(
    state: AppState,
    target: Target,
    relative: String,
    id: Uuid,
) -> Result<ProjectFile, AppError> {
    let selected = path(&target.directory, &relative, false)
        .map_err(|_| Error::Invalid("Invalid file path").response(id))?;
    target.permission(&state, target.caller, id)?;
    files::run(state.workspaces.file_slots.clone(), id, async move {
        let mut ctx = files::Files::open(state, target.project, target.caller, false, id).await?;
        target.pinned(&mut ctx).map_err(|e| e.response(id))?;
        ctx.saved_version(&selected)
            .await
            .map_err(|e| e.response(id))?
            .ok_or_else(|| Error::Missing.response(id))
            .map(files_db::Version::response)
    })
    .await
}
pub(crate) async fn write(
    state: AppState,
    target: Target,
    request: WriteRequest,
    id: Uuid,
) -> Result<ProjectFile, AppError> {
    let WriteRequest {
        path: relative,
        operation,
        expected,
        content,
    } = request;
    let selected = path(&target.directory, &relative, false)
        .map_err(|_| Error::Invalid("Invalid file path").response(id))?;
    if operation.is_nil() || content.len() > files_guest::FILE_LIMIT || expected > i64::MAX as u64 {
        return Err(Error::Invalid("Invalid file operation or size").response(id));
    }
    target.permission(&state, target.caller, id)?;
    let pinned = target.expected();
    files::run(state.workspaces.file_slots.clone(), id, async move {
        files::write_bound(
            state,
            target.project,
            target.caller,
            Operation {
                id: operation,
                path: selected,
                expected,
                content,
                author: target.caller.principal,
                restored_from: None,
                created_at: crate::ai_sandbox::now(),
                version: None,
                state: "pending".into(),
            },
            id,
            Some((pinned, target.runtime)),
        )
        .await
    })
    .await
}
/// Check receipts only. Task cancellation/recovery never replays a shared-file write.
pub(crate) async fn settle(
    state: AppState,
    target: Target,
    actor: Caller,
    request: WriteRequest,
    id: Uuid,
) -> Result<Option<SandboxFolderMutation>, AppError> {
    let WriteRequest {
        path: relative,
        operation,
        expected,
        content,
    } = request;
    let selected = path(&target.directory, &relative, false)
        .map_err(|_| Error::Invalid("Invalid file path").response(id))?;
    target.permission(&state, actor, id)?;
    files::run(state.workspaces.file_slots.clone(),id,async move {
        let local=state.clone();let saved=target.clone();let selected=selected.clone();
        let record=blocking(id,move||{
            let b=binding(&local,saved.project)?;saved.check(&b)?;
            let db=open(&local,&b)?;
            let Some(op)=db.file_operation(operation)? else {return Ok(None);};
            if op.path!=selected || op.expected!=expected || op.content!=content || op.author!=saved.caller.principal || op.restored_from.is_some() {
                return Err(Error::Conflict("Folder operation provenance changed"));
            }
            if actor.principal!=op.author && actor.operator().is_err() {
                let owner=local.store.sandbox_project(saved.project)?.ok_or(Error::Missing)?.owner;
                if owner!=actor.principal {return Err(Error::Denied);}
            }
            Ok(Some(op))
        }).await?;
        let Some(mut op)=record else{return Ok(None);};
        if op.state=="failed" {return Ok(None);}
        if op.version.is_none() {
            let mut ctx=files::Files::open(state.clone(),target.project,actor,true,id).await?;
            target.check(&ctx.binding).map_err(|e|e.response(id))?;
            if !ctx.receipt(&op).await.map_err(|e|e.response(id))? {
                return Err(Error::Conflict("The folder write is unconfirmed; explicitly recover its original file operation").response(id));
            }
            op=ctx.db.file_operation(operation).map_err(|e|e.response(id))?.ok_or_else(||Error::Missing.response(id))?;
        }
        let version=op.version.ok_or_else(||Error::Conflict("Folder write remains unconfirmed").response(id))?;
        blocking(id,move||{
            let b=binding(&state,target.project)?;target.check(&b)?;
            let db=open(&state,&b)?;let after=db.file_version(&op.path,version)?;
            actor.project(&state,target.project,SandboxPermission::Read).map_err(|_|Error::Denied)?;
            Ok(Some(SandboxFolderMutation {operation_id:op.id,path:op.path,before_version:op.expected,
                after_version:version,sha256:after.metadata.sha256}))
        }).await
    }).await
}

#[cfg(test)]
mod tests;
