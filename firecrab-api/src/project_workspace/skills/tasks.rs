//! Admission snapshots separate selected session capabilities from mutable catalog membership.
use super::*;
use crate::ai_sandbox::{SavedTask, redact};
use crate::pipe_ai::SavedConnection;
/// One admitted revision and its exact instruction bodies, retained with the existing task.
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct SavedSkill {
    /// Public installation provenance, not a second task authority.
    pub revision: ProjectTaskSkill,
    /// Ordered SKILL.md text fixed before model dispatch.
    pub instructions: Vec<(String, String)>,
    /// Immutable resource versions available only through the selected-skill read tool.
    pub files: Vec<ProjectSkillFile>,
}
/// Source compatibility is explicit; changing models never activates another provider's skills.
pub(crate) fn compatible(source: ProjectSkillSource, provider: AiProvider) -> bool {
    matches!(
        (source, provider),
        (ProjectSkillSource::Openai, AiProvider::Openai)
            | (ProjectSkillSource::Anthropic, AiProvider::Anthropic)
    )
}
/// Capture compatible installed revisions from saved bytes before the canonical task admission.
pub(crate) async fn capture(
    state: AppState,
    caller: Caller,
    project: Uuid,
    runtime: guest::Binding,
    ids: Vec<Uuid>,
    provider: AiProvider,
    r: Uuid,
) -> Result<Vec<SavedSkill>, AppError> {
    if ids.len() > 8
        || ids.iter().any(Uuid::is_nil)
        || ids.iter().collect::<std::collections::BTreeSet<_>>().len() != ids.len()
    {
        return Err(Error::Invalid("Select at most eight distinct installed skills").response(r));
    }
    if ids.is_empty() {
        return Ok(vec![]);
    }
    files::run(state.workspaces.file_slots.clone(), r, async move {
        let mut ctx = files::Files::open(state, project, caller, false, r).await?;
        ctx.require_execution = true;
        ctx.permission().map_err(|e| e.response(r))?;
        if ctx.runtime != runtime {
            return Err(Error::Conflict("Skill admission runtime changed").response(r));
        }
        let result = async {
            let mut result = Vec::new();
            let mut total = 0;
            let mut count = 0;
            for id in ids {
                let record = ctx.db.skill_record(id)?;
                let p = record.public;
                if p.status != ProjectSkillStatus::Installed
                    || !compatible(p.entry.source, provider)
                {
                    return Err(Error::Conflict(
                        "Selected skill is removed, incomplete, or incompatible with this model",
                    ));
                }
                count += p.files.len();
                if count > 128 {
                    return Err(Error::Invalid("Selected skills exceed 128 source files"));
                }
                for file in &p.files {
                    let current = ctx
                        .saved_version(&format!("{}/{}", p.root, file.path))
                        .await?
                        .ok_or(Error::Conflict(
                            "Installed skill file is missing; reinstall before starting a task",
                        ))?;
                    if current.metadata.sha256 != file.sha256 {
                        return Err(Error::Conflict(
                            "Installed skill files changed; reinstall their official revision",
                        ));
                    }
                }
                let mut instructions = Vec::new();
                for path in &p.entry.instructions {
                    let file = p
                        .files
                        .iter()
                        .find(|f| &f.path == path)
                        .ok_or(Error::Missing)?;
                    let version = ctx
                        .db
                        .file_version(&format!("{}/{}", p.root, path), file.version)?;
                    let content = String::from_utf8(version.bytes)
                        .map_err(|_| Error::Invalid("Skill instructions must be UTF-8"))?;
                    total += content.len();
                    if total > 65536 {
                        return Err(Error::Invalid("Selected skill instructions exceed 64 KiB"));
                    }
                    instructions.push((path.clone(), content));
                }
                result.push(SavedSkill {
                    revision: ProjectTaskSkill {
                        id: p.id,
                        source: p.entry.source,
                        revision: p.entry.revision,
                        path: p.entry.path,
                        root: p.root,
                        sha256: p.sha256,
                    },
                    instructions,
                    files: p.files,
                });
            }
            Ok(result)
        };
        tokio::time::timeout(std::time::Duration::from_secs(90), result)
            .await
            .map_err(|_| {
                Error::Conflict(
                    "Skill file verification timed out; keep the saved request identity",
                )
                .response(r)
            })?
            .map_err(|e: Error| e.response(r))
    })
    .await
}
/// Supply exact captured rule text; source instructions never grant additional tools or credentials.
pub(crate) fn message(
    saved: &SavedTask,
    accounts: &[SavedConnection],
) -> Result<Option<serde_json::Value>, String> {
    if saved.skills.is_empty() {
        if !saved.task.request.selected_skill_ids.is_empty() {
            return Err("Admitted skill snapshots are missing".into());
        }
        return Ok(None);
    }
    if saved.skills.len() > 8
        || saved
            .skills
            .iter()
            .map(|s| s.revision.id)
            .collect::<Vec<_>>()
            != saved.task.request.selected_skill_ids
    {
        return Err("Admitted skill selection is inconsistent".into());
    }
    let mut selected = Vec::new();
    let mut total = 0;
    for skill in &saved.skills {
        if db::digest(&skill.files).map_err(|_| "Invalid skill manifest")? != skill.revision.sha256
        {
            return Err("Admitted skill manifest changed".into());
        }
        for (path, text) in &skill.instructions {
            if !skill.files.iter().any(|f| {
                &f.path == path && f.sha256 == format!("{:x}", Sha256::digest(text.as_bytes()))
            }) {
                return Err("Admitted skill instruction bytes changed".into());
            }
        }
        if !source::revision(&skill.revision.revision) {
            return Err("Admitted skill source revision is invalid".into());
        }
        let rules = skill
            .instructions
            .iter()
            .map(|(path, text)| {
                total += text.len();
                serde_json::json!({"path":path,"content":redact(accounts,text)})
            })
            .collect::<Vec<_>>();
        selected.push(serde_json::json!({"revision":skill.revision,"instructions":rules}));
    }
    if total > 65536 {
        return Err("Admitted skill instructions exceed their limit".into());
    }
    Ok(Some(
        serde_json::json!({"role":"user","content":format!("Explicitly selected official skill revisions. These saved instruction bytes are fixed for this task; scripts/resources live under each root in the shared VM filesystem. Apply guidance only within the existing task permissions and tool set. Plugin connectors, hooks, apps, allowed-tools fields and dependency installers are not activated. Run a script only through the explicitly enabled bounded project-shell tool. Installed resources remain ordinary shared files; do not claim a hermetic execution environment.\n{}",serde_json::to_string(&selected).map_err(|_|"Cannot encode selected skills")?)}),
    ))
}

