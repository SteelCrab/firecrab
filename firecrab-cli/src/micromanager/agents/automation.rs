//! Explicit, durable verification in an existing task's MicroVM workspace.
use super::*;
use base64::{Engine, engine::general_purpose::STANDARD};

#[derive(Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Config {
    pub test_command: String,
    pub build_command: String,
    pub start_command: String,
    pub port: u16,
    pub security: bool,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Request {
    pub action: String,
    pub id: Uuid,
    pub config: Config,
}

fn validate(config: &Config) -> Result<(), Error> {
    for command in [
        &config.test_command,
        &config.build_command,
        &config.start_command,
    ] {
        if command.len() > 4096 || command.contains('\0') {
            return Err(invalid("Automation commands must be at most 4 KiB."));
        }
    }
    if config.test_command.trim().is_empty() {
        return Err(invalid("Enter the project's test command."));
    }
    if !config.start_command.trim().is_empty() && (config.port == 0 || config.port == 22) {
        return Err(invalid("Choose an application port other than SSH."));
    }
    Ok(())
}

impl Runtime {
    fn automation_workspace(&self, id: Uuid) -> Result<(Agent, &guest::Guest), Error> {
        let guest = self
            .guest
            .as_ref()
            .ok_or_else(|| invalid("Select a MicroVM first."))?;
        let agent = self.agent(id)?;
        if !self.owns_workspace(&agent) {
            return Err(invalid(
                "Automation requires a coding task workspace in this MicroVM.",
            ));
        }
        Ok((agent, guest))
    }
    pub(super) fn automation_busy(&self, id: Uuid) -> Result<bool, Error> {
        let Some(guest) = &self.guest else {
            return Ok(false);
        };
        let root = self.automation_root(id)?;
        let state=guest.shell(&format!(r#"root={root}
if [ -f "$root/latest" ]; then
  job=$(cat "$root/latest")
  case "$job" in *[!a-f0-9-]*|'') exit 1;; esac
  if [ "$(cat "$root/$job/state")" = running ] && [ "$(cat "$root/$job/boot")" = "$(cat /proc/sys/kernel/random/boot_id)" ]; then printf running; fi
fi
"#),Duration::from_secs(8))?;
        Ok(state.trim() == "running")
    }
    fn workspace_key(&self, id: Uuid) -> Result<String, Error> {
        Ok(if self.agent(id)?.cwd == project::WORKSPACE {
            "project".into()
        } else {
            id.to_string()
        })
    }
    pub(super) fn automation_root(&self, id: Uuid) -> Result<String, Error> {
        Ok(format!(
            "/root/.local/share/firecrab/automation/{}",
            self.workspace_key(id)?
        ))
    }
    fn automation_config_path(&self, id: Uuid) -> Result<PathBuf, Error> {
        Ok(self
            .home
            .join("automation")
            .join(format!("{}.json", self.workspace_key(id)?)))
    }
    pub fn automation_status(&self, id: Uuid) -> Result<Value, Error> {
        let (agent, guest) = self.automation_workspace(id)?;
        let cwd = guest::quote(&agent.cwd);
        let root = self.automation_root(id)?;
        let output = guest.shell(&format!(r#"set -eu
cd {cwd}
for file in package.json Cargo.toml pyproject.toml requirements.txt; do
  if [ -f "$file" ]; then printf 'FILE:%s:' "$file"; head -c 8192 "$file" | base64 -w0; printf '\n'; fi
done
printf 'BRANCH:'; git branch --show-current 2>/dev/null || true; printf '\n'
printf 'HEAD:'; git rev-parse HEAD 2>/dev/null || true; printf '\n'
printf 'REMOTE:'; git config --get remote.origin.url 2>/dev/null || true; printf '\n'
if [ -f .github/workflows/firecrab-checks.yml ]; then printf 'WORKFLOW:present\n'; fi
if command -v trivy >/dev/null 2>&1; then printf 'SCANNER:ready\n'; fi
root={root}
if [ -f "$root/latest" ]; then
  job=$(cat "$root/latest")
  case "$job" in *[!a-f0-9-]*|'') exit 1;; esac
  printf 'JOB:%s\n' "$job"
  if [ -f "$root/$job/boot" ] && [ "$(cat "$root/$job/boot")" != "$(cat /proc/sys/kernel/random/boot_id)" ]; then
    printf 'STATE:interrupted\n'
  else
    if [ -f "$root/$job/pid" ] && [ "$(cat "$root/$job/state")" = running ] && ! kill -0 "$(cat "$root/$job/pid")" 2>/dev/null; then printf 'STATE:interrupted\n'; fi
    for field in state phase exit; do
      if [ -f "$root/$job/$field" ]; then printf '%s:' "$field"; cat "$root/$job/$field"; printf '\n'; fi
    done
    if [ -f "$root/$job/app.pid" ] && [ -f "$root/$job/app.start" ]; then
      pid=$(cat "$root/$job/app.pid")
      case "$pid" in *[!0-9]*|'') exit 1;; esac
      if [ -r "/proc/$pid/stat" ] && [ "$(sed 's/^.*) //' /proc/$pid/stat | awk '{{print $20}}')" = "$(cat "$root/$job/app.start")" ] && [ "$(sed 's/^.*) //' /proc/$pid/stat | awk '{{print $1}}')" != Z ]; then
        printf 'APPRUNNING:true\n'
        if [ -f "$root/$job/app" ]; then printf 'app:ready\n'; fi
      fi
    fi
    if [ -f "$root/$job/preview.pid" ] && [ -f "$root/$job/preview.start" ]; then
      printf 'PREVIEW:present\n'
      pid=$(cat "$root/$job/preview.pid")
      case "$pid" in *[!0-9]*|'') exit 1;; esac
      if [ -r "/proc/$pid/stat" ] && [ "$(sed 's/^.*) //' /proc/$pid/stat | awk '{{print $20}}')" = "$(cat "$root/$job/preview.start")" ] && [ "$(sed 's/^.*) //' /proc/$pid/stat | awk '{{print $1}}')" != Z ]; then
        printf 'PREVIEWRUNNING:true\n'
        printf 'PUBLICURL:'; grep -oE 'https://[a-z0-9-]+[.]trycloudflare[.]com' "$root/$job/preview.log" | tail -n1 || true; printf '\n'
      else
        printf 'PREVIEWERROR:'; tail -c 1200 "$root/$job/preview.log" | base64 -w0; printf '\n'
      fi
    fi
  fi
  if [ -f "$root/$job/log" ]; then printf 'LOG:'; tail -c 24000 "$root/$job/log" | base64 -w0; printf '\n'; fi
  if [ -f "$root/$job/config" ]; then printf 'CONFIG:'; cat "$root/$job/config" | base64 -w0; printf '\n'; fi
  if [ -f "$root/$job/counts" ]; then printf 'COUNTS:'; head -c 8192 "$root/$job/counts" | base64 -w0; printf '\n'; fi

fi
"#), Duration::from_secs(20))?;
        parse_status(&output, &self.automation_config_path(id)?, guest.id, &agent)
    }
    pub fn automate(&self, id: Uuid, input: Request) -> Result<Value, Error> {
        if ["run", "configure_ci"].contains(&input.action.as_str()) {
            validate(&input.config)?;
        }
        let (agent, guest) = self.automation_workspace(id)?;
        if ["run", "configure_ci"].contains(&input.action.as_str()) {
            for peer in self.records()?.iter().filter(|peer| {
                peer.cwd == agent.cwd && !peer.closed && peer.role != "authentication"
            }) {
                if peer.pending_task.is_some() || self.pane(peer)?["agent_status"] == "working" {
                    return Err(invalid(
                        "Wait for all agents in this VM workspace to finish before running verification.",
                    ));
                }
            }
        }
        let root = self.automation_root(id)?;
        match input.action.as_str() {
            "check_preview" => return self.check_automation_preview(id),
            "start_preview" => {
                let state = self.automation_status(id)?;
                if state["run"]["state"] != "passed" || state["run"]["appReady"] != true {
                    return Err(invalid(
                        "Pass verification and start the test app before publishing it.",
                    ));
                }
                let port = state["run"]["config"]["port"]
                    .as_u64()
                    .ok_or_else(|| invalid("The test app port is unavailable."))?;
                self.ensure_preview_connector()?;
                guest.shell(&format!(r#"set -eu
root={root}
job=$(cat "$root/latest")
case "$job" in *[!a-f0-9-]*|'') exit 1;; esac
if [ -f "$root/$job/preview.pid" ]; then
  pid=$(cat "$root/$job/preview.pid")
  case "$pid" in *[!0-9]*|'') exit 1;; esac
  if [ -r "/proc/$pid/stat" ] && [ "$(sed 's/^.*) //' /proc/$pid/stat | awk '{{print $20}}')" = "$(cat "$root/$job/preview.start")" ] && [ "$(sed 's/^.*) //' /proc/$pid/stat | awk '{{print $1}}')" != Z ]; then exit 0; fi
fi
nohup setsid cloudflared --no-autoupdate tunnel --url http://127.0.0.1:{port} --protocol http2 --grace-period 1s </dev/null > "$root/$job/preview.log" 2>&1 &
pid=$!
printf '%s' "$pid" > "$root/$job/preview.pid"
sed 's/^.*) //' "/proc/$pid/stat" | awk '{{print $20}}' > "$root/$job/preview.start"
"#),Duration::from_secs(15))?;
            }
            "stop_preview" => {
                guest.shell(&stop_preview_script(&root), Duration::from_secs(10))?;
            }
            "stop_app" => {
                guest.shell(&stop_preview_script(&root), Duration::from_secs(10))?;
                guest.shell(
                    &format!(
                        r#"set -eu
root={root}
job=$(cat "$root/latest")
case "$job" in *[!a-f0-9-]*|'') exit 1;; esac
[ "$(cat "$root/$job/boot")" = "$(cat /proc/sys/kernel/random/boot_id)" ] || exit 1
pid=$(cat "$root/$job/app.pid")
case "$pid" in *[!0-9]*|'') exit 1;; esac
if [ -d "/proc/$pid" ] && [ "$(sed 's/^.*) //' /proc/$pid/stat | awk '{{print $1}}')" != Z ]; then
  current=$(sed 's/^.*) //' /proc/$pid/stat | awk '{{print $20}}')
  [ "$current" = "$(cat "$root/$job/app.start")" ] || exit 1
  [ "$(sed 's/^.*) //' /proc/$pid/stat | awk '{{print $3}}')" = "$pid" ] || exit 1
  kill -TERM "-$pid"
fi
rm -f "$root/$job/app"
"#
                    ),
                    Duration::from_secs(10),
                )?;
            }
            "run" => {
                let current = self.automation_status(id)?;
                if current["run"]["appRunning"] == true
                    && current["run"]["id"] != input.id.to_string()
                {
                    return Err(invalid(
                        "Stop this task's existing test app before starting a new verification.",
                    ));
                }
                // The VM owns the lock and process. A browser/native-server restart cannot replay it.
                let script = pipeline(&agent.cwd, &root, input.id, &input.config);
                let encoded = STANDARD.encode(script);
                let config = STANDARD.encode(
                    serde_json::to_vec(&input.config)
                        .map_err(|_| invalid("Invalid automation settings"))?,
                );
                guest.shell(&format!(r#"set -eu
umask 077
root={root}
mkdir -p "$root"
boot=$(cat /proc/sys/kernel/random/boot_id)
if [ -f "$root/lock/boot" ] && [ "$(cat "$root/lock/boot")" != "$boot" ]; then rm -f "$root/lock/boot"; rmdir "$root/lock"; fi
job="$root/{job}"
if [ -d "$job" ]; then
  [ "$(cat "$job/config")" = "$(printf %s {config} | base64 -d)" ] || exit 1
  exit 0
fi
mkdir "$root/lock" 2>/dev/null || {{ echo 'Verification is already running in this workspace.' >&2; exit 1; }}
printf '%s' "$boot" > "$root/lock/boot"
mkdir "$job"
printf '%s' "$boot" > "$job/boot"
printf %s {config} | base64 -d > "$job/config"
printf %s {encoded} | base64 -d > "$job/worker.sh"
printf 'running' > "$job/state"
printf 'starting' > "$job/phase"
printf '%s' {job} > "$root/latest"
nohup sh "$job/worker.sh" </dev/null >"$job/log" 2>&1 &
printf '%s' "$!" > "$job/pid"
"#, job=input.id), Duration::from_secs(20))?;
            }
            "configure_ci" => {
                if self.automation_busy(id)? {
                    return Err(invalid(
                        "Wait for verification to finish before changing its workflow.",
                    ));
                }
                let workflow = workflow(&input.config);
                guest.shell(&format!("set -eu\ncd {}\nmkdir -p .github/workflows\n(set -C; printf %s {} | base64 -d > .github/workflows/firecrab-checks.yml) || {{ echo 'firecrab-checks.yml already exists. Inspect or edit it before replacing it.' >&2; exit 1; }}\n", guest::quote(&agent.cwd), guest::quote(&STANDARD.encode(workflow))), Duration::from_secs(10))?;
            }
            "github" => {
                if !guest.authenticated.contains("github") {
                    return Err(invalid(
                        "Connect GitHub inside this MicroVM before loading workflow runs.",
                    ));
                }
                let output = guest.shell(&format!("cd {} && gh run list --limit 8 --json databaseId,name,headBranch,headSha,status,conclusion,url,createdAt", guest::quote(&agent.cwd)), Duration::from_secs(20))?;
                let runs: Value = serde_json::from_str(&output)
                    .map_err(|_| invalid("GitHub returned an invalid workflow list."))?;
                return Ok(json!({"runs":runs}));
            }
            _ => return Err(invalid("Unsupported automation action")),
        }
        let path = self.automation_config_path(id)?;
        fs::create_dir_all(path.parent().unwrap())?;
        resource::write(&path, &input.config)?;
        self.automation_status(id)
    }
    fn ensure_preview_connector(&self) -> Result<(), Error> {
        let guest = self
            .guest
            .as_ref()
            .ok_or_else(|| invalid("Select a MicroVM first."))?;
        let arch = guest.shell(
            "if command -v cloudflared >/dev/null 2>&1; then printf installed; else uname -m; fi",
            Duration::from_secs(8),
        )?;
        let name = match arch.trim() {
            "installed" => return Ok(()),
            "aarch64" | "arm64" => "cloudflared-linux-arm64",
            "x86_64" | "amd64" => "cloudflared-linux-amd64",
            _ => return Err(invalid("Unsupported preview connector architecture.")),
        };
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(20))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent("Firecrab/preview-connector")
            .build()
            .map_err(|e| invalid(e.to_string()))?;
        let release: Value = client
            .get("https://api.github.com/repos/cloudflare/cloudflared/releases/latest")
            .send()
            .and_then(|r| r.error_for_status())
            .map_err(|_| invalid("The official Cloudflare release could not be read."))?
            .json()
            .map_err(|_| invalid("Invalid Cloudflare release metadata"))?;
        let (url, sha) = connector_asset(&release, name)?;
        guest.shell(
            &format!(
                r#"set -eu
umask 077
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
curl --connect-timeout 15 --max-time 120 --retry 2 -fsSL {url} -o "$work/cloudflared"
printf '%s  %s\n' {sha} "$work/cloudflared" | sha256sum -c - >/dev/null
mkdir -p /root/.local/bin
install -m 755 "$work/cloudflared" /root/.local/bin/cloudflared
"#,
                url = guest::quote(&url),
                sha = guest::quote(&sha)
            ),
            Duration::from_secs(150),
        )?;
        Ok(())
    }
    fn check_automation_preview(&self, id: Uuid) -> Result<Value, Error> {
        let state = self.automation_status(id)?;
        if state["run"]["state"] != "passed"
            || state["run"]["appReady"] != true
            || state["preview"]["running"] != true
        {
            return Err(invalid(
                "Start and verify this task's app and preview first.",
            ));
        }
        let address = state["preview"]["publicUrl"]
            .as_str()
            .ok_or_else(|| invalid("Wait for the tunnel's HTTPS address."))?;
        let url = preview_url(address)?;
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(12))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| invalid(e.to_string()))?;
        let started = Instant::now();
        let response = client.get(url).send().map_err(|_| {
            invalid("The public HTTPS check failed. Inspect the tunnel and app, then retry.")
        })?;
        Ok(
            json!({"url":address,"status":response.status().as_u16(),"ok":response.status().is_success(),"latencyMs":started.elapsed().as_millis()}),
        )
    }
}

