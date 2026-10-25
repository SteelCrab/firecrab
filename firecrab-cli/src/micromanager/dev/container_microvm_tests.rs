//! Real MicroVM checks inside the source acceptance test's two owned containers.

use super::*;
use reqwest::{Method, StatusCode, blocking::Client};
use serde_json::json;
use uuid::Uuid;

const ALIAS: &str = "alpine-3.24.1";
const VERSION: &str = "alpine-3.24.1-v5";
const CIDR: &str = "10.231.42.0/24";
const DNS_FIXTURE: &str = r#"
from pathlib import Path
with Path('/etc/hosts').open('a') as hosts:
    hosts.write('\n192.0.2.42 example.com\n')
"#;
const PROCESS: &str = r#"
import json, pathlib, subprocess, sys, uuid
vm = uuid.UUID(sys.argv[1])
unit = 'firecrab-vm-' + vm.hex + '.service'
shim = int(subprocess.check_output(['systemctl', 'show', '--property=MainPID', '--value', unit], text=True).strip())
assert shim > 1, (unit, shim)
def process(pid):
    directory = pathlib.Path('/proc') / str(pid)
    fields = (directory / 'stat').read_text().rsplit(')', 1)[1].split()
    assert fields[0] != 'Z', (pid, fields[0])
    argv = [part.decode() for part in (directory / 'cmdline').read_bytes().split(b'\0') if part]
    return {'pid': pid, 'starttime': fields[19]}, argv
identity, argv = process(shim)
assert 'vm-shim' in argv, argv
assert uuid.UUID(argv[argv.index('--vm-id') + 1]) == vm, argv
children = set()
for child_file in pathlib.Path('/proc', str(shim), 'task').glob('*/children'):
    try:
        children.update(int(pid) for pid in child_file.read_text().split())
    except FileNotFoundError:
        pass
firecrackers = []
for child in children:
    try:
        child_identity, child_argv = process(child)
    except (FileNotFoundError, ProcessLookupError):
        continue
    if not child_argv or pathlib.Path(child_argv[0]).name != 'firecracker':
        continue
    for option, filename in [('--api-sock', 'fc.sock'), ('--config-file', 'fc.json')]:
        path = pathlib.PurePosixPath(child_argv[child_argv.index(option) + 1])
        assert vm.hex in path.parts and path.name == filename, (vm, child_argv)
    firecrackers.append(child_identity)
assert len(firecrackers) == 1, (unit, children, firecrackers)
print(json.dumps({'unit': unit, 'shim': identity, 'firecracker': firecrackers[0]}))
"#;
const SSH: &str = r#"
import socket, sys, time
deadline = time.monotonic() + float(sys.argv[2])
last = 'no SSH banner'
while time.monotonic() < deadline:
    try:
        with socket.create_connection((sys.argv[1], 22), timeout=2) as connection:
            connection.settimeout(2)
            banner = connection.recv(255)
            if banner.startswith(b'SSH-'):
                print(banner.decode(errors='replace').strip())
                break
            last = repr(banner)
    except OSError as error:
        last = str(error)
    time.sleep(0.25)
else:
    raise SystemExit('guest SSH did not become ready: ' + last)
"#;

/// HTTP calls are pinned to this runtime's assigned loopback API and one scenario deadline.
struct Api {
    client: Client,
    base: String,
    deadline: Instant,
}

impl Api {
    fn new(runtime: &Runtime, deadline: Instant) -> io::Result<Self> {
        Ok(Self {
            client: Client::builder()
                .no_proxy()
                .connect_timeout(Duration::from_secs(5))
                .build()
                .map_err(failed)?,
            base: format!("http://127.0.0.1:{}", runtime.instance.api_port),
            deadline,
        })
    }

