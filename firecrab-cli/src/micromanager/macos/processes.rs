//! Match helpers by executable, owner and managed-home environment before signalling.
use std::ffi::{OsStr, c_void};
use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use super::super::report;
use super::lifecycle::Layout;

unsafe extern "C" {
    fn proc_pidpath(pid: i32, buffer: *mut c_void, size: u32) -> i32;
    fn sysctl(
        name: *mut i32,
        count: u32,
        old: *mut c_void,
        size: *mut usize,
        new: *mut c_void,
        new_size: usize,
    ) -> i32;
    fn getuid() -> u32;
    fn kill(pid: i32, signal: i32) -> i32;
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("could not inspect management helpers: {0}")]
    Inspect(#[from] io::Error),
    #[error("management helper {0} did not stop; no disk recovery was attempted")]
    StillRunning(i32),
}

struct Arguments {
    argv: Vec<Vec<u8>>,
    environment: Vec<Vec<u8>>,
}

fn parse_arguments(data: &[u8]) -> Option<Arguments> {
    let count = i32::from_ne_bytes(data.get(..4)?.try_into().ok()?);
    if !(1..=4096).contains(&count) {
        return None;
    }
    let mut fields = data[4..].split(|byte| *byte == 0);
    fields.next()?; // KERN_PROCARGS2 starts with the executable path and padding.
    let first = fields.find(|value| !value.is_empty())?;
    let mut argv = vec![first.to_vec()];
    for _ in 1..count {
        argv.push(fields.next()?.to_vec());
    }
    Some(Arguments {
        argv,
        environment: fields
            .filter(|value| !value.is_empty())
            .map(<[u8]>::to_vec)
            .collect(),
    })
}

fn environment<'a>(arguments: &'a Arguments, key: &[u8]) -> Option<&'a [u8]> {
    arguments
        .environment
        .iter()
        .find_map(|field| field.strip_prefix(key))
}

fn owns_home(arguments: &Arguments, expected: &Path) -> bool {
    if !arguments
        .argv
        .get(1)
        .is_some_and(|command| command == b"run" || command == b"provision")
    {
        return false;
    }
    let home = match environment(arguments, b"FIRECRAB_MICROMANAGER_HOME=") {
        Some(value) if !value.is_empty() => PathBuf::from(OsStr::from_bytes(value)),
        _ => match environment(arguments, b"HOME=") {
            Some(value) if !value.is_empty() => PathBuf::from(OsStr::from_bytes(value))
                .join("Library/Application Support/Firecrab/micromanager"),
            _ => return false,
        },
    };
    home.is_absolute() && fs::canonicalize(home).ok().as_deref() == Some(expected)
}

fn executable(pid: i32) -> Option<PathBuf> {
    let mut buffer = [0_u8; 4096];
    // libproc reads only the named process's executable path.
    let count = unsafe { proc_pidpath(pid, buffer.as_mut_ptr().cast(), buffer.len() as u32) };
    if count <= 0 {
        return None;
    }
    let path = buffer.split(|byte| *byte == 0).next()?;
    fs::canonicalize(OsStr::from_bytes(path)).ok()
}

