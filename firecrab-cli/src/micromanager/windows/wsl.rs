//! Every call into `wsl.exe`, and the one path translation the guest needs.

use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;

/// The WSL distribution microManager imports and owns. It is never the user's
/// own distribution, so uninstalling cannot touch their data.
pub const DISTRO_NAME: &str = "firecrab-debian";

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("could not run {program}: {source}")]
    Spawn {
        program: String,
        #[source]
        source: std::io::Error,
    },
    #[error("`{command}` failed: {detail}")]
    Failed { command: String, detail: String },
    #[error("{0} is not on a drive WSL can reach through /mnt")]
    UnmappablePath(PathBuf),
}

/// Runs `wsl.exe` and returns its stdout, failing on a non-zero exit.
pub fn run(args: &[&str]) -> Result<String, Error> {
    run_program("wsl.exe", args)
}

/// Runs a Windows console program the backend drives (`wsl.exe`, `schtasks.exe`).
pub fn run_program(program: &str, args: &[&str]) -> Result<String, Error> {
    let output = execute(program, args).map_err(|source| Error::Spawn {
        program: program.to_string(),
        source,
    })?;
    if !output.success {
        return Err(Error::Failed {
            command: format!("{program} {}", args.join(" ")),
            detail: failure_detail(&output.stderr, &output.stdout),
        });
    }
    Ok(decode_console(&output.stdout))
}

/// What the backend needs from a finished process.
struct Output {
    success: bool,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

/// The single place the backend starts a process, so tests can answer instead.
fn execute(program: &str, args: &[&str]) -> std::io::Result<Output> {
    #[cfg(test)]
    if let Some(output) = fake::reply(program, args) {
        return Ok(output);
    }
    let output = ProcessCommand::new(program).args(args).output()?;
    Ok(Output {
        success: output.status.success(),
        stdout: output.stdout,
        stderr: output.stderr,
    })
}

/// Scripted answers for `wsl.exe` and `schtasks.exe`, per test thread, so the
/// backend's orchestration runs under test on any host without touching WSL.
#[cfg(test)]
pub mod fake {
    use std::cell::RefCell;

    use super::Output;

    /// One scripted answer: `Ok(stdout)` exits 0, `Err(stderr)` exits 1.
    pub type Reply = Result<String, String>;
    type Handler = Box<dyn FnMut(&str) -> Reply>;

    thread_local! {
        static HANDLER: RefCell<Option<Handler>> = const { RefCell::new(None) };
        static CALLS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    }

    /// Answers every call on this thread until the guard drops. The handler
    /// sees `program arg arg...` joined by spaces.
    #[must_use]
    pub fn answer(handler: impl FnMut(&str) -> Reply + 'static) -> Guard {
        HANDLER.with(|slot| *slot.borrow_mut() = Some(Box::new(handler)));
        CALLS.with(|calls| calls.borrow_mut().clear());
        Guard
    }

    pub struct Guard;

    impl Guard {
        /// Every command line answered so far, in order.
        pub fn calls(&self) -> Vec<String> {
            CALLS.with(|calls| calls.borrow().clone())
        }
    }

    impl Drop for Guard {
        fn drop(&mut self) {
            HANDLER.with(|slot| *slot.borrow_mut() = None);
        }
    }

    pub(super) fn reply(program: &str, args: &[&str]) -> Option<Output> {
        let line = std::iter::once(program)
            .chain(args.iter().copied())
            .collect::<Vec<_>>()
            .join(" ");
        HANDLER.with(|slot| {
            let mut slot = slot.borrow_mut();
            let handler = slot.as_mut()?;
            CALLS.with(|calls| calls.borrow_mut().push(line.clone()));
            Some(match handler(&line) {
                Ok(stdout) => Output {
                    success: true,
                    stdout: stdout.into_bytes(),
                    stderr: Vec::new(),
                },
                Err(stderr) => Output {
                    success: false,
                    stdout: Vec::new(),
                    stderr: stderr.into_bytes(),
                },
            })
        })
    }
}

/// Runs `script` with `sh` as root inside the managed distribution. `--exec`
/// hands the script over as one argument instead of re-parsing it in a shell.
pub fn root_shell(script: &str) -> Result<String, Error> {
    run(&[
        "-d",
        DISTRO_NAME,
        "-u",
        "root",
        "--exec",
        "sh",
        "-c",
        script,
    ])
}

/// Names of the registered distributions, default first. `wsl.exe` exits
/// non-zero when there are none, which is an empty list rather than a failure.
pub fn distributions() -> Vec<String> {
    run(&["--list", "--quiet"])
        .map(|text| parse_list(&text))
        .unwrap_or_default()
}

/// Names of the distributions that are running right now.
pub fn running() -> Vec<String> {
    run(&["--list", "--running", "--quiet"])
        .map(|text| parse_list(&text))
        .unwrap_or_default()
}

/// Asking a stopped distribution anything boots it, so readiness checks look here first.
pub fn is_running(name: &str) -> bool {
    contains(&running(), name)
}

pub fn parse_list(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect()
}

/// WSL treats distribution names case-insensitively.
pub fn contains(names: &[String], name: &str) -> bool {
    names.iter().any(|listed| listed.eq_ignore_ascii_case(name))
}

/// `wsl.exe` writes UTF-16LE, while `cmd.exe` and Linux programs write bytes.
pub fn decode_console(bytes: &[u8]) -> String {
    let body = bytes.strip_prefix(&[0xFF, 0xFE]).unwrap_or(bytes);
    let utf16le = body.len() >= 2 && body.len().is_multiple_of(2) && body[1] == 0;
    if !utf16le {
        return String::from_utf8_lossy(body).into_owned();
    }
    let units: Vec<u16> = body
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    String::from_utf16_lossy(&units)
}

/// `wsl.exe` reports its own errors on stdout, the guest's on stderr.
fn failure_detail(stderr: &[u8], stdout: &[u8]) -> String {
    [stderr, stdout]
        .iter()
        .map(|bytes| decode_console(bytes).trim().to_string())
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("; ")
}

/// `C:\Users\dev\x` becomes `/mnt/c/Users/dev/x`, the path the distribution sees.
pub fn guest_path(path: &Path) -> Result<String, Error> {
    let unmappable = || Error::UnmappablePath(path.to_path_buf());
    let text = path.to_str().ok_or_else(unmappable)?;
    let (drive, rest) = text.split_once(":\\").ok_or_else(unmappable)?;
    let [letter] = drive.as_bytes() else {
        return Err(unmappable());
    };
    if !letter.is_ascii_alphabetic() {
        return Err(unmappable());
    }
    Ok(format!(
        "/mnt/{}/{}",
        letter.to_ascii_lowercase() as char,
        rest.replace('\\', "/")
    ))
}

/// Quotes a value for a POSIX shell script.
pub fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_utf16_wsl_output() {
        let mut bytes = vec![0xFF, 0xFE];
        for unit in "Debian\r\nfirecrab-debian\r\n".encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        assert_eq!(
            parse_list(&decode_console(&bytes)),
            ["Debian", "firecrab-debian"]
        );
    }

