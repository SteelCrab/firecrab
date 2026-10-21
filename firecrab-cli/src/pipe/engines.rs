//! Pinned upstream binaries, verified before any executable is replaced.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
const MANIFEST: &str = include_str!("../../../packaging/pipe-engines.json");

#[derive(Deserialize)]
pub(super) struct Engine {
    pub version: String,
    assets: BTreeMap<String, Asset>,
}

#[derive(Deserialize)]
pub(super) struct Asset {
    pub url: String,
    pub sha256: String,
}

pub(super) fn manifest() -> Result<BTreeMap<String, Engine>> {
    Ok(serde_json::from_str(MANIFEST)?)
}

impl Engine {
    pub fn asset(&self, arch: &str) -> Result<&Asset> {
        self.assets.get(arch).ok_or_else(|| {
            format!("pipe engines require x86_64 or aarch64 Linux, found {arch}").into()
        })
    }
}

pub(super) fn managed_binary(libdir: &Path, name: &str) -> PathBuf {
    libdir.join("pipe").join(name)
}

#[cfg(target_os = "linux")]
mod install {
    use std::fs;
    use std::io::{Read, Seek, SeekFrom, Write};
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use std::time::Duration;

    use sha2::{Digest, Sha256};

    use super::*;

    const MAX_DOWNLOAD: u64 = 128 * 1024 * 1024;

    #[derive(Deserialize, serde::Serialize)]
    struct Installed {
        version: String,
        asset_sha256: String,
        binary_sha256: String,
    }

    fn digest(file: &mut fs::File) -> Result<String> {
        file.seek(SeekFrom::Start(0))?;
        let mut hash = Sha256::new();
        let mut buffer = [0; 64 * 1024];
        loop {
            let length = file.read(&mut buffer)?;
            if length == 0 {
                break;
            }
            hash.update(&buffer[..length]);
        }
        Ok(format!("{:x}", hash.finalize()))
    }

    fn current(binary: &Path, record: &Path, engine: &Engine, asset: &Asset) -> bool {
        let Ok(bytes) = fs::read(record) else {
            return false;
        };
        let Ok(record) = serde_json::from_slice::<Installed>(&bytes) else {
            return false;
        };
        let Ok(mut file) = fs::File::open(binary) else {
            return false;
        };
        let Ok(metadata) = fs::symlink_metadata(binary) else {
            return false;
        };
        if !metadata.is_file() || metadata.uid() != 0 || metadata.mode() & 0o111 == 0 {
            return false;
        }
        record.version == engine.version
            && record.asset_sha256 == asset.sha256
            && digest(&mut file).is_ok_and(|value| value == record.binary_sha256)
    }

    fn install_one(
        client: &reqwest::blocking::Client,
        directory: &Path,
        name: &str,
        engine: &Engine,
        asset: &Asset,
    ) -> Result<()> {
        let binary = directory.join(name);
        let record = directory.join(format!("{name}.json"));
        if current(&binary, &record, engine, asset) {
            println!("{name} {} is already installed", engine.version);
            return Ok(());
        }
        println!(
            "Installing {name} {} (verified upstream release)",
            engine.version
        );
        let response = client.get(&asset.url).send()?.error_for_status()?;
        let mut download = tempfile::tempfile()?;
        let length = std::io::copy(&mut response.take(MAX_DOWNLOAD + 1), &mut download)?;
        if length > MAX_DOWNLOAD || digest(&mut download)? != asset.sha256 {
            return Err(format!(
                "{name}: download size or SHA-256 verification failed; existing executable kept"
            )
            .into());
        }
        download.seek(SeekFrom::Start(0))?;
        let mut candidate = tempfile::NamedTempFile::new_in(directory)?;
        std::io::copy(&mut download, &mut candidate)?;
        let installed = Installed {
            version: engine.version.clone(),
            asset_sha256: asset.sha256.clone(),
            binary_sha256: digest(candidate.as_file_mut())?,
        };
        candidate
            .as_file()
            .set_permissions(fs::Permissions::from_mode(0o755))?;
        candidate.as_file().sync_all()?;
        candidate.persist(&binary)?;
        let mut marker = tempfile::NamedTempFile::new_in(directory)?;
        marker.write_all(&serde_json::to_vec_pretty(&installed)?)?;
        marker
            .as_file()
            .set_permissions(fs::Permissions::from_mode(0o644))?;
        marker.as_file().sync_all()?;
        marker.persist(record)?;
        fs::File::open(directory)?.sync_all()?;
        Ok(())
    }

