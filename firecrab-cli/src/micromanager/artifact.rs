//! Pinned artifact download and verification shared by every microManager host.

use std::fs::{self, File};
use std::io::{self, BufRead, IsTerminal, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use reqwest::StatusCode;
use reqwest::header::{CONTENT_LENGTH, HeaderMap, RANGE};

use sha2::{Digest, Sha256, Sha512};

use super::report;

const GAUGE_WIDTH: usize = 24;
const GAUGE_INTERVAL: Duration = Duration::from_millis(100);
/// Sizing the plan is a courtesy; a mirror that stalls on HEAD must not hold the prompt.
const PROBE_TIMEOUT: Duration = Duration::from_secs(15);

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
    #[error("download declined")]
    Declined,
}

/// Downloads whatever is missing or corrupt. Every artifact lands at
/// `directory.join(spec.filename)`, so callers address them by spec.
///
/// On an interactive terminal the pending downloads are listed with their
/// mirrors, digests, and sizes and need a `y` first, unless `assume_yes`.
/// Anything else (CI, a pipe, a script) proceeds without asking.
pub fn fetch_all(specs: &[ArtifactSpec], directory: &Path, assume_yes: bool) -> Result<(), Error> {
    let show_progress = io::stdout().is_terminal();
    let interactive = show_progress && io::stdin().is_terminal();
    let mut stdin = io::stdin().lock();
    let confirm = (interactive && !assume_yes).then_some(&mut stdin as &mut dyn BufRead);
    fetch(specs, directory, confirm, show_progress)
}

/// `confirm` is where the answer comes from when the user must approve the
/// downloads first; `None` downloads without asking.
fn fetch(
    specs: &[ArtifactSpec],
    directory: &Path,
    confirm: Option<&mut dyn BufRead>,
    show_progress: bool,
) -> Result<(), Error> {
    fs::create_dir_all(directory).map_err(|source| Error::CreateDirectory {
        path: directory.to_path_buf(),
        source,
    })?;
    let mut cached_specs = Vec::with_capacity(specs.len());
    for spec in specs {
        cached_specs.push(cached(&directory.join(spec.filename), spec)?);
    }
    let pending: Vec<&ArtifactSpec> = specs
        .iter()
        .zip(&cached_specs)
        .filter_map(|(spec, is_cached)| (!is_cached).then_some(spec))
        .collect();

    let mut client = None;
    if let Some(input) = confirm
        && !pending.is_empty()
    {
        let client = client.insert(build_client()?);
        let sized: Vec<(&ArtifactSpec, Option<u64>)> = pending
            .iter()
            .map(|spec| (*spec, probe_content_length(client, spec.url)))
            .collect();
        report!("{}", render_plan(&sized));
        crate::micromanager::print_report(format_args!("> "));
        if !confirmed(input)? {
            return Err(Error::Declined);
        }
    }

    for (spec, is_cached) in specs.iter().zip(&cached_specs) {
        if *is_cached {
            report!("[PASS] {}: cached", spec.label);
            continue;
        }
        report!("[DOWNLOAD] {}", spec.label);
        let client = match &mut client {
            Some(client) => client,
            none => none.insert(build_client()?),
        };
        download_one(
            client,
            spec,
            directory,
            &directory.join(spec.filename),
            show_progress,
        )?;
        report!("[PASS] {}: verified", spec.label);
    }
    Ok(())
}