    #[test]
    fn decodes_plain_utf8_output() {
        assert_eq!(decode_console(b"Debian\n"), "Debian\n");
    }

    #[test]
    fn blank_lines_are_not_distributions() {
        assert!(parse_list("  \r\n\r\n").is_empty());
    }

    #[test]
    fn distribution_names_match_case_insensitively() {
        let listing = parse_list("Ubuntu\r\nFIRECRAB-DEBIAN\r\n");
        assert!(contains(&listing, DISTRO_NAME));
        assert!(!contains(&parse_list("Debian\r\n"), DISTRO_NAME));
    }

    #[test]
    fn failure_detail_joins_both_streams() {
        assert_eq!(failure_detail(b"guest\n", b""), "guest");
        assert_eq!(failure_detail(b"guest", b"wsl"), "guest; wsl");
    }

    #[test]
    fn windows_paths_map_into_the_distribution() {
        assert_eq!(
            guest_path(Path::new("C:\\Users\\dev\\firecrab\\a.tgz")).expect("mapped"),
            "/mnt/c/Users/dev/firecrab/a.tgz"
        );
        assert_eq!(
            guest_path(Path::new("D:\\lab\\x")).expect("mapped"),
            "/mnt/d/lab/x"
        );
    }

    #[test]
    fn unc_and_relative_paths_are_rejected() {
        assert!(guest_path(Path::new("\\\\server\\share\\x")).is_err());
        assert!(guest_path(Path::new("artifacts\\x.tgz")).is_err());
        assert!(guest_path(Path::new("12:\\x")).is_err());
    }

    #[test]
    fn a_missing_program_is_a_spawn_error() {
        let error = run_program("firecrab-no-such-program", &[]).expect_err("nothing to run");
        assert!(matches!(error, Error::Spawn { .. }));
    }

    #[test]
    fn a_failed_command_reports_its_line_and_output() {
        let _wsl = fake::answer(|_| Err("There is no distribution with the supplied name.".into()));
        let Err(Error::Failed { command, detail }) = run(&["--terminate", "x"]) else {
            panic!("a non-zero exit is a failure");
        };
        assert_eq!(command, "wsl.exe --terminate x");
        assert_eq!(detail, "There is no distribution with the supplied name.");
    }

    #[test]
    fn listings_come_from_wsl_and_an_error_is_an_empty_list() {
        let wsl = fake::answer(|line| match line {
            "wsl.exe --list --quiet" => Ok("Debian\r\nfirecrab-debian\r\n".into()),
            "wsl.exe --list --running --quiet" => Err("There are no running distributions.".into()),
            other => panic!("unexpected {other}"),
        });
        assert_eq!(distributions(), ["Debian", "firecrab-debian"]);
        assert!(!is_running(DISTRO_NAME));
        assert_eq!(wsl.calls().len(), 2);
    }

    #[test]
    fn root_shell_passes_the_script_as_one_argument() {
        let wsl = fake::answer(|_| Ok("ok\n".into()));
        assert_eq!(root_shell("echo 'a b'").expect("runs"), "ok\n");
        assert_eq!(
            wsl.calls(),
            ["wsl.exe -d firecrab-debian -u root --exec sh -c echo 'a b'"]
        );
    }

    #[test]
    fn shell_quote_survives_embedded_quotes() {
        assert_eq!(shell_quote("/mnt/c/it's"), "'/mnt/c/it'\\''s'");
    }
}