fn arguments(pid: i32) -> io::Result<Option<Arguments>> {
    // These constants are CTL_KERN and KERN_PROCARGS2 from the macOS SDK.
    let mut name = [1, 49, pid];
    let mut size = 0;
    let result = unsafe {
        sysctl(
            name.as_mut_ptr(),
            3,
            std::ptr::null_mut(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    let mut data = vec![0_u8; size];
    let result = unsafe {
        sysctl(
            name.as_mut_ptr(),
            3,
            data.as_mut_ptr().cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    data.truncate(size);
    Ok(parse_arguments(&data))
}

fn helpers(layout: &Layout) -> Result<Vec<i32>, Error> {
    if !layout.helper_path().is_file() {
        return Ok(vec![]);
    }
    let helper = fs::canonicalize(layout.helper_path())?;
    let home = fs::canonicalize(&layout.managed_home)?;
    let output = Command::new("/bin/ps")
        .args(["-axo", "uid=,pid="])
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other("ps could not list process owners").into());
    }
    let uid = unsafe { getuid() };
    let mut matches = Vec::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let mut fields = line.split_whitespace();
        if fields.next().and_then(|value| value.parse::<u32>().ok()) != Some(uid) {
            continue;
        }
        let Some(pid) = fields
            .next()
            .and_then(|value| value.parse::<i32>().ok())
            .filter(|pid| *pid > 1)
        else {
            continue;
        };
        if executable(pid).as_deref() != Some(helper.as_path()) {
            continue;
        }
        match arguments(pid) {
            Ok(Some(arguments)) if owns_home(&arguments, &home) => matches.push(pid),
            Ok(_) => {}
            // A process disappearing while queried is already stopped.
            Err(_) if executable(pid).is_none() => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(matches)
}

pub fn stop_helpers(layout: &Layout) -> Result<(), Error> {
    let pending = helpers(layout)?;
    if pending.is_empty() {
        return Ok(());
    }
    report!("[STOP] remaining management helpers: {}", pending.len());
    for pid in pending {
        // Re-read identity immediately before every signal to reject a reused PID.
        if helpers(layout)?.contains(&pid) {
            unsafe {
                kill(pid, 15);
            }
        }
    }
    let started = Instant::now();
    let mut next_report = Duration::ZERO;
    loop {
        let pending = helpers(layout)?;
        if pending.is_empty() {
            return Ok(());
        }
        let elapsed = started.elapsed();
        if elapsed >= Duration::from_secs(25) {
            break;
        }
        if elapsed >= next_report {
            report!(
                "[WAIT] helper shutdown: {}s/25s; remaining={}",
                elapsed.as_secs(),
                pending.len()
            );
            next_report = elapsed + Duration::from_secs(10);
        }
        thread::sleep(Duration::from_millis(250));
    }
    for pid in helpers(layout)? {
        report!("[STOP] unresponsive management helper: {pid}");
        if helpers(layout)?.contains(&pid) {
            unsafe {
                kill(pid, 9);
            }
        }
    }
    let started = Instant::now();
    loop {
        let pending = helpers(layout)?;
        let Some(pid) = pending.first() else {
            return Ok(());
        };
        if started.elapsed() >= Duration::from_secs(5) {
            return Err(Error::StillRunning(*pid));
        }
        thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn process(home: &[u8], command: &[u8]) -> Arguments {
        let mut data = 2_i32.to_ne_bytes().to_vec();
        data.extend_from_slice(b"/bin/helper\0\0\0/bin/helper\0");
        data.extend_from_slice(command);
        data.extend_from_slice(b"\0FIRECRAB_MICROMANAGER_HOME=");
        data.extend_from_slice(home);
        data.extend_from_slice(b"\0OTHER=unrelated\0");
        parse_arguments(&data).unwrap()
    }

    #[test]
    fn only_vm_commands_in_the_same_managed_home_are_owned() {
        let root = tempfile::tempdir().unwrap();
        let expected = fs::canonicalize(root.path()).unwrap();
        assert!(owns_home(
            &process(expected.as_os_str().as_bytes(), b"run"),
            &expected
        ));
        assert!(owns_home(
            &process(expected.as_os_str().as_bytes(), b"provision"),
            &expected
        ));
        assert!(!owns_home(
            &process(expected.as_os_str().as_bytes(), b"doctor"),
            &expected
        ));
        let other = tempfile::tempdir().unwrap();
        assert!(!owns_home(
            &process(other.path().as_os_str().as_bytes(), b"run"),
            &expected
        ));
        assert!(!owns_home(&process(b"relative/path", b"run"), &expected));
    }

    #[test]
    fn native_process_matching_stops_only_the_helper_in_this_home() {
        let root = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let layout = Layout {
            install_dir: root.path().join("bin"),
            managed_home: root.path().join("home"),
        };
        fs::create_dir_all(&layout.install_dir).unwrap();
        fs::create_dir_all(&layout.managed_home).unwrap();
        let source = root.path().join("helper.c");
        fs::write(
            &source,
            "#include <unistd.h>\nint main(void) { for (;;) sleep(10); }\n",
        )
        .unwrap();
        assert!(
            Command::new("/usr/bin/cc")
                .arg(&source)
                .arg("-o")
                .arg(layout.helper_path())
                .status()
                .unwrap()
                .success()
        );
        let mut owned = Command::new(layout.helper_path())
            .arg("run")
            .env("FIRECRAB_MICROMANAGER_HOME", &layout.managed_home)
            .spawn()
            .unwrap();
        let mut foreign = Command::new(layout.helper_path())
            .arg("run")
            .env("FIRECRAB_MICROMANAGER_HOME", other.path())
            .spawn()
            .unwrap();
        let matches = helpers(&layout);
        let stopped = stop_helpers(&layout);
        let foreign_alive = foreign.try_wait().unwrap().is_none();
        let _ = owned.kill();
        let _ = foreign.kill();
        let _ = owned.wait();
        let _ = foreign.wait();
        assert!(matches.unwrap().contains(&(owned.id() as i32)));
        assert!(stopped.is_ok(), "{stopped:?}");
        assert!(
            foreign_alive,
            "another managed home's helper must be preserved"
        );
    }

    #[test]
    fn argument_parser_does_not_confuse_arguments_with_environment() {
        let arguments = process(b"/path with spaces/micromanager", b"run");
        assert_eq!(arguments.argv, [b"/bin/helper".to_vec(), b"run".to_vec()]);
        assert_eq!(
            environment(&arguments, b"FIRECRAB_MICROMANAGER_HOME="),
            Some(b"/path with spaces/micromanager".as_slice())
        );
        assert!(parse_arguments(&[]).is_none());
        assert!(parse_arguments(&(-1_i32).to_ne_bytes()).is_none());
    }
}
