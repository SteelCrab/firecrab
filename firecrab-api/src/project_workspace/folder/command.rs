//! Native command receipts, transported only through the admitted project runtime.
use super::*;
use firecrab_api_types::SandboxGuestCommand;

pub(crate) struct Reply {
    pub state: u32,
    pub exit: Option<i32>,
    pub cleanup_required: bool,
    pub truncated: bool,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}
fn number(bytes: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(bytes[at..at + 4].try_into().expect("validated frame"))
}
pub(crate) fn frame(
    operation: Uuid,
    action: u32,
    command: Option<&SandboxGuestCommand>,
) -> Result<Vec<u8>, Error> {
    if operation.is_nil() || !(1..=4).contains(&action) || (action == 4) != command.is_none() {
        return Err(Error::Invalid("Invalid project command action"));
    }
    let mut body = Vec::new();
    if let Some(c) = command {
        crate::ai_sandbox::guest_protocol::validate(c)
            .map_err(|_| Error::Invalid("Invalid bounded project command"))?;
        files_guest::path(&c.cwd, true)?;
        for n in [
            c.limits.wall_seconds as u32,
            c.limits.cpu_seconds as u32,
            c.limits.memory_mib,
            c.limits.output_bytes as u32,
            c.cwd.len() as u32,
            c.argv.len() as u32,
            c.env.len() as u32,
            c.stdin.len() as u32,
        ] {
            body.extend_from_slice(&n.to_be_bytes());
        }
        body.extend_from_slice(c.cwd.as_bytes());
        for value in c
            .argv
            .iter()
            .cloned()
            .chain(c.env.iter().map(|(key, value)| format!("{key}={value}")))
        {
            body.extend_from_slice(&(value.len() as u32).to_be_bytes());
            body.extend_from_slice(value.as_bytes());
        }
        body.extend_from_slice(c.stdin.as_bytes());
    }
    if body.len() > 98304 {
        return Err(Error::Invalid("Project command frame exceeds its bound"));
    }
    let mut frame = Vec::with_capacity(28 + body.len());
    frame.extend_from_slice(b"FCX1");
    frame.extend_from_slice(&action.to_be_bytes());
    frame.extend_from_slice(operation.as_bytes());
    frame.extend_from_slice(&(body.len() as u32).to_be_bytes());
    frame.extend(body);
    Ok(frame)
}
pub(crate) fn decode(bytes: &[u8]) -> Result<Reply, Error> {
    if bytes.len() == 8 && &bytes[..4] == b"FCXE" {
        return Err(match number(bytes, 4) {
            1 => Error::Invalid("Invalid project command frame or limits"),
            2 => Error::Missing,
            3 => Error::Conflict("Project command request identity changed"),
            4 => Error::Busy,
            5 => Error::Denied,
            6 => Error::Conflict("Project command retention limit reached"),
            8 => Error::Conflict("Project command helper, volume or identity is unavailable"),
            _ => Error::Conflict("Project command outcome is unconfirmed"),
        });
    }
    if bytes.len() < 80
        || &bytes[..4] != b"FCXR"
        || number(bytes, 4) as usize != bytes.len() - 8
        || &bytes[8..12] != b"FCXS"
    {
        return Err(Error::Conflict("Invalid project command reply"));
    }
    let m = &bytes[8..80];
    let state = number(m, 4);
    let flags = number(m, 60);
    let out = number(m, 64) as usize;
    let err = number(m, 68) as usize;
    let boot = Uuid::from_slice(&m[40..56])
        .map_err(|_| Error::Conflict("Invalid command boot identity"))?;
    let exit = number(m, 56);
    if state > 3
        || number(m, 8) < 100000
        || boot.is_nil()
        || flags & !31 != 0
        || out > 65536
        || err > 65536
        || bytes.len() != 80 + out + err
        || (state == 1
            && (number(m, 16) < 2
                || number(m, 20) < 2
                || m[24..32] == [0; 8]
                || m[32..40] == [0; 8]))
        || (exit != u32::MAX && exit > 255)
        || (state < 2 && exit != u32::MAX)
        || (state == 2 && (exit == u32::MAX || flags & 1 != 0))
    {
        return Err(Error::Conflict(
            "Invalid project command process or output proof",
        ));
    }
    Ok(Reply {
        state,
        exit: (exit != u32::MAX).then_some(exit as i32),
        cleanup_required: flags & 1 != 0,
        truncated: flags & 6 != 0,
        stdout: bytes[80..80 + out].to_vec(),
        stderr: bytes[80 + out..].to_vec(),
    })
}
async fn exchange(ctx: &mut files::Files, frame: Vec<u8>) -> Result<Vec<u8>, Error> {
    ctx.permission()?;
    if ctx.digest.len() != 64 || !ctx.digest.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(Error::Conflict("Invalid project import identity"));
    }
    let b = &ctx.binding;
    let script = format!(
        "set -eu\nBB=\"\"; [ ! -x /etc/firecrab/busybox ] || BB=/etc/firecrab/busybox\n$BB base64 -d <<'FIRECRAB_PROJECT_COMMAND' | /usr/local/sbin/firecrab-console-resize --workspace-command {} {} {} {}\n{}\nFIRECRAB_PROJECT_COMMAND\n",
        b.project_id,
        b.database_id,
        b.volume_id,
        ctx.digest,
        STANDARD.encode(frame)
    );
    let response = guest::project_file_protocol(
        &ctx.state,
        &ctx.runtime,
        &ctx.db.directory,
        script.into_bytes(),
        80 + 2 * 65536,
    )
    .await?;
    ctx.permission()?;
    if binding(&ctx.state, b.project_id)? != *b
        || guest::binding(&ctx.state, b.vm_id).ok() != Some(ctx.runtime)
    {
        return Err(Error::Conflict(
            "Project command runtime changed during access",
        ));
    }
    if response.code != 0 {
        return Err(Error::Conflict(
            "Project command helper did not confirm its protocol",
        ));
    }
    Ok(response.stdout)
}
pub(crate) async fn probe(state: AppState, target: Target, id: Uuid) -> Result<(), AppError> {
    target.permission(&state, target.caller, id)?;
    files::run(state.workspaces.file_slots.clone(), id, async move {
        let mut ctx = files::Files::open(state, target.project, target.caller, true, id).await?;
        target.pinned(&mut ctx).map_err(|e| e.response(id))?;
        let bytes = exchange(
            &mut ctx,
            frame(target.project, 4, None).map_err(|e| e.response(id))?,
        )
        .await
        .map_err(|e| e.response(id))?;
        if bytes != b"FCXP\0\0\0\x04\0\0\0\x01" {
            return Err(Error::Conflict(
                "Install a runtime helper with project command protocol 1",
            )
            .response(id));
        }
        Ok(())
    })
    .await
}
/// Only observation/cancellation may cross a boot of the same verified binding.
/// Rebinding a volume to another VM/generation still requires the original fence to settle.
fn pin_receipt(
    target: &Target,
    current: &Binding,
    runtime: guest::Binding,
    action: u32,
) -> Result<(), Error> {
    target.check(current)?;
    if !(1..=3).contains(&action)
        || runtime.vm_id != target.runtime.vm_id
        || runtime.runtime_id.is_nil()
        || current.verified_runtime_id != Some(runtime.runtime_id)
        || (action == 1 && runtime != target.runtime)
    {
        return Err(Error::Conflict("The admitted command runtime changed"));
    }
    Ok(())
}
/// Start once, observe, or create an exact cancellation fence before a delayed launch arrives.
pub(crate) async fn request(
    state: AppState,
    target: Target,
    actor: Caller,
    operation: Uuid,
    command: SandboxGuestCommand,
    action: u32,
    id: Uuid,
) -> Result<Reply, AppError> {
    target.permission(&state, actor, id)?;
    let mut command = command;
    command.cwd = path(&target.directory, &command.cwd, true)
        .map_err(|_| Error::Invalid("Invalid command working directory").response(id))?;
    let frame = frame(operation, action, Some(&command)).map_err(|e| e.response(id))?;
    files::run(state.workspaces.file_slots.clone(), id, async move {
        let mut ctx = files::Files::open(state, target.project, actor, true, id).await?;
        ctx.require_execution = true;
        ctx.permission().map_err(|e| e.response(id))?;
        pin_receipt(&target, &ctx.binding, ctx.runtime, action).map_err(|e| e.response(id))?;
        let reply = decode(
            &exchange(&mut ctx, frame)
                .await
                .map_err(|e| e.response(id))?,
        )
        .map_err(|e| e.response(id))?;
        if reply.stdout.len() > command.limits.output_bytes
            || reply.stderr.len() > command.limits.output_bytes
        {
            return Err(Error::Conflict("Command output exceeds its admitted bound").response(id));
        }
        Ok(reply)
    })
    .await
}
#[cfg(test)]
mod tests;
