//! The Firecrab release a managed guest installs.
//!
//! Debian and Firecracker are pinned in this source; Firecrab is not. A pin
//! would provision every new guest with a release older than the CLI that
//! created it. The tag comes from GitHub's `releases/latest`, and each asset is
//! verified against the `SHA256SUMS` that release publishes, as `firecrab
//! update` does for the host bundle. That command only exists on Linux, so the
//! lookup is repeated here.

use std::borrow::Cow;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

use super::artifact::{ArtifactSpec, HashAlgorithm};
use super::report;

const LATEST_API: &str = "https://api.github.com/repos/SteelCrab/firecrab/releases/latest";
const RELEASES: &str = "https://github.com/SteelCrab/firecrab/releases";
const SUMS: &str = "SHA256SUMS";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// The release's guest installer, and the name it is kept under in the
/// downloads directory so the provisioning script need not know the version.
pub const INSTALLER_ASSET: &str = "install.sh";
pub const INSTALLER_FILE: &str = "install-firecrab.sh";

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("could not fetch {url}: {detail}")]
    Fetch { url: String, detail: String },
    #[error("the latest Firecrab release is tagged {0:?}, not vMAJOR.MINOR.PATCH")]
    UnexpectedTag(String),
    #[error("release {tag} publishes no SHA-256 for {asset}")]
    Unlisted { tag: String, asset: String },
    #[error("could not record {path}: {source}")]
    Record {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// The newest published release and the digests it lists.
#[derive(Debug)]
pub struct Release {
    tag: String,
    base: String,
    sums: String,
}

impl Release {
    pub fn latest() -> Result<Self, Error> {
        let release = Self::resolve(LATEST_API, RELEASES)?;
        report!("[PASS] Firecrab release: {} is the latest", release.tag);
        Ok(release)
    }

    fn resolve(api_url: &str, base: &str) -> Result<Self, Error> {
        let tag = latest_tag(api_url)?;
        if !is_release_tag(&tag) {
            return Err(Error::UnexpectedTag(tag));
        }
        let sums_url = asset_url(base, &tag, SUMS);
        let sums = fetch(&sums_url)?
            .text()
            .map_err(|error| fetch_error(&sums_url, error))?;
        Ok(Self {
            tag,
            base: base.to_owned(),
            sums,
        })
    }

    pub fn tag(&self) -> &str {
        &self.tag
    }

    /// `asset` of this release, stored as `filename` once downloaded and
    /// verified against the digest the release lists for it.
    pub fn artifact(
        &self,
        label: &'static str,
        asset: &str,
        filename: &'static str,
    ) -> Result<ArtifactSpec, Error> {
        let digest = listed_digest(&self.sums, asset).ok_or_else(|| Error::Unlisted {
            tag: self.tag.clone(),
            asset: asset.to_owned(),
        })?;
        Ok(ArtifactSpec {
            label,
            filename: Cow::Borrowed(filename),
            url: Cow::Owned(asset_url(&self.base, &self.tag, asset)),
            algorithm: HashAlgorithm::Sha256,
            digest: Cow::Owned(digest),
        })
    }

    pub fn installer(&self) -> Result<ArtifactSpec, Error> {
        self.artifact("Firecrab guest installer", INSTALLER_ASSET, INSTALLER_FILE)
    }

    /// Keeps the digests the downloads were verified against, so they can be
    /// re-hashed later without asking GitHub which release was the latest.
    pub fn record(&self, directory: &Path) -> Result<(), Error> {
        let path = directory.join(SUMS);
        fs::write(&path, &self.sums).map_err(|source| Error::Record { path, source })
    }
}

/// The digest [`Release::record`] kept for `asset`.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub fn recorded_digest(directory: &Path, asset: &str) -> Option<String> {
    listed_digest(&fs::read_to_string(directory.join(SUMS)).ok()?, asset)
}

#[derive(Deserialize)]
struct Latest {
    tag_name: String,
}

fn latest_tag(api_url: &str) -> Result<String, Error> {
    fetch(api_url)?
        .json::<Latest>()
        .map(|latest| latest.tag_name)
        .map_err(|error| fetch_error(api_url, error))
}