fn preview_url(address: &str) -> Result<reqwest::Url, Error> {
    let url = reqwest::Url::parse(address).map_err(|_| invalid("Invalid preview address"))?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || !url
            .host_str()
            .is_some_and(|h| h.ends_with(".trycloudflare.com"))
        || url.port().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(invalid(
            "Only the saved Cloudflare Quick Tunnel address can be checked.",
        ));
    }
    Ok(url)
}
fn connector_asset(release: &Value, name: &str) -> Result<(String, String), Error> {
    let asset = release["assets"]
        .as_array()
        .and_then(|assets| assets.iter().find(|a| a["name"] == name))
        .ok_or_else(|| invalid("The official connector asset is missing."))?;
    let tag = release["tag_name"]
        .as_str()
        .ok_or_else(|| invalid("Missing connector release"))?;
    let url = asset["browser_download_url"]
        .as_str()
        .ok_or_else(|| invalid("Missing connector download address"))?;
    if !["cloudflared-linux-arm64", "cloudflared-linux-amd64"].contains(&name)
        || url
            != format!("https://github.com/cloudflare/cloudflared/releases/download/{tag}/{name}")
    {
        return Err(invalid("Unexpected connector download source"));
    }
    let sha = asset["digest"]
        .as_str()
        .and_then(|s| s.strip_prefix("sha256:"))
        .filter(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or_else(|| invalid("The connector's published checksum is unavailable."))?;
    Ok((url.into(), sha.into()))
}
fn stop_preview_script(root: &str) -> String {
    format!(
        r#"set -eu
root={root}
if [ ! -f "$root/latest" ]; then exit 0; fi
job=$(cat "$root/latest")
case "$job" in *[!a-f0-9-]*|'') exit 1;; esac
if [ ! -f "$root/$job/preview.pid" ]; then exit 0; fi
[ "$(cat "$root/$job/boot")" = "$(cat /proc/sys/kernel/random/boot_id)" ] || exit 1
pid=$(cat "$root/$job/preview.pid")
case "$pid" in *[!0-9]*|'') exit 1;; esac
if [ -r "/proc/$pid/stat" ] && [ "$(sed 's/^.*) //' /proc/$pid/stat | awk '{{print $1}}')" != Z ]; then
  [ "$(sed 's/^.*) //' /proc/$pid/stat | awk '{{print $20}}')" = "$(cat "$root/$job/preview.start")" ] || exit 1
  [ "$(sed 's/^.*) //' /proc/$pid/stat | awk '{{print $3}}')" = "$pid" ] || exit 1
  kill -TERM "-$pid"
  for attempt in $(seq 1 40); do
    if [ ! -r "/proc/$pid/stat" ] || [ "$(sed 's/^.*) //' /proc/$pid/stat | awk '{{print $1}}')" = Z ]; then break; fi
    sleep .1
  done
  if [ -r "/proc/$pid/stat" ] && [ "$(sed 's/^.*) //' /proc/$pid/stat | awk '{{print $1}}')" != Z ]; then echo 'The preview connector is still stopping. Retry after inspecting it.' >&2; exit 1; fi
fi
rm -f "$root/$job/preview.pid" "$root/$job/preview.start"
"#
    )
}