    fn call(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
        accepted: &[StatusCode],
        timeout: Duration,
    ) -> io::Result<Value> {
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(failed(format!(
                "MicroVM scenario deadline reached before {method} {path}"
            )));
        }
        let mut request = self
            .client
            .request(method.clone(), format!("{}{path}", self.base))
            .timeout(timeout.min(remaining));
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().map_err(failed)?;
        let status = response.status();
        let bytes = response.bytes().map_err(failed)?;
        if !accepted.contains(&status) {
            return Err(failed(format!(
                "{method} {path} returned {status}: {}",
                String::from_utf8_lossy(&bytes)
                    .chars()
                    .take(4096)
                    .collect::<String>()
            )));
        }
        if bytes.is_empty() || status == StatusCode::NOT_FOUND {
            return Ok(Value::Null);
        }
        serde_json::from_slice(&bytes).map_err(failed)
    }

    fn get(&self, path: &str) -> io::Result<Value> {
        self.call(
            Method::GET,
            path,
            None,
            &[StatusCode::OK],
            Duration::from_secs(10),
        )
    }

    fn image_job(&self, action: &str) -> io::Result<()> {
        let path = format!("/api/images/{ALIAS}/{action}");
        let mut job = self.call(
            Method::POST,
            &path,
            None,
            &[StatusCode::OK, StatusCode::ACCEPTED],
            Duration::from_secs(30),
        )?;
        let deadline = Instant::now() + Duration::from_secs(600);
        loop {
            match job["status"].as_str() {
                Some("succeeded") => return Ok(()),
                Some("failed") => return Err(failed(format!("image {action} failed: {job}"))),
                Some("idle" | "running") => {}
                _ => return Err(failed(format!("invalid image {action} status: {job}"))),
            }
            if Instant::now() >= deadline {
                return Err(failed(format!("image {action} timed out: {job}")));
            }
            thread::sleep(Duration::from_millis(250));
            job = self.get(&path)?;
        }
    }

    fn install_image(&self) -> io::Result<Value> {
        let path = format!("/api/images/{ALIAS}");
        let mut image = self.get(&path)?;
        if image["installed"] != true {
            self.image_job("package")?;
            self.image_job("install")?;
            image = self.get(&path)?;
        }
        assert_eq!(image["alias"], ALIAS, "{image}");
        assert_eq!(image["version"], VERSION, "{image}");
        assert_eq!(image["installed"], true, "{image}");
        let mut hashes = json!({ "version": VERSION });
        for name in ["kernelSha256", "rootfsSha256", "initrdSha256"] {
            let digest = image[name]
                .as_str()
                .ok_or_else(|| failed(format!("missing image {name}: {image}")))?;
            assert!(
                digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit()),
                "{name}: {digest}"
            );
            hashes[name] = digest.into();
        }
        Ok(hashes)
    }
}

/// One VM and network returned by this API, with namespace-local process identity.
pub(super) struct MicroVm<'a> {
    runtime: &'a Runtime,
    api: Api,
    network: Uuid,
    id: Option<Uuid>,
    ipv4: String,
    mac: String,
    image: Value,
    process: Option<Value>,
    cleaned: bool,
}

fn response_id(value: &Value) -> io::Result<Uuid> {
    value["id"]
        .as_str()
        .ok_or_else(|| failed(format!("API response has no id: {value}")))?
        .parse()
        .map_err(failed)
}

