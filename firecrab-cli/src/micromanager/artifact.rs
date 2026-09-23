//! Pinned artifact download and verification shared by every microManager host.

use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use reqwest::StatusCode;
use reqwest::header::RANGE;

use sha2::{Digest, Sha256, Sha512};

/// Each host pins with the digests its upstreams publish, so one backend alone
/// does not have to use every algorithm.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HashAlgorithm {
    Sha256,
    /// Debian's cloud images publish only SHA-512; Windows never pins one.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    Sha512,
}

#[derive(Clone, Copy, Debug)]
pub struct ArtifactSpec {
    pub label: &'static str,
    pub filename: &'static str,
    pub url: &'static str,
    pub algorithm: HashAlgorithm,
    pub digest: &'static str,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("could not create artifact directory {path}: {source}")]
    CreateDirectory {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("could not download {url}: {source}")]
    Request {
        url: String,
        #[source]
        source: reqwest::Error,
    },
    #[error("artifact request failed for {url}: HTTP {status}")]
    HttpStatus {
        url: String,
        status: reqwest::StatusCode,
    },
    #[error("could not {action} {path}: {source}")]
    Io {
        action: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("checksum mismatch for {path}: expected {expected}, got {actual}")]
    Checksum {
        path: PathBuf,
        expected: String,
        actual: String,
    },
}

/// Downloads whatever is missing or corrupt. Every artifact lands at
/// `directory.join(spec.filename)`, so callers address them by spec.
pub fn fetch_all(specs: &[ArtifactSpec], directory: &Path) -> Result<(), Error> {
    fs::create_dir_all(directory).map_err(|source| Error::CreateDirectory {
        path: directory.to_path_buf(),
        source,
    })?;
    let mut client = None;
    for spec in specs {
        let destination = directory.join(spec.filename);
        if cached(&destination, spec)? {
            println!("[PASS] {}: cached", spec.label);
            continue;
        }
        println!("[DOWNLOAD] {}", spec.label);
        let client = match &mut client {
            Some(client) => client,
            none => none.insert(build_client()?),
        };
        download_one(client, spec, directory, &destination)?;
        println!("[PASS] {}: verified", spec.label);
    }
    Ok(())
}

pub fn verify(path: &Path, algorithm: HashAlgorithm, expected: &str) -> Result<(), Error> {
    let mut file = File::open(path).map_err(|source| io_error("open artifact", path, source))?;
    let actual = match algorithm {
        HashAlgorithm::Sha256 => digest_reader::<Sha256>(&mut file, path)?,
        HashAlgorithm::Sha512 => digest_reader::<Sha512>(&mut file, path)?,
    };
    if actual != expected {
        return Err(Error::Checksum {
            path: path.to_owned(),
            expected: expected.to_string(),
            actual,
        });
    }
    Ok(())
}

/// A corrupt cached file is removed so the caller downloads it again.
fn cached(destination: &Path, spec: &ArtifactSpec) -> Result<bool, Error> {
    if !destination.is_file() {
        return Ok(false);
    }
    match verify(destination, spec.algorithm, spec.digest) {
        Ok(()) => Ok(true),
        Err(Error::Checksum { .. }) => {
            fs::remove_file(destination)
                .map_err(|source| io_error("remove corrupt artifact", destination, source))?;
            Ok(false)
        }
        Err(error) => Err(error),
    }
}