fn decode(value: &str) -> String {
    STANDARD
        .decode(value)
        .ok()
        .and_then(|b| String::from_utf8(b).ok())
        .unwrap_or_default()
}
fn parse_status(text: &str, config_path: &Path, vm: Uuid, agent: &Agent) -> Result<Value, Error> {
    let mut fields = std::collections::HashMap::new();
    let mut files = std::collections::HashMap::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("FILE:") {
            if let Some((path, data)) = rest.split_once(':') {
                files.insert(path, decode(data));
            }
        } else if let Some((key, value)) = line.split_once(':') {
            fields.insert(key, value);
        }
    }
    let mut config = Config {
        port: 3000,
        security: true,
        ..Config::default()
    };
    if files.contains_key("Cargo.toml") {
        config.test_command = "cargo test --workspace --locked".into();
        config.build_command = "cargo build --workspace --locked".into();
    } else if let Some(package) = files.get("package.json") {
        if let Ok(package) = serde_json::from_str::<Value>(package) {
            config.test_command = if package["scripts"]["test"].is_string() {
                "npm ci && npm test"
            } else {
                ""
            }
            .into();
            config.build_command = if package["scripts"]["build"].is_string() {
                "npm run build"
            } else {
                ""
            }
            .into();
        }
    } else if files.contains_key("pyproject.toml") || files.contains_key("requirements.txt") {
        config.test_command = "python3 -m pytest".into();
    }
    if let Ok(saved) = fs::read(config_path) {
        config = serde_json::from_slice(&saved)
            .map_err(|_| invalid("Saved automation settings could not be read."))?;
    }
    let mut findings = json!({"vulnerabilities":0,"secrets":0,"misconfigurations":0});
    let mut report_available = false;
    if let Some(counts) = fields.get("COUNTS") {
        report_available = true;
        for line in decode(counts).lines() {
            let counts = line
                .split_whitespace()
                .map(str::parse::<u64>)
                .collect::<Result<Vec<_>, _>>();
            if let Ok(counts) = counts
                && counts.len() == 3
            {
                for (index, key) in ["vulnerabilities", "secrets", "misconfigurations"]
                    .iter()
                    .enumerate()
                {
                    findings[*key] = json!(
                        findings[*key]
                            .as_u64()
                            .unwrap_or(0)
                            .saturating_add(counts[index])
                    );
                }
            } else {
                report_available = false;
                break;
            }
        }
    }
    let run=fields.get("JOB").map(|id|json!({"id":id,"state":fields.get("STATE").or(fields.get("state")).copied().unwrap_or("unknown"),"phase":fields.get("phase").copied().unwrap_or("starting"),"exitCode":fields.get("exit").and_then(|v|v.parse::<i32>().ok()),"appReady":fields.get("app")==Some(&"ready"),"appRunning":fields.get("APPRUNNING")==Some(&"true"),"config":fields.get("CONFIG").and_then(|c|serde_json::from_str::<Value>(&decode(c)).ok()),"log":decode(fields.get("LOG").copied().unwrap_or("")),"findings":if report_available {findings} else {Value::Null}}));
    Ok(
        json!({"vmId":vm,"agentId":agent.id,"config":config,"branch":fields.get("BRANCH"),"head":fields.get("HEAD"),"repository":fields.get("REMOTE").and_then(|url|guest::repository_url(url).ok()),"workflowConfigured":fields.contains_key("WORKFLOW"),"scannerAvailable":fields.contains_key("SCANNER"),"run":run,"preview":if fields.contains_key("PREVIEW"){json!({"running":fields.get("PREVIEWRUNNING")==Some(&"true"),"publicUrl":fields.get("PUBLICURL").filter(|s|!s.is_empty()),"error":fields.get("PREVIEWERROR").map(|e|decode(e))})}else{Value::Null}}),
    )
}

