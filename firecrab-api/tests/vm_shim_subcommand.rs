//! Runs the real `firecrab-api vm-shim` subcommand against a fake
//! Firecracker and talks to it over its socket with hand-built frames, so the
//! binary entry point and the wire format are both pinned.

use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

const HELLO: u8 = 1;
const OUTPUT: u8 = 2;
const EXITED: u8 = 3;
const TERMINATE: u8 = 17;

const FAKE_FIRECRACKER: &str = "#!/usr/bin/env python3
import signal, sys, time
signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))
print(\"booted\", flush=True)
while True:
    time.sleep(1)
";

struct Vm {
    _directory: tempfile::TempDir,
    runtime: PathBuf,
    shim: Child,
}

impl Drop for Vm {
    // A failed assertion must not leave a shim holding the test harness's
    // output pipes open; its Firecracker follows it (parent-death signal).
    fn drop(&mut self) {
        let _ = self.shim.kill();
        let _ = self.shim.wait();
    }
}

fn write_executable(path: &Path, body: &str) {
    let temporary = path.with_extension("tmp");
    {
        let mut file = fs::File::create(&temporary).unwrap();
        file.write_all(body.as_bytes()).unwrap();
        file.sync_all().unwrap();
    }
    fs::set_permissions(&temporary, fs::Permissions::from_mode(0o755)).unwrap();
    fs::rename(&temporary, path).unwrap();
}

fn start(firecracker_body: &str) -> Vm {
    start_with_mode(firecracker_body, 0o700)
}

/// Starts the shim on a runtime directory with `mode` (the API creates
/// them `0700`).
fn start_with_mode(firecracker_body: &str, mode: u32) -> Vm {
    // Short path: the shim socket must fit AF_UNIX's ~108-byte limit.
    let directory = tempfile::tempdir_in("/tmp").unwrap();
    let firecracker = directory.path().join("firecracker");
    write_executable(&firecracker, firecracker_body);
    let runtime = directory.path().join("vm/r/one");
    fs::create_dir_all(&runtime).unwrap();
    fs::set_permissions(&runtime, fs::Permissions::from_mode(mode)).unwrap();
    fs::write(runtime.join("fc.json"), "{}").unwrap();
    let shim = Command::new(env!("CARGO_BIN_EXE_firecrab-api"))
        .arg("vm-shim")
        .arg("--vm-id")
        .arg(uuid::Uuid::new_v4().to_string())
        .arg("--runtime-dir")
        .arg(&runtime)
        .arg("--firecracker")
        .arg(&firecracker)
        .arg("--stop-grace-ms")
        .arg("2000")
        .spawn()
        .unwrap();
    Vm {
        _directory: directory,
        runtime,
        shim,
    }
}

fn connect(runtime: &Path) -> UnixStream {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(stream) = UnixStream::connect(runtime.join("shim.sock")) {
            stream
                .set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            return stream;
        }
        assert!(Instant::now() < deadline, "the shim never accepted");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn read_frame(stream: &mut UnixStream) -> (u8, Vec<u8>) {
    let mut header = [0_u8; 5];
    stream.read_exact(&mut header).unwrap();
    let length = u32::from_be_bytes([header[1], header[2], header[3], header[4]]) as usize;
    let mut payload = vec![0_u8; length];
    stream.read_exact(&mut payload).unwrap();
    (header[0], payload)
}

fn wait_for(shim: &mut Child) -> std::process::ExitStatus {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = shim.try_wait().unwrap() {
            return status;
        }
        assert!(Instant::now() < deadline, "the shim never exited");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn the_vm_shim_subcommand_serves_a_vm_until_it_is_terminated() {
    let mut vm = start(FAKE_FIRECRACKER);
    let mut stream = connect(&vm.runtime);

    let (kind, payload) = read_frame(&mut stream);
    assert_eq!(kind, HELLO);
    let hello: serde_json::Value = serde_json::from_slice(&payload).unwrap();
    assert_eq!(hello["version"], 1);
    assert!(hello["vmm_pid"].as_u64().unwrap() > 0);

    let mut console = String::new();
    while !console.contains("booted") {
        let (kind, payload) = read_frame(&mut stream);
        assert_eq!(kind, OUTPUT);
        console.push_str(&String::from_utf8_lossy(&payload));
    }

    stream.write_all(&[TERMINATE, 0, 0, 0, 0]).unwrap();
    let exited = loop {
        let (kind, payload) = read_frame(&mut stream);
        if kind == EXITED {
            break serde_json::from_slice::<serde_json::Value>(&payload).unwrap();
        }
    };
    assert_eq!(exited["code"], 0);

    assert!(wait_for(&mut vm.shim).success());
    assert!(vm.runtime.join("exit.json").exists());
    assert!(
        fs::read_to_string(vm.runtime.join("console.log"))
            .unwrap()
            .contains("booted")
    );
    assert!(!vm.runtime.join("shim.sock").exists());
}

#[test]
fn sigterm_to_the_shim_stops_its_vm_cleanly() {
    let mut vm = start(FAKE_FIRECRACKER);
    let mut stream = connect(&vm.runtime);
    let mut console = String::new();
    while !console.contains("booted") {
        let (_, payload) = read_frame(&mut stream);
        console.push_str(&String::from_utf8_lossy(&payload));
    }

    // SAFETY: signals our own child.
    unsafe {
        libc::kill(vm.shim.id() as i32, libc::SIGTERM);
    }

    assert!(wait_for(&mut vm.shim).success());
    let recorded: serde_json::Value =
        serde_json::from_slice(&fs::read(vm.runtime.join("exit.json")).unwrap()).unwrap();
    assert_eq!(recorded["code"], 0);
}

/// Real Firecracker does not handle SIGTERM: it dies of it. A stop that was
/// asked for must still end the shim (and so its systemd unit) successfully.
#[test]
fn a_requested_stop_exits_zero_even_when_sigterm_kills_the_vmm() {
    let mut vm = start(
        "#!/usr/bin/env python3
import sys, time
print(\"booted\", flush=True)
while True:
    time.sleep(1)
",
    );
    let mut stream = connect(&vm.runtime);
    let mut console = String::new();
    while !console.contains("booted") {
        let (_, payload) = read_frame(&mut stream);
        console.push_str(&String::from_utf8_lossy(&payload));
    }

    // SAFETY: signals our own child.
    unsafe {
        libc::kill(vm.shim.id() as i32, libc::SIGTERM);
    }

    assert!(wait_for(&mut vm.shim).success());
    let recorded: serde_json::Value =
        serde_json::from_slice(&fs::read(vm.runtime.join("exit.json")).unwrap()).unwrap();
    assert_eq!(recorded["signal"], libc::SIGTERM);
    assert_eq!(recorded["stop_requested"], true);
}

#[test]
fn a_vm_that_crashes_makes_the_shim_fail() {
    let mut vm = start("#!/bin/sh\nexit 3\n");
    let status = wait_for(&mut vm.shim);
    assert_eq!(status.code(), Some(1));
    let recorded: serde_json::Value =
        serde_json::from_slice(&fs::read(vm.runtime.join("exit.json")).unwrap()).unwrap();
    assert_eq!(recorded["code"], 3);
}

#[test]
fn bad_arguments_are_a_usage_error() {
    let status = Command::new(env!("CARGO_BIN_EXE_firecrab-api"))
        .arg("vm-shim")
        .arg("--vm-id")
        .arg("not-a-uuid")
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(2));
}

#[test]
fn a_runtime_directory_others_can_read_is_refused_before_firecracker_starts() {
    let mut vm = start_with_mode(FAKE_FIRECRACKER, 0o755);

    assert!(!wait_for(&mut vm.shim).success());
    let reason = fs::read_to_string(vm.runtime.join("shim.err")).unwrap();
    assert!(reason.contains("runtime directory"), "{reason}");
    assert!(!vm.runtime.join("console.log").exists());
}