/// Artifacts are single large streams, so HTTP/2 multiplexing buys nothing here,
/// and salsa.debian.org resets its HTTP/2 stream partway through the rootfs.
fn build_client() -> Result<reqwest::blocking::Client, Error> {
    reqwest::blocking::Client::builder()
        .user_agent(concat!("firecrab-micromanager/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(30))
        .http1_only()
        .build()
        .map_err(|source| Error::Request {
            url: "client initialization".to_string(),
            source,
        })
}

/// salsa.debian.org drops long transfers partway through and sometimes answers a
/// retry with a 500, so one clean request is not something a user can count on.
const ATTEMPTS: u32 = 6;
const RETRY_DELAY: Duration = if cfg!(test) {
    Duration::ZERO
} else {
    Duration::from_secs(3)
};

/// Stages into a temporary file so an interrupted download never lands under the
/// artifact's real name, where the next run would treat it as complete.
fn download_one(
    client: &reqwest::blocking::Client,
    spec: &ArtifactSpec,
    directory: &Path,
    destination: &Path,
) -> Result<(), Error> {
    let mut staged = tempfile::NamedTempFile::new_in(directory)
        .map_err(|source| io_error("create partial artifact", directory, source))?;
    let mut attempt = 1;
    while let Err(error) = resume(client, spec, staged.as_file_mut()) {
        if attempt == ATTEMPTS || !retryable(&error) {
            return Err(error);
        }
        println!("[RETRY] {}: {error}", spec.label);
        attempt += 1;
        thread::sleep(RETRY_DELAY * attempt);
    }
    staged
        .flush()
        .map_err(|source| io_error("flush artifact", staged.path(), source))?;
    staged
        .as_file()
        .sync_all()
        .map_err(|source| io_error("sync artifact", staged.path(), source))?;
    verify(staged.path(), spec.algorithm, spec.digest)?;
    staged
        .persist(destination)
        .map_err(|error| io_error("publish artifact", destination, error.error))?;
    Ok(())
}

/// Continues from whatever an earlier attempt already wrote. A server that
/// ignores the range sends the whole body again, so the file starts over.
fn resume(
    client: &reqwest::blocking::Client,
    spec: &ArtifactSpec,
    file: &mut File,
) -> Result<(), Error> {
    let written = file
        .seek(SeekFrom::End(0))
        .map_err(|source| io_error("inspect partial artifact", Path::new(spec.filename), source))?;
    let mut request = client.get(spec.url);
    if written > 0 {
        request = request.header(RANGE, format!("bytes={written}-"));
    }
    let mut response = request.send().map_err(|source| Error::Request {
        url: spec.url.to_string(),
        source,
    })?;
    match response.status() {
        StatusCode::PARTIAL_CONTENT => {}
        // Everything is already here; the digest check decides whether it is right.
        StatusCode::RANGE_NOT_SATISFIABLE if written > 0 => return Ok(()),
        status if status.is_success() => {
            file.set_len(0)
                .and_then(|()| file.seek(SeekFrom::Start(0)).map(drop))
                .map_err(|source| io_error("restart artifact", Path::new(spec.filename), source))?;
        }
        status => {
            return Err(Error::HttpStatus {
                url: spec.url.to_string(),
                status,
            });
        }
    }
    io::copy(&mut response, file)
        .map_err(|source| io_error("write artifact", Path::new(spec.filename), source))?;
    Ok(())
}

/// A 4xx will not change on retry; a dropped connection or a 5xx often does.
fn retryable(error: &Error) -> bool {
    match error {
        Error::HttpStatus { status, .. } => status.is_server_error(),
        Error::Request { .. } | Error::Io { .. } => true,
        Error::CreateDirectory { .. } | Error::Checksum { .. } => false,
    }
}

fn digest_reader<D: Digest + Default>(reader: &mut File, path: &Path) -> Result<String, Error> {
    let mut digest = D::default();
    let mut buffer = [0_u8; 1024 * 1024];
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|source| io_error("hash artifact", path, source))?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    let output = digest.finalize();
    let mut encoded = String::with_capacity(output.len() * 2);
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for byte in output {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    Ok(encoded)
}

fn io_error(action: &'static str, path: &Path, source: io::Error) -> Error {
    Error::Io {
        action,
        path: path.to_owned(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ABC_SHA256: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
    const ABC_SHA512: &str = "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f";
    const ABCDEF_SHA256: &str = "bef57ec7f53a6d40beb640a780a639c83bc29ac8a9816f1fc6c5c6dcd93c4721";
    const WRONG_SHA256: &str = "0000000000000000000000000000000000000000000000000000000000000000";

    fn spec(algorithm: HashAlgorithm, digest: &'static str) -> ArtifactSpec {
        ArtifactSpec {
            label: "test artifact",
            filename: "abc.bin",
            url: "https://example.invalid/abc.bin",
            algorithm,
            digest,
        }
    }

    fn write_abc(directory: &Path) -> PathBuf {
        let path = directory.join("abc.bin");
        fs::write(&path, b"abc").expect("artifact written");
        path
    }

    #[test]
    fn sha256_and_sha512_match_the_published_vectors() {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = write_abc(directory.path());
        assert!(verify(&path, HashAlgorithm::Sha256, ABC_SHA256).is_ok());
        assert!(verify(&path, HashAlgorithm::Sha512, ABC_SHA512).is_ok());
    }

    #[test]
    fn a_wrong_digest_is_reported_with_both_values() {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = write_abc(directory.path());
        let Err(Error::Checksum {
            expected, actual, ..
        }) = verify(&path, HashAlgorithm::Sha256, WRONG_SHA256)
        else {
            panic!("a mismatched digest must fail");
        };
        assert_eq!(expected, WRONG_SHA256);
        assert_eq!(actual, ABC_SHA256);
    }

    #[test]
    fn a_verified_cache_is_reused_without_network() {
        let directory = tempfile::tempdir().expect("temp dir");
        write_abc(directory.path());
        fetch_all(&[spec(HashAlgorithm::Sha256, ABC_SHA256)], directory.path())
            .expect("cached artifact is reused");
        assert!(directory.path().join("abc.bin").is_file());
    }

    #[test]
    fn a_corrupt_cache_is_discarded_before_downloading() {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = write_abc(directory.path());
        // The URL is unreachable, so reaching the download proves the cache was rejected.
        let error = fetch_all(
            &[spec(HashAlgorithm::Sha256, WRONG_SHA256)],
            directory.path(),
        )
        .expect_err("a corrupt cache must not be accepted");
        assert!(matches!(error, Error::Request { .. }));
        assert!(!path.exists(), "the corrupt file is removed");
    }

    /// Answers each connection with the next canned response and records the
    /// request it answered, so a test can see what a retry asked for.
    fn serve(responses: Vec<&'static str>) -> (&'static str, thread::JoinHandle<Vec<String>>) {
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("test listener binds");
        let url = format!(
            "http://{}/abc.bin",
            listener.local_addr().expect("bound address")
        );
        let handle = thread::spawn(move || {
            let mut requests = Vec::new();
            for response in responses {
                let (mut stream, _) = listener.accept().expect("client connects");
                let mut buffer = [0_u8; 4096];
                let read = stream.read(&mut buffer).expect("request is readable");
                requests.push(String::from_utf8_lossy(&buffer[..read]).to_lowercase());
                stream
                    .write_all(response.as_bytes())
                    .expect("response is written");
            }
            requests
        });
        (Box::leak(url.into_boxed_str()), handle)
    }

    fn remote_spec(url: &'static str) -> ArtifactSpec {
        ArtifactSpec {
            url,
            ..spec(HashAlgorithm::Sha256, ABCDEF_SHA256)
        }
    }

    #[test]
    fn a_dropped_transfer_resumes_from_the_bytes_already_written() {
        let (url, server) = serve(vec![
            "HTTP/1.1 200 OK\r\nContent-Length: 6\r\n\r\nabc",
            "HTTP/1.1 206 Partial Content\r\nContent-Length: 3\r\nContent-Range: bytes 3-5/6\r\nConnection: close\r\n\r\ndef",
        ]);
        let directory = tempfile::tempdir().expect("temp dir");
        fetch_all(&[remote_spec(url)], directory.path()).expect("the retry completes the file");

        let requests = server.join().expect("server finishes");
        assert!(
            !requests[0].contains("range:"),
            "the first request asks for everything"
        );
        assert!(
            requests[1].contains("range: bytes=3-"),
            "the retry resumes: {}",
            requests[1]
        );
        assert_eq!(
            fs::read(directory.path().join("abc.bin")).expect("published"),
            b"abcdef"
        );
    }

    #[test]
    fn a_server_that_ignores_the_range_restarts_the_file() {
        let (url, server) = serve(vec![
            "HTTP/1.1 200 OK\r\nContent-Length: 6\r\n\r\nabc",
            "HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\nabcdef",
        ]);
        let directory = tempfile::tempdir().expect("temp dir");
        fetch_all(&[remote_spec(url)], directory.path())
            .expect("the full body replaces the partial one");

        server.join().expect("server finishes");
        assert_eq!(
            fs::read(directory.path().join("abc.bin")).expect("published"),
            b"abcdef"
        );
    }

    #[test]
    fn a_client_error_is_not_retried() {
        let (url, server) = serve(vec!["HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n"]);
        let directory = tempfile::tempdir().expect("temp dir");
        let error =
            fetch_all(&[remote_spec(url)], directory.path()).expect_err("a missing artifact fails");

        server.join().expect("server finishes");
        // A retry would have hit the closed listener and failed as a request error instead.
        assert!(
            matches!(error, Error::HttpStatus { status, .. } if status == StatusCode::NOT_FOUND)
        );
        assert!(!directory.path().join("abc.bin").exists());
    }

    #[test]
    fn a_missing_directory_is_created() {
        let directory = tempfile::tempdir().expect("temp dir");
        let nested = directory.path().join("downloads");
        let error = fetch_all(&[spec(HashAlgorithm::Sha256, ABC_SHA256)], &nested)
            .expect_err("an empty directory forces a download");
        assert!(matches!(error, Error::Request { .. }));
        assert!(nested.is_dir());
    }
}