    pub(super) fn run() -> Result<()> {
        if !nix::unistd::geteuid().is_root() {
            return Err("pipe install requires root; run with sudo".into());
        }
        if nginx_binary().is_none() {
            return Err("Nginx is unavailable; install the nginx package with your Linux package manager, then run pipe install".into());
        }
        let manifest = manifest()?;
        let arch = std::env::consts::ARCH;
        for engine in manifest.values() {
            engine.asset(arch)?;
        }
        let env = crate::service::env::ServiceEnv::from_process_env();
        let directory = env.libdir.join("pipe");
        fs::create_dir_all(&directory)?;
        let metadata = fs::symlink_metadata(&directory)?;
        if !metadata.is_dir() || metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
            return Err(
                "managed pipe binary directory must be a root-owned, non-writable directory".into(),
            );
        }
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o755))?;
        let lock = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(directory.join("install.lock"))?;
        fs2::FileExt::try_lock_exclusive(&lock).map_err(|_| "another pipe install is running")?;
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(120))
            .build()?;
        for (name, engine) in manifest {
            install_one(&client, &directory, &name, &engine, engine.asset(arch)?)?;
        }
        println!("Pipe engines installed; run pipe apply to configure and start the services");
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::net::TcpListener;

        fn response(bytes: Vec<u8>) -> (String, std::thread::JoinHandle<()>) {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let thread = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0; 1024];
                assert!(stream.read(&mut request).unwrap() > 0);
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    bytes.len()
                )
                .unwrap();
                stream.write_all(&bytes).unwrap();
            });
            (url, thread)
        }

        #[test]
        fn verified_download_installs_an_executable_and_a_matching_digest_record() {
            let directory = tempfile::tempdir().unwrap();
            let bytes = b"verified fixture executable".to_vec();
            let expected = format!("{:x}", Sha256::digest(&bytes));
            let (url, server) = response(bytes.clone());
            let asset = Asset {
                url,
                sha256: expected.clone(),
            };
            let engine = Engine {
                version: "fixture-version".into(),
                assets: BTreeMap::new(),
            };
            install_one(
                &reqwest::blocking::Client::new(),
                directory.path(),
                "cloudflared",
                &engine,
                &asset,
            )
            .unwrap();
            server.join().unwrap();
            let binary = directory.path().join("cloudflared");
            let record_path = directory.path().join("cloudflared.json");
            let record: Installed =
                serde_json::from_slice(&fs::read(&record_path).unwrap()).unwrap();
            assert_eq!(fs::read(&binary).unwrap(), bytes);
            assert_eq!(
                fs::metadata(&binary).unwrap().permissions().mode() & 0o777,
                0o755
            );
            assert_eq!(record.version, engine.version);
            assert_eq!(record.asset_sha256, expected);
            assert_eq!(record.binary_sha256, expected);
            if nix::unistd::geteuid().is_root() {
                assert!(current(&binary, &record_path, &engine, &asset));
            }
            fs::write(&binary, b"tampered").unwrap();
            assert!(!current(&binary, &record_path, &engine, &asset));
        }

        #[test]
        fn a_wrong_digest_keeps_the_existing_executable() {
            let directory = tempfile::tempdir().unwrap();
            fs::write(directory.path().join("cloudflared"), b"previous").unwrap();
            let (url, server) = response(b"unverified candidate".to_vec());
            let asset = Asset {
                url,
                sha256: "0".repeat(64),
            };
            let engine = Engine {
                version: "test".into(),
                assets: BTreeMap::new(),
            };
            let error = install_one(
                &reqwest::blocking::Client::new(),
                directory.path(),
                "cloudflared",
                &engine,
                &asset,
            )
            .unwrap_err();
            server.join().unwrap();
            assert!(error.to_string().contains("SHA-256 verification failed"));
            assert_eq!(
                fs::read(directory.path().join("cloudflared")).unwrap(),
                b"previous"
            );
        }
    }
}

#[cfg(target_os = "linux")]
pub(super) fn install() -> Result<()> {
    install::run()
}

#[cfg(target_os = "linux")]
pub(super) fn nginx_binary() -> Option<PathBuf> {
    let mut paths = vec![
        PathBuf::from("/usr/sbin/nginx"),
        PathBuf::from("/usr/bin/nginx"),
    ];
    if let Some(path) = std::env::var_os("PATH") {
        paths.extend(std::env::split_paths(&path).map(|path| path.join("nginx")));
    }
    paths.into_iter().find(|path| {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_architectures_use_pinned_official_assets_and_digests() {
        let manifest = manifest().unwrap();
        for name in ["cloudflared"] {
            let engine = &manifest[name];
            for arch in ["x86_64", "aarch64"] {
                let asset = engine.asset(arch).unwrap();
                assert!(asset.url.starts_with("https://github.com/"));
                assert!(asset.url.contains(&format!("/{}/", engine.version)));
                assert!(!asset.url.contains("latest"));
                assert_eq!(asset.sha256.len(), 64);
                assert!(asset.sha256.bytes().all(|c| c.is_ascii_hexdigit()));
            }
            assert!(engine.asset("riscv64").is_err());
        }
        assert_eq!(
            managed_binary(Path::new("/opt/firecrab/lib"), "cloudflared"),
            Path::new("/opt/firecrab/lib/pipe/cloudflared")
        );
    }
}
