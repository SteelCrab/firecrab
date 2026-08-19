//! The Rust half of `scripts/firecrab-release.sh`: the same arch/libc
//! detection, asset naming, URL assembly and `SHA256SUMS` parsing rules,
//! re-implemented rather than shelled out to so `firecrab update` needs no
//! bash payload on the host.

use std::io::Read;
use std::path::Path;
use std::time::Duration;

use sha2::{Digest, Sha256};

use super::UpdateError;

/// `firecrab_release_repo`'s default.
pub const DEFAULT_RELEASE_REPO: &str = "SteelCrab/firecrab";
/// Connect + transfer budget for one asset. Generous because a host bundle is
/// tens of megabytes; the connect half fails fast on an unreachable host.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(1800);
/// How long to wait for the TCP/TLS handshake alone.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// `FIRECRAB_RELEASE_REPO`, else `SteelCrab/firecrab` (`firecrab_release_repo`).
pub fn release_repo() -> String {
    std::env::var("FIRECRAB_RELEASE_REPO")
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_RELEASE_REPO.to_owned())
}

/// `FIRECRAB_RELEASE_BASE`, else GitHub's releases root for
/// [`release_repo`] (`firecrab_release_base`).
pub fn release_base() -> String {
    std::env::var("FIRECRAB_RELEASE_BASE")
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| format!("https://github.com/{}/releases", release_repo()))
}

/// This host's release architecture (`firecrab_host_arch`).
pub fn host_arch() -> Result<&'static str, UpdateError> {
    match std::env::consts::ARCH {
        "x86_64" => Ok("x86_64"),
        "aarch64" => Ok("aarch64"),
        other => Err(UpdateError::UnsupportedArch(other.to_owned())),
    }
}

/// `firecrab_normalize_libc`: only `gnu`/`glibc`/`musl` are accepted.
pub fn normalize_libc(value: &str) -> Result<&'static str, UpdateError> {
    match value {
        "gnu" | "glibc" => Ok("gnu"),
        "musl" => Ok("musl"),
        other => Err(UpdateError::UnsupportedLibc(other.to_owned())),
    }
}

/// The libc flavour of the bundle this host should install: an explicit
/// override, else `FIRECRAB_LIBC`, else this binary's own compile target.
///
/// Unlike `firecrab_host_libc` this does **not** probe the filesystem for
/// `/lib/ld-musl-*.so.1`. It does not need to: release bundles are built per
/// libc, so the CLI currently running was itself installed from the bundle
/// this host uses, and its compile target is that bundle's libc by definition.
pub fn host_libc(override_value: Option<&str>) -> Result<&'static str, UpdateError> {
    if let Some(value) = override_value {
        return normalize_libc(value);
    }
    if let Ok(value) = std::env::var("FIRECRAB_LIBC")
        && !value.is_empty()
    {
        return normalize_libc(&value);
    }
    Ok(if cfg!(target_env = "musl") {
        "musl"
    } else {
        "gnu"
    })
}

/// `firecrab_host_tarball`'s asset name.
pub fn host_tarball(arch: &str, libc: &str) -> String {
    format!("firecrab-host-{arch}-{libc}.tar.gz")
}

/// `firecrab_release_asset_url`'s tagged form. The `latest/download/` shape is
/// deliberately unused: the tag is already known from the check, so the
/// download gets exactly the release that was checked even if a newer one is
/// published in between.
pub fn asset_url(base: &str, tag: &str, asset: &str) -> String {
    format!("{}/download/{tag}/{asset}", base.trim_end_matches('/'))
}

/// `firecrab_verify_sha256`'s awk lookup: the published hash for `asset_name`,
/// accepting the plain, binary-mode (`*name`) and path-prefixed (`./name`)
/// spellings. The last one is not optional — the release workflow builds
/// `SHA256SUMS` with `(cd dist && sha256sum ./*)`, so every real name is
/// `./firecrab-host-...`.
pub fn expected_sha256(sums_text: &str, asset_name: &str) -> Option<String> {
    let suffix = format!("/{asset_name}");
    sums_text.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        let hash = fields.next()?;
        let name = fields.next()?;
        let name = name.strip_prefix('*').unwrap_or(name);
        (name == asset_name || name.ends_with(&suffix)).then(|| hash.to_owned())
    })
}

/// Streams `url` into `dest`, removing a partial file if anything fails.
pub fn download_to(url: &str, dest: &Path) -> Result<(), UpdateError> {
    let fail = |detail: String| UpdateError::Download {
        url: url.to_owned(),
        detail,
    };
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(DOWNLOAD_TIMEOUT)
        .build()
        .expect("reqwest client build");
    let mut response = client.get(url).send().map_err(|e| fail(e.to_string()))?;
    let status = response.status();
    if !status.is_success() {
        return Err(fail(format!("HTTP {}", status.as_u16())));
    }
    let mut file = std::fs::File::create(dest).map_err(|e| fail(e.to_string()))?;
    if let Err(error) = response.copy_to(&mut file) {
        drop(file);
        let _ = std::fs::remove_file(dest);
        return Err(fail(error.to_string()));
    }
    Ok(())
}