// The release is intentional: use the same checked scanner in the VM and generated CI.
const TRIVY_VERSION: &str = "0.76.1";
fn scanner_setup() -> String {
    format!(
        r#"if ! command -v trivy >/dev/null 2>&1 || [ "$(trivy --version | head -n1)" != "Version: {TRIVY_VERSION}" ]; then
  tools=$(mktemp -d)
  case $(uname -m) in aarch64|arm64) arch=ARM64;; x86_64|amd64) arch=64bit;; *) echo 'Unsupported scanner architecture' >&2; exit 1;; esac
  archive=trivy_{TRIVY_VERSION}_Linux-$arch.tar.gz
  base=https://github.com/aquasecurity/trivy/releases/download/v{TRIVY_VERSION}
  curl --connect-timeout 15 --max-time 120 --retry 2 -fsSL "$base/$archive" -o "$tools/$archive"
  curl --connect-timeout 15 --max-time 30 -fsSL "$base/trivy_{TRIVY_VERSION}_checksums.txt" -o "$tools/checksums"
  (cd "$tools"; grep "  $archive\$" checksums > selected; test -s selected; sha256sum -c selected; tar -xzf "$archive" trivy)
  mkdir -p /root/.local/bin
  install -m 755 "$tools/trivy" /root/.local/bin/trivy
  rm -rf "$tools"
