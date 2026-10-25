//! Opt-in Linux coverage for the real private Docker runtime, without guest deployment.

use super::*;
use std::net::{Ipv4Addr, TcpListener};

const MARKER_PATH: &str = "/var/lib/firecrab/runtime-test-marker";
const API: &str = r#"
import http.server, pathlib, sys
path = pathlib.Path('/var/lib/firecrab/runtime-test-marker')
path.write_text(sys.argv[1])
class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        body = path.read_bytes()
        self.send_response(200)
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)
http.server.HTTPServer(('127.0.0.1', 5523), Handler).serve_forever()
"#;
const PROBE: &str = r#"
import fcntl, json, os, pathlib, stat
cgroups = pathlib.Path('/proc/self/cgroup').read_text().strip().splitlines()
assert len(cgroups) == 1, cgroups
cgroup = cgroups[0]
assert cgroup.startswith('0::/'), cgroup
assert '..' not in pathlib.PurePosixPath(cgroup[3:]).parts, cgroup
probe = pathlib.Path('/sys/fs/cgroup/firecrab-runtime-probe')
probe.mkdir()
probe.rmdir()
with open('/dev/kvm', 'r+b', buffering=0) as kvm:
    version = fcntl.ioctl(kvm.fileno(), 0xAE00, 0)
assert version == 12, version
assert stat.S_ISCHR(os.stat('/dev/net/tun').st_mode)
tun = os.open('/dev/net/tun', os.O_RDWR)
os.close(tun)
with open('/proc/sys/net/ipv4/ip_forward', 'r+') as forwarding:
    value = forwarding.read()
    forwarding.seek(0)
    forwarding.write(value)
namespaces = {name: os.readlink('/proc/self/ns/' + name)
              for name in ['net', 'pid', 'ipc', 'mnt', 'cgroup']}
print(json.dumps(namespaces))
"#;

/// One randomly named test runtime, with cleanup restricted by all ownership labels.
struct Runtime {
    _directory: tempfile::TempDir,
    instance: Instance,
    docker: Docker,
    name: String,
    volume: String,
    image: String,
    ports: Option<(TcpListener, TcpListener)>,
    cleaned: bool,
}

impl Runtime {
    fn new() -> io::Result<Self> {
        let directory = tempfile::tempdir()?;
        let id = uuid::Uuid::new_v4().simple().to_string()[..16].to_owned();
        let api = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let resource = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let instance = super::super::instance::test_instance(
            directory.path(),
            &id,
            api.local_addr()?.port(),
            resource.local_addr()?.port(),
        );
        fs::create_dir(instance.source.join("scripts"))?;
        for (path, source) in [
            ("install.sh", include_str!("../../../../install.sh")),
            (
                "scripts/firecrab-release.sh",
                include_str!("../../../../scripts/firecrab-release.sh"),
            ),
            (
                "scripts/install-firecracker.sh",
                include_str!("../../../../scripts/install-firecracker.sh"),
            ),
        ] {
            fs::write(instance.source.join(path), source)?;
        }
        let name = format!("firecrab-dev-{id}");
        Ok(Self {
            _directory: directory,
            instance,
            docker: Docker::local()?,
            volume: format!("{name}-data"),
            image: format!("{name}:runtime-v1"),
            name,
            ports: Some((api, resource)),
            cleaned: false,
        })
    }

    fn create(&self) -> io::Result<()> {
        let engine = self.docker.engine()?;
        eprintln!("[RUNTIME TEST] creating {}", self.name);
        create_runtime(
            &self.docker,
            &self.instance,
            &self.name,
            &self.volume,
            &engine,
            true,
        )?;
        let metadata = self
            .docker
            .inspect("container", &self.name)?
            .ok_or_else(|| failed("created test container is missing"))?;
        container_owned(&metadata, &self.instance, &self.name, &self.volume)?;
        storage_owned(&self.docker, &self.instance, &self.name, &self.volume, true)
    }