fn asset_url(base: &str, tag: &str, asset: &str) -> String {
    format!("{base}/download/{tag}/{asset}")
}

/// `vMAJOR.MINOR.PATCH` and nothing else, because the tag becomes part of a
/// download URL.
fn is_release_tag(tag: &str) -> bool {
    let Some(version) = tag.strip_prefix('v') else {
        return false;
    };
    let parts: Vec<&str> = version.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
}

/// The digest `sha256sum` wrote for `asset`: a name that may carry a `*`
/// (binary mode) or `./` prefix, which is how the release workflow writes them.
fn listed_digest(sums: &str, asset: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let (digest, name) = line.split_once(char::is_whitespace)?;
        let name = name.trim_start();
        let name = name.strip_prefix('*').unwrap_or(name);
        let name = name.strip_prefix("./").unwrap_or(name);
        (name == asset && is_sha256(digest)).then(|| digest.to_ascii_lowercase())
    })
}

fn is_sha256(digest: &str) -> bool {
    digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn fetch(url: &str) -> Result<reqwest::blocking::Response, Error> {
    let response = reqwest::blocking::Client::builder()
        // GitHub's API answers 403 to a request without one.
        .user_agent(concat!("firecrab-micromanager/", env!("CARGO_PKG_VERSION")))
        .timeout(REQUEST_TIMEOUT)
        .build()
        .and_then(|client| client.get(url).send())
        .map_err(|error| fetch_error(url, error))?;
    let status = response.status();
    if !status.is_success() {
        return Err(Error::Fetch {
            url: url.to_owned(),
            detail: format!("HTTP {}", status.as_u16()),
        });
    }
    Ok(response)
}

fn fetch_error(url: &str, error: reqwest::Error) -> Error {
    Error::Fetch {
        url: url.to_owned(),
        detail: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    use super::*;

    const INSTALLER_SHA256: &str =
        "af2fb56b92dff1559cdaa449aa025808e0990437c16ccc0bb2b4d558ef50dacb";
    const BUNDLE_SHA256: &str = "2e995ed27d8c19848baaf38c8bf8a2d5d126ab90776738d91790524a8564e982";

    fn sums() -> String {
        format!(
            "{BUNDLE_SHA256}  ./firecrab-host-aarch64-gnu.tar.gz\n{INSTALLER_SHA256}  ./install.sh\n"
        )
    }

    fn release(sums: &str) -> Release {
        Release {
            tag: "v0.3.1".to_owned(),
            base: "https://example.invalid/releases".to_owned(),
            sums: sums.to_owned(),
        }
    }

    /// Answers each connection with the body registered for its path, or a 404,
    /// and returns the address it listens on.
    fn serve(routes: Vec<(&'static str, String)>) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("test listener binds");
        let address = format!("http://{}", listener.local_addr().expect("bound address"));
        let handle = thread::spawn(move || {
            for _ in 0..routes.len() {
                let (mut stream, _) = listener.accept().expect("client connects");
                let mut buffer = [0_u8; 4096];
                let read = stream.read(&mut buffer).expect("request is readable");
                let request = String::from_utf8_lossy(&buffer[..read]);
                let path = request.split_whitespace().nth(1).unwrap_or_default();
                let response = match routes.iter().find(|(route, _)| *route == path) {
                    Some((_, body)) => format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    ),
                    None => {
                        "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                            .to_owned()
                    }
                };
                stream
                    .write_all(response.as_bytes())
                    .expect("response is written");
            }
        });
        (address, handle)
    }

    #[test]
    fn the_latest_release_supplies_the_tag_and_its_digests() {
        let (server, handle) = serve(vec![
            ("/api", r#"{"tag_name":"v0.3.1","draft":false}"#.to_owned()),
            ("/releases/download/v0.3.1/SHA256SUMS", sums()),
        ]);
        let release = Release::resolve(&format!("{server}/api"), &format!("{server}/releases"))
            .expect("the release resolves");
        handle.join().expect("server finishes");

        assert_eq!(release.tag(), "v0.3.1");
        let installer = release.installer().expect("the installer is listed");
        assert_eq!(
            installer.url,
            format!("{server}/releases/download/v0.3.1/install.sh")
        );
        assert_eq!(installer.filename, INSTALLER_FILE);
        assert_eq!(installer.algorithm, HashAlgorithm::Sha256);
        assert_eq!(installer.digest, INSTALLER_SHA256);

        let bundle = release
            .artifact(
                "bundle",
                "firecrab-host-aarch64-gnu.tar.gz",
                "firecrab-host-aarch64-gnu.tar.gz",
            )
            .expect("the bundle is listed");
        assert_eq!(bundle.digest, BUNDLE_SHA256);
    }

    #[test]
    fn a_tag_that_is_not_a_plain_version_is_refused() {
        for tag in [
            "../../evil/releases/download/v1.0.0",
            "latest",
            "v0.3",
            "0.3.1",
            "v0.3.1-rc1",
            "v0.3.1/x",
            "v0..1",
            "",
        ] {
            assert!(!is_release_tag(tag), "{tag:?}");
        }
        assert!(is_release_tag("v0.3.1"));
        assert!(is_release_tag("v10.20.300"));

        let (server, handle) = serve(vec![("/api", r#"{"tag_name":"v1.0.0/../x"}"#.to_owned())]);
        let error = Release::resolve(&format!("{server}/api"), &server)
            .expect_err("the tag would escape the release path");
        handle.join().expect("server finishes");
        assert!(matches!(error, Error::UnexpectedTag(tag) if tag == "v1.0.0/../x"));
    }

    #[test]
    fn an_unreachable_release_api_is_reported_with_its_url() {
        let url = "http://127.0.0.1:1/api";
        let error = latest_tag(url).expect_err("nothing listens on port 1");
        assert!(matches!(error, Error::Fetch { url: failed, .. } if failed == url));
    }

    #[test]
    fn an_http_error_status_is_reported() {
        let (server, handle) = serve(vec![("/other", String::new())]);
        let error = latest_tag(&format!("{server}/api")).expect_err("the API answers 404");
        handle.join().expect("server finishes");
        assert!(
            matches!(&error, Error::Fetch { detail, .. } if detail == "HTTP 404"),
            "{error}"
        );
    }

    #[test]
    fn an_asset_the_release_does_not_list_is_refused() {
        let release = release(&sums());
        for asset in [
            "firecrab-host-x86_64-gnu.tar.gz",
            "install.sh.sig",
            "stall.sh",
        ] {
            let error = release
                .artifact("asset", asset, "asset")
                .expect_err("only exact names match");
            assert!(
                matches!(&error, Error::Unlisted { tag, asset: named } if tag == "v0.3.1" && named == asset),
                "{asset}: {error}"
            );
        }
    }

    #[test]
    fn digests_are_read_as_sha256sum_writes_them() {
        let upper = INSTALLER_SHA256.to_ascii_uppercase();
        for line in [
            format!("{INSTALLER_SHA256}  install.sh"),
            format!("{INSTALLER_SHA256} *install.sh"),
            format!("{INSTALLER_SHA256}  ./install.sh"),
            format!("{upper}  install.sh"),
        ] {
            assert_eq!(
                listed_digest(&line, "install.sh").as_deref(),
                Some(INSTALLER_SHA256),
                "{line}"
            );
        }
        for line in [
            "abc123  install.sh".to_owned(),
            INSTALLER_SHA256.to_owned(),
            format!("{INSTALLER_SHA256}  other/install.sh"),
            String::new(),
        ] {
            assert_eq!(listed_digest(&line, "install.sh"), None, "{line:?}");
        }
    }

    #[test]
    fn a_digest_that_is_not_sha256_is_never_trusted() {
        let release = release("abc123  ./install.sh\n");
        assert!(matches!(release.installer(), Err(Error::Unlisted { .. })));
    }

    #[test]
    fn the_recorded_digests_outlive_the_run() {
        let directory = tempfile::tempdir().expect("temp dir");
        assert_eq!(recorded_digest(directory.path(), "install.sh"), None);

        release(&sums())
            .record(directory.path())
            .expect("the record is written");
        assert_eq!(
            recorded_digest(directory.path(), "install.sh").as_deref(),
            Some(INSTALLER_SHA256)
        );
        assert_eq!(recorded_digest(directory.path(), "missing"), None);
    }
}