impl<'a> MicroVm<'a> {
    pub(super) fn boot(runtime: &'a Runtime, deadline: Instant) -> io::Result<Self> {
        eprintln!(
            "[RUNTIME TEST] preparing {VERSION} MicroVM in {}",
            runtime.name
        );
        let api = Api::new(runtime, deadline)?;
        let image = api.install_image()?;
        // This file belongs to the selected container. dnsmasq uses its local
        // hosts entry to satisfy guest DNS readiness without internet egress.
        runtime.exec(&["python3", "-c", DNS_FIXTURE])?;
        let network = api.call(
            Method::POST,
            "/api/micro-networks",
            Some(json!({
                "name": format!("rt-net-{}", runtime.instance.id),
                "subnetCidr": CIDR, "internetEnabled": false,
            })),
            &[StatusCode::CREATED],
            Duration::from_secs(30),
        )?;
        assert_eq!(network["subnetCidr"], CIDR, "{network}");
        assert_eq!(network["internetEnabled"], false, "{network}");
        let mut vm = Self {
            runtime,
            api,
            network: response_id(&network)?,
            id: None,
            ipv4: String::new(),
            mac: String::new(),
            image,
            process: None,
            cleaned: false,
        };
        let created = vm.api.call(
            Method::POST,
            "/api/vms",
            Some(json!({
                "name": format!("rt-vm-{}", runtime.instance.id), "template": ALIAS,
                "cpu": 1, "ram": 256, "diskGb": 1, "microNetworkId": vm.network,
                "egressPolicy": "isolated",
            })),
            &[StatusCode::CREATED],
            Duration::from_secs(30),
        )?;
        let id = response_id(&created)?;
        vm.id = Some(id);
        vm.api.call(
            Method::POST,
            &format!("/api/vms/{id}/start"),
            None,
            &[StatusCode::OK, StatusCode::ACCEPTED],
            Duration::from_secs(300),
        )?;
        let ready = Instant::now() + Duration::from_secs(300);
        loop {
            let detail = vm.api.get(&format!("/api/vms/{id}"))?;
            if detail["state"] == "running" {
                vm.ipv4 = detail["ipv4"]
                    .as_str()
                    .ok_or_else(|| failed(format!("running VM has no IPv4: {detail}")))?
                    .to_owned();
                vm.mac = detail["mac"]
                    .as_str()
                    .ok_or_else(|| failed(format!("running VM has no MAC: {detail}")))?
                    .to_owned();
                assert_eq!(detail["templateVersion"], VERSION, "{detail}");
                assert!(vm.ipv4.starts_with("10.231.42."), "{detail}");
                let log = vm.api.get(&format!("/api/vms/{id}/log"))?;
                if log["consoleLog"]
                    .as_str()
                    .is_some_and(|log| log.contains("FIRECRAB_NETWORK_READY"))
                {
                    break;
                }
            }
            if Instant::now() >= ready {
                return Err(failed(format!("VM {id} did not become ready: {detail}")));
            }
            thread::sleep(Duration::from_millis(250));
        }
        vm.runtime.exec(&["python3", "-c", SSH, &vm.ipv4, "60"])?;
        vm.process = Some(vm.process_identity()?);
        vm.require_running()?;
        eprintln!(
            "[RUNTIME TEST] {} VM {id} ready with live SSH",
            runtime.name
        );
        Ok(vm)
    }

    fn id(&self) -> Uuid {
        self.id.expect("a booted test VM has an id")
    }

    fn process_identity(&self) -> io::Result<Value> {
        serde_json::from_str(&self.runtime.exec(&[
            "python3",
            "-c",
            PROCESS,
            &self.id().to_string(),
        ])?)
        .map_err(failed)
    }

    pub(super) fn require_same_image(&self, other: &MicroVm<'_>) {
        assert_eq!(
            self.image, other.image,
            "A/B installed different pinned image bytes"
        );
    }

    pub(super) fn require_absent_from(&self, other: &Runtime) -> io::Result<()> {
        assert_ne!(self.runtime.instance.id, other.instance.id);
        Api::new(other, self.api.deadline)?.call(
            Method::GET,
            &format!("/api/vms/{}", self.id()),
            None,
            &[StatusCode::NOT_FOUND],
            Duration::from_secs(10),
        )?;
        Ok(())
    }

    /// Require the same live VM, shim, Firecracker process, console marker, and SSH listener.
    pub(super) fn require_running(&self) -> io::Result<()> {
        let id = self.id();
        let detail = self.api.get(&format!("/api/vms/{id}"))?;
        assert_eq!(response_id(&detail)?, id, "{detail}");
        assert_eq!(detail["state"], "running", "{detail}");
        assert_eq!(detail["ipv4"], self.ipv4, "{detail}");
        assert_eq!(detail["mac"], self.mac, "{detail}");
        assert_eq!(self.process.as_ref().unwrap(), &self.process_identity()?);
        let log = self.api.get(&format!("/api/vms/{id}/log"))?;
        assert!(
            log["consoleLog"]
                .as_str()
                .is_some_and(|log| log.contains("FIRECRAB_NETWORK_READY")),
            "{log}"
        );
        self.runtime
            .exec(&["python3", "-c", SSH, &self.ipv4, "15"])?;
        assert_eq!(
            self.runtime.exec(&["cat", MARKER_PATH])?,
            self.runtime.instance.id
        );
        Ok(())
    }

