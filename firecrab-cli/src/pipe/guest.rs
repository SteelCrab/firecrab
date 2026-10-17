//! Native commands execute inside the managed Linux guest. Private data uses stdin.

use super::config::Config;
use serde::{Deserialize, Serialize};
#[cfg(any(target_os = "macos", target_os = "windows", test))]
use std::{fs::File, io::Write, path::Path};

#[cfg(any(target_os = "macos", target_os = "windows", test))]
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
pub(super) const MAX_BUNDLE_BYTES: u64 = 1024 * 1024;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Bundle {
    pub config: Config,
    pub credentials: Option<serde_json::Value>,
}

#[cfg(any(target_os = "macos", target_os = "windows", test))]
pub(super) fn input(path: &Path) -> Result<File> {
    let mut config = Config::read(path)?;
    let credentials = config.credentials()?;
    if let Some(tunnel) = &mut config.tunnel {
        tunnel.credentials_file = "credentials.json".into();
    }
    let bytes = serde_json::to_vec(&Bundle {
        config,
        credentials,
    })?;
    if bytes.len() as u64 > MAX_BUNDLE_BYTES {
        return Err("pipe configuration/credentials exceed 1 MiB".into());
    }
    let mut file = tempfile::tempfile()?;
    file.write_all(&bytes)?;
    use std::io::{Seek, SeekFrom};
    file.seek(SeekFrom::Start(0))?;
    Ok(file)
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
pub(super) fn run(args: Vec<String>, input: Option<File>) -> Result<()> {
    let script = r#"if test -f /etc/systemd/system/firecrab-api.service.d/90-firecrab-dev.conf && test -x /usr/local/lib/firecrab-dev/bin/firecrab; then cli=/usr/local/lib/firecrab-dev/bin/firecrab; else cli=$(command -v firecrab) || { echo 'Firecrab CLI is missing in the managed guest' >&2; exit 1; }; fi; exec "$cli" pipe "$@""#;
    let mut command = vec![
        "sh".into(),
        "-c".into(),
        script.into(),
        "firecrab-pipe".into(),
    ];
    command.extend(args);
    let code = crate::micromanager::run_guest_command(&command, input)?;
    if code != 0 {
        return Err(format!("managed guest pipe command failed (exit {code})").into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn credentials_are_streamed_with_guest_paths() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("private.json"), r#"{"AccountTag":"account","TunnelSecret":"private-secret","TunnelID":"11111111-1111-4111-8111-111111111111"}"#).unwrap();
        let manifest = directory.path().join("pipe.json");
        std::fs::write(&manifest, r#"{"routes":[{"host":"app.example.com","upstreams":["127.0.0.1:18081"]}],"tunnel":{"id":"11111111-1111-4111-8111-111111111111","credentialsFile":"private.json"}}"#).unwrap();
        let bundle: Bundle = serde_json::from_reader(input(&manifest).unwrap()).unwrap();
        assert_eq!(
            bundle.config.tunnel.unwrap().credentials_file,
            Path::new("credentials.json")
        );
        assert_eq!(
            bundle.credentials.unwrap()["TunnelSecret"],
            "private-secret"
        );
    }
}
