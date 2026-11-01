//! Conditional native-scope termination never removes retained editor data or browser buffers.
use super::*;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Stop {
    expected_instance: String,
}
fn same_instance(editor: &Editor, expected: &str) -> Result<(), Error> {
    if instance(editor) != expected {
        return Err(Error::Conflict(
            "The editor instance changed; this stop cannot affect its replacement",
        ));
    }
    Ok(())
}
/// Idempotent for this execution only. A late retry cannot stop a replacement execution.
pub(super) async fn request(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    Path((project, view)): Path<(Uuid, Uuid)>,
    caller: Caller,
    ValidatedJson(request): ValidatedJson<Stop>,
) -> Result<Json<serde_json::Value>, AppError> {
    if request.expected_instance.len() != 64
        || !request
            .expected_instance
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(
            Error::Invalid("Use the exact retained editor instance identity").response(id.0),
        );
    }
    files::run(state.workspaces.file_slots.clone(), id.0, async move {
        authorize(&state, caller, project, SandboxPermission::Read, id.0)?;
        let local = state.clone();
        let expected = request.expected_instance.clone();
        let retained = blocking(id.0, move || {
            let b = binding(&local, project)?;
            let db = open(&local, &b)?;
            let editor = db.editor(caller, view, true)?;
            same_instance(&editor, &expected)?;
            caller
                .project(&local, project, SandboxPermission::Read)
                .map_err(|_| Error::Denied)?;
            Ok(editor)
        })
        .await?;
        if matches!(retained.status.as_str(), "stopped" | "interrupted") {
            return Ok(Json(response(project, &retained)));
        }
        terminals::preauthorize(&state, caller, project, id.0)?;
        let mut ctx = files::Files::open(state.clone(), project, caller, false, id.0).await?;
        terminals::execute(&state, caller, &ctx.binding, id.0)?;
        // Re-read after acquiring the runtime/project lock, before committing stop intent.
        let mut editor = ctx
            .db
            .editor(caller, view, true)
            .map_err(|e| e.response(id.0))?;
        same_instance(&editor, &request.expected_instance).map_err(|e| e.response(id.0))?;
        if matches!(editor.status.as_str(), "stopped" | "interrupted") {
            return Ok(Json(response(project, &editor)));
        }
        if editor.runtime != ctx.runtime || editor.generation != ctx.binding.generation {
            return Err(Error::Conflict(
                "The editor runtime changed; reconcile its retained state",
            )
            .response(id.0));
        }
        editor.status = "stopping".into();
        persist(&ctx, &editor).map_err(|e| e.response(id.0))?;
        // This owned worker survives the HTTP client's disconnect. Maintenance
        // reconciles the committed intent if a guest response remains uncertain.
        finish(&mut ctx, &mut editor, id.0).await?;
        authorize(&state, caller, project, SandboxPermission::Read, id.0)?;
        Ok(Json(response(project, &editor)))
    })
    .await
}
/// Verify the original UID/kernel proof before stopping only that native execution scope.
pub(super) async fn finish(
    ctx: &mut files::Files,
    editor: &mut Editor,
    id: Uuid,
) -> Result<(), AppError> {
    if editor.runtime != ctx.runtime || editor.generation != ctx.binding.generation {
        return Err(
            Error::Conflict("Editor stop belongs to an earlier runtime binding").response(id),
        );
    }
    let frame =
        terminals_guest::frame(editor.attempt, 2, 0, 0, 0, &[]).map_err(|e| e.response(id))?;
    match terminals_guest::request(ctx, frame).await {
        Err(Error::Missing) if editor.proof.is_none() => {}
        Err(Error::Missing) => {
            return Err(Error::Conflict(
                "The native editor scope is missing; termination cannot be confirmed",
            )
            .response(id));
        }
        Err(error) => return Err(error.response(id)),
        Ok(native) => {
            if editor
                .proof
                .as_ref()
                .is_some_and(|proof| proof != &native.proof)
            {
                return Err(Error::Conflict(
                    "Private editor process identity changed; stop was refused",
                )
                .response(id));
            }
            if native.status == ProjectTerminalStatus::Running || native.cleanup_required {
                let frame = terminals_guest::frame(editor.attempt, 5, 0, 0, 0, &[])
                    .map_err(|e| e.response(id))?;
                let stopped = terminals_guest::request(ctx, frame)
                    .await
                    .map_err(|e| e.response(id))?;
                if stopped.proof != native.proof
                    || stopped.status == ProjectTerminalStatus::Running
                    || stopped.cleanup_required
                {
                    return Err(
                        Error::Conflict("Private editor termination is not yet confirmed")
                            .response(id),
                    );
                }
            }
        }
    }
    editor.status = "stopped".into();
    persist(ctx, editor).map_err(|e| e.response(id))
}
