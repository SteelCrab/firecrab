//! Native CLI agent sessions. Herdr owns PTYs; the browser only observes and sends explicit input.
//! Every managed home gets its own named Herdr server, separate from the user's Herdr sessions.
use super::resource::{self, Error};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};
use uuid::Uuid;

pub mod automation;
pub mod editor;
pub mod github;
mod guest;
pub mod project;
pub mod rules;
pub mod skills;
pub mod terminal;

const DRIVERS: &[(&str, &str, &str)] = &[
    ("codex", "Codex", "codex"),
    ("claude", "Claude Code", "claude"),
    ("gemini", "Gemini CLI", "gemini"),
    ("opencode", "OpenCode", "opencode"),
    ("copilot", "GitHub Copilot", "copilot"),
    ("cursor", "Cursor Agent", "agent"),
    ("github", "GitHub", "gh"),
];

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Agent {
    pub id: Uuid,
    pub name: String,
    pub role: String,
    pub kind: String,
    pub cwd: String,
    pub model: String,
    pub workspace_id: String,
    pub pane_id: String,
    pub terminal_id: String,
    pub closed: bool,
    pub launch_error: Option<String>,
    pub last_output: String,
    #[serde(default)]
    pub request_digest: String,
    #[serde(default)]
    pending_task: Option<PendingTask>,
}
#[derive(Clone, Serialize, Deserialize)]
struct PendingTask {
    text: String,
    uncertain: bool,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Create {
    pub id: Uuid,
    pub name: String,
    pub role: String,
    pub kind: String,
    pub cwd: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub prompt: String,
    #[serde(default)]
    pub source_digest: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Input {
    pub text: Option<String>,
    pub key: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TaskStart {
    pub id: Uuid,
    pub input: String,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub repository: String,
    #[serde(default)]
    pub branch: String,
    #[serde(default)]
    pub cli: String,
    #[serde(default)]
    pub model: String,
}

fn invalid(message: impl Into<String>) -> Error {
    Error::Invalid(message.into())
}
/// Last step marker written by `guest_install.sh`; 1 before the first marker.
fn preparation_step(log: &str) -> u8 {
    log.lines()
        .filter_map(|l| l.strip_prefix("FIRECRAB_STEP=")?.parse::<u8>().ok())
        .rfind(|n| *n <= 4)
        .unwrap_or(1)
}
/// The failed step and its last message, as a terminal would display it.
fn preparation_failure(log: &str) -> String {
    let step = preparation_step(log);
    let stage = match step {
        1 => "system packages",
        2 => "Herdr terminal sessions",
        3 => "coding CLI",
        _ => "environment check",
    };
    // Only the failed step's output explains it; an earlier step's success line would mislead.
    let cause = log
        .lines()
        .rev()
        .take_while(|line| !line.starts_with("FIRECRAB_STEP="))
        .map(displayed_line)
        .find(|line| !line.is_empty());
    match cause {
        Some(cause) => {
            format!("MicroVM environment preparation failed at step {step} of 4 ({stage}): {cause}")
        }
        None => format!(
            "MicroVM environment preparation failed at step {step} of 4 ({stage}). Open the installation log for details."
        ),
    }
}
/// Drop ANSI escapes and control characters, keep the text after the last carriage return.
fn displayed_line(line: &str) -> String {
    let line = line
        .rsplit('\r')
        .find(|part| !part.is_empty())
        .unwrap_or("");
    let mut text = String::new();
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            if chars.next() == Some('[') {
                chars.find(|c| ('@'..='~').contains(c));
            }
        } else if !c.is_control() {
            text.push(c);
        }
    }
    let text = text.trim();
    match text.char_indices().nth(240) {
        Some((end, _)) => format!("{}…", &text[..end]),
        None => text.to_owned(),
    }
}
fn executable(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()).find_map(|dir| {
        let path = dir.join(if cfg!(windows) {
            format!("{name}.exe")
        } else {
            name.into()
        });
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if !path
                .metadata()
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            {
                return None;
            }
        }
        path.is_file().then_some(path)
    })
}

