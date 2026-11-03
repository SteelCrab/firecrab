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
fn base_package() -> Result<String, Error> {
    let (arch, _) = release()?;
    Ok(format!(
        "/workspace/shared/.firecrab-workspace/editor-package/openvscode-server-v{VERSION}-linux-{arch}"
    ))
}
/// Bind new editors to immutable code without replacing a running editor's files.
pub(super) fn revision() -> Result<String, Error> {
    let (_, archive_sha) = release()?;
    Ok(bundle_revision(
        archive_sha,
        include_bytes!("editor/package.json"),
        include_bytes!("editor/extension.js"),
    ))
}
/// Length-delimited bytes keep the Rust and trusted Node publication identities equal.
fn bundle_revision(archive_sha: &str, manifest: &[u8], extension: &[u8]) -> String {
    let mut hash = Sha256::new();
    hash.update(b"firecrab-editor-bundle-v1\0");
    hash.update(archive_sha.as_bytes());
    for bytes in [manifest, extension] {
        hash.update((bytes.len() as u64).to_be_bytes());
        hash.update(bytes);
    }
    format!("{:x}", hash.finalize())
}
/// A legacy record keeps using its original package until its process ends.
pub(super) fn guest_package(revision: Option<&str>) -> Result<String, Error> {
    let Some(revision) = revision else {
        return base_package();
    };
    if revision.len() != 64
        || !revision
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(Error::Conflict("Invalid retained editor bundle revision"));
    }
    Ok(format!(
        "/workspace/shared/.firecrab-workspace/editor-package/bundles/{revision}"
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
    let package = base_package()?;
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
            "set -eu\numask 077\ngetconf GNU_LIBC_VERSION >/dev/null\n[ ! -L {target} ]\nmkdir -p {target}\ncase \"$(stat -c '%u:%a' {target})\" in 0:700|0:755) ;; *) exit 1 ;; esac\nmkdir -m 700 {target}/stage-{nonce}\nbase64 -d <<'FIRECRAB_PINNED_EDITOR_ARCHIVE' > {target}/stage-{nonce}/archive.tar.gz\n{}\nFIRECRAB_PINNED_EDITOR_ARCHIVE\nprintf '{sha}  {target}/stage-{nonce}/archive.tar.gz\\n' | sha256sum -c - >/dev/null\ncd {target}/stage-{nonce}\numask 022\ntar --no-same-owner --no-same-permissions -xzf archive.tar.gz\nprintf {sha} > openvscode-server-v{VERSION}-linux-{}/firecrab-archive-sha256\nsync\n[ ! -e {package} ]\nmv openvscode-server-v{VERSION}-linux-{} {package}\nchmod 755 {target}\nsync\nrm {target}/stage-{nonce}/archive.tar.gz\nrmdir {target}/stage-{nonce}\nprintf installed\n",
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
    let revision = revision()?;
    if editor.bundle_revision.as_deref() != Some(revision.as_str()) {
        return Err(Error::Conflict(
            "New editor launch requires the current immutable bundle",
        ));
    }
    let config = serde_json::json!({
        "base": package, "archiveSha": sha, "revision": revision,
        "manifest": STANDARD.encode(include_bytes!("editor/package.json")),
        "extension": STANDARD.encode(include_bytes!("editor/extension.js")),
        "nonce": Uuid::new_v4(),
    });
    let script = format!(
        "set -eu\nbase64 -d <<'FIRECRAB_EDITOR_BUNDLE' | {package}/node - {}\n{}\nFIRECRAB_EDITOR_BUNDLE\n",
        STANDARD.encode(serde_json::to_vec(&config)?),
        STANDARD.encode(include_str!("editor/bundle.cjs")),
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
            "Cannot publish the immutable editor filesystem bundle",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::bundle_revision;
    #[test]
    fn bundle_digest_matches_the_node_publication_contract() {
        assert_eq!(
            bundle_revision(&"a".repeat(64), b"{}", b"bridge"),
            "3736b00bf8f2964eaac9007f1559b25e76983450bb9e8da0d36ea93f0e93bddc"
        );
    }
}
