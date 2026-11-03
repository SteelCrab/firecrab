//! Fixed binary frames; user-controlled paths and bytes never become shell syntax.
use super::{Binding, Error, db::ProjectDb, files_db::Operation};
use crate::{ai_sandbox::guest, state::AppState};
use base64::{Engine, engine::general_purpose::STANDARD};
use firecrab_api_types::ProjectFileEntry;
use uuid::Uuid;

pub(super) const FILE_LIMIT: usize = 1024 * 1024;
const PATH_LIMIT: usize = 1024;
const FRAME_LIMIT: usize = 36 + PATH_LIMIT + 2 * FILE_LIMIT;

pub(super) enum Reply {
    Data(Vec<u8>),
    Rejected(u32),
}

pub(super) fn path(value: &str, root_allowed: bool) -> Result<(), Error> {
    if value.is_empty() && root_allowed {
        return Ok(());
    }
    if value.is_empty()
        || value.len() > PATH_LIMIT
        || value.contains('\\')
        || value.chars().any(char::is_control)
    {
        return Err(Error::Invalid("Use a relative workspace path"));
    }
    let mut count = 0;
    for part in value.split('/') {
        count += 1;
        if count > 32
            || part.is_empty()
            || part.len() > 255
            || matches!(part, "." | ".." | "lost+found")
            || part.starts_with(".firecrab-")
        {
            return Err(Error::Invalid("Path is outside the project or is reserved"));
        }
    }
    Ok(())
}

pub(super) fn frame(
    action: u32,
    path: &str,
    id: Uuid,
    base: Option<&[u8]>,
    bytes: &[u8],
) -> Result<Vec<u8>, Error> {
    self::path(path, matches!(action, 1 | 4))?;
    if !(1..=4).contains(&action)
        || bytes.len() > FILE_LIMIT
        || base.is_some_and(|b| b.len() > FILE_LIMIT)
    {
        return Err(Error::Invalid("Invalid project file frame"));
    }
    let base_length = if action == 3 {
        base.map_or(u32::MAX, |v| v.len() as u32)
    } else {
        0
    };
    let mut result =
        Vec::with_capacity(36 + path.len() + base.map_or(0, <[u8]>::len) + bytes.len());
    result.extend_from_slice(b"FCW1");
    for value in [action, path.len() as u32, base_length, bytes.len() as u32] {
        result.extend_from_slice(&value.to_be_bytes());
    }
    result.extend_from_slice(id.as_bytes());
    result.extend_from_slice(path.as_bytes());
    if action == 3
        && let Some(base) = base
    {
        result.extend_from_slice(base);
    }
    result.extend_from_slice(bytes);
    Ok(result)
}

pub(super) fn operation_frame(db: &ProjectDb, op: &Operation) -> Result<Vec<u8>, Error> {
    let base = if op.expected == 0 {
        None
    } else {
        Some(db.file_version(&op.path, op.expected)?.bytes)
    };
    frame(3, &op.path, op.id, base.as_deref(), &op.content)
}

pub(super) fn decode(bytes: Vec<u8>) -> Result<Reply, Error> {
    if bytes.len() < 8 {
        return Err(Error::Conflict(
            "Guest file protocol is unavailable or incomplete",
        ));
    }
    let number = u32::from_be_bytes(bytes[4..8].try_into().expect("four bytes"));
    match &bytes[..4] {
        b"FCWR" if number as usize == bytes.len() - 8 && number as usize <= FRAME_LIMIT => {
            Ok(Reply::Data(bytes[8..].to_vec()))
        }
        b"FCWE" if bytes.len() == 8 && (1..=9).contains(&number) => Ok(Reply::Rejected(number)),
        _ => Err(Error::Conflict(
            "Guest file protocol returned an invalid response",
        )),
    }
}

pub(super) fn rejected(code: u32) -> Error {
    match code {
        1 => Error::Invalid("Use a valid relative workspace path"),
        2 => Error::Missing,
        3 => {
            Error::Conflict("The file changed; reload before saving with a new operation identity")
        }
        4 => Error::Busy,
        5 => {
            Error::Invalid("Workspace links, special files and private metadata are not accessible")
        }
        6 => Error::Conflict("The file or retained history exceeds its limit"),
        8 => Error::Conflict("The selected guest mount or project identity could not be verified"),
        9 => Error::Conflict("The file write is uncertain; reconcile the same operation identity"),
        _ => Error::Conflict(
            "Guest file operation failed; retain the project and inspect its runtime",
        ),
    }
}

pub(super) fn entries(mut bytes: &[u8]) -> Result<Vec<ProjectFileEntry>, Error> {
    let mut result = Vec::new();
    while !bytes.is_empty() {
        if bytes.len() < 5 || result.len() >= 4096 {
            return Err(Error::Conflict("Invalid guest directory response"));
        }
        let directory = match bytes[0] {
            b'd' => true,
            b'f' => false,
            _ => return Err(Error::Conflict("Invalid guest file type")),
        };
        let size = u32::from_be_bytes(bytes[1..5].try_into().expect("four bytes")) as usize;
        if size == 0 || size > 255 || bytes.len() < 5 + size {
            return Err(Error::Conflict("Invalid guest file name"));
        }
        if let Ok(name) = std::str::from_utf8(&bytes[5..5 + size])
            && !name.contains('/')
        {
            self::path(name, false)?;
            result.push(ProjectFileEntry {
                name: name.into(),
                directory,
            });
        }
        bytes = &bytes[5 + size..];
    }
    result.sort_by(|a, b| {
        b.directory
            .cmp(&a.directory)
            .then_with(|| a.name.cmp(&b.name))
    });
    Ok(result)
}

pub(super) async fn request(
    state: &AppState,
    binding: &Binding,
    runtime: &guest::Binding,
    directory: &std::path::Path,
    digest: &str,
    frame: Vec<u8>,
) -> Result<Reply, Error> {
    if digest.len() != 64 || !digest.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(Error::Conflict("Project import identity is invalid"));
    }
    let command = format!(
        "set -eu\nBB=\"\"; [ ! -x /etc/firecrab/busybox ] || BB=/etc/firecrab/busybox\n$BB base64 -d <<'FIRECRAB_PROJECT_FILE_FRAME' | /usr/local/sbin/firecrab-console-resize --workspace-files {} {} {} {}\n{}\nFIRECRAB_PROJECT_FILE_FRAME\n",
        binding.project_id,
        binding.database_id,
        binding.volume_id,
        digest,
        STANDARD.encode(frame)
    );
    let output = guest::project_file_protocol(
        state,
        runtime,
        directory,
        command.into_bytes(),
        FRAME_LIMIT + 8,
    )
    .await?;
    if output.code != 0 {
        tracing::warn!(vm_id=%binding.vm_id,code=output.code,"Trusted project file protocol unavailable");
        return Err(Error::Conflict(
            "Project file protocol is unavailable; verify the guest helper and runtime",
        ));
    }
    decode(output.stdout)
}