fi
"#
    )
}
fn pipeline(cwd: &str, root: &str, id: Uuid, config: &Config) -> String {
    let mut s = format!(
        r#"#!/bin/sh
set -eu
export PATH={path}
export CI=true
umask 077
root={root}
job="$root/{id}"
finish() {{ result=$?; trap - EXIT; printf '%s' "$result" > "$job/exit"; if [ "$result" -eq 0 ]; then printf passed > "$job/state"; elif [ "$result" -eq 10 ] && [ "$(cat "$job/phase")" = security ]; then printf findings > "$job/state"; else printf failed > "$job/state"; fi; rm -f "$root/lock/boot"; rmdir "$root/lock" 2>/dev/null || :; exit "$result"; }}
trap finish EXIT
cd {cwd}
printf tests > "$job/phase"
echo 'Running project tests'
timeout --kill-after=5 900 sh -c {test}
"#,
        path = guest::quote(guest::GUEST_PATH),
        cwd = guest::quote(cwd),
        test = guest::quote(&config.test_command)
    );
    if config.security {
        s.push_str("printf security > \"$job/phase\"\necho 'Scanning vulnerabilities, exposed secrets and configuration'\n");
        s.push_str(&scanner_setup());
        s.push_str(r#"set +e
timeout --kill-after=5 900 trivy fs --scanners vuln,secret,misconfig --severity HIGH,CRITICAL --exit-code 10 --format json --output "$job/report.json" .
result=$?
set -e
[ "$result" -eq 0 ] || [ "$result" -eq 10 ] || exit "$result"
trivy convert --format template --template '{{range .}}{{len .Vulnerabilities}} {{len .Secrets}} {{len .Misconfigurations}}{{"\n"}}{{end}}' --output "$job/counts" "$job/report.json"
[ "$result" -eq 0 ] || exit "$result"
"#);
    }
    if !config.build_command.trim().is_empty() {
        s.push_str(&format!("printf build > \"$job/phase\"\necho 'Building project'\ntimeout --kill-after=5 900 sh -c {}\n",guest::quote(&config.build_command)));
    }
    if !config.start_command.trim().is_empty() {
        s.push_str(&format!(r#"printf preview > "$job/phase"
echo 'Starting test application'
if curl --connect-timeout 1 --max-time 2 -s -o /dev/null http://{app}/; then echo 'The app port is already in use. Choose another port before starting this app.' >&2; exit 1; fi
command -v setsid >/dev/null || {{ echo 'Install util-linux to start the test app.' >&2; exit 1; }}
nohup setsid env FIRECRAB_AUTOMATION_JOB={id} sh -c {start} </dev/null >"$job/app.log" 2>&1 &
pid=$!
printf '%s' "$pid" > "$job/app.pid"
sed 's/^.*) //' "/proc/$pid/stat" | awk '{{print $20}}' > "$job/app.start"
for attempt in $(seq 1 45); do
  if curl --connect-timeout 1 --max-time 2 -fsS -o /dev/null http://{app}/; then printf ready > "$job/app"; printf complete > "$job/phase"; exit 0; fi
  sleep 1
done
echo 'The test app did not become ready. Listen on 127.0.0.1 or 0.0.0.0 at the selected port.' >&2
exit 1
"#,app=std::net::SocketAddr::new(std::net::Ipv4Addr::LOCALHOST.into(),config.port),start=guest::quote(&config.start_command)));
    }
    s.push_str("printf complete > \"$job/phase\"\necho 'Verification completed'\n");
    s
}
fn workflow(config: &Config) -> String {
    let quoted = |s: &str| serde_json::to_string(s).unwrap();
    let mut s = String::from(
        "name: Firecrab checks\non: [push, pull_request, workflow_dispatch]\npermissions:\n  contents: read\njobs:\n  verify:\n    runs-on: ubuntu-latest\n    timeout-minutes: 50\n    env:\n      CI: 'true'\n    steps:\n      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1\n        with:\n          persist-credentials: false\n",
    );
    s.push_str(&format!(
        "      - name: Test\n        run: {}\n",
        quoted(&config.test_command)
    ));
    if config.security {
        s.push_str("      - name: Security scan\n        run: |\n");
        // GitHub-hosted runners install into the current user's bin directory.
        for line in scanner_setup()
            .replace("/root/.local/bin", "$HOME/.local/bin")
            .lines()
        {
            s.push_str(&format!("          {line}\n"));
        }
        s.push_str(r#"          export PATH="$HOME/.local/bin:$PATH"
          set +e
          trivy fs --scanners vuln,secret,misconfig --severity HIGH,CRITICAL --format json --output "$RUNNER_TEMP/firecrab-trivy.json" --exit-code 10 .
          result=$?
          set -e
          [ "$result" -eq 0 ] || [ "$result" -eq 10 ] || exit "$result"
          trivy convert --format template --template '{{range .}}Vulnerabilities: {{len .Vulnerabilities}} | Secrets: {{len .Secrets}} | Configuration: {{len .Misconfigurations}}{{"\n"}}{{end}}' "$RUNNER_TEMP/firecrab-trivy.json" >> "$GITHUB_STEP_SUMMARY"
          [ "$result" -eq 0 ]
"#);
    }
    if !config.build_command.trim().is_empty() {
        s.push_str(&format!(
            "      - name: Build\n        run: {}\n",
            quoted(&config.build_command)
        ));
    }
    s.push_str("# After checks, deploy to the selected MicroVM explicitly from AI Sandbox.\n# Configure production deployment credentials and approvals separately.\n");
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preview_checks_reject_other_origins_and_unverified_connector_assets() {
        for address in [
            "http://qa.trycloudflare.com",
            "https://127.0.0.1",
            "https://qa.trycloudflare.com.attacker.test",
            "https://user@qa.trycloudflare.com",
            "https://qa.trycloudflare.com:8443",
            "https://qa.trycloudflare.com/?token=anything",
        ] {
            assert!(preview_url(address).is_err(), "{address}");
        }
        assert!(preview_url("https://qa.trycloudflare.com").is_ok());
        let name = "cloudflared-linux-arm64";
        let mut release = json!({"tag_name":"release","assets":[{"name":name,"browser_download_url":format!("https://github.com/cloudflare/cloudflared/releases/download/release/{name}"),"digest":format!("sha256:{}","a".repeat(64))}]});
        assert!(connector_asset(&release, name).is_ok());
        release["assets"][0]["browser_download_url"] = json!("https://attacker.test/connector");
        assert!(connector_asset(&release, name).is_err());
        release["assets"][0]["browser_download_url"] = json!(format!(
            "https://github.com/cloudflare/cloudflared/releases/download/release/{name}"
        ));
        release["assets"][0]["digest"] = Value::Null;
        assert!(connector_asset(&release, name).is_err());
    }
    #[test]
    #[ignore = "Explicit disposable qa-automation MicroVM; installs Trivy, never calls an AI model"]
    fn real_disposable_microvm_pipeline() {
        let home = std::env::var("FIRECRAB_AUTOMATION_QA_HOME").expect("Private QA managed home");
        let vm: Uuid = std::env::var("FIRECRAB_AUTOMATION_QA_VM")
            .expect("Disposable QA VM")
            .parse()
            .unwrap();
        let api_port = super::super::super::dev::instance::api_port(Path::new(&home)).unwrap();
        let metadata: Value =
            reqwest::blocking::get(format!("http://127.0.0.1:{api_port}/api/vms/{vm}"))
                .unwrap()
                .json()
                .unwrap();
        assert!(
            metadata["name"]
                .as_str()
                .unwrap()
                .starts_with("qa-automation-")
        );
        let runtime = Runtime::for_vm(Path::new(&home), vm, false).unwrap();
        for previous in runtime.records().unwrap() {
            assert!(
                previous.closed
                    && previous.role == "qa"
                    && previous.name == "Disposable automation QA",
                "Never borrow a user's agent workspace"
            );
            if runtime.automation_status(previous.id).unwrap()["run"]["appRunning"] == true {
                runtime
                    .automate(
                        previous.id,
                        Request {
                            action: "stop_app".into(),
                            id: Uuid::new_v4(),
                            config: Config::default(),
                        },
                    )
                    .unwrap();
            }
        }
        runtime.save(&[]).unwrap();
        let guest = runtime.guest.as_ref().unwrap();
        runtime.ensure_preview_connector().unwrap();
        let help = guest
            .shell("cloudflared tunnel --help", Duration::from_secs(10))
            .unwrap();
        assert!(help.contains("grace-period"));
        guest.shell("export DEBIAN_FRONTEND=noninteractive; apt-get update >/dev/null && apt-get install -y --no-install-recommends curl git coreutils ca-certificates util-linux >/dev/null",Duration::from_secs(300)).unwrap();
        let id = Uuid::new_v4();
        let cwd = format!("/workspace/firecrab/{id}");
        let agent = Agent {
            id,
            name: "Disposable automation QA".into(),
            role: "qa".into(),
            kind: "codex".into(),
            cwd: cwd.clone(),
            model: String::new(),
            workspace_id: String::new(),
            pane_id: String::new(),
            terminal_id: String::new(),
            closed: true,
            launch_error: None,
            last_output: String::new(),
            request_digest: String::new(),
            pending_task: None,
        };
        runtime.save(std::slice::from_ref(&agent)).unwrap();
        let nginx = format!(
            "pid {cwd}/nginx.pid; error_log stderr; events {{}} http {{ access_log off; server {{ listen 0.0.0.0:8765; location / {{ return 200 'FIRECRAB_AUTOMATION_QA_OK'; }} }} }}"
        );
        guest.shell(&format!("set -eu; mkdir -p {cwd}; cd {cwd}; git init -q; printf %s {} | base64 -d > nginx.conf",guest::quote(&STANDARD.encode(nginx))),Duration::from_secs(10)).unwrap();
        let config = Config {
            test_command: "printf tests-ok > tests.flag".into(),
            build_command: "printf build-ok > build.flag".into(),
            start_command: format!("/usr/sbin/nginx -c {cwd}/nginx.conf -g 'daemon off;'"),
            port: 8765,
            security: false,
        };
        let request = |action: &str, run: Uuid, config: &Config| Request {
            action: action.into(),
            id: run,
            config: config.clone(),
        };
        let wait = || {
            for _ in 0..240 {
                let state = runtime.automation_status(id).unwrap();
                if state["run"]["state"] != "running" {
                    return state;
                }
                std::thread::sleep(Duration::from_secs(2));
            }
            panic!("Disposable QA verification did not finish");
        };
        let run = Uuid::new_v4();
        runtime.automate(id, request("run", run, &config)).unwrap();
        let state = wait();
        assert_eq!(state["run"]["state"], "passed", "{state}");
        assert_eq!(state["run"]["appReady"], true, "{state}");
        assert!(
            guest
                .shell(
                    "curl --noproxy '*' -fsS http://127.0.0.1:8765",
                    Duration::from_secs(10)
                )
                .unwrap()
                .contains("FIRECRAB_AUTOMATION_QA_OK")
        );
        assert_eq!(
            runtime.automate(id, request("run", run, &config)).unwrap()["run"]["id"],
            run.to_string()
        );
        let preview_root = format!("/root/.local/share/firecrab/automation/{id}");
        guest
            .shell(
                &format!(
                    r#"set -eu
job={preview_root}/{run}
nohup setsid sleep 900 </dev/null >/dev/null 2>&1 &
pid=$!
printf '%s' "$pid" > "$job/preview.pid"
sed 's/^.*) //' /proc/$pid/stat | awk '{{print $20}}' > "$job/preview.start"
printf 'https://fixture-only-not-published.trycloudflare.com' > "$job/preview.log"
"#
                ),
                Duration::from_secs(10),
            )
            .unwrap();
        assert_eq!(
            runtime.automation_status(id).unwrap()["preview"]["running"],
            true
        );
        runtime
            .automate(id, request("stop_preview", Uuid::new_v4(), &config))
            .unwrap();
        assert!(runtime.automation_status(id).unwrap()["preview"].is_null());
        runtime
            .automate(id, request("stop_app", Uuid::new_v4(), &config))
            .unwrap();
        let bad = Config {
            test_command: "exit 7".into(),
            start_command: String::new(),
            ..config.clone()
        };
        runtime
            .automate(id, request("run", Uuid::new_v4(), &bad))
            .unwrap();
        assert_eq!(wait()["run"]["state"], "failed");
        let fake = format!("GITHUB_TOKEN=ghp_{}ABcd", Uuid::new_v4().simple());
        guest
            .shell(
                &format!(
                    "printf %s {} > {cwd}/scanner-fixture.env",
                    guest::quote(&fake)
                ),
                Duration::from_secs(10),
            )
            .unwrap();
        let scan = Config {
            test_command: "true".into(),
            start_command: String::new(),
            security: true,
            ..config
        };
        runtime
            .automate(id, request("run", Uuid::new_v4(), &scan))
            .unwrap();
        let state = wait();
        assert_eq!(state["run"]["state"], "findings", "{state}");
        assert!(
            state["run"]["findings"]["secrets"].as_u64().unwrap_or(0) > 0,
            "{state}"
        );
        runtime
            .automate(id, request("configure_ci", Uuid::new_v4(), &scan))
            .unwrap();
        assert!(
            runtime
                .automate(id, request("configure_ci", Uuid::new_v4(), &scan))
                .is_err(),
            "Never overwrite an existing workflow"
        );
        println!(
            "Disposable VM verified: test/build/app, idempotent run, owned app stop, failed test gate, real scanner secret finding, non-overwriting CI generation. No AI model calls or public publication."
        );
    }
    #[cfg(unix)]
    fn run_pipeline(
        test: &str,
        security: bool,
    ) -> (tempfile::TempDir, std::process::Output, PathBuf) {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let bin = root.join("bin");
        fs::create_dir(&bin).unwrap();
        let timeout = bin.join("timeout");
        fs::write(&timeout, "#!/bin/sh\nshift; shift; exec \"$@\"\n").unwrap();
        fs::set_permissions(&timeout, fs::Permissions::from_mode(0o755)).unwrap();
        let trivy = bin.join("trivy");
        fs::write(&trivy,format!("#!/bin/sh\ncase \"$1\" in --version) printf 'Version: {TRIVY_VERSION}\\n';; fs) exit 10;; convert) while [ \"$1\" != --output ]; do shift; done; printf '1 2 3\\n' > \"$2\";; esac\n")).unwrap();
        fs::set_permissions(&trivy, fs::Permissions::from_mode(0o755)).unwrap();
        let id = Uuid::new_v4();
        let job = root.join(id.to_string());
        fs::create_dir(&job).unwrap();
        fs::create_dir(root.join("lock")).unwrap();
        fs::write(root.join("lock/boot"), "").unwrap();
        let config = Config {
            test_command: test.into(),
            build_command: "printf built > built".into(),
            security,
            ..Config::default()
        };
        let script = pipeline(root.to_str().unwrap(), root.to_str().unwrap(), id, &config).replace(
            &format!("export PATH={}", guest::quote(guest::GUEST_PATH)),
            &format!(
                "export PATH={}",
                guest::quote(&format!("{}:/usr/bin:/bin", bin.display()))
            ),
        );
        let output = Command::new("/bin/sh")
            .arg("-c")
            .arg(script)
            .output()
            .unwrap();
        (temp, output, job)
    }
    #[cfg(unix)]
    #[test]
    fn failed_tests_stop_before_build_and_release_the_vm_lock() {
        let (temp, output, job) = run_pipeline("exit 7", false);
        assert_eq!(output.status.code(), Some(7));
        assert_eq!(fs::read_to_string(job.join("state")).unwrap(), "failed");
        assert_eq!(fs::read_to_string(job.join("phase")).unwrap(), "tests");
        assert!(!temp.path().join("built").exists());
        assert!(!temp.path().join("lock").exists());
    }
    #[cfg(unix)]
    #[test]
    fn passing_tests_build_but_security_findings_block_build() {
        let (temp, output, job) = run_pipeline("printf tested > tested", false);
        assert!(output.status.success(), "{:?}", output);
        assert!(temp.path().join("built").exists());
        assert_eq!(fs::read_to_string(job.join("state")).unwrap(), "passed");
        let (temp, output, job) = run_pipeline("true", true);
        assert_eq!(output.status.code(), Some(10));
        assert_eq!(fs::read_to_string(job.join("state")).unwrap(), "findings");
        assert_eq!(fs::read_to_string(job.join("counts")).unwrap(), "1 2 3\n");
        assert!(!temp.path().join("built").exists());
        let (_temp, output, job) = run_pipeline("exit 10", false);
        assert_eq!(output.status.code(), Some(10));
        assert_eq!(fs::read_to_string(job.join("state")).unwrap(), "failed");
    }
    #[test]
    fn automation_cannot_fall_back_to_the_native_host_workspace() {
        let temp = tempfile::tempdir().unwrap();
        let runtime = Runtime::new(temp.path());
        assert!(runtime.automation_workspace(Uuid::new_v4()).is_err());
        assert!(!temp.path().join("automation").exists());
    }
    #[test]
    fn ci_commands_are_yaml_strings_and_no_preview_command_is_published() {
        let command = "printf 'ok'\necho $HOME; echo \\\"quoted\\\"";
        let config = Config {
            test_command: command.into(),
            start_command: "private preview command".into(),
            security: true,
            ..Config::default()
        };
        let yaml = workflow(&config);
        assert!(yaml.contains(&serde_json::to_string(command).unwrap()));
        assert!(yaml.contains("persist-credentials: false"));
        assert!(yaml.contains("actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1"));
        assert!(!yaml.contains("private preview command"));
        assert!(!yaml.contains("pull_request_target"));
    }
}