fn confirmed(input: &mut dyn BufRead) -> Result<bool, Error> {
    let mut answer = String::new();
    input
        .read_line(&mut answer)
        .map_err(|source| io_error("read download confirmation", Path::new("<stdin>"), source))?;
    Ok(matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

fn probe_content_length(client: &reqwest::blocking::Client, url: &str) -> Option<u64> {
    let response = client.head(url).timeout(PROBE_TIMEOUT).send().ok()?;
    if !response.status().is_success() {
        return None;
    }
    content_length_header(response.headers())
}

/// A HEAD response carries no body, so `Response::content_length` reports the
/// empty body decoder (0) rather than the header the mirror actually sent.
/// Read the header directly; some mirrors answer HEAD with `Content-Length: 0`,
/// which is unknown rather than empty.
fn content_length_header(headers: &HeaderMap) -> Option<u64> {
    let length: u64 = headers.get(CONTENT_LENGTH)?.to_str().ok()?.parse().ok()?;
    (length > 0).then_some(length)
}

fn algorithm_name(algorithm: HashAlgorithm) -> &'static str {
    match algorithm {
        HashAlgorithm::Sha256 => "sha256",
        HashAlgorithm::Sha512 => "sha512",
    }
}

fn render_plan(sized: &[(&ArtifactSpec, Option<u64>)]) -> String {
    let mut plan = String::new();
    for (spec, size) in sized {
        plan.push_str(&format!(
            "{}\n  mirror    {}\n  {:<9} {}\n  size      {}\n\n",
            spec.label,
            spec.url,
            algorithm_name(spec.algorithm),
            spec.digest,
            size.map_or_else(|| "unknown".to_string(), format_bytes)
        ));
    }
    plan.push_str(&"-".repeat(50));
    plan.push('\n');
    plan.push_str(&plan_summary(sized));
    plan
}

fn plan_summary(sized: &[(&ArtifactSpec, Option<u64>)]) -> String {
    let known_count = sized.iter().filter(|(_, size)| size.is_some()).count();
    let total_known: u64 = sized.iter().filter_map(|(_, size)| *size).sum();
    let noun = if sized.len() == 1 {
        "artifact"
    } else {
        "artifacts"
    };
    if known_count == 0 {
        format!(
            "Download {} {noun} (total size unknown)? [y/N]",
            sized.len()
        )
    } else {
        format!(
            "Download {} {noun}, {}{}? [y/N]",
            sized.len(),
            if known_count == sized.len() {
                ""
            } else {
                "at least "
            },
            format_bytes(total_known)
        )
    }
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

fn render_gauge(downloaded: u64, total: Option<u64>, bytes_per_sec: f64) -> String {
    match total {
        Some(total) if total > 0 => {
            let ratio = (downloaded as f64 / total as f64).clamp(0.0, 1.0);
            let filled = ((ratio * GAUGE_WIDTH as f64).round() as usize).min(GAUGE_WIDTH);
            let bar = "█".repeat(filled) + &"░".repeat(GAUGE_WIDTH - filled);
            format!(
                "[{bar}] {:>3}%  {}/{}  {}/s",
                (ratio * 100.0).round() as u64,
                format_bytes(downloaded),
                format_bytes(total),
                format_bytes(bytes_per_sec as u64)
            )
        }
        _ => format!(
            "{} downloaded  {}/s",
            format_bytes(downloaded),
            format_bytes(bytes_per_sec as u64)
        ),
    }
}

/// Clears to end of line (`\x1b[K`) after each redraw so a shorter frame never leaves
/// a tail of the previous, longer frame behind. Only ever printed when stdout is a TTY.
fn render_progress_line(downloaded: u64, total: Option<u64>, bytes_per_sec: f64) -> String {
    format!(
        "\r  {}\x1b[K",
        render_gauge(downloaded, total, bytes_per_sec)
    )
}

/// Copies `response` into `file`, redrawing a gauge on a terminal. `resumed_from`
/// is what an earlier attempt already wrote, so a resumed transfer keeps its
/// place on the gauge while the rate counts only this attempt's bytes.
fn copy_with_progress(
    response: &mut impl Read,
    file: &mut File,
    spec: &ArtifactSpec,
    resumed_from: u64,
    total: Option<u64>,
    show_progress: bool,
) -> Result<(), Error> {
    let mut buffer = [0_u8; 64 * 1024];
    let mut received: u64 = 0;
    let started = Instant::now();
    let mut last_render = started;
    let draw = |received: u64| {
        let rate = received as f64 / started.elapsed().as_secs_f64().max(0.001);
        crate::micromanager::print_report(format_args!(
            "{}",
            render_progress_line(resumed_from + received, total, rate)
        ));
    };
    let result = loop {
        let read = match response.read(&mut buffer) {
            Ok(0) => break Ok(()),
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(source) => {
                break Err(io_error(
                    "read download stream",
                    Path::new(spec.url),
                    source,
                ));
            }
        };
        if let Err(source) = file.write_all(&buffer[..read]) {
            break Err(io_error("write artifact", Path::new(spec.filename), source));
        }
        received += read as u64;
        if show_progress && last_render.elapsed() >= GAUGE_INTERVAL {
            draw(received);
            last_render = Instant::now();
        }
    };
    if show_progress {
        // Finish the gauge line even on failure so a [RETRY] starts on its own line.
        draw(received);
        report!("");
    }
    result
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
    show_progress: bool,
) -> Result<(), Error> {
    let mut staged = tempfile::NamedTempFile::new_in(directory)
        .map_err(|source| io_error("create partial artifact", directory, source))?;
    let mut attempt = 1;
    while let Err(error) = resume(client, spec, staged.as_file_mut(), show_progress) {
        if attempt == ATTEMPTS || !retryable(&error) {
            return Err(error);
        }
        report!("[RETRY] {}: {error}", spec.label);
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
    show_progress: bool,
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
    let resumed_from = match response.status() {
        StatusCode::PARTIAL_CONTENT => written,
        // Everything is already here; the digest check decides whether it is right.
        StatusCode::RANGE_NOT_SATISFIABLE if written > 0 => return Ok(()),
        status if status.is_success() => {
            file.set_len(0)
                .and_then(|()| file.seek(SeekFrom::Start(0)).map(drop))
                .map_err(|source| io_error("restart artifact", Path::new(spec.filename), source))?;
            0
        }
        status => {
            return Err(Error::HttpStatus {
                url: spec.url.to_string(),
                status,
            });
        }
    };
    let total = response
        .content_length()
        .map(|remaining| resumed_from + remaining);
    copy_with_progress(
        &mut response,
        file,
        spec,
        resumed_from,
        total,
        show_progress,
    )
}

/// A 4xx will not change on retry; a dropped connection or a 5xx often does.
fn retryable(error: &Error) -> bool {
    match error {
        Error::HttpStatus { status, .. } => status.is_server_error(),
        Error::Request { .. } | Error::Io { .. } => true,
        Error::CreateDirectory { .. } | Error::Checksum { .. } | Error::Declined => false,
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

    /// No prompt and no gauge, whatever the test runner's stdout happens to be.
    fn fetch_quietly(specs: &[ArtifactSpec], directory: &Path) -> Result<(), Error> {
        fetch(specs, directory, None, false)
    }

    fn fetch_answering(
        specs: &[ArtifactSpec],
        directory: &Path,
        answer: &'static str,
    ) -> Result<(), Error> {
        fetch(specs, directory, Some(&mut answer.as_bytes()), false)
    }

    #[test]
    fn declining_the_prompt_downloads_nothing() {
        let directory = tempfile::tempdir().expect("temp dir");
        for answer in ["n\n", "\n", "", "yep\n"] {
            let error = fetch_answering(
                &[spec(HashAlgorithm::Sha256, ABC_SHA256)],
                directory.path(),
                answer,
            )
            .expect_err("only y or yes approves");
            assert!(matches!(error, Error::Declined), "{answer:?}: {error}");
        }
        assert!(!directory.path().join("abc.bin").exists());
    }

    #[test]
    fn a_fully_cached_install_never_prompts() {
        let directory = tempfile::tempdir().expect("temp dir");
        write_abc(directory.path());
        fetch_answering(
            &[spec(HashAlgorithm::Sha256, ABC_SHA256)],
            directory.path(),
            "n\n",
        )
        .expect("nothing is pending, so a no is never read");
    }

    #[test]
    fn approving_the_prompt_sizes_then_downloads() {
        let (url, server) = serve(vec![
            "HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\n",
            "HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\nabcdef",
        ]);
        let directory = tempfile::tempdir().expect("temp dir");
        fetch_answering(&[remote_spec(url)], directory.path(), " Yes \n")
            .expect("an approved download completes");

        let requests = server.join().expect("server finishes");
        assert!(requests[0].starts_with("head "), "{}", requests[0]);
        assert!(requests[1].starts_with("get "), "{}", requests[1]);
        assert_eq!(
            fs::read(directory.path().join("abc.bin")).expect("published"),
            b"abcdef"
        );
    }

    #[test]
    fn content_length_header_is_read_from_the_header_not_the_body() {
        use reqwest::header::HeaderValue;

        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_LENGTH, HeaderValue::from_static("302029976"));
        assert_eq!(content_length_header(&headers), Some(302_029_976));

        // A redirect answered with `Content-Length: 0` is unknown, not empty.
        headers.insert(CONTENT_LENGTH, HeaderValue::from_static("0"));
        assert_eq!(content_length_header(&headers), None);

        headers.insert(CONTENT_LENGTH, HeaderValue::from_static("not-a-number"));
        assert_eq!(content_length_header(&headers), None);

        assert_eq!(content_length_header(&HeaderMap::new()), None);
    }

    #[test]
    fn format_bytes_renders_familiar_units() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(1023), "1023 B");
        assert_eq!(format_bytes(1024), "1.0 KiB");
        assert_eq!(format_bytes(1024 * 1024), "1.0 MiB");
        assert_eq!(format_bytes(432_768_614), "412.7 MiB");
    }

    #[test]
    fn plan_lists_every_mirror_digest_and_size() {
        let debian = spec(HashAlgorithm::Sha512, ABC_SHA512);
        let plan = render_plan(&[(&debian, Some(302_029_976))]);
        assert!(
            plan.contains("  mirror    https://example.invalid/abc.bin"),
            "{plan}"
        );
        assert!(
            plan.contains(&format!("  sha512    {ABC_SHA512}")),
            "{plan}"
        );
        assert!(plan.contains("  size      288.0 MiB"), "{plan}");
        assert!(
            plan.ends_with("Download 1 artifact, 288.0 MiB? [y/N]"),
            "{plan}"
        );
    }

    #[test]
    fn plan_summary_is_honest_about_unknown_sizes() {
        let one = spec(HashAlgorithm::Sha256, ABC_SHA256);
        let two = spec(HashAlgorithm::Sha256, ABCDEF_SHA256);

        let some = plan_summary(&[(&one, Some(1024)), (&two, None)]);
        assert_eq!(some, "Download 2 artifacts, at least 1.0 KiB? [y/N]");

        let none = plan_summary(&[(&one, None), (&two, None)]);
        assert_eq!(none, "Download 2 artifacts (total size unknown)? [y/N]");
    }

    #[test]
    fn gauge_renders_zero_fifty_and_hundred_percent() {
        let empty = render_gauge(0, Some(100), 0.0);
        assert!(empty.contains("  0%"), "{empty}");
        assert!(!empty.contains('█'), "{empty}");

        let half = render_gauge(50, Some(100), 10.0);
        assert!(half.contains(" 50%"), "{half}");
        assert!(half.contains('█') && half.contains('░'), "{half}");

        let full = render_gauge(100, Some(100), 10.0);
        assert!(full.contains("100%"), "{full}");
        assert!(!full.contains('░'), "{full}");
    }

    #[test]
    fn gauge_without_a_total_shows_bytes_instead_of_percent() {
        let line = render_gauge(2_097_152, None, 1_048_576.0);
        assert_eq!(line, "2.0 MiB downloaded  1.0 MiB/s");
        let zero_total = render_gauge(0, Some(0), 0.0);
        assert!(!zero_total.contains('%'), "{zero_total}");
    }

    #[test]
    fn progress_line_redraws_in_place_and_clears_the_previous_tail() {
        let line = render_progress_line(50, Some(100), 10.0);
        assert!(line.starts_with("\r  ["), "{line:?}");
        assert!(line.ends_with("\x1b[K"), "{line:?}");
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
        fetch_quietly(&[spec(HashAlgorithm::Sha256, ABC_SHA256)], directory.path())
            .expect("cached artifact is reused");
        assert!(directory.path().join("abc.bin").is_file());
    }

    #[test]
    fn a_corrupt_cache_is_discarded_before_downloading() {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = write_abc(directory.path());
        // The URL is unreachable, so reaching the download proves the cache was rejected.
        let error = fetch_quietly(
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
        fetch_quietly(&[remote_spec(url)], directory.path()).expect("the retry completes the file");

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
        fetch_quietly(&[remote_spec(url)], directory.path())
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
        let error = fetch_quietly(&[remote_spec(url)], directory.path())
            .expect_err("a missing artifact fails");

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
        let error = fetch_quietly(&[spec(HashAlgorithm::Sha256, ABC_SHA256)], &nested)
            .expect_err("an empty directory forces a download");
        assert!(matches!(error, Error::Request { .. }));
        assert!(nested.is_dir());
    }
}