#[derive(Clone)]
pub struct Runtime {
    home: PathBuf,
    program: Option<PathBuf>,
    session: String,
    bin_dir: Option<PathBuf>,
    guest: Option<guest::Guest>,
    /// Controller home: the shared skill store lives under `runtime/skills`.
    skills_home: PathBuf,
}
impl Runtime {
    pub fn new(home: &Path) -> Self {
        let digest = format!(
            "{:x}",
            Sha256::digest(home.as_os_str().to_string_lossy().as_bytes())
        );
        Self {
            home: home.join("runtime/agents"),
            program: executable("herdr"),
            bin_dir: None,
            guest: None,
            skills_home: home.to_path_buf(),
            session: format!("firecrab-{}", &digest[..16]),
        }
    }
    pub fn preparation(&self) -> Result<Value, Error> {
        let path = self.home.join("preparation.json");
        match fs::read(&path) {
            Ok(bytes) => {
                let mut value: Value = serde_json::from_slice(&bytes)?;
                if value["state"] == "installing" && value["pid"] != std::process::id() {
                    value["state"] = json!("interrupted");
                    value["error"] = json!(
                        "Environment preparation was interrupted. Check the MicroVM before retrying."
                    );
                }
                if let Ok(mut file) = fs::File::open(self.home.join("preparation.log")) {
                    use std::io::{Seek, SeekFrom};
                    let size = file.metadata()?.len();
                    file.seek(SeekFrom::Start(size.saturating_sub(16384)))?;
                    let mut bytes = Vec::new();
                    file.take(16384).read_to_end(&mut bytes)?;
                    let log = String::from_utf8_lossy(&bytes).into_owned();
                    value["step"] = json!(preparation_step(&log));
                    value["log"] = json!(log);
                }
                if let Some(map) = value.as_object_mut() {
                    map.remove("pid");
                }
                Ok(value)
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Value::Null),
            Err(e) => Err(e.into()),
        }
    }
    pub fn prepare(&self, cli: &str) -> Result<Value, Error> {
        if !["codex", "claude", "github"].contains(&cli) {
            return Err(invalid(
                "Select GitHub, Codex or Claude Code to prepare the environment.",
            ));
        }
        if self.preparation()?["state"] == "installing" {
            return Err(Error::Busy);
        }
        let guest = self
            .guest
            .clone()
            .ok_or_else(|| invalid("Select a MicroVM first."))?;
        let path = self.home.join("preparation.json");
        let log_path = self.home.join("preparation.log");
        let id = Uuid::new_v4();
        let log = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&log_path)?;
        let state =
            json!({"id":id,"state":"installing","pid":std::process::id(),"cli":cli,"error":null});
        resource::write(&path, &state)?;
        let cli = cli.to_owned();
        let runtime = self.clone();
        std::thread::spawn(move || {
            let execute = || -> Result<(), Error> {
                let script = format!(
                    "timeout 900 /bin/sh -c {} -- {}",
                    guest::quote(concat!(
                        include_str!("agents/prepare_lock.sh"),
                        include_str!("agents/guest_install.sh")
                    )),
                    guest::quote(&cli)
                );
                let mut child = guest
                    .command(&script)?
                    .stdout(Stdio::from(log.try_clone()?))
                    .stderr(Stdio::from(log))
                    .spawn()?;
                let deadline = Instant::now() + Duration::from_secs(920);
                loop {
                    if let Some(status) = child.try_wait()? {
                        return if status.success() {
                            Ok(())
                        } else {
                            let log = fs::read(&log_path).unwrap_or_default();
                            Err(invalid(preparation_failure(&String::from_utf8_lossy(&log))))
                        };
                    }
                    if Instant::now() > deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err(invalid(
                            "MicroVM environment preparation timed out. Inspect the VM before retrying.",
                        ));
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            };
            let result = execute();
            let (state, error) = match result {
                Ok(()) => ("ready", None),
                Err(e) => ("failed", Some(e.to_string())),
            };
            let _ = resource::write(
                &path,
                &json!({"id":id,"state":state,"cli":cli,"error":error}),
            );
            if state == "ready" {
                runtime.after_preparation(&log_path);
            }
        });
        Ok(state)
    }
    /// Apply skills and AGENTS.md wiring to freshly installed CLIs. Best effort: outcomes are logged.
    fn after_preparation(&self, log_path: &Path) {
        let mut line = match self.sync_skills() {
            Ok(value) => format!(
                "FIRECRAB_SKILLS={}\n",
                value["state"].as_str().unwrap_or("")
            ),
            Err(error) => format!("FIRECRAB_SKILLS=failed {error}\n"),
        };
        line.push_str(&match self.ensure_rule_wiring() {
            Ok(()) => "FIRECRAB_RULES=ready\n".to_owned(),
            Err(error) => format!("FIRECRAB_RULES=failed {error}\n"),
        });
        if let Ok(mut file) = fs::OpenOptions::new().append(true).open(log_path) {
            use std::io::Write;
            let _ = file.write_all(line.as_bytes());
        }
    }
    pub fn login(&self, id: Uuid, cli: &str) -> Result<Value, Error> {
        if self.guest.is_none() {
            return Err(invalid("Select a MicroVM before connecting an account."));
        }
        if !["codex", "claude", "github"].contains(&cli) {
            return Err(invalid("Select a supported CLI account."));
        }
        self.create_session(
            Create {
                id,
                name: format!(
                    "{} sign in",
                    if cli == "claude" {
                        "Claude Code"
                    } else if cli == "github" {
                        "GitHub"
                    } else {
                        "Codex"
                    }
                ),
                role: "authentication".into(),
                kind: cli.into(),
                cwd: "/root".into(),
                model: String::new(),
                prompt: String::new(),
                source_digest: String::new(),
            },
            true,
        )
    }
    pub fn for_vm(home: &Path, id: Uuid, probe: bool) -> Result<Self, Error> {
        let mut target = guest::Guest::connect(home, id)?;
        if probe {
            target.probe()?;
        }
        let digest = format!(
            "{:x}",
            Sha256::digest(home.as_os_str().to_string_lossy().as_bytes())
        );
        Ok(Self {
            home: target.home.clone(),
            program: target
                .available
                .contains("herdr")
                .then(|| PathBuf::from("herdr")),
            session: format!("firecrab-{}", &digest[..16]),
            bin_dir: None,
            guest: Some(target),
            skills_home: home.to_path_buf(),
        })
    }
    fn guest_script(&self, args: &[&str]) -> String {
        let config = format!(
            "/root/.local/share/firecrab/agents/{}/config.toml",
            self.session
        );
        format!(
            "mkdir -p {}; exec env HERDR_CONFIG_PATH={} herdr --session {} {}",
            guest::quote(&format!(
                "/root/.local/share/firecrab/agents/{}",
                self.session
            )),
            guest::quote(&config),
            guest::quote(&self.session),
            args.iter()
                .map(|s| guest::quote(s))
                .collect::<Vec<_>>()
                .join(" ")
        )
    }
    fn has_driver(&self, name: &str) -> bool {
        if let Some(guest) = &self.guest {
            return guest.available.contains(name);
        }
        self.bin_dir.as_ref().map_or_else(
            || executable(name).is_some(),
            |dir| dir.join(name).is_file(),
        )
    }
    fn records(&self) -> Result<Vec<Agent>, Error> {
        match fs::read(self.home.join("sessions.json")) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|_| invalid("Agent session metadata is invalid")),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(vec![]),
            Err(e) => Err(e.into()),
        }
    }
    fn save(&self, records: &[Agent]) -> Result<(), Error> {
        fs::create_dir_all(&self.home)?;
        resource::write(&self.home.join("sessions.json"), &records)
    }
    fn command(&self, args: &[&str]) -> Result<Command, Error> {
        if let Some(guest) = &self.guest {
            return guest.command(&self.guest_script(args));
        }
        let mut cmd = Command::new(self.program.as_ref().ok_or_else(|| {
            invalid("Install Herdr on the native Firecrab host to run CLI agents")
        })?);
        cmd.args(["--session", &self.session])
            .args(args)
            .env("HERDR_CONFIG_PATH", self.home.join("config.toml"))
            .stdin(Stdio::null());
        Ok(cmd)
    }
    fn run(&self, args: &[&str]) -> Result<Value, Error> {
        let mut child = self
            .command(args)?
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        let read = |mut stream: Box<dyn Read + Send>| {
            let mut bytes = Vec::new();
            let _ = stream.by_ref().take(1024 * 1024).read_to_end(&mut bytes);
            let _ = io::copy(&mut stream, &mut io::sink());
            bytes
        };
        let out = std::thread::spawn(move || read(Box::new(stdout)));
        let err = std::thread::spawn(move || read(Box::new(stderr)));
        let deadline = Instant::now() + Duration::from_secs(40);
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Err(invalid(
                    "Agent operation timed out. Inspect the retained session before retrying.",
                ));
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        let output = out.join().unwrap_or_default();
        let error = err.join().unwrap_or_default();
        if !status.success() {
            return Err(invalid(
                String::from_utf8_lossy(if error.is_empty() { &output } else { &error })
                    .chars()
                    .take(1500)
                    .collect::<String>(),
            ));
        }
        if args.starts_with(&["pane", "read"]) {
            return Ok(json!({"text":String::from_utf8_lossy(&output)}));
        }
        let value: Value = match serde_json::from_slice(&output) {
            Ok(value) => value,
            Err(_)
                if args.starts_with(&["pane", "send-text"])
                    || args.starts_with(&["pane", "send-keys"])
                    || args.starts_with(&["pane", "run"])
                    || args.starts_with(&["agent", "start"])
                    || args.starts_with(&["agent", "prompt"])
                    || args.starts_with(&["workspace", "close"]) =>
            {
                Value::Null
            }
            Err(_) => return Err(invalid("Herdr returned an unsupported response")),
        };
        if let Some(error) = value.get("error") {
            return Err(invalid(
                error["message"]
                    .as_str()
                    .unwrap_or("Agent runtime request failed"),
            ));
        }
        Ok(value.get("result").cloned().unwrap_or(value))
    }
    fn ensure_server(&self) -> Result<(), Error> {
        if self
            .run(&["status", "server", "--json"])
            .is_ok_and(|value| value["running"] == true)
        {
            return Ok(());
        }
        if let Some(guest) = &self.guest {
            let directory = format!("/root/.local/share/firecrab/agents/{}", self.session);
            let command = self
                .guest_script(&["server"])
                .replacen("exec env", "nohup env", 1);
            guest.shell(
                &format!(
                    "mkdir -p {}; {} </dev/null >{}/server.log 2>&1 &",
                    guest::quote(&directory),
                    command,
                    guest::quote(&directory)
                ),
                Duration::from_secs(10),
            )?;
            for _ in 0..40 {
                if self
                    .run(&["status", "server", "--json"])
                    .is_ok_and(|v| v["running"] == true)
                {
                    return Ok(());
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            return Err(invalid(
                "The MicroVM agent runtime could not start. Inspect its VM terminal.",
            ));
        }
        fs::create_dir_all(&self.home)?;
        let mut cmd = self.command(&["server"])?;
        cmd.stdout(Stdio::null()).stderr(Stdio::null());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }
        let mut child = cmd.spawn()?;
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        let deadline = Instant::now() + Duration::from_secs(8);
        while Instant::now() < deadline {
            if self
                .run(&["status", "server", "--json"])
                .is_ok_and(|value| value["running"] == true)
            {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        Err(invalid("The private agent runtime could not start"))
    }
    pub fn status(&self) -> Result<Value, Error> {
        let records = self.records()?;
        let mut reason = None;
        let panes = if self.program.is_some() && self.home.is_dir() {
            match self.run(&["pane", "list"]) {
                Ok(value) => Some(value),
                Err(e) => {
                    reason = Some(e.to_string());
                    None
                }
            }
        } else {
            None
        };
        let agents = records
            .iter()
            .map(|record| {
                let pane = panes
                    .as_ref()
                    .and_then(|p| p["panes"].as_array().or_else(|| p.as_array()))
                    .and_then(|rows| {
                        rows.iter().find(|p| {
                            p["pane_id"] == record.pane_id && p["terminal_id"] == record.terminal_id
                        })
                    });
                self.projection(record, pane)
            })
            .collect::<Vec<_>>();
        let mut status = json!({"available":self.program.is_some(),"running":panes.is_some(),"reason":reason,"drivers":DRIVERS.iter().filter(|(id,_,command)| *id != "github" && (self.guest.is_none() || ["codex","claude"].contains(id) || self.has_driver(command))).map(|(id,name,command)|json!({"id":id,"name":name,"available":self.has_driver(command),"authenticated":self.guest.as_ref().is_none_or(|g|g.authenticated.contains(*id))})).collect::<Vec<_>>(),"agents":agents});
        if let Some(guest) = &self.guest {
            status["vmId"] = json!(guest.id);
            status["githubConnected"] = json!(guest.authenticated.contains("github"));
            status["githubAvailable"] = json!(guest.available.contains("gh"));
            status["github"] = self.github_status()?;
            status["project"] = self.vm_project()?;
            status["preparation"] = self.preparation()?;
        }
        Ok(status)
    }
    fn projection(&self, agent: &Agent, pane: Option<&Value>) -> Value {
        let mut value = serde_json::to_value(agent).unwrap();
        let state = if agent.closed {
            "closed"
        } else if agent.role == "authentication"
            && self
                .guest
                .as_ref()
                .is_some_and(|g| g.authenticated.contains(&agent.kind))
        {
            "done"
        } else if let Some(pane) = pane {
            let state = pane["agent_status"].as_str().unwrap_or("unknown");
            if agent.launch_error.is_some() && state == "unknown" {
                "failed"
            } else {
                state
            }
        } else {
            "disconnected"
        };
        value["state"] = json!(state);
        value.as_object_mut().unwrap().remove("lastOutput");
        value.as_object_mut().unwrap().remove("requestDigest");
        value.as_object_mut().unwrap().remove("pendingTask");
        value["hasPendingTask"] = json!(agent.pending_task.as_ref().is_some_and(|t| !t.uncertain));
        value["taskDeliveryUncertain"] =
            json!(agent.pending_task.as_ref().is_some_and(|t| t.uncertain));
        value
    }
    fn agent(&self, id: Uuid) -> Result<Agent, Error> {
        self.records()?
            .into_iter()
            .find(|a| a.id == id)
            .ok_or_else(|| invalid("Agent session does not belong to this Firecrab instance"))
    }
    fn pane(&self, agent: &Agent) -> Result<Value, Error> {
        let pane = self.run(&["pane", "get", &agent.pane_id])?;
        let pane = pane.get("pane").unwrap_or(&pane);
        if pane["workspace_id"] != agent.workspace_id
            || pane["pane_id"] != agent.pane_id
            || pane["terminal_id"] != agent.terminal_id
        {
            return Err(invalid("Agent terminal identity changed"));
        }
        Ok(pane.clone())
    }
    pub fn inspect(&self, id: Uuid) -> Result<Value, Error> {
        let agent = self.agent(id)?;
        if agent.closed {
            return Ok(json!({"agent":self.projection(&agent,None),"text":agent.last_output}));
        }
        let pane = self.pane(&agent)?;
        let read = self.run(&[
            "pane",
            "read",
            &agent.pane_id,
            "--source",
            "recent",
            "--lines",
            "250",
            "--format",
            "text",
        ])?;
        Ok(
            json!({"agent":self.projection(&agent,Some(&pane)),"text":read["text"].as_str().unwrap_or("")}),
        )
    }
    pub fn start_task(&self, mut input: TaskStart) -> Result<Value, Error> {
        let mut repository = guest::repository_url(&input.repository)?;
        let mut branch = input.branch.clone();
        if input.input.trim().is_empty() || input.input.len() > 16384 {
            return Err(invalid(
                "Enter a task up to 16 KiB before starting an agent",
            ));
        }
        if input.cli == "github" {
            return Err(invalid("Select a coding CLI for this task."));
        }
        if let Some(guest) = &self.guest {
            if self.preparation()?["state"] == "installing" {
                return Err(Error::Busy);
            }
            if input.cli.is_empty() {
                return Err(invalid("Select a CLI for this task."));
            }
            if !guest.available.contains("herdr") {
                return Err(invalid(
                    "Prepare the CLI environment in the selected MicroVM first.",
                ));
            }
            if ["codex", "claude"].contains(&input.cli.as_str())
                && !guest.authenticated.contains(&input.cli)
            {
                return Err(invalid(
                    "Connect this CLI account inside the selected MicroVM before starting work.",
                ));
            }
            (input.cwd, repository, branch) = self.task_project(&input)?;
            // New sessions read skills at startup; a failed sync is reported on the skill screen.
            let _ = self.sync_skills();
        }
        let kind = if !input.cli.is_empty() {
            input.cli
        } else {
            DRIVERS
                .iter()
                .find(|d| self.has_driver(d.2))
                .map(|d| d.0.to_owned())
                .ok_or_else(|| invalid("Install a coding CLI on the native host"))?
        };
        let name = input
            .input
            .trim()
            .lines()
            .next()
            .unwrap_or("Task")
            .chars()
            .take(30)
            .collect();
        self.create(Create {
            id: input.id,
            name,
            role: String::new(),
            kind,
            cwd: input.cwd,
            model: input.model,
            source_digest: format!(
                "{:x}",
                Sha256::digest(
                    serde_json::to_vec(&json!({"repository":repository,"branch":branch}))
                        .map_err(|_| invalid("Invalid repository request"))?
                )
            ),
            prompt: input.input,
        })
    }
    pub fn create(&self, input: Create) -> Result<Value, Error> {
        self.create_session(input, false)
    }
    fn create_session(&self, input: Create, login: bool) -> Result<Value, Error> {
        if input.id.is_nil()
            || input.name.trim().is_empty()
            || input.name.len() > 128
            || input.role.len() > 128
            || input.model.len() > 128
            || input.source_digest.len() > 128
            || input.prompt.len() > 16384
            || input.cwd.len() > 4096
        {
            return Err(invalid(
                "Use a name up to 128 bytes and a task up to 16 KiB",
            ));
        }
        let digest = format!(
            "{:x}",
            Sha256::digest(
                serde_json::to_vec(&input).map_err(|_| invalid("Invalid agent settings"))?
            )
        );
        let mut records = self.records()?;
        if let Some(agent) = records.iter().find(|a| a.id == input.id) {
            if agent.request_digest != digest {
                return Err(invalid(
                    "This request already created a session with different settings. Inspect that session, then create a new agent.",
                ));
            }
            return Ok(self.projection(agent, None));
        }
        if records.iter().filter(|a| !a.closed).count() >= 16 {
            return Err(invalid("At most 16 CLI agent sessions may be open"));
        }
        let driver = DRIVERS
            .iter()
            .find(|d| d.0 == input.kind)
            .ok_or_else(|| invalid("Unsupported agent CLI"))?;
        if !self.has_driver(driver.2) {
            return Err(invalid(format!(
                "{} is not installed in the selected environment",
                driver.1
            )));
        }
        if !input.model.is_empty() && !["codex", "claude"].contains(&input.kind.as_str()) {
            return Err(invalid(
                "Model override is supported for Codex and Claude Code",
            ));
        }
        if !Path::new(&input.cwd).is_absolute() {
            return Err(invalid("Working directory must be absolute"));
        }
        let cwd = if self.guest.is_some() {
            PathBuf::from(&input.cwd)
        } else {
            fs::canonicalize(&input.cwd).map_err(|_| invalid("Select an existing project folder on the Firecrab host. Linux paths such as /root belong inside a MicroVM."))?
        };
        if self.guest.is_none() && !cwd.is_dir() {
            return Err(invalid("Select an existing absolute working directory"));
        }
        self.ensure_server()?;
        let created = self.run(&[
            "workspace",
            "create",
            "--cwd",
            cwd.to_str()
                .ok_or_else(|| invalid("Working directory must be UTF-8"))?,
            "--label",
            &input.name,
            "--no-focus",
        ])?;
        let workspace = created["workspace"]["workspace_id"]
            .as_str()
            .ok_or_else(|| invalid("Herdr did not return a workspace identity"))?
            .to_owned();
        let pane = created["root_pane"]["pane_id"]
            .as_str()
            .ok_or_else(|| invalid("Herdr did not return a terminal identity"))?
            .to_owned();
        let mut agent = Agent {
            id: input.id,
            name: input.name.trim().into(),
            role: input.role.trim().into(),
            kind: input.kind,
            cwd: cwd.to_string_lossy().into_owned(),
            model: input.model,
            workspace_id: workspace,
            pane_id: pane,
            terminal_id: created["root_pane"]["terminal_id"]
                .as_str()
                .ok_or_else(|| invalid("Herdr did not return a terminal generation"))?
                .into(),
            closed: false,
            launch_error: Some(
                "Agent launch is pending. Inspect its terminal before taking further action."
                    .into(),
            ),
            last_output: String::new(),
            request_digest: digest,
            pending_task: (!input.prompt.trim().is_empty()).then(|| PendingTask {
                text: input.prompt.clone(),
                uncertain: false,
            }),
        };
        records.push(agent.clone());
        self.save(&records)?;
        let alias = format!("fc-{}", &agent.id.simple().to_string()[..12]);
        let mut args = vec![
            "agent",
            "start",
            &alias,
            "--kind",
            &agent.kind,
            "--pane",
            &agent.pane_id,
            "--timeout",
            "30000",
        ];
        if !agent.model.is_empty() {
            args.extend([
                "--",
                if agent.kind == "claude" {
                    "--model"
                } else {
                    "-m"
                },
                &agent.model,
            ]);
        }
        if login {
            let command = match agent.kind.as_str() {
                "codex" => "codex login --device-auth",
                "claude" => "claude auth login",
                "github" => {
                    "gh auth login --hostname github.com --git-protocol https --web && gh auth setup-git --hostname github.com"
                }
                _ => return Err(invalid("This CLI login is not supported.")),
            };
            args = vec!["pane", "run", &agent.pane_id, command];
        }
        agent.launch_error = self.run(&args).err().map(|e| e.to_string());
        records
            .iter_mut()
            .find(|a| a.id == agent.id)
            .unwrap()
            .launch_error = agent.launch_error.clone();
        self.save(&records)?;
        if agent.launch_error.is_none() && agent.pending_task.is_some() {
            return self.deliver_task(agent.id);
        }
        Ok(self.projection(&agent, None))
    }
    pub fn deliver_task(&self, id: Uuid) -> Result<Value, Error> {
        let mut records = self.records()?;
        let agent = records
            .iter_mut()
            .find(|a| a.id == id)
            .ok_or_else(|| invalid("Agent session not found"))?;
        if agent.closed {
            return Err(invalid("This agent session is closed"));
        }
        let pane = self.pane(agent)?;
        let Some(task) = agent.pending_task.as_mut() else {
            return Ok(self.projection(agent, Some(&pane)));
        };
        if task.uncertain {
            return Err(invalid(
                "The previous delivery may have reached the CLI. Inspect the terminal instead of automatically replaying the task.",
            ));
        }
        if !["idle", "done"].contains(&pane["agent_status"].as_str().unwrap_or("")) {
            return Err(invalid(
                "Finish the CLI startup or approval prompt before sending the waiting task.",
            ));
        }
        let text = if agent.role.is_empty() {
            task.text.clone()
        } else {
            format!("Role: {}\n\n{}", agent.role, task.text)
        };
        task.uncertain = true;
        let pane_id = agent.pane_id.clone();
        self.save(&records)?;
        let result = self.run(&["agent", "prompt", &pane_id, &text]);
        let agent = records.iter_mut().find(|a| a.id == id).unwrap();
        match result {
            Ok(_) => {
                agent.pending_task = None;
                agent.launch_error = None;
            }
            Err(e) => agent.launch_error = Some(e.to_string()),
        }
        let value = self.projection(agent, Some(&pane));
        self.save(&records)?;
        Ok(value)
    }
    pub fn input(&self, id: Uuid, input: Input) -> Result<Value, Error> {
        let agent = self.agent(id)?;
        if agent.closed {
            return Err(invalid("This agent session is closed"));
        }
        if input.text.is_some() && self.automation_busy(id)? {
            return Err(invalid(
                "Verification is running in this workspace. Wait before sending new coding instructions.",
            ));
        }
        self.pane(&agent)?;
        match (input.text, input.key) {
            (Some(text), None) if !text.is_empty() && text.len() <= 16384 => {
                self.run(&["pane", "send-text", &agent.pane_id, &text])?;
                self.run(&["pane", "send-keys", &agent.pane_id, "enter"])?;
            }
            (None, Some(key))
                if [
                    "enter", "esc", "tab", "up", "down", "left", "right", "ctrl+c",
                ]
                .contains(&key.as_str()) =>
            {
                self.run(&["pane", "send-keys", &agent.pane_id, &key])?;
            }
            _ => {
                return Err(invalid(
                    "Send text up to 16 KiB or one supported terminal key",
                ));
            }
        }
        self.inspect(id)
    }
    pub fn close(&self, id: Uuid) -> Result<Value, Error> {
        let mut records = self.records()?;
        let index = records
            .iter()
            .position(|a| a.id == id)
            .ok_or_else(|| invalid("Unknown agent session"))?;
        if records[index].closed {
            return Ok(self.projection(&records[index], None));
        }
        self.pane(&records[index])?;
        let output = self.inspect(id)?["text"].as_str().unwrap_or("").to_owned();
        self.run(&["workspace", "close", &records[index].workspace_id])?;
        records[index].closed = true;
        records[index].last_output = output;
        self.save(&records)?;
        Ok(self.projection(&records[index], None))
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
#[path = "agents/qa/herdr.rs"]
mod manual_qa;
