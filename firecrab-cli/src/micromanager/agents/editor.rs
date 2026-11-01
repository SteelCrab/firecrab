//! Web VS Code runs in the task's VM. The only browser listener is a pinned SSH loopback forward.
use super::*;
use base64::{Engine, engine::general_purpose::STANDARD};
use std::net::{Ipv4Addr, TcpListener};

const BASE: &str = "/root/.local/share/firecrab/web-vscode";
const INSTALL: &str = "/root/.local/share/firecrab/web-vscode-install";
const INSTALL_WORKER: &str = r#"
worker_alive() {
  lock=/root/.local/share/firecrab/web-vscode-install/lock
  [ -d "$lock" ] || return 1
  [ -f "$lock/boot" ] && [ "$(cat "$lock/boot")" = "$(cat /proc/sys/kernel/random/boot_id)" ] || return 1
  if [ ! -f "$lock/pid" ] || [ ! -f "$lock/start" ]; then
    [ "$(($(date +%s) - $(stat -c %Y "$lock")))" -lt 15 ]; return
  fi
  worker=$(cat "$lock/pid")
  case "$worker" in *[!0-9]*|'') return 1;; esac
  [ -r "/proc/$worker/stat" ] || return 1
  [ "$(sed 's/^.*) //' /proc/$worker/stat | awk '{print $20}')" = "$(cat "$lock/start")" ] && [ "$(sed 's/^.*) //' /proc/$worker/stat | awk '{print $1}')" != Z ]
}
"#;
const BINARY: &str = "/root/.local/lib/firecrab/openvscode/bin/openvscode-server";

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Forward {
    vm_id: Uuid,
    agent_id: Uuid,
    boot: String,
    remote_pid: u32,
    remote_port: u16,
    local_port: u16,
    pid: u32,
    identity: String,
    token: String,
}
fn fields(text: &str) -> std::collections::HashMap<&str, &str> {
    text.lines().filter_map(|l| l.split_once(':')).collect()
}
fn decoded(value: Option<&&str>) -> String {
    value
        .and_then(|v| STANDARD.decode(v).ok())
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default()
}