/// Lowercase hex SHA-256 of a file on disk.
pub fn file_sha256(path: &Path) -> Result<String, UpdateError> {
    let mut file = std::fs::File::open(path).map_err(|error| UpdateError::Download {
        url: path.display().to_string(),
        detail: error.to_string(),
    })?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| UpdateError::Download {
                url: path.display().to_string(),
                detail: error.to_string(),
            })?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::super::ENV_LOCK;
    use super::*;

    /// Exactly what `.github/workflows/release.yml:300`'s
    /// `(cd dist && sha256sum ./* > SHA256SUMS)` produces: every name carries
    /// a `./` prefix, so plain equality would never match.
    const SUMS: &str = "\
be3c6071577be45dcc1c1f56fc1cc57360cdfe575357996b798c2bc017bcaeba  ./firecrab-host-x86_64-gnu.tar.gz
e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855  ./firecrab-host-aarch64-musl.tar.gz
d6a32738d876fc3bd42d42560afaacb6e1e2674434a5f514f89a491eed292c6b  ./install.sh
";

    #[test]
    fn host_arch_accepts_only_the_two_release_arches() {
        // The running binary is one of the two the release builds, so this
        // must resolve on any host CI runs on.
        let arch = host_arch().expect("a supported architecture");
        assert!(arch == "x86_64" || arch == "aarch64", "{arch}");
    }

    #[test]
    fn release_repo_defaults_to_steelcrab_firecrab() {
        let _guard = ENV_LOCK.lock().unwrap();
        // SAFETY: serialized by ENV_LOCK — see the note on its declaration.
        unsafe { std::env::remove_var("FIRECRAB_RELEASE_REPO") };
        assert_eq!(release_repo(), DEFAULT_RELEASE_REPO);
    }

    #[test]
    fn release_repo_reads_env_override() {
        let _guard = ENV_LOCK.lock().unwrap();
        // SAFETY: serialized by ENV_LOCK — see the note on its declaration.
        unsafe { std::env::set_var("FIRECRAB_RELEASE_REPO", "acme/fork") };
        let result = release_repo();
        unsafe { std::env::remove_var("FIRECRAB_RELEASE_REPO") };
        assert_eq!(result, "acme/fork");
    }

    #[test]
    fn release_base_defaults_to_github_releases_for_release_repo() {
        let _guard = ENV_LOCK.lock().unwrap();
        // SAFETY: serialized by ENV_LOCK — see the note on its declaration.
        // Both vars matter here: an unrelated FIRECRAB_RELEASE_REPO left set
        // would change the default this derives.
        unsafe { std::env::remove_var("FIRECRAB_RELEASE_BASE") };
        unsafe { std::env::remove_var("FIRECRAB_RELEASE_REPO") };
        assert_eq!(
            release_base(),
            "https://github.com/SteelCrab/firecrab/releases"
        );
    }

    #[test]
    fn release_base_reads_env_override() {
        let _guard = ENV_LOCK.lock().unwrap();
        // SAFETY: serialized by ENV_LOCK — see the note on its declaration.
        unsafe { std::env::set_var("FIRECRAB_RELEASE_BASE", "https://example.test/releases") };
        let result = release_base();
        unsafe { std::env::remove_var("FIRECRAB_RELEASE_BASE") };
        assert_eq!(result, "https://example.test/releases");
    }

    #[test]
    fn normalize_libc_rejects_unknown_values() {
        assert_eq!(normalize_libc("gnu").unwrap(), "gnu");
        assert_eq!(normalize_libc("glibc").unwrap(), "gnu");
        assert_eq!(normalize_libc("musl").unwrap(), "musl");
        for bad in ["", "uclibc", "GNU", "musl-1.2"] {
            assert!(
                matches!(normalize_libc(bad), Err(UpdateError::UnsupportedLibc(_))),
                "{bad} should not be accepted"
            );
        }
    }

    #[test]
    fn host_libc_override_wins_regardless_of_env() {
        // The override branch returns before ever reading FIRECRAB_LIBC, so
        // this needs no ENV_LOCK: it can't race with the env-touching tests
        // below no matter what value they leave behind.
        assert_eq!(host_libc(Some("musl")).unwrap(), "musl");
        // normalize_libc's aliasing applies here too.
        assert_eq!(host_libc(Some("glibc")).unwrap(), "gnu");
    }

    #[test]
    fn host_libc_reads_env_var_when_no_override() {
        let _guard = ENV_LOCK.lock().unwrap();
        // SAFETY: serialized by ENV_LOCK — see the note on its declaration.
        unsafe { std::env::set_var("FIRECRAB_LIBC", "musl") };
        let result = host_libc(None);
        unsafe { std::env::remove_var("FIRECRAB_LIBC") };
        assert_eq!(result.unwrap(), "musl");
    }

    #[test]
    fn host_libc_falls_back_to_compile_target_when_unset() {
        let _guard = ENV_LOCK.lock().unwrap();
        // SAFETY: serialized by ENV_LOCK — see the note on its declaration.
        unsafe { std::env::remove_var("FIRECRAB_LIBC") };
        let expected = if cfg!(target_env = "musl") {
            "musl"
        } else {
            "gnu"
        };
        assert_eq!(host_libc(None).unwrap(), expected);
    }

    #[test]
    fn host_tarball_matches_install_sh_naming() {
        for arch in ["x86_64", "aarch64"] {
            for libc in ["gnu", "musl"] {
                assert_eq!(
                    host_tarball(arch, libc),
                    format!("firecrab-host-{arch}-{libc}.tar.gz")
                );
            }
        }
    }

    #[test]
    fn asset_url_matches_the_release_script() {
        // scripts/firecrab-release.sh's firecrab_release_asset_url, tagged
        // branch: "{base}/download/{tag}/{asset}".
        assert_eq!(
            asset_url(
                "https://github.com/SteelCrab/firecrab/releases",
                "v0.1.2",
                "firecrab-host-x86_64-gnu.tar.gz"
            ),
            "https://github.com/SteelCrab/firecrab/releases/download/v0.1.2/firecrab-host-x86_64-gnu.tar.gz"
        );
    }

    #[test]
    fn expected_sha256_handles_the_dot_slash_prefix_sha256sum_writes() {
        assert_eq!(
            expected_sha256(SUMS, "firecrab-host-x86_64-gnu.tar.gz").as_deref(),
            Some("be3c6071577be45dcc1c1f56fc1cc57360cdfe575357996b798c2bc017bcaeba")
        );
        // Plain and binary-mode (`*name`) spellings must work too — awk in
        // firecrab_verify_sha256 accepts all three.
        let plain = "aa11  firecrab-host-x86_64-gnu.tar.gz\n";
        assert_eq!(
            expected_sha256(plain, "firecrab-host-x86_64-gnu.tar.gz").as_deref(),
            Some("aa11")
        );
        let binary = "bb22 *firecrab-host-x86_64-gnu.tar.gz\n";
        assert_eq!(
            expected_sha256(binary, "firecrab-host-x86_64-gnu.tar.gz").as_deref(),
            Some("bb22")
        );
    }

    #[test]
    fn expected_sha256_returns_none_for_an_unlisted_asset() {
        assert_eq!(
            expected_sha256(SUMS, "firecrab-host-riscv64-gnu.tar.gz"),
            None
        );
        assert_eq!(expected_sha256("", "anything"), None);
    }

    #[test]
    fn file_sha256_matches_a_known_digest() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("payload");
        std::fs::write(&path, b"firecrab").expect("write");
        assert_eq!(
            file_sha256(&path).expect("hash"),
            "be3c6071577be45dcc1c1f56fc1cc57360cdfe575357996b798c2bc017bcaeba"
        );
    }

    #[test]
    fn download_to_reports_the_url_it_could_not_reach() {
        // Port 1 is reserved and never listening — refused immediately, well
        // inside the client timeout (same trick as api_client.rs's
        // unreachable_client_returns_unreachable_error).
        let dir = tempfile::tempdir().expect("tempdir");
        let dest = dir.path().join("bundle.tar.gz");
        let error = download_to("http://127.0.0.1:1/bundle.tar.gz", &dest).unwrap_err();
        assert!(
            matches!(&error, UpdateError::Download { url, .. } if url.contains("127.0.0.1:1")),
            "{error}"
        );
        assert!(
            !dest.exists(),
            "a failed download must not leave a partial file"
        );
    }

    /// Minimal one-shot HTTP/1.1 listener: accepts a single connection,
    /// discards whatever it read, then writes a fixed 200 response carrying
    /// `body`. `firecrab-cli` has no async runtime (`download_to` is
    /// `reqwest::blocking`-only), so this uses plain `std::net` rather than
    /// pulling in axum/tokio just for one test — reqwest only needs a peer
    /// that speaks HTTP, not a real server.
    fn serve_once(body: &'static [u8]) -> (String, std::thread::JoinHandle<()>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind test listener");
        let addr = listener.local_addr().expect("listener addr");
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept test connection");
            // Drain the request so the client isn't left waiting on us to
            // read before we respond; the request itself is never parsed.
            let mut buf = [0_u8; 1024];
            let _ = stream.read(&mut buf);
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream
                .write_all(header.as_bytes())
                .expect("write response header");
            stream.write_all(body).expect("write response body");
            stream.flush().expect("flush response");
        });
        (format!("http://{addr}"), handle)
    }

    #[test]
    fn download_to_writes_the_response_body_to_dest() {
        let body: &[u8] = b"firecrab-bundle-bytes";
        let (base, handle) = serve_once(body);
        let dir = tempfile::tempdir().expect("tempdir");
        let dest = dir.path().join("bundle.tar.gz");
        download_to(&format!("{base}/bundle.tar.gz"), &dest).expect("download succeeds");
        assert_eq!(std::fs::read(&dest).expect("read downloaded file"), body);
        handle.join().expect("server thread panicked");
    }
}
