//! Resolve one published release before downloading or changing an installation.

use std::time::Duration;

use serde::Deserialize;

use super::artifact::HashAlgorithm;
use super::manifest::{Artifact, Firecrab};

#[derive(Debug, thiserror::Error)]
#[error("could not resolve the latest Firecrab release: {0}")]
/// A failed release lookup or an incomplete checksum catalog.
pub struct Error(String);

#[derive(Deserialize)]
struct Payload {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
}

/// One selected tag and its own checksum catalog; never a floating asset URL.
pub struct Release {
    pub tag: String,
    base: String,
    checksums: String,
}

fn setting(name: &str, default: String) -> String {
    std::env::var(name)
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or(default)
}

/// Look up the latest stable release and fetch only that tag's checksums.
pub fn latest() -> Result<Release, Error> {
    let repository = setting("FIRECRAB_RELEASE_REPO", "SteelCrab/firecrab".into());
    let api = setting(
        "FIRECRAB_RELEASE_API",
        format!("https://api.github.com/repos/{repository}/releases/latest"),
    );
    let base = setting(
        "FIRECRAB_RELEASE_BASE",
        format!("https://github.com/{repository}/releases"),
    );
    resolve(&api, &base)
}

fn resolve(api: &str, base: &str) -> Result<Release, Error> {
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .user_agent(concat!("firecrab/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|error| Error(error.to_string()))?;
    let get = |url: &str| {
        client
            .get(url)
            .send()
            .and_then(reqwest::blocking::Response::error_for_status)
            .and_then(reqwest::blocking::Response::text)
            .map_err(|error| Error(format!("{url}: {error}")))
    };
    let payload: Payload =
        serde_json::from_str(&get(api)?).map_err(|error| Error(error.to_string()))?;
    validate_release(&payload)?;
    let base = base.trim_end_matches('/').to_owned();
    let checksums = get(&format!("{base}/download/{}/SHA256SUMS", payload.tag_name))?;
    Ok(Release {
        tag: payload.tag_name,
        base,
        checksums,
    })
}

fn validate_release(payload: &Payload) -> Result<(), Error> {
    if payload.draft || payload.prerelease {
        return Err(Error(
            "the latest endpoint did not return a stable release".into(),
        ));
    }
    let tag = &payload.tag_name;
    if tag.is_empty()
        || !tag
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
        || tag == "."
        || tag == ".."
    {
        return Err(Error(format!("unsafe release tag {tag:?}")));
    }
    Ok(())
}

impl Release {
    /// Resolve a required asset, rejecting missing or duplicate checksum entries.
    pub fn artifact(&self, name: &str, filename: String) -> Result<Artifact, Error> {
        let mut matches = self.checksums.lines().filter_map(|line| {
            let mut fields = line.split_whitespace();
            let hash = fields.next()?;
            let entry = fields.next()?.trim_start_matches('*');
            (entry.strip_prefix("./").unwrap_or(entry) == name).then_some(hash)
        });
        let digest = matches
            .next()
            .ok_or_else(|| Error(format!("{} SHA256SUMS has no {name}", self.tag)))?;
        if matches.next().is_some()
            || digest.len() != 64
            || !digest.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(Error(format!("invalid or duplicate SHA-256 for {name}")));
        }
        Ok(Artifact {
            filename,
            url: format!("{}/download/{}/{name}", self.base, self.tag),
            algorithm: HashAlgorithm::Sha256,
            digest: digest.to_ascii_lowercase(),
        })
    }

    /// Exact installation inputs recorded in the installed manifest.
    pub fn firecrab(&self) -> Result<Firecrab, Error> {
        Ok(Firecrab {
            version: self.tag.clone(),
            installer: self.artifact("install.sh", format!("install-firecrab-{}.sh", self.tag))?,
            aarch64: self.artifact(
                "firecrab-host-aarch64-gnu.tar.gz",
                "firecrab-host-aarch64-gnu.tar.gz".into(),
            )?,
            x86_64: self.artifact(
                "firecrab-host-x86_64-gnu.tar.gz",
                "firecrab-host-x86_64-gnu.tar.gz".into(),
            )?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(sums: String) -> Release {
        Release {
            tag: "v9.8.7".into(),
            base: "https://example.com/releases".into(),
            checksums: sums,
        }
    }

    #[test]
    fn one_resolved_tag_supplies_the_installer_and_both_architectures() {
        let names = [
            "install.sh",
            "firecrab-host-aarch64-gnu.tar.gz",
            "firecrab-host-x86_64-gnu.tar.gz",
        ];
        let sums = names
            .map(|name| format!("{}  ./{name}\n", "a".repeat(64)))
            .join("");
        let selected = release(sums).firecrab().unwrap();
        assert_eq!(selected.version, "v9.8.7");
        assert_eq!(selected.installer.filename, "install-firecrab-v9.8.7.sh");
        for artifact in [selected.installer, selected.aarch64, selected.x86_64] {
            assert!(artifact.url.contains("/download/v9.8.7/"));
            assert_eq!(artifact.digest, "a".repeat(64));
        }
    }

    #[test]
    fn missing_or_ambiguous_checksums_fail_before_installation() {
        for sums in [
            String::new(),
            "bad  install.sh".into(),
            format!("{0} install.sh\n{0} install.sh\n", "a".repeat(64)),
        ] {
            assert!(
                release(sums)
                    .artifact("install.sh", "installer.sh".into())
                    .is_err()
            );
        }
    }

    #[test]
    fn only_stable_safe_release_tags_are_accepted() {
        for tag in ["", "..", "../v0.4.1", "v0.4.1/other", "v0.4.1\n"] {
            assert!(
                validate_release(&Payload {
                    tag_name: tag.into(),
                    draft: false,
                    prerelease: false
                })
                .is_err()
            );
        }
        assert!(
            validate_release(&Payload {
                tag_name: "v0.4.1".into(),
                draft: false,
                prerelease: true
            })
            .is_err()
        );
    }

    #[test]
    fn resolution_uses_the_checksum_file_of_the_returned_tag() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let worker = std::thread::spawn(move || {
            for (path, body) in [
                ("/latest", "{\"tag_name\":\"v9.8.7\"}".to_owned()),
                (
                    "/download/v9.8.7/SHA256SUMS",
                    format!("{}  install.sh\n", "b".repeat(64)),
                ),
            ] {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut request = [0; 4096];
                let length = stream.read(&mut request).unwrap();
                assert!(
                    String::from_utf8_lossy(&request[..length])
                        .starts_with(&format!("GET {path} "))
                );
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
        });
        let selected = resolve(&format!("{base}/latest"), &base).unwrap();
        assert_eq!(selected.tag, "v9.8.7");
        assert_eq!(
            selected
                .artifact("install.sh", "install.sh".into())
                .unwrap()
                .digest,
            "b".repeat(64)
        );
        worker.join().unwrap();
    }
}