    fn exec(&self, args: &[&str]) -> io::Result<String> {
        let mut command = vec!["exec", &self.name];
        command.extend_from_slice(args);
        let output = self.docker.output(&command)?;
        if !output.status.success() {
            return Err(failed(format!(
                "{} exec failed: {}",
                self.name,
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        String::from_utf8(output.stdout).map_err(failed)
    }

    fn start(&mut self) -> io::Result<Value> {
        // Reserve both instances' host ports until Docker is about to bind them.
        self.ports.take();
        self.docker.checked(&["start", &self.name])?;
        let started = Instant::now();
        loop {
            // systemctl returns nonzero for the usable "degraded" state too.
            let state =
                self.docker
                    .output(&["exec", &self.name, "systemctl", "is-system-running"])?;
            let ready = matches!(
                String::from_utf8_lossy(&state.stdout).trim(),
                "running" | "degraded"
            );
            if ready
                && self
                    .exec(&["systemctl", "is-active", "firecrab-dev-publish.service"])
                    .is_ok_and(|state| state.trim() == "active")
            {
                break;
            }
            if started.elapsed() >= Duration::from_secs(60) {
                return Err(failed(format!(
                    "{} systemd and production API relay did not become ready",
                    self.name
                )));
            }
            thread::sleep(Duration::from_millis(250));
        }
        let namespaces: Value =
            serde_json::from_str(&self.exec(&["python3", "-c", PROBE])?).map_err(failed)?;
        self.docker.checked(&[
            "exec",
            "-d",
            &self.name,
            "python3",
            "-c",
            API,
            &self.instance.id,
        ])?;
        wait_api(&self.instance)?;
        self.require_marker()?;
        Ok(namespaces)
    }

    fn require_marker(&self) -> io::Result<()> {
        let client = reqwest::blocking::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(2))
            .build()
            .map_err(failed)?;
        let url = format!("http://127.0.0.1:{}/api/host", self.instance.api_port);
        let marker = client
            .get(url)
            .send()
            .and_then(reqwest::blocking::Response::error_for_status)
            .and_then(reqwest::blocking::Response::text)
            .map_err(failed)?;
        if marker != self.instance.id {
            return Err(failed(format!(
                "{} returned a foreign API marker",
                self.name
            )));
        }
        let saved = self.exec(&["cat", MARKER_PATH])?;
        if saved != marker {
            return Err(failed(format!(
                "{} lost its private volume marker",
                self.name
            )));
        }
        Ok(())
    }

    fn cleanup(&mut self) -> io::Result<()> {
        let mut errors = Vec::new();
        for (kind, name) in [
            ("container", self.name.as_str()),
            ("network", self.name.as_str()),
            ("volume", self.volume.as_str()),
            ("image", self.image.as_str()),
        ] {
            let result = (|| {
                let Some(metadata) = self.docker.inspect(kind, name)? else {
                    return Ok(());
                };
                owned(&metadata, kind, &self.instance)?;
                if kind == "network"
                    && metadata["Containers"]
                        .as_object()
                        .is_some_and(|containers| !containers.is_empty())
                {
                    return Err(failed(
                        "refusing to remove a test network with live members",
                    ));
                }
                eprintln!("[RUNTIME TEST] removing owned {kind} {name}");
                if kind == "container" {
                    self.docker.checked(&["rm", "--force", name])
                } else {
                    self.docker.checked(&[kind, "rm", name])
                }
            })();
            if let Err(error) = result {
                eprintln!("[RUNTIME TEST] cleanup {kind} {name}: {error}");
                errors.push(format!("{kind} {name}: {error}"));
            }
        }
        self.cleaned = errors.is_empty();
        if self.cleaned {
            Ok(())
        } else {
            Err(failed(errors.join("; ")))
        }
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        if self.cleaned {
            return;
        }
        // Dump only this test's labeled container before cleanup on setup/assertion failure.
        if let Ok(Some(metadata)) = self.docker.inspect("container", &self.name)
            && owned(&metadata, "container", &self.instance).is_ok()
            && let Ok(output) = self.docker.output(&["logs", "--tail", "100", &self.name])
        {
            eprintln!(
                "[RUNTIME TEST] {} logs:\n{}{}",
                self.name,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        if let Err(error) = self.cleanup() {
            eprintln!("[RUNTIME TEST] cleanup incomplete: {error}");
        }
    }
}

#[test]
#[ignore = "requires an explicitly selected local rootful Linux Docker engine and real KVM/TUN"]
fn private_docker_runtimes_keep_namespaces_data_and_stop_independent() -> io::Result<()> {
    let mut first = Runtime::new()?;
    let mut second = Runtime::new()?;
    if first.docker.endpoint != second.docker.endpoint {
        return Err(failed(
            "Docker context changed while selecting runtime fixtures",
        ));
    }
    first.create()?;
    second.create()?;
    let first_namespaces = first.start()?;
    let second_namespaces = second.start()?;
    for name in ["net", "pid", "ipc", "mnt", "cgroup"] {
        let host = fs::read_link(format!("/proc/self/ns/{name}"))?;
        assert_ne!(first_namespaces[name], second_namespaces[name], "{name}");
        for namespaces in [&first_namespaces, &second_namespaces] {
            assert_ne!(namespaces[name].as_str(), host.to_str(), "host {name}");
        }
    }
    assert_ne!(first.instance.id, second.instance.id);
    assert_ne!(first.volume, second.volume);
    run_with(
        &first.docker,
        &first.instance,
        Some(DevCommand::Stop),
        false,
        false,
        true,
    )?;
    let stopped = first
        .docker
        .inspect("container", &first.name)?
        .ok_or_else(|| failed("stopped runtime disappeared"))?;
    assert_eq!(stopped["State"]["Running"], false);
    storage_owned(
        &first.docker,
        &first.instance,
        &first.name,
        &first.volume,
        true,
    )?;
    second.require_marker()?;
    first.cleanup()?;
    second.require_marker()?;
    assert!(!first.instance.primary_home.exists());
    assert!(!second.instance.primary_home.exists());
    second.cleanup()
}