    pub(super) fn require_persisted(&self) -> io::Result<()> {
        let detail = self.api.get(&format!("/api/vms/{}", self.id()))?;
        assert_eq!(response_id(&detail)?, self.id(), "{detail}");
        assert_eq!(detail["templateVersion"], VERSION, "{detail}");
        Ok(())
    }

    /// Cleanup only returned UUIDs; container ownership cleanup remains the final backstop.
    pub(super) fn cleanup(&mut self) -> io::Result<()> {
        if self.cleaned {
            return Ok(());
        }
        let api = Api::new(self.runtime, Instant::now() + Duration::from_secs(180))?;
        if let Some(id) = self.id {
            let stopped = Instant::now() + Duration::from_secs(120);
            loop {
                let detail = api.call(
                    Method::GET,
                    &format!("/api/vms/{id}"),
                    None,
                    &[StatusCode::OK, StatusCode::NOT_FOUND],
                    Duration::from_secs(10),
                )?;
                if detail.is_null() {
                    break;
                }
                if response_id(&detail)? != id {
                    return Err(failed(format!(
                        "foreign VM response during cleanup: {detail}"
                    )));
                }
                match detail["state"].as_str() {
                    Some("created" | "stopped" | "error") => break,
                    Some("running" | "pausing" | "paused" | "resuming") => {
                        api.call(
                            Method::POST,
                            &format!("/api/vms/{id}/stop"),
                            None,
                            &[
                                StatusCode::OK,
                                StatusCode::ACCEPTED,
                                StatusCode::NO_CONTENT,
                                StatusCode::NOT_FOUND,
                                StatusCode::CONFLICT,
                            ],
                            stopped.saturating_duration_since(Instant::now()),
                        )?;
                    }
                    Some("starting" | "stopping") => {}
                    _ => return Err(failed(format!("invalid VM state during cleanup: {detail}"))),
                }
                if Instant::now() >= stopped {
                    return Err(failed(format!("VM {id} did not stop before cleanup")));
                }
                thread::sleep(Duration::from_millis(250));
            }
            api.call(
                Method::DELETE,
                &format!("/api/vms/{id}"),
                None,
                &[StatusCode::NO_CONTENT, StatusCode::NOT_FOUND],
                Duration::from_secs(30),
            )?;
        }
        api.call(
            Method::DELETE,
            &format!("/api/micro-networks/{}", self.network),
            None,
            &[StatusCode::NO_CONTENT, StatusCode::NOT_FOUND],
            Duration::from_secs(15),
        )?;
        self.cleaned = true;
        Ok(())
    }
}

impl Drop for MicroVm<'_> {
    fn drop(&mut self) {
        if self.cleaned {
            return;
        }
        if let Some(id) = self.id {
            if let Ok(api) = Api::new(self.runtime, Instant::now() + Duration::from_secs(15))
                && let Ok(log) = api.get(&format!("/api/vms/{id}/log"))
            {
                eprintln!(
                    "[RUNTIME TEST] {} VM {id} console: {log}",
                    self.runtime.name
                );
            }
            let unit = format!("firecrab-vm-{}.service", id.simple());
            if let Ok(journal) = self.runtime.exec(&[
                "journalctl",
                "--no-pager",
                "-n",
                "60",
                "-u",
                &unit,
                "-u",
                "firecrab-api",
                "-u",
                "firecrab-helper",
            ]) {
                eprintln!(
                    "[RUNTIME TEST] {} VM journal:\n{journal}",
                    self.runtime.name
                );
            }
        }
        if let Err(error) = self.cleanup() {
            eprintln!("[RUNTIME TEST] owned VM/network cleanup incomplete: {error}");
        }
    }
}
