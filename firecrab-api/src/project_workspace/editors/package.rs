//! Only pinned, official OpenVSCode artifacts can enter a privileged guest bootstrap.
use super::super::db as project_db;
use super::*;
use futures_util::StreamExt;
use std::io::Read;
pub(super) const VERSION: &str = "1.109.5";
/// Select the exact official artifact and SHA-256 for the controller architecture.
pub(super) fn release() -> Result<(&'static str, &'static str), Error> {
    match std::env::consts::ARCH {
        "aarch64" => Ok((
            "arm64",
            "36d9c14036489b63de84ebace837fcacf7e60e669a0dc715802c5443684ea4dc",
        )),
        "x86_64" => Ok((
            "x64",
            "b433bf4f0227321a7014d8460d10a8f958adc0f45aa79bd889e84e65e8f88363",
        )),
        _ => Err(Error::Conflict(
            "Web editor requires a supported Linux x64 or arm64 runtime",
        )),
    }
}
/// Recover or download a bounded pinned package into private controller storage.
pub(super) async fn archive(state: &AppState) -> Result<PathBuf, Error> {
    let (arch, sha) = release()?;
    if let Some(path) = std::env::var_os("FIRECRAB_EDITOR_PACKAGE_ARCHIVE") {
        let path = PathBuf::from(path);
        validate(&path, sha)?;
        return Ok(path);
    }
    let root = state
        .workspaces
        .root
        .parent()
        .ok_or(Error::Invalid("Invalid private workspace root"))?
        .join("editor-cache");
    project_db::private_dir(&root)?;
    let name = format!("openvscode-server-v{VERSION}-linux-{arch}.tar.gz");
    let path = root.join(&name);
    if path.exists() {
        validate(&path, sha)?;
        return Ok(path);
    }
    let lock = project_db::private_file(&root.join("download.lock"), true)?;
    let _lock = project_db::OwnerLock::acquire(lock)?;
    if path.exists() {
        validate(&path, sha)?;
        return Ok(path);
    }
    let temporary = root.join(format!("{}.partial", Uuid::new_v4()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .custom_flags(libc::O_NOFOLLOW)
        .mode(0o600)
        .open(&temporary)?;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(180))
        .build()
        .map_err(|_| Error::Conflict("Cannot initialize the official editor download"))?;
    let result=async{
        let response=client.get(format!("https://github.com/gitpod-io/openvscode-server/releases/download/openvscode-server-v{VERSION}/{name}")).send().await
            .map_err(|_|Error::Conflict("Official editor download failed; stage the pinned archive for offline use"))?;
        if !response.status().is_success(){return Err(Error::Conflict("Official editor download was rejected"));}
        let mut stream=response.bytes_stream();let mut total=0usize;
        while let Some(chunk)=stream.next().await{let chunk=chunk.map_err(|_|Error::Conflict("Editor download was interrupted"))?;
            total=total.checked_add(chunk.len()).ok_or(Error::Invalid("Editor package is too large"))?;
            if total>128*1024*1024{return Err(Error::Invalid("Editor package is too large"));}file.write_all(&chunk)?;}
        file.sync_all()?;validate(&temporary,sha)?;fs::rename(&temporary,&path)?;File::open(&root)?.sync_all()?;Ok(path.clone())
    }.await;
    // Failed downloads are task-owned partial artifacts; never replace a retained workspace.
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}
/// Hash the opened regular archive and reject an unexpected official release.
fn validate(path: &std::path::Path, expected: &str) -> Result<(), Error> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    let stat = file.metadata()?;
    if !stat.is_file() || stat.len() > 128 * 1024 * 1024 {
        return Err(Error::Invalid("Invalid editor archive"));
    }
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    if format!("{:x}", hash.finalize()) != expected {
        return Err(Error::Conflict(
            "Official editor archive SHA-256 does not match its pinned release",
        ));
    }
    Ok(())
}
/// Construct a fixed retained package path without caller-supplied shell text.
pub(super) fn guest_package() -> Result<String, Error> {
    let (arch, _) = release()?;
    Ok(format!(
        "/workspace/shared/.firecrab-workspace/editor-package/openvscode-server-v{VERSION}-linux-{arch}"
    ))
}
/// Install the verified archive and trusted filesystem bridge after scope checks.
pub(super) async fn install(
    context: &mut files::Files,
    archive: &std::path::Path,
    editor: &Editor,
) -> Result<(), Error> {
    current(
        &context.state,
        editor.caller,
        &context.binding,
        editor,
        Uuid::new_v4(),
    )
    .map_err(|_| Error::Denied)?;
    let (_, sha) = release()?;
    let package = guest_package()?;
    let target = "/workspace/shared/.firecrab-workspace/editor-package";
    let probe = format!(
        "set -eu\ngetconf GNU_LIBC_VERSION >/dev/null\n[ ! -L {target} ]\ncase \"$(stat -c '%u:%a' {target})\" in 0:700|0:755) ;; *) exit 1 ;; esac\n[ ! -L {package} ]\n[ \"$(stat -c '%u:%a' {package})\" = 0:755 ]\n[ ! -L {package}/node ]\n[ \"$(stat -c '%u:%a' {package}/node)\" = 0:755 ]\n[ ! -L {package}/firecrab-archive-sha256 ]\n[ \"$(cat {package}/firecrab-archive-sha256)\" = {sha} ]\nchmod 755 {target}\nprintf ready\n"
    );
    let output = guest::project_editor_protocol(
        &context.state,
        &context.runtime,
        &context.db.directory,
        probe.into_bytes(),
        1024,
    )
    .await?;
    if output.code != 0 {
        let native = fs::read(archive)?;
        let nonce = Uuid::new_v4().simple().to_string();
        let script = format!(
            "set -eu\numask 077\ngetconf GNU_LIBC_VERSION >/dev/null\n[ ! -L {target} ]\nmkdir -p {target}\ncase \"$(stat -c '%u:%a' {target})\" in 0:700|0:755) ;; *) exit 1 ;; esac\nmkdir -m 700 {target}/stage-{nonce}\nbase64 -d <<'FIRECRAB_PINNED_EDITOR_ARCHIVE' > {target}/stage-{nonce}/archive.tar.gz\n{}\nFIRECRAB_PINNED_EDITOR_ARCHIVE\nprintf '{sha}  {target}/stage-{nonce}/archive.tar.gz\\n' | sha256sum -c - >/dev/null\ncd {target}/stage-{nonce}\numask 022\ntar --no-same-owner --no-same-permissions -xzf archive.tar.gz\nprintf {sha} > openvscode-server-v{VERSION}-linux-{}/firecrab-archive-sha256\n[ ! -e {package} ]\nmv openvscode-server-v{VERSION}-linux-{} {package}\nchmod 755 {target}\nprintf installed\n",
            STANDARD.encode(native),
            release()?.0,
            release()?.0
        );
        current(
            &context.state,
            editor.caller,
            &context.binding,
            editor,
            Uuid::new_v4(),
        )
        .map_err(|_| Error::Denied)?;
        let result = guest::project_editor_protocol(
            &context.state,
            &context.runtime,
            &context.db.directory,
            script.into_bytes(),
            1024,
        )
        .await?;
        if result.code != 0 {
            return Err(Error::Conflict(
                "Editor installation failed; use a compatible glibc guest and retain partial installation evidence",
            ));
        }
    }
    current(
        &context.state,
        editor.caller,
        &context.binding,
        editor,
        Uuid::new_v4(),
    )
    .map_err(|_| Error::Denied)?;
    let extension = format!("{package}/extensions/firecrab-project-workspace");
    let manifest = include_str!("editor/package.json");
    let bridge = include_str!("editor/extension.js");
    let bridge_sha = format!(
        "{:x}",
        Sha256::digest(format!("{manifest}{bridge}").as_bytes())
    );
    let temporary = format!(
        "{package}/extensions/.firecrab-stage-{}",
        Uuid::new_v4().simple()
    );
    // Publish once. A second editor must not truncate a running editor's browser code.
    let script = format!(
        "set -eu\numask 022\n[ ! -L {package}/extensions ]\n[ \"$(stat -c '%u:%a' {package}/extensions)\" = 0:755 ]\n[ ! -L {extension} ]\nif [ -e {extension} ]; then\n[ \"$(stat -c '%u:%a' {extension})\" = 0:755 ]\n[ ! -L {extension}/firecrab-bridge-sha256 ]\n[ \"$(cat {extension}/firecrab-bridge-sha256)\" = {bridge_sha} ]\nexit 0\nfi\nmkdir -m 755 {temporary}\nbase64 -d <<'FIRECRAB_EDITOR_MANIFEST' > {temporary}/package.json\n{}\nFIRECRAB_EDITOR_MANIFEST\nbase64 -d <<'FIRECRAB_EDITOR_EXTENSION' > {temporary}/extension.js\n{}\nFIRECRAB_EDITOR_EXTENSION\nprintf {bridge_sha} > {temporary}/firecrab-bridge-sha256\nsync\nmv {temporary} {extension}\n",
        STANDARD.encode(manifest),
        STANDARD.encode(bridge)
    );
    let result = guest::project_editor_protocol(
        &context.state,
        &context.runtime,
        &context.db.directory,
        script.into_bytes(),
        1024,
    )
    .await?;
    if result.code != 0 {
        return Err(Error::Conflict(
            "Cannot prepare the authorized editor filesystem extension",
        ));
    }
    Ok(())
}
