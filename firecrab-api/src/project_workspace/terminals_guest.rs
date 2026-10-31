//! Fixed binary PTY requests reuse pinned SSH; user input is never a root shell fragment.
use super::{terminals_db::Proof, *};

pub(super) struct Reply {
    /// Confirmed lifecycle state; recovery never replays a launch.
    pub status: ProjectTerminalStatus,
    /// Confirmed native kernel and process identity.
    pub proof: Proof,
    /// Earliest retained output byte cursor.
    pub start: u64,
    /// Exclusive output cursor already cached in SQLite.
    pub end: u64,
    /// Confirmed child exit status, absent if the process was interrupted.
    pub exit: Option<u32>,
    /// Whether earlier output was evicted by the explicit retention bound.
    pub truncated: bool,
    /// Whether process termination still needs native UID-scope reconciliation.
    pub cleanup_required: bool,
    /// Bounded output bytes returned by this native response.
    pub bytes: Vec<u8>,
    /// Last confirmed shared rows and columns.
    pub dimensions: Option<(u16, u16)>,
}
/// Decode a network-order field after the caller validates the frame length.
fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_be_bytes(
        bytes[offset..offset + 4]
            .try_into()
            .expect("checked frame size"),
    )
}
/// Decode a network-order cursor after frame bounds validation.
fn u64_at(bytes: &[u8], offset: usize) -> u64 {
    u64::from_be_bytes(
        bytes[offset..offset + 8]
            .try_into()
            .expect("checked frame size"),
    )
}
/// Encode only supported native actions and bounded terminal input.
pub(super) fn frame(
    id: Uuid,
    action: u32,
    cursor: u64,
    rows: u16,
    columns: u16,
    bytes: &[u8],
) -> Result<Vec<u8>, Error> {
    if id.is_nil()
        || !(1..=5).contains(&action)
        || cursor > i64::MAX as u64
        || bytes.len() > 16384
        || (action == 1 && bytes.len() > 1024)
        || (!matches!(action, 1 | 3) && !bytes.is_empty())
        || (matches!(action, 1 | 4)
            && (!(2..=500).contains(&rows) || !(2..=1000).contains(&columns)))
    {
        return Err(Error::Invalid(
            "Invalid terminal frame, dimensions or input size",
        ));
    }
    let mut result = Vec::with_capacity(40 + bytes.len());
    result.extend_from_slice(b"FCT1");
    result.extend_from_slice(&action.to_be_bytes());
    result.extend_from_slice(id.as_bytes());
    result.extend_from_slice(&cursor.to_be_bytes());
    result.extend_from_slice(&rows.to_be_bytes());
    result.extend_from_slice(&columns.to_be_bytes());
    result.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    result.extend_from_slice(bytes);
    Ok(result)
}
/// Validate native process identity, replay cursors and terminal dimensions.
pub(super) fn decode(bytes: &[u8]) -> Result<Reply, Error> {
    if bytes.len() < 8 {
        return Err(Error::Conflict("Guest terminal protocol is incomplete"));
    }
    if &bytes[..4] == b"FCTE" && bytes.len() == 8 {
        return Err(match u32_at(bytes, 4) {
            1 => Error::Invalid("Invalid terminal folder, frame or dimensions"),
            2 => Error::Missing,
            3 => Error::Conflict("Terminal creation or process identity changed"),
            4 => Error::Busy,
            5 => Error::Denied,
            6 => Error::Conflict("Project terminal retention limit reached"),
            8 => Error::Conflict("The retained terminal volume or project marker is not verified"),
            9 => Error::Conflict(
                "Terminal operation is uncertain; reconnect without replaying input",
            ),
            _ => Error::Conflict("Guest terminal protocol is unavailable"),
        });
    }
    if &bytes[..4] != b"FCTR"
        || bytes.len() < 104
        || bytes.len() > 104 + 32768
        || u32_at(bytes, 4) as usize != bytes.len() - 8
        || &bytes[8..12] != b"FCTS"
    {
        return Err(Error::Conflict("Invalid guest terminal response"));
    }
    let body = &bytes[8..];
    let status = match u32_at(body, 4) {
        1 => ProjectTerminalStatus::Running,
        2 => ProjectTerminalStatus::Exited,
        3 => ProjectTerminalStatus::Interrupted,
        _ => return Err(Error::Conflict("Terminal launch is not confirmed")),
    };
    let boot = Uuid::from_slice(&body[40..56])
        .map_err(|_| Error::Conflict("Invalid terminal boot identity"))?;
    let begin = u64_at(body, 56);
    let start = u64_at(body, 88);
    let end = u64_at(body, 64);
    let flags = u32_at(body, 76);
    if boot.is_nil()
        || u32_at(body, 8) < 100000
        || u32_at(body, 16) < 2
        || u32_at(body, 20) < 2
        || u64_at(body, 24) == 0
        || u64_at(body, 32) == 0
        || begin > start
        || start > end
        || end > i64::MAX as u64
        || body.len() - 96 > (end - start) as usize
        || flags & !3 != 0
    {
        return Err(Error::Conflict(
            "Invalid terminal process or replay identity",
        ));
    }
    let rows = u32_at(body, 80);
    let columns = u32_at(body, 84);
    let dimensions = if rows == 0 && columns == 0 {
        None
    } else {
        if !(2..=500).contains(&rows) || !(2..=1000).contains(&columns) {
            return Err(Error::Conflict("Invalid shared terminal dimensions"));
        }
        Some((rows as u16, columns as u16))
    };
    let exit = u32_at(body, 72);
    Ok(Reply {
        status,
        proof: Proof {
            boot,
            uid: u32_at(body, 8),
            supervisor: u32_at(body, 16),
            supervisor_ticks: u64_at(body, 24),
            child: u32_at(body, 20),
            child_ticks: u64_at(body, 32),
        },
        start,
        end,
        exit: (exit != u32::MAX).then_some(exit),
        truncated: flags & 1 != 0,
        cleanup_required: flags & 2 != 0,
        bytes: body[96..].to_vec(),
        dimensions,
    })
}
/// Send the fixed native protocol over a pinned project SSH connection.
pub(super) async fn request(context: &mut files::Files, frame: Vec<u8>) -> Result<Reply, Error> {
    context.permission()?;
    let digest = &context.digest;
    if digest.len() != 64 || !digest.bytes().all(|c| c.is_ascii_hexdigit()) {
        return Err(Error::Conflict("Invalid project import identity"));
    }
    let binding = &context.binding;
    let script = format!(
        "set -eu\nBB=\"\"; [ ! -x /etc/firecrab/busybox ] || BB=/etc/firecrab/busybox\n$BB base64 -d <<'FIRECRAB_PROJECT_TERMINAL_FRAME' | /usr/local/sbin/firecrab-console-resize --workspace-terminal {} {} {} {}\n{}\nFIRECRAB_PROJECT_TERMINAL_FRAME\n",
        binding.project_id,
        binding.database_id,
        binding.volume_id,
        digest,
        STANDARD.encode(frame)
    );
    let output = guest::project_file_protocol(
        &context.state,
        &context.runtime,
        &context.db.directory,
        script.into_bytes(),
        104 + 32768,
    )
    .await?;
    context.permission()?;
    if super::binding(&context.state, binding.project_id)? != *binding
        || guest::binding(&context.state, binding.vm_id).ok() != Some(context.runtime)
    {
        return Err(Error::Conflict(
            "Project runtime changed during terminal access",
        ));
    }
    if output.code != 0 {
        return Err(Error::Conflict("The guest terminal helper is unavailable"));
    }
    decode(&output.stdout)
}
