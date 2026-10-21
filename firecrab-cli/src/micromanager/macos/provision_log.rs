use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::PathBuf;

use super::super::report;

const MAX_LINE_BYTES: usize = 64 * 1024;

pub(super) struct Output {
    path: PathBuf,
    offset: u64,
    pending: Vec<u8>,
    warned: bool,
}

impl Output {
    pub fn new(path: PathBuf) -> io::Result<Self> {
        // Guest logging appends. Do not replay an earlier installation.
        let offset = match fs::metadata(&path) {
            Ok(metadata) => metadata.len(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => 0,
            Err(error) => return Err(error),
        };
        Ok(Self {
            path,
            offset,
            pending: Vec::new(),
            warned: false,
        })
    }

    pub fn print_new(&mut self, finished: bool) {
        if let Err(error) = self.read_new(finished, |line| report!("{line}"))
            && !self.warned
        {
            report!(
                "[WARNING] could not display installation output from {}: {error}",
                self.path.display()
            );
            self.warned = true;
        }
    }

    fn read_new(&mut self, finished: bool, mut emit: impl FnMut(&str)) -> io::Result<()> {
        let mut file = match File::open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        };
        let length = file.metadata()?.len();
        if length < self.offset {
            self.offset = 0;
            self.pending.clear();
        }
        file.seek(SeekFrom::Start(self.offset))?;
        // Read a snapshot of the appended bytes so logging cannot hold up VM polling.
        let mut reader = BufReader::new(file.take(length - self.offset));
        loop {
            let remaining = (MAX_LINE_BYTES - self.pending.len()) as u64;
            let count = (&mut reader)
                .take(remaining)
                .read_until(b'\n', &mut self.pending)?;
            if count == 0 {
                break;
            }
            self.offset += count as u64;
            if self.pending.ends_with(b"\n") || self.pending.len() == MAX_LINE_BYTES {
                emit_installation_line(&self.pending, &mut emit);
                self.pending.clear();
            }
        }
        if finished && !self.pending.is_empty() {
            emit_installation_line(&self.pending, &mut emit);
            self.pending.clear();
        }
        Ok(())
    }
}

fn emit_installation_line(bytes: &[u8], emit: &mut impl FnMut(&str)) {
    // Keep bash's `set -x` trace in the log file, out of installation output.
    if bytes.starts_with(b"+") {
        return;
    }
    let line = String::from_utf8_lossy(bytes);
    emit(line.trim_end_matches(['\r', '\n']));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::process::{Command, Stdio};
    use std::time::Duration;

    #[test]
    fn only_new_complete_lines_are_displayed_without_losing_split_utf8() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("provision.log");
        fs::write(&path, "previous install\n").unwrap();
        let mut output = Output::new(path.clone()).unwrap();
        let mut writer = fs::OpenOptions::new().append(true).open(&path).unwrap();
        let mut lines = Vec::new();
        writer.write_all(b"Get:1 Debian\n\xec\x84").unwrap();
        output
            .read_new(false, |line| lines.push(line.to_string()))
            .unwrap();
        assert_eq!(lines, ["Get:1 Debian"]);
        writer.write_all(b"\xa4\xec\xb9\x98\r\n").unwrap();
        output
            .read_new(false, |line| lines.push(line.to_string()))
            .unwrap();
        output
            .read_new(false, |line| lines.push(line.to_string()))
            .unwrap();
        assert_eq!(lines, ["Get:1 Debian", "설치"]);
    }

    #[test]
    fn shell_traces_stay_in_the_file_and_final_errors_are_displayed() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("provision.log");
        let mut output = Output::new(path.clone()).unwrap();
        output
            .read_new(false, |_| panic!("log is not created yet"))
            .unwrap();
        let log = "+ apt-get install\n++ uname -r\nUnpacking curl...\nSetting up curl...\nE: download failed";
        fs::write(&path, log).unwrap();
        let mut lines = Vec::new();
        output
            .read_new(true, |line| lines.push(line.to_string()))
            .unwrap();
        assert_eq!(
            lines,
            [
                "Unpacking curl...",
                "Setting up curl...",
                "E: download failed"
            ]
        );
        assert_eq!(fs::read_to_string(path).unwrap(), log);
    }

    #[test]
    fn truncated_logs_restart_at_the_beginning() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("provision.log");
        fs::write(&path, "old installation output\n").unwrap();
        let mut output = Output::new(path.clone()).unwrap();
        fs::write(&path, "new\n").unwrap();
        let mut lines = Vec::new();
        output
            .read_new(false, |line| lines.push(line.to_string()))
            .unwrap();
        assert_eq!(lines, ["new"]);
    }

    #[test]
    fn installation_output_arrives_before_helper_exit_and_includes_its_final_error() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("provision.log");
        let mut output = Output::new(path.clone()).unwrap();
        let mut child = Command::new("/bin/sh")
            .args([
                "-c",
                "printf 'Unpacking curl...\\n'; sleep 1; printf 'E: install failed'; exit 7",
            ])
            .stdout(Stdio::from(File::create(path).unwrap()))
            .spawn()
            .unwrap();
        let mut lines = Vec::new();
        let run = super::super::wait_for_provisioning(
            &mut child,
            || {
                output
                    .read_new(false, |line| lines.push(line.to_string()))
                    .unwrap();
                false
            },
            Duration::from_secs(30),
            Duration::from_secs(30),
        )
        .unwrap();
        assert_eq!(lines, ["Unpacking curl..."]);
        assert!(
            matches!(run, super::super::ProvisionRun::Exited(status) if status.code() == Some(7))
        );
        output
            .read_new(true, |line| lines.push(line.to_string()))
            .unwrap();
        assert_eq!(lines, ["Unpacking curl...", "E: install failed"]);
    }
}
