//! Pinned SSH transport to the selected MicroVM. Host paths and accounts are never task inputs.
use super::*;
use std::collections::HashSet;
use std::net::IpAddr;

#[cfg(all(test, unix))]
mod tests;

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
    /// Scopes Firecrab needs that the VM's GitHub OAuth or classic token lacks.
    /// Empty when GitHub is not connected, the token has no scopes, or the check timed out.
    pub github_missing_scopes: Vec<String>,
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
            github_missing_scopes: Vec::new(),
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
        self.execute(script, None, limit, 65536)
    }
    /// Like `shell`, keeping up to `capacity` bytes of stdout (for base64 file reads).
    pub fn shell_capture(
        &self,
        script: &str,
        limit: Duration,
        capacity: u64,
    ) -> Result<String, Error> {
        self.execute(script, None, limit, capacity)
    }
    /// Like `shell`, with `input` streamed to the remote command's stdin.
    pub fn shell_with_input(
        &self,
        script: &str,
        input: &[u8],
        limit: Duration,
    ) -> Result<String, Error> {
        self.execute(script, Some(input), limit, 65536)
    }
    fn execute(
        &self,
        script: &str,
        input: Option<&[u8]>,
        limit: Duration,
        capacity: u64,
    ) -> Result<String, Error> {
        let mut command = self.command(script)?;
        if input.is_some() {
            command.stdin(Stdio::piped());
        }
        let mut child = command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        // A writer thread keeps a large input from deadlocking against unread output.
        let writer = input.map(|bytes| {
            use std::io::Write;
            let mut stdin = child.stdin.take().unwrap();
            let bytes = bytes.to_vec();
            std::thread::spawn(move || {
                let _ = stdin.write_all(&bytes);
            })
        });
        let out = child.stdout.take().unwrap();
        let err = child.stderr.take().unwrap();
        let read = move |mut stream: Box<dyn Read + Send>, capacity: u64| {
            let mut data = Vec::new();
            let _ = stream.by_ref().take(capacity).read_to_end(&mut data);
            let _ = io::copy(&mut stream, &mut io::sink());
            data
        };
        let output = std::thread::spawn(move || read(Box::new(out), capacity));
        let errors = std::thread::spawn(move || read(Box::new(err), 65536));
        let deadline = Instant::now() + limit;
        loop {
            if let Some(status) = child.try_wait()? {
                if let Some(writer) = writer {
                    let _ = writer.join();
                }
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
        type Cached = (Instant, HashSet<String>, HashSet<String>, Vec<String>);
        static CACHE: std::sync::OnceLock<
            std::sync::Mutex<std::collections::HashMap<PathBuf, Cached>>,
        > = std::sync::OnceLock::new();
        let cache = CACHE.get_or_init(Default::default);
        // An older answer is still the best fallback for a check that runs out of time.
        let previous = match cache
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&self.home)
        {
            Some((at, available, authenticated, missing))
                if at.elapsed() < Duration::from_secs(5) =>
            {
                self.available = available.clone();
                self.authenticated = authenticated.clone();
                self.github_missing_scopes = missing.clone();
                return Ok(());
            }
            Some((_, _, authenticated, _)) => authenticated.clone(),
            None => HashSet::new(),
        };
        let mut script = String::from("set -eu\nexport DISABLE_AUTOUPDATER=1\n");
        for command in ["herdr", "git", "gh"]
            .into_iter()
            .chain(DRIVERS.iter().map(|d| d.2))
        {
            script.push_str(&format!(
                "if command -v {command} >/dev/null 2>&1; then printf 'tool:{command}\\n'; fi\n"
            ));
        }
        // Login checks call the provider and run slowly on a busy VM. Run them
        // together, and report a check that ran out of time as unknown.
        script.push_str(concat!(
            "if command -v timeout >/dev/null 2>&1; then\n",
            "login() { name=$1; shift; command -v \"$1\" >/dev/null 2>&1 || return 0; rc=0; timeout 12 \"$@\" >/dev/null 2>&1 || rc=$?; ",
            "if [ \"$rc\" -eq 0 ]; then printf 'auth:%s\\n' \"$name\"; elif [ \"$rc\" -eq 124 ]; then printf 'auth-unknown:%s\\n' \"$name\"; fi; }\n",
            "login codex codex login status &\n",
            "login claude claude auth status &\n",
            // The GitHub check reads the active account's scopes from the same call. gh prints
            // `Token scopes:` only for OAuth (gho_) and classic (ghp_) tokens; a blank line ends
            // the active account. Only the missing scope name leaves the VM.
            "github() { command -v gh >/dev/null 2>&1 || return 0; rc=0; out=$(timeout 12 gh auth status --hostname github.com 2>&1) || rc=$?; ",
            "if [ \"$rc\" -eq 0 ]; then printf 'auth:github\\n'; ",
            "scopes=$(printf '%s\\n' \"$out\" | awk 'NF==0{exit} /Token scopes:/{print; exit}'); ",
            "case \"$scopes\" in ''|*workflow*) ;; *) printf 'github-missing-scope:workflow\\n';; esac; ",
            "elif [ \"$rc\" -eq 124 ]; then printf 'auth-unknown:github\\n'; fi; }\n",
            "github &\n",
            "wait\nfi\n",
            // Grok Build has no status command. `grok login` keeps its session in this
            // file and `grok logout` removes it. Only its presence is checked.
            "if command -v grok >/dev/null 2>&1 && [ -s /root/.grok/auth.json ]; then printf 'auth:grok\\n'; fi\n",
        ));
        let result = self.shell(&script, Duration::from_secs(20))?;
        (self.available, self.authenticated) = parse_probe(&result, &previous);
        self.github_missing_scopes = result
            .lines()
            .filter_map(|line| line.strip_prefix("github-missing-scope:"))
            .filter(|_| self.authenticated.contains("github"))
            .map(str::to_owned)
            .collect();
        cache.lock().unwrap_or_else(|p| p.into_inner()).insert(
            self.home.clone(),
            (
                Instant::now(),
                self.available.clone(),
                self.authenticated.clone(),
                self.github_missing_scopes.clone(),
            ),
        );
        Ok(())
    }
    /// The model and reasoning effort set in the CLI's own settings, empty when unknown.
    pub fn cli_defaults(&self, kind: &str) -> (String, String) {
        let path = match kind {
            "codex" => "/root/.codex/config.toml",
            "claude" => "/root/.claude/settings.json",
            _ => return Default::default(),
        };
        self.shell(&format!("cat -- {}", quote(path)), Duration::from_secs(5))
            .map(|text| parse_cli_defaults(kind, &text))
            .unwrap_or_default()
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

/// Tools and logins reported by the probe script. A login check that ran out
/// of time keeps its last known answer; only a definite reply changes it.
pub(super) fn parse_probe(
    output: &str,
    previous: &HashSet<String>,
) -> (HashSet<String>, HashSet<String>) {
    let lines = |prefix: &'static str| {
        output
            .lines()
            .filter_map(move |line| line.strip_prefix(prefix).map(str::to_owned))
    };
    let mut authenticated: HashSet<String> = lines("auth:").collect();
    authenticated.extend(lines("auth-unknown:").filter(|name| previous.contains(name)));
    (lines("tool:").collect(), authenticated)
}

/// Top-level keys only: Codex profiles and Claude Code per-model settings are not resolved.
pub(super) fn parse_cli_defaults(kind: &str, text: &str) -> (String, String) {
    let (settings, effort): (Value, _) = match kind {
        "codex" => (
            toml::from_str(text).unwrap_or_default(),
            "model_reasoning_effort",
        ),
        "claude" => (
            serde_json::from_str(text).unwrap_or_default(),
            "effortLevel",
        ),
        _ => return Default::default(),
    };
    let read = |key: &str| {
        settings[key]
            .as_str()
            .filter(|v| v.len() <= 128 && !v.contains(char::is_control))
            .unwrap_or_default()
            .to_owned()
    };
    (read("model"), read(effort))
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
#[path = "qa/guest.rs"]
mod terminal_qa;