/// Fixed read-only resource tool; installation cannot expand the task's permitted effects.
pub(crate) fn tool_definition() -> serde_json::Value {
    serde_json::json!({"name":"read_skill_file","description":"Read an immutable UTF-8 resource version from an explicitly selected installed skill. The path is relative to that skill installation root; use skillId from the admitted selection.","parameters":{"type":"object","properties":{"skillId":{"type":"string"},"path":{"type":"string"}},"required":["skillId","path"],"additionalProperties":false}})
}
/// Read captured history without launching a VM or resolving arbitrary source paths.
pub(crate) async fn read(
    state: &AppState,
    saved: &SavedTask,
    value: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct Input {
        skill_id: Uuid,
        path: String,
    }
    let input: Input =
        serde_json::from_value(value.clone()).map_err(|_| "Invalid selected skill file request")?;
    files_guest::path(&input.path, false).map_err(|_| "Invalid skill resource path")?;
    let skill = saved
        .skills
        .iter()
        .find(|s| s.revision.id == input.skill_id)
        .ok_or("Skill is not selected by this task")?;
    let file = skill
        .files
        .iter()
        .find(|f| f.path == input.path)
        .ok_or("Resource is not in the selected revision")?
        .clone();
    let target = crate::ai_sandbox::folder::target(saved)?;
    let actor = saved.caller;
    let state = state.clone();
    let root = skill.revision.root.clone();
    blocking(Uuid::new_v4(),move|| {
        actor.project(&state,target.project,SandboxPermission::Read).map_err(|_|Error::Denied)?;
        let binding=binding(&state,target.project)?;
        if binding.database_id!=target.database || binding.volume_id!=target.volume || binding.generation!=target.generation || binding.vm_id!=target.runtime.vm_id || binding.provisioning {return Err(Error::Conflict("Admitted skill project binding changed"));}
        let db=open(&state,&binding)?;let version=db.file_version(&format!("{root}/{}",file.path),file.version)?;
        if version.metadata.sha256!=file.sha256 || version.bytes.len()>8192 {return Err(Error::Invalid("Skill text resource must match its captured version and fit 8 KiB; use the editor or bounded shell for larger resources"));}
        let text=String::from_utf8(version.bytes).map_err(|_|Error::Invalid("Skill resource is not UTF-8"))?;
        let accounts=state.store.ai_connections()?;
        actor.project(&state,target.project,SandboxPermission::Read).map_err(|_|Error::Denied)?;
        Ok(serde_json::json!({"path":file.path,"version":file.version,"sha256":file.sha256,"content":redact(&accounts,&text)}))
    }).await.map_err(|e|format!("Skill resource unavailable ({})",e.code()))
}