impl Runtime {
    fn editor_workspace(&self, id: Uuid) -> Result<(Agent, &guest::Guest), Error> {
        let guest = self
            .guest
            .as_ref()
            .ok_or_else(|| invalid("Select a MicroVM first."))?;
        let agent = self.agent(id)?;
        if !self.owns_workspace(&agent) {
            return Err(invalid(
                "Web VS Code requires this agent's project inside the selected MicroVM.",
            ));
        }
        Ok((agent, guest))
    }
    fn editor_record(&self, id: Uuid) -> PathBuf {
        self.home.join("editors").join(format!("{id}.json"))
    }
    fn remote_editor(&self, id: Uuid) -> Result<String, Error> {
        let (agent, guest) = self.editor_workspace(id)?;
        guest.shell(&format!(r#"set -eu
{INSTALL_WORKER}
test -d {cwd}
printf 'BOOT:'; cat /proc/sys/kernel/random/boot_id
if [ -x {BINARY} ]; then printf 'INSTALLED:yes\n'; fi
if [ -f {INSTALL}/state ]; then
  state=$(cat {INSTALL}/state)
  if [ "$state" = installing ] && ! worker_alive; then state=interrupted; fi
  printf 'INSTALL:%s\n' "$state"
fi
if [ -f {INSTALL}/phase ]; then printf 'PHASE:'; cat {INSTALL}/phase; printf '\n'; fi
if [ -f {INSTALL}/log ]; then printf 'LOG:'; tail -c 5000 {INSTALL}/log | base64 -w0; printf '\n'; fi
root={BASE}/{id}
if [ -f "$root/boot" ] && [ "$(cat "$root/boot")" = "$(cat /proc/sys/kernel/random/boot_id)" ] && [ -f "$root/pid" ]; then
  pid=$(cat "$root/pid")
  case "$pid" in *[!0-9]*|'') exit 1;; esac
  if [ -r "/proc/$pid/stat" ] && [ "$(sed 's/^.*) //' /proc/$pid/stat | awk '{{print $20}}')" = "$(cat "$root/start")" ] && [ "$(sed 's/^.*) //' /proc/$pid/stat | awk '{{print $1}}')" != Z ]; then
    printf 'PID:%s\n' "$pid"
    port=$(grep -oE '(127[.]0[.]0[.]1|localhost):[1-9][0-9]*' "$root/log" | head -n1 | cut -d: -f2 || true)
    if [ -n "$port" ]; then printf 'PORT:%s\n' "$port"; fi
  fi
fi
"#,cwd=guest::quote(&agent.cwd)),Duration::from_secs(15))
    }
    pub fn editor_status(&self, id: Uuid) -> Result<Value, Error> {
        let (agent, guest) = self.editor_workspace(id)?;
        let text = self.remote_editor(id)?;
        let f = fields(&text);
        let state = if f.contains_key("PID") && f.contains_key("PORT") {
            "running"
        } else if f.contains_key("INSTALLED") {
            "installed"
        } else {
            f.get("INSTALL").copied().unwrap_or("not_installed")
        };
        let error = (state == "failed").then(|| decoded(f.get("LOG")));
        Ok(
            json!({"vmId":guest.id,"agentId":id,"projectPath":agent.cwd,"state":state,"phase":f.get("PHASE"),"error":error}),
        )
    }
    pub fn open_editor(&self, id: Uuid) -> Result<Value, Error> {
        let (agent, guest) = self.editor_workspace(id)?;
        let text = self.remote_editor(id)?;
        let f = fields(&text);
        if !f.contains_key("INSTALLED") {
            if f.get("INSTALL") == Some(&"installing") {
                return self.editor_status(id);
            }
            self.install_editor(id)?;
            return self.editor_status(id);
        }
        let mut token = None;
        if !f.contains_key("PID") {
            let secret = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
            let encoded = STANDARD.encode(secret.as_bytes());
            guest.shell(&format!(r#"set -eu
umask 077
root={BASE}/{id}
mkdir -p "$root"
printf %s {encoded} | base64 -d > "$root/token"
cat /proc/sys/kernel/random/boot_id > "$root/boot"
cd {cwd}
nohup setsid {BINARY} --host 127.0.0.1 --port 0 --connection-token-file "$root/token" --server-base-path /{id} --user-data-dir "$root/data" --extensions-dir "$root/extensions" --disable-telemetry </dev/null > "$root/log" 2>&1 &
pid=$!
printf '%s' "$pid" > "$root/pid"
sed 's/^.*) //' /proc/$pid/stat | awk '{{print $20}}' > "$root/start"
"#,cwd=guest::quote(&agent.cwd)),Duration::from_secs(10))?;
            token = Some(secret);
        }
        let mut ready = None;
        for _ in 0..50 {
            let text = self.remote_editor(id)?;
            let f = fields(&text);
            if let (Some(pid), Some(port), Some(boot)) =
                (f.get("PID"), f.get("PORT"), f.get("BOOT"))
            {
                ready = Some((
                    pid.parse::<u32>()
                        .map_err(|_| invalid("Invalid editor process"))?,
                    port.parse::<u16>()
                        .map_err(|_| invalid("Invalid editor port"))?,
                    (*boot).to_owned(),
                ));
                break;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        let (remote_pid,remote_port,boot)=ready.ok_or_else(||invalid("Web VS Code did not become ready. Check that this VM has enough memory and supports the official Linux runtime."))?;
        let path = self.editor_record(id);
        let saved: Option<Forward> = fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok());
        let token = match token {
            Some(t) => t,
            None => guest
                .shell(&format!("cat {BASE}/{id}/token"), Duration::from_secs(5))?
                .trim()
                .to_owned(),
        };
        if token.len() != 64 || !token.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(invalid("Invalid editor connection identity."));
        }
        if let Some(saved) = &saved {
            if saved.vm_id == guest.id
                && saved.agent_id == id
                && saved.boot == boot
                && saved.remote_pid == remote_pid
                && saved.remote_port == remote_port
                && saved.token == token
                && process_identity(saved.pid).as_deref() == Some(&saved.identity)
                && listener_owned(saved.pid, saved.local_port)
            {
                return editor_connection(saved, &agent.cwd);
            }
            stop_forward(saved)?;
        }
        fs::create_dir_all(path.parent().unwrap())?;
        let local =
            TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).map_err(|e| invalid(e.to_string()))?;
        let local_port = local.local_addr()?.port();
        drop(local);
        let log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path.with_extension("log"))?;
        let mut forward = guest.forward(local_port, remote_port);
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            forward.process_group(0);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            forward.creation_flags(0x08000200);
        }
        let mut child = forward
            .stdout(Stdio::null())
            .stderr(Stdio::from(log))
            .spawn()?;
        let result = (|| {
            for _ in 0..100 {
                if child.try_wait()?.is_some() {
                    return Err(invalid(
                        "The private editor SSH connection could not be opened.",
                    ));
                }
                if listener_owned(child.id(), local_port) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            if !listener_owned(child.id(), local_port) {
                return Err(invalid("The private editor port did not become ready."));
            }
            let identity = process_identity(child.id())
                .ok_or_else(|| invalid("The private editor process identity is unavailable."))?;
            let record = Forward {
                vm_id: guest.id,
                agent_id: id,
                boot,
                remote_pid,
                remote_port,
                local_port,
                pid: child.id(),
                identity,
                token,
            };
            resource::write(&path, &record)?;
            editor_connection(&record, &agent.cwd)
        })();
        if result.is_err() {
            let _ = child.kill();
            let _ = child.wait();
        }
        result
    }
    fn install_editor(&self, id: Uuid) -> Result<(), Error> {
        let (_, guest) = self.editor_workspace(id)?;
        let arch = guest.shell("uname -m", Duration::from_secs(5))?;
        let arch = match arch.trim() {
            "aarch64" | "arm64" => "arm64",
            "x86_64" | "amd64" => "x64",
            _ => {
                return Err(invalid(
                    "This VM architecture is not supported by the official Web VS Code runtime.",
                ));
            }
        };
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(20))
            .user_agent("Firecrab/web-vscode")
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| invalid(e.to_string()))?;
        let release: Value = client
            .get("https://api.github.com/repos/gitpod-io/openvscode-server/releases/latest")
            .send()
            .and_then(|r| r.error_for_status())
            .map_err(|_| invalid("The official Web VS Code release is unavailable."))?
            .json()
            .map_err(|_| invalid("Invalid editor release metadata."))?;
        let (url, sha, folder, bytes) = release_asset(&release, arch)?;
        let script = format!(
            r#"#!/bin/sh
set -eu
umask 077
root={INSTALL}
mkdir -p "$root"
finish() {{ result=$?; trap - EXIT; if [ "$result" -eq 0 ]; then printf installed > "$root/state"; else printf failed > "$root/state"; fi; rm -rf "$root/work"; rm -f "$root/lock/boot" "$root/lock/pid" "$root/lock/start"; rmdir "$root/lock" 2>/dev/null || :; exit "$result"; }}
trap finish EXIT
printf '%s' "$$" > "$root/lock/pid"
sed 's/^.*) //' /proc/$$/stat | awk '{{print $20}}' > "$root/lock/start"
printf dependencies > "$root/phase"
if command -v apt-get >/dev/null 2>&1; then
  export DEBIAN_FRONTEND=noninteractive
  apt-get update
  apt-get install -y --no-install-recommends ca-certificates curl tar coreutils util-linux libstdc++6 libatomic1
elif command -v apk >/dev/null 2>&1; then
  apk add --no-cache ca-certificates curl tar coreutils util-linux libstdc++ gcompat
fi
available=$(df -Pk "$root" | tail -n1 | awk '{{print $4}}')
[ "$available" -ge {required} ] || {{ echo 'The VM needs more free disk space to install Web VS Code.' >&2; exit 1; }}
mkdir -p "$root/work"
printf download > "$root/phase"
curl --connect-timeout 15 --max-time 300 --retry 2 -fsSL {url} -o "$root/work/editor.tar.gz"
printf verify > "$root/phase"
printf '%s  %s\n' {sha} "$root/work/editor.tar.gz" | sha256sum -c -
printf extract > "$root/phase"
tar -xzf "$root/work/editor.tar.gz" -C "$root/work"
"$root/work/{folder}/node" --version
mkdir -p /root/.local/lib/firecrab
mv "$root/work/{folder}" /root/.local/lib/firecrab/openvscode
printf ready > "$root/phase"
"#,
            url = guest::quote(&url),
            sha = guest::quote(&sha),
            required = bytes.saturating_mul(6) / 1024 + 131072
        );
        let encoded = STANDARD.encode(script);
        guest.shell(
            &format!(
                r#"set -eu
umask 077
root={INSTALL}
mkdir -p "$root"
{INSTALL_WORKER}
if [ -d "$root/lock" ]; then
  worker_alive && exit 0
  rm -f "$root/lock/boot" "$root/lock/pid" "$root/lock/start"
  rmdir "$root/lock"
fi
mkdir "$root/lock" 2>/dev/null || exit 0
cat /proc/sys/kernel/random/boot_id > "$root/lock/boot"
printf %s {encoded} | base64 -d > "$root/worker.sh"
printf installing > "$root/state"
printf preparing > "$root/phase"
nohup sh "$root/worker.sh" </dev/null > "$root/log" 2>&1 &
"#
            ),
            Duration::from_secs(10),
        )?;
        Ok(())
    }
    pub fn stop_editor(&self, id: Uuid) -> Result<Value, Error> {
        let (_, guest) = self.editor_workspace(id)?;
        let path = self.editor_record(id);
        if let Ok(bytes) = fs::read(&path) {
            let saved: Forward = serde_json::from_slice(&bytes)
                .map_err(|_| invalid("Invalid editor connection record."))?;
            stop_forward(&saved)?;
        }
        guest.shell(&format!(r#"set -eu
root={BASE}/{id}
if [ ! -f "$root/pid" ]; then exit 0; fi
[ "$(cat "$root/boot")" = "$(cat /proc/sys/kernel/random/boot_id)" ] || exit 0
pid=$(cat "$root/pid")
case "$pid" in *[!0-9]*|'') exit 1;; esac
if [ -r "/proc/$pid/stat" ] && [ "$(sed 's/^.*) //' /proc/$pid/stat | awk '{{print $1}}')" != Z ]; then
  [ "$(sed 's/^.*) //' /proc/$pid/stat | awk '{{print $20}}')" = "$(cat "$root/start")" ] || exit 1
  [ "$(sed 's/^.*) //' /proc/$pid/stat | awk '{{print $3}}')" = "$pid" ] || exit 1
  kill -TERM "-$pid"
fi
rm -f "$root/pid" "$root/start" "$root/token"
"#),Duration::from_secs(10))?;
        if path.is_file() {
            fs::remove_file(path)?;
        }
        self.editor_status(id)
    }
}
use std::fs::OpenOptions;
fn editor_connection(record: &Forward, cwd: &str) -> Result<Value, Error> {
    let mut url = reqwest::Url::parse(&format!(
        "http://127.0.0.1:{}/{}/",
        record.local_port, record.agent_id
    ))
    .map_err(|_| invalid("Invalid private editor address."))?;
    url.query_pairs_mut()
        .append_pair("folder", cwd)
        .append_pair("tkn", &record.token);
    Ok(
        json!({"vmId":record.vm_id,"agentId":record.agent_id,"state":"ready","projectPath":cwd,"url":url.as_str()}),
    )
}
fn release_asset(release: &Value, arch: &str) -> Result<(String, String, String, u64), Error> {
    if !["arm64", "x64"].contains(&arch) {
        return Err(invalid("Unsupported editor architecture."));
    }
    let tag = release["tag_name"]
        .as_str()
        .filter(|t| {
            t.starts_with("openvscode-server-v")
                && t.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b".-".contains(&b))
        })
        .ok_or_else(|| invalid("Invalid official editor release."))?;
    let folder = format!("{tag}-linux-{arch}");
    let name = format!("{folder}.tar.gz");
    let asset = release["assets"]
        .as_array()
        .and_then(|a| a.iter().find(|a| a["name"] == name))
        .ok_or_else(|| invalid("Official editor asset is unavailable."))?;
    let url = asset["browser_download_url"]
        .as_str()
        .filter(|url| {
            *url == format!(
                "https://github.com/gitpod-io/openvscode-server/releases/download/{tag}/{name}"
            )
        })
        .ok_or_else(|| invalid("Unexpected editor download source."))?;
    let sha = asset["digest"]
        .as_str()
        .and_then(|s| s.strip_prefix("sha256:"))
        .filter(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or_else(|| invalid("The official editor checksum is unavailable."))?;
    let bytes = asset["size"]
        .as_u64()
        .filter(|v| *v > 0 && *v < 1024 * 1024 * 1024)
        .ok_or_else(|| invalid("Invalid editor package size."))?;
    Ok((url.into(), sha.into(), folder, bytes))
}
fn process_identity(pid: u32) -> Option<String> {
    #[cfg(unix)]
    let output = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "lstart=", "-o", "args="])
        .output()
        .ok()?;
    #[cfg(windows)]
    let output=Command::new("powershell.exe").args(["-NoProfile","-Command",&format!("$p=Get-CimInstance Win32_Process -Filter 'ProcessId={pid}';if($p){{$p.CreationDate.ToString('o')+' '+$p.CommandLine}}")]).output().ok()?;
    let text = String::from_utf8(output.stdout).ok()?;
    let text = text.trim();
    (output.status.success() && !text.is_empty() && !text.contains("<defunct>"))
        .then(|| text.to_owned())
}
fn listener_owned(pid: u32, port: u16) -> bool {
    #[cfg(target_os = "macos")]
    {
        Command::new("/usr/sbin/lsof")
            .args([
                "-nP",
                "-a",
                "-p",
                &pid.to_string(),
                &format!("-iTCP@127.0.0.1:{port}"),
                "-sTCP:LISTEN",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }
    #[cfg(target_os = "linux")]
    {
        let sockets = fs::read_dir(format!("/proc/{pid}/fd"))
            .ok()
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok())
            .filter_map(|e| fs::read_link(e.path()).ok())
            .map(|p| p.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        fs::read_to_string("/proc/net/tcp").is_ok_and(|tcp| {
            tcp.lines().skip(1).any(|line| {
                let f = line.split_whitespace().collect::<Vec<_>>();
                f.len() > 9
                    && f[1] == format!("0100007F:{port:04X}")
                    && f[3] == "0A"
                    && sockets.contains(&format!("socket:[{}]", f[9]))
            })
        })
    }
    #[cfg(windows)]
    {
        Command::new("powershell.exe").args(["-NoProfile","-Command",&format!("(Get-NetTCPConnection -LocalAddress 127.0.0.1 -LocalPort {port} -State Listen -ErrorAction SilentlyContinue).OwningProcess -contains {pid}")]).output().is_ok_and(|o|String::from_utf8_lossy(&o.stdout).trim()=="True")
    }
}
fn stop_forward(record: &Forward) -> Result<(), Error> {
    if process_identity(record.pid).as_deref() != Some(&record.identity)
        || !listener_owned(record.pid, record.local_port)
    {
        return Ok(());
    }
    #[cfg(unix)]
    let result = Command::new("kill")
        .args(["-TERM", &record.pid.to_string()])
        .status()?;
    #[cfg(windows)]
    let result = Command::new("taskkill.exe")
        .args(["/PID", &record.pid.to_string()])
        .status()?;
    if !result.success() {
        return Err(invalid(
            "The private editor connection could not be closed.",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn editor_assets_require_the_official_source_and_a_published_checksum() {
        let tag = "openvscode-server-v1.0.0";
        let name = format!("{tag}-linux-arm64.tar.gz");
        let mut data = json!({"tag_name":tag,"assets":[{"name":name,"size":12345,"browser_download_url":format!("https://github.com/gitpod-io/openvscode-server/releases/download/{tag}/{name}"),"digest":format!("sha256:{}","a".repeat(64))}]});
        assert!(release_asset(&data, "arm64").is_ok());
        data["assets"][0]["digest"] = Value::Null;
        assert!(release_asset(&data, "arm64").is_err());
        data["assets"][0]["digest"] = json!(format!("sha256:{}", "a".repeat(64)));
        data["assets"][0]["browser_download_url"] = json!("https://attacker.test/editor.tar.gz");
        assert!(release_asset(&data, "arm64").is_err());
        assert!(release_asset(&data, "../../host").is_err());
    }
    #[test]
    fn editor_connections_are_local_and_open_the_exact_agent_folder() {
        let record = Forward {
            vm_id: Uuid::new_v4(),
            agent_id: Uuid::new_v4(),
            boot: "boot".into(),
            remote_pid: 1,
            remote_port: 3000,
            local_port: 43210,
            pid: 1,
            identity: "test".into(),
            token: "a".repeat(64),
        };
        let cwd = format!("/workspace/firecrab/{}", record.agent_id);
        let value = editor_connection(&record, &cwd).unwrap();
        let url = reqwest::Url::parse(value["url"].as_str().unwrap()).unwrap();
        assert_eq!(url.host_str(), Some("127.0.0.1"));
        assert_eq!(url.port(), Some(43210));
        assert_eq!(
            url.query_pairs().find(|(k, _)| k == "folder").unwrap().1,
            cwd
        );
        assert_eq!(url.path(), format!("/{}/", record.agent_id));
        let temp = tempfile::tempdir().unwrap();
        assert!(
            Runtime::new(temp.path())
                .editor_workspace(record.agent_id)
                .is_err()
        );
    }
}

#[cfg(test)]
#[path = "qa/editor.rs"]
mod manual_qa;
