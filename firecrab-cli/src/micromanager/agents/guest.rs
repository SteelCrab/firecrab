//! Pinned SSH transport to the selected MicroVM. Host paths and accounts are never task inputs.
use super::*;
use std::collections::HashSet;
use std::net::IpAddr;

pub(super) const GUEST_PATH: &str = "/root/.local/bin:/usr/local/bin:/usr/bin:/bin:/sbin";

pub(super) fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[derive(Clone)]
pub(super) struct Guest {
    pub id: Uuid,
    pub home: PathBuf,
    pub available: HashSet<String>,
    pub authenticated: HashSet<String>,
    pub(super) address: IpAddr,
    pub(super) key: PathBuf,
    pub(super) known: PathBuf,
    pub(super) proxy: String,
    #[cfg(test)]
    pub(super) program: Option<PathBuf>,
}
impl Guest {
    pub fn connect(home: &Path, id: Uuid) -> Result<Self, Error> {
        #[cfg(any(target_os = "macos", test))]
        let port = super::super::dev::instance::api_port(home)?;
        #[cfg(not(any(target_os = "macos", test)))]
        let port = 5523;
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(8))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| invalid(e.to_string()))?;
        let base = format!("http://127.0.0.1:{port}");
        let get = |path: &str| -> Result<reqwest::blocking::Response, Error> {
            client
                .get(format!("{base}{path}"))
                .send()
                .and_then(|r| r.error_for_status())
                .map_err(|_| {
                    invalid(
                        "The selected MicroVM API is unavailable. Check its state and try again.",
                    )
                })
        };
        let host: Value = get("/api/host")?
            .json()
            .map_err(|_| invalid("Invalid host identity response"))?;
        if host["platform"]["controllerId"] != resource::controller_id(home)? {
            return Err(invalid(
                "The MicroVM belongs to another controller. Reload this dashboard.",
            ));
        }
        let vm: Value = get(&format!("/api/vms/{id}"))?
            .json()
            .map_err(|_| invalid("Invalid MicroVM response"))?;
        if vm["id"] != id.to_string() || vm["state"] != "running" {
            return Err(invalid(
                "Start the selected MicroVM before connecting its CLI agents.",
            ));
        }
        if vm["purpose"].as_str().is_some_and(|p| p != "instance") {
            return Err(invalid(
                "Select a regular MicroVM. Internal service VMs cannot run Sandbox tasks.",
            ));
        }
        let address: IpAddr = vm["ipv4"]
            .as_str()
            .ok_or_else(|| invalid("The MicroVM has no network address yet."))?
            .parse()
            .map_err(|_| invalid("Invalid MicroVM address"))?;
        let host_key: Value = get(&format!("/api/vms/{id}/ssh-host-key"))?
            .json()
            .map_err(|_| invalid("The MicroVM SSH identity is unavailable."))?;
        let fields = host_key["publicKey"]
            .as_str()
            .unwrap_or("")
            .split_whitespace()
            .take(2)
            .collect::<Vec<_>>();
        if fields.len() != 2
            || fields[0] != "ssh-ed25519"
            || !fields[1]
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"+/=".contains(&c))
        {
            return Err(invalid("The MicroVM has no valid pinned SSH host key."));
        }
        let directory = home.join("runtime/agents/microvms").join(id.to_string());
        fs::create_dir_all(&directory)?;
        let key = directory.join("operator-key");
        let pem = get(&format!("/api/vms/{id}/ssh-key"))?
            .text()
            .map_err(|_| invalid("The MicroVM operator key is unavailable."))?;
        if pem.len() > 16384 || !pem.starts_with("-----BEGIN OPENSSH PRIVATE KEY-----") {
            return Err(invalid("Invalid MicroVM operator key"));
        }
        private_write(&key, pem.as_bytes())?;
        let known = directory.join("known_hosts");
        private_write(
            &known,
            format!("firecrab-microvm-{id} {} {}\n", fields[0], fields[1]).as_bytes(),
        )?;
        let proxy = proxy_command(home)?;
        Ok(Self {
            id,
            home: directory,
            available: HashSet::new(),
            authenticated: HashSet::new(),
            address,
            key,
            known,
            proxy,
            #[cfg(test)]
            program: None,
        })
    }
    fn ssh_base(&self) -> Command {
        let default = PathBuf::from(if cfg!(windows) {
            "ssh.exe"
        } else {
            "/usr/bin/ssh"
        });
        #[cfg(test)]
        let program = self.program.clone().unwrap_or(default);
        #[cfg(not(test))]
        let program = default;
        let mut command = Command::new(program);
        command
            .args([
                "-F",
                "none",
                "-T",
                "-o",
                "BatchMode=yes",
                "-o",
                "IdentitiesOnly=yes",
                "-o",
                "IdentityAgent=none",
                "-o",
                "StrictHostKeyChecking=yes",
                "-o",
                if cfg!(windows) {
                    "GlobalKnownHostsFile=NUL"
                } else {
                    "GlobalKnownHostsFile=/dev/null"
                },
                "-o",
                "ForwardAgent=no",
                "-o",
                "ConnectTimeout=5",
                "-o",
                "ServerAliveInterval=5",
                "-o",
                "ServerAliveCountMax=2",
                "-o",
                "LogLevel=ERROR",
            ])
            .arg("-o")
            .arg(ssh_path("UserKnownHostsFile", &self.known))
            .arg("-o")
            .arg(format!("HostKeyAlias=firecrab-microvm-{}", self.id))
            .arg("-o")
            .arg(format!("ProxyCommand={}", self.proxy))
            .arg("-i")
            .arg(&self.key);
        command.stdin(Stdio::null());
        command
    }
    pub fn command(&self, script: &str) -> Result<Command, Error> {
        let mut command = self.ssh_base();
        command
            .arg("-o")
            .arg("ClearAllForwardings=yes")
            .arg(format!("root@{}", self.address))
            .arg(format!(
                "env PATH={} /bin/sh -c {}",
                quote(GUEST_PATH),
                quote(script)
            ));
        Ok(command)
    }
    pub fn forward(&self, local: u16, remote: u16) -> Command {
        let mut command = self.ssh_base();
        command
            .args([
                "-N",
                "-o",
                "ClearAllForwardings=no",
                "-o",
                "ExitOnForwardFailure=yes",
                "-L",
            ])
            .arg(format!("127.0.0.1:{local}:127.0.0.1:{remote}"))
            .arg(format!("root@{}", self.address));
        command
    }
    pub fn shell(&self, script: &str, limit: Duration) -> Result<String, Error> {
        let mut child = self
            .command(script)?
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let out = child.stdout.take().unwrap();
        let err = child.stderr.take().unwrap();
        let read = |mut stream: Box<dyn Read + Send>| {
            let mut data = Vec::new();
            let _ = stream.by_ref().take(65536).read_to_end(&mut data);
            let _ = io::copy(&mut stream, &mut io::sink());
            data
        };
        let output = std::thread::spawn(move || read(Box::new(out)));
        let errors = std::thread::spawn(move || read(Box::new(err)));
        let deadline = Instant::now() + limit;
        loop {
            if let Some(status) = child.try_wait()? {
                let output = output.join().unwrap_or_default();
                let errors = errors.join().unwrap_or_default();
                if status.success() {
                    return Ok(String::from_utf8_lossy(&output).into());
                }
                return Err(invalid(format!(
                    "MicroVM: {}",
                    String::from_utf8_lossy(&errors)
                        .chars()
                        .take(1400)
                        .collect::<String>()
                )));
            }
            if Instant::now() > deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Err(invalid(
                    "The MicroVM command timed out. Check its terminal before retrying.",
                ));
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    pub fn probe(&mut self) -> Result<(), Error> {
        type Cached = (Instant, HashSet<String>, HashSet<String>);
        static CACHE: std::sync::OnceLock<
            std::sync::Mutex<std::collections::HashMap<PathBuf, Cached>>,
        > = std::sync::OnceLock::new();
        let cache = CACHE.get_or_init(Default::default);
        if let Some((at, available, authenticated)) = cache
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&self.home)
            && at.elapsed() < Duration::from_secs(5)
        {
            self.available = available.clone();
            self.authenticated = authenticated.clone();
            return Ok(());
        }
        let mut script = String::from("set -eu\nexport DISABLE_AUTOUPDATER=1\n");
        for command in ["herdr", "git", "gh"]
            .into_iter()
            .chain(DRIVERS.iter().map(|d| d.2))
        {
            script.push_str(&format!(
                "if command -v {command} >/dev/null 2>&1; then printf 'tool:{command}\\n'; fi\n"
            ));
        }
        script.push_str("if command -v timeout >/dev/null 2>&1; then\nif command -v codex >/dev/null 2>&1 && timeout 5 codex login status >/dev/null 2>&1; then printf 'auth:codex\\n'; fi\nif command -v claude >/dev/null 2>&1 && timeout 5 claude auth status >/dev/null 2>&1; then printf 'auth:claude\\n'; fi\nif command -v gh >/dev/null 2>&1 && timeout 5 gh auth status --hostname github.com >/dev/null 2>&1; then printf 'auth:github\\n'; fi\nfi\n");
        let result = self.shell(&script, Duration::from_secs(20))?;
        self.available = result
            .lines()
            .filter_map(|s| s.strip_prefix("tool:").map(str::to_owned))
            .collect();
        self.authenticated = result
            .lines()
            .filter_map(|s| s.strip_prefix("auth:").map(str::to_owned))
            .collect();
        cache.lock().unwrap_or_else(|p| p.into_inner()).insert(
            self.home.clone(),
            (
                Instant::now(),
                self.available.clone(),
                self.authenticated.clone(),
            ),
        );
        Ok(())
    }
    pub fn workspace(&self, project: &super::project::VmProject) -> Result<String, Error> {
        use base64::{Engine, engine::general_purpose::STANDARD};
        let root = quote(&project.path);
        let stage = quote(&format!("/workspace/firecrab/.project-{}", Uuid::new_v4()));
        let manifest = super::project::MANIFEST;
        let encoded = STANDARD.encode(serde_json::to_vec(project)?);
        let mut script = format!(
            r#"set -eu
umask 077
root={root}
stage={stage}
lock=/root/.local/share/firecrab/vm-repository.lock
manifest={manifest}
if [ -f "$manifest" ] && [ "$(base64 -w0 "$manifest")" = {encoded} ] && [ -d "$root" ]; then exit 0; fi
if [ -e "$root" ] || [ -L "$root" ]; then printf '%s\n' 'The VM project folder already exists. Its files were preserved; inspect the VM before retrying.' >&2; exit 1; fi
mkdir -p /root/.local/share/firecrab /workspace/firecrab
if [ -d "$lock" ] && [ -f "$lock/pid" ] && [ -f "$lock/boot" ] && [ -f "$lock/start" ]; then
  old=$(cat "$lock/pid")
  case "$old" in *[!0-9]*|'') exit 1;; esac
  if [ "$(cat "$lock/boot")" != "$(cat /proc/sys/kernel/random/boot_id)" ] || [ ! -r "/proc/$old/stat" ] || [ "$(sed 's/^.*) //' /proc/$old/stat | awk '{{print $20}}')" != "$(cat "$lock/start")" ] || [ "$(sed 's/^.*) //' /proc/$old/stat | awk '{{print $1}}')" = Z ]; then
    rm -f -- "$lock/pid" "$lock/boot" "$lock/start"
    rmdir "$lock"
  fi
fi
mkdir "$lock" || {{ printf '%s\n' 'Repository setup is still running in this VM. Wait before retrying.' >&2; exit 1; }}
printf '%s' "$$" > "$lock/pid"
cat /proc/sys/kernel/random/boot_id > "$lock/boot"
sed 's/^.*) //' /proc/$$/stat | awk '{{print $20}}' > "$lock/start"
finish() {{ code=$?; trap - EXIT; if [ -d "$stage" ]; then rm -rf -- "$stage"; fi; rm -f -- "$lock/pid" "$lock/boot" "$lock/start"; rmdir "$lock"; exit "$code"; }}
trap finish EXIT
trap 'exit 1' HUP INT TERM
[ ! -e "$stage" ] || exit 1
"#
        );
        if let Some(repo) = &project.repository {
            script.push_str("GIT_TERMINAL_PROMPT=0 timeout 110 git");
            if self.authenticated.contains("github") {
                script.push_str(" -c 'credential.helper=!gh auth git-credential'");
            }
            script.push_str(" clone --single-branch");
            if !project.branch.is_empty() {
                script.push_str(&format!(" --branch {}", quote(&project.branch)));
            }
            script.push_str(&format!(" -- {} \"$stage\"\n", quote(repo)));
        } else {
            script.push_str("mkdir \"$stage\"\n");
        }
        script.push_str(&format!(
            r#"test ! -e "$root"
mv -T -- "$stage" "$root"
printf '%s' {encoded} | base64 -d > "$manifest.tmp"
mv -T -- "$manifest.tmp" "$manifest"
"#
        ));
        self.shell(&script, Duration::from_secs(120))?;
        Ok(project.path.clone())
    }
}
fn private_write(path: &Path, data: &[u8]) -> Result<(), Error> {
    if fs::symlink_metadata(path).is_ok_and(|m| !m.is_file()) {
        return Err(invalid("Invalid private SSH file"));
    }
    if fs::read(path).is_ok_and(|old| old == data) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        }
        return Ok(());
    }
    let temporary = path.with_extension(format!("{}.tmp", Uuid::new_v4()));
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary)?;
    use std::io::Write;
    file.write_all(data)?;
    drop(file);
    fs::rename(&temporary, path)?;
    Ok(())
}
fn proxy_command(home: &Path) -> Result<String, Error> {
    #[cfg(target_os = "macos")]
    {
        let runtime = home.join("runtime");
        let marker = fs::read_to_string(runtime.join("manager-ready")).map_err(|_| {
            invalid("The Firecrab management connection is unavailable. Reconnect service dev.")
        })?;
        let ip: IpAddr = marker
            .lines()
            .find_map(|l| l.strip_prefix("ip="))
            .ok_or_else(|| invalid("The management VM is not ready."))?
            .parse()
            .map_err(|_| invalid("Invalid management VM address"))?;
        let settings = super::super::macos::agent_ssh_arguments(home)?;
        let mut args = vec![
            "/usr/bin/ssh".to_owned(),
            "-F".into(),
            "none".into(),
            "-i".into(),
            runtime
                .join("manager_ed25519")
                .to_string_lossy()
                .into_owned(),
            "-o".into(),
            ssh_path("UserKnownHostsFile", &runtime.join("known_hosts")),
            "-o".into(),
            "StrictHostKeyChecking=yes".into(),
            "-o".into(),
            "BatchMode=yes".into(),
        ];
        args.extend(
            settings
                .into_iter()
                .map(|s| s.to_string_lossy().into_owned()),
        );
        args.extend(["-W".into(), "%h:%p".into(), format!("root@{ip}")]);
        Ok(args
            .iter()
            .map(|s| {
                if s == "%h:%p" {
                    quote(s)
                } else {
                    quote(&s.replace('%', "%%"))
                }
            })
            .collect::<Vec<_>>()
            .join(" "))
    }
    #[cfg(target_os = "windows")]
    {
        if super::super::dev::instance::load(home)?.is_some() {
            return Err(invalid(
                "MicroVM CLI access is unavailable in this container development instance.",
            ));
        }
        Ok(r#"wsl.exe -d firecrab-debian -u root --exec /bin/bash -c "exec 3<>/dev/tcp/$1/$2 || exit; cat <&3 & reader=$!; cat >&3; kill $reader 2>/dev/null || :; wait $reader 2>/dev/null || :" -- %h %p"#.into())
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = home;
        Err(invalid(
            "MicroVM CLI access requires the native Firecrab dashboard.",
        ))
    }
}
pub(super) fn repository_url(input: &str) -> Result<Option<String>, Error> {
    let input = input.trim();
    if input.len() > 512 {
        return Err(invalid("GitHub repository address is too long."));
    }
    if input.is_empty() {
        return Ok(None);
    }
    let path = input
        .strip_prefix("https://github.com/")
        .unwrap_or(input)
        .trim_end_matches('/')
        .trim_end_matches(".git");
    let parts = path.split('/').collect::<Vec<_>>();
    if parts.len() != 2
        || parts.iter().any(|part| {
            part.is_empty()
                || part.starts_with('.')
                || part.contains("..")
                || !part
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
        })
    {
        return Err(invalid(
            "Enter a GitHub repository as owner/repository or https://github.com/owner/repository.",
        ));
    }
    Ok(Some(format!("https://github.com/{path}.git")))
}

fn ssh_path(name: &str, path: &Path) -> String {
    format!(
        "{name}=\"{}\"",
        path.to_string_lossy()
            .replace('\\', "\\\\")
            .replace('\"', "\\\"")
    )
}

#[cfg(test)]
mod terminal_qa {
    use super::*;
    use base64::{Engine, engine::general_purpose::STANDARD};

    #[test]
    #[ignore = "Explicit owned qa-terminal VM; exercises real SSH logging without restarting the VM"]
    fn real_disposable_vm_keeps_authentication_logs_out_of_the_console() {
        let home = PathBuf::from(std::env::var("FIRECRAB_TERMINAL_QA_HOME").unwrap());
        let id: Uuid = std::env::var("FIRECRAB_TERMINAL_QA_VM")
            .unwrap()
            .parse()
            .unwrap();
        let port = super::super::super::dev::instance::api_port(&home).unwrap();
        let base = format!("http://127.0.0.1:{port}/api/vms/{id}");
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap();
        let vm: Value = client.get(&base).send().unwrap().json().unwrap();
        assert!(vm["name"].as_str().unwrap().starts_with("qa-terminal-"));
        let runtime = Runtime::for_vm(&home, id, false).unwrap();
        assert!(
            runtime.records().unwrap().is_empty(),
            "Do not touch a VM with agent sessions"
        );
        let guest = runtime.guest.as_ref().unwrap();
        let script =
            fs::read_to_string(std::env::var("FIRECRAB_TERMINAL_QA_SCRIPT").unwrap()).unwrap();
        assert!(script.contains("exec /usr/sbin/sshd -D -E \"$log_file\""));
        let encoded = STANDARD.encode(script);
        let mask = guest.shell("umask", Duration::from_secs(5)).unwrap();
        let boot = guest
            .shell(
                "cat /proc/sys/kernel/random/boot_id",
                Duration::from_secs(5),
            )
            .unwrap();
        let probe = guest.command("printf ready > /tmp/firecrab-ssh-qa-ready; sleep 4; printf ACTIVE_SSH_SESSION_SURVIVED").unwrap().stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
        guest.shell(&format!(r#"set -eu
for attempt in 1 2 3 4 5; do [ ! -f /tmp/firecrab-ssh-qa-ready ] || break; sleep .1; done
test -f /tmp/firecrab-ssh-qa-ready
pid=$(cat /run/firecrab/sshd.pid)
case "$pid" in *[!0-9]*|'') exit 1;; esac
tr '\000' ' ' < "/proc/$pid/cmdline" | grep -q 'sshd.*-D'
printf %s {encoded} | base64 -d > /etc/firecrab/services.d/sshd
chmod 755 /etc/firecrab/services.d/sshd
kill -TERM "$pid"
nohup /etc/firecrab/busybox setsid /etc/firecrab/services.d/sshd </dev/null >/run/firecrab/sshd-startup.log 2>&1 &
printf '%s' "$!" > /run/firecrab/sshd.pid
"#),Duration::from_secs(10)).unwrap();
        let result = probe.wait_with_output().unwrap();
        assert!(
            result.status.success(),
            "Existing SSH session ended during listener update"
        );
        assert_eq!(
            String::from_utf8(result.stdout).unwrap(),
            "ACTIVE_SSH_SESSION_SURVIVED"
        );
        guest
            .shell(
                "printf '\\nFIRECRAB_SSH_QA_QUIET\\n' > /dev/console",
                Duration::from_secs(10),
            )
            .unwrap();
        for _ in 0..3 {
            assert_eq!(
                guest
                    .shell("printf SSH_STILL_WORKS", Duration::from_secs(10))
                    .unwrap(),
                "SSH_STILL_WORKS"
            );
        }
        let audit = guest.shell("test \"$(stat -c %a /var/log/firecrab/sshd.log)\" = 600; grep -c 'Accepted publickey' /var/log/firecrab/sshd.log",Duration::from_secs(10)).unwrap();
        assert!(audit.trim().parse::<u32>().unwrap() >= 3);
        let log: Value = client
            .get(format!("{base}/log"))
            .send()
            .unwrap()
            .json()
            .unwrap();
        let tail = log["consoleLog"]
            .as_str()
            .unwrap()
            .rsplit("FIRECRAB_SSH_QA_QUIET")
            .next()
            .unwrap();
        assert!(
            !tail.contains("Accepted publickey")
                && !tail.contains("Received disconnect")
                && !tail.contains("Disconnected from user")
        );
        assert_eq!(
            guest
                .shell(
                    "cat /proc/sys/kernel/random/boot_id",
                    Duration::from_secs(5)
                )
                .unwrap(),
            boot
        );
        assert_eq!(guest.shell("umask", Duration::from_secs(5)).unwrap(), mask);
        println!(
            "Real SSH authentication records are private, the serial console stays quiet, and an existing SSH session survives. VM boot identity unchanged."
        );
    }
}
