//! Closing the caller's input cancels only its guest deployment process group.

use std::fs::File;
use std::io::{self, Write};
use std::process::{Command, ExitStatus, Stdio};

pub fn runner() -> String {
    include_str!("../../../../scripts/micromanager/dev-session.sh").replace("\r\n", "\n")
}

pub enum Input {
    File(File),
    Session(File),
}

impl Input {
    pub fn status(self, command: &mut Command) -> io::Result<ExitStatus> {
        let mut script = match self {
            Self::File(file) => return command.stdin(Stdio::from(file)).status(),
            Self::Session(script) => script,
        };
        let mut child = command.stdin(Stdio::piped()).spawn()?;
        // Child::wait closes child.stdin. Keep the writer outside Child so EOF
        // means cancellation, rather than the normal start of the wait.
        let mut input = child.stdin.take().expect("piped session input");
        if let Err(error) = io::copy(&mut script, &mut input).and_then(|_| input.write_all(&[0])) {
            drop(input);
            let _ = child.wait();
            return Err(error);
        }
        let status = child.wait();
        drop(input);
        status
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::io::{Read, Seek};

    fn input(bytes: &[u8]) -> File {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(bytes).unwrap();
        file.rewind().unwrap();
        file
    }

    #[test]
    fn session_preserves_arguments_and_exit_status_while_input_stays_open() {
        let output = tempfile::NamedTempFile::new().unwrap();
        let argument = "literal 'quoted' $argument 한글";
        let status = Input::Session(input(b"printf '%s' \"$2\" > \"$1\"; sleep 0.05; exit 7"))
            .status(Command::new("bash").args([
                "-c",
                &runner(),
                "--",
                output.path().to_str().unwrap(),
                argument,
            ]))
            .unwrap();
        assert_eq!(status.code(), Some(7));
        assert_eq!(std::fs::read_to_string(output.path()).unwrap(), argument);
    }

    #[test]
    fn file_input_streams_binary_archives_without_session_framing() {
        let bytes = b"source archive\0\xff\x01";
        let mut output = tempfile::tempfile().unwrap();
        let status = Input::File(input(bytes))
            .status(Command::new("cat").stdout(output.try_clone().unwrap()))
            .unwrap();
        assert!(status.success());
        output.rewind().unwrap();
        let mut received = Vec::new();
        output.read_to_end(&mut received).unwrap();
        assert_eq!(received, bytes);
    }

    #[test]
    fn a_transport_that_exits_before_reading_reports_broken_input() {
        let error = Input::Session(input(&vec![b'x'; 1024 * 1024]))
            .status(Command::new("bash").args(["-c", "exit 23"]))
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
    }
}
