//! Bring sessions back after the private Herdr server restarts, for example after a
//! MicroVM stop and start. Herdr restores its saved layout: workspace and pane ids
//! stay, every terminal is new, and no CLI runs in it. Firecrab rebinds each open
//! session once, then starts its CLI again. Claude Code and Codex continue their
//! conversation; a CLI without a known conversation id starts a new one in the same
//! folder. A Codex thread id is known only from the rollout its own pane process held.
use super::*;
use fs2::FileExt;
use std::{collections::BTreeMap, sync::Mutex};

#[cfg(all(test, unix))]
mod tests;

/// Herdr settings for Firecrab's private server. Firecrab starts each CLI itself,
/// so Herdr must not start a second one in the same pane.
pub(super) const HERDR_CONFIG: &str = "[session]\nresume_agents_on_restore = false\n";
const SERVER_LOCK: &str = "herdr.lock";
pub(super) const RELAUNCH_LOCK: &str = "relaunch.lock";
/// Every read-modify-write of `sessions.json` holds this lock.
pub(super) const RECORDS_LOCK: &str = "records.lock";
/// Written before a server start and removed after its sessions are rebound.
const RESTORE_PENDING: &str = "restore-pending";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) enum Recovery {
    /// The pane is back, but its CLI does not run yet.
    Pending,
    /// The CLI continued its previous conversation.
    Resumed,
    /// The CLI started a new conversation in the same folder.
    Fresh,
}

/// An exclusive file lock, released on drop.
pub(super) struct Held(fs::File);
impl Drop for Held {
    fn drop(&mut self) {
        // Closing alone can leave a lock held by descriptors inherited at fork.
        let _ = FileExt::unlock(&self.0);
    }
}

/// Open coding sessions. Sign-in helpers are never started again.
fn restorable(agent: &Agent) -> bool {
    !agent.closed && agent.role != "authentication" && !agent.pane_id.is_empty()
}
/// A restored session whose CLI does not run yet.
fn waiting(agent: &Agent) -> bool {
    restorable(agent) && agent.recovery == Some(Recovery::Pending)
}

/// Arguments after `--` in `herdr agent start`. With `resume`, the CLI continues the
/// conversation saved under this session's conversation id.
pub(super) fn cli_args(agent: &Agent, resume: bool) -> Vec<String> {
    let mut args = Vec::new();
    match agent.conversation_id {
        // Codex names its thread itself; Firecrab only resumes one it recorded.
        Some(thread) if agent.kind == "codex" => {
            if resume {
                args.extend(["resume".to_owned(), thread.to_string()]);
            }
        }
        Some(conversation) => {
            args.push(if resume { "--resume" } else { "--session-id" }.to_owned());
            args.push(conversation.to_string());
        }
        None => {}
    }
    let (model, effort) = match &agent.chosen {
        Some(chosen) => (chosen.model.as_str(), chosen.effort.as_str()),
        None => (agent.model.as_str(), ""),
    };
    let claude = agent.kind == "claude";
    // Codex and Grok Build both take `-m`.
    if !model.is_empty() {
        args.push(if claude { "--model" } else { "-m" }.to_owned());
        args.push(model.to_owned());
    }
    if !effort.is_empty() {
        if claude {
            args.extend(["--effort".to_owned(), effort.to_owned()]);
        } else if agent.kind == "codex" {
            // Codex has no effort flag. Any `-c` makes it skip its shared
            // background server, so only a chosen effort adds one.
            args.extend(["-c".to_owned(), format!("model_reasoning_effort={effort}")]);
        }
        // Grok Build documents `--effort` but not its levels, so it takes no effort.
    }
    if agent.kind == "codex" {
        // Without the shared background server, the pane's own Codex process holds
        // its rollout. That ties each thread to its session, and `-c` prints no
        // "Running without the shared background server" warning.
        args.push("--no-daemon".to_owned());
    }
    args
}

/// Prints `found` when a Claude Code transcript for `id` holds a user message.
/// A transcript with only metadata cannot be resumed.
fn transcript_check(id: Uuid) -> String {
    format!(
        r#"for f in "${{CLAUDE_CONFIG_DIR:-$HOME/.claude}}"/projects/*/{id}.jsonl; do if [ -f "$f" ] && grep -Eq '"type": ?"user"' "$f"; then printf found; break; fi; done"#
    )
}

/// Prints `found` when a Codex rollout of thread `id` holds a user message.
fn rollout_check(id: Uuid) -> String {
    format!(
        r#"for f in "${{CODEX_HOME:-$HOME/.codex}}"/sessions/*/*/*/rollout-*-{id}.jsonl "${{CODEX_HOME:-$HOME/.codex}}"/sessions/*/*/*/rollout-*-{id}_*.jsonl; do if [ -f "$f" ] && grep -Eq '"type":"(user_message|UserMessage)"' "$f"; then printf found; break; fi; done"#
    )
}

/// The thread id in a Codex rollout name: `rollout-<timestamp>-<thread>.jsonl`, or
/// `rollout-<timestamp>-<thread>_<rollout>.jsonl` after a rewind.
fn rollout_thread(name: &str) -> Option<Uuid> {
    let core = name.strip_prefix("rollout-")?.strip_suffix(".jsonl")?;
    if core.get(19..20)? != "-" {
        return None;
    }
    let ids = core.get(20..)?;
    Uuid::parse_str(ids.split_once('_').map_or(ids, |(thread, _)| thread)).ok()
}

/// Codex threads are read at most this often, off the request path.
const THREAD_CHECK: Duration = Duration::from_secs(10);
static THREADS_CHECKED: Mutex<BTreeMap<PathBuf, Instant>> = Mutex::new(BTreeMap::new());

impl Runtime {
    pub(super) fn lock_file(&self, name: &str) -> Result<fs::File, Error> {
        fs::create_dir_all(&self.home)?;
        Ok(fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.home.join(name))?)
    }
    pub(super) fn lock(&self, name: &str) -> Result<Held, Error> {
        let file = self.lock_file(name)?;
        file.lock_exclusive()?;
        Ok(Held(file))
    }
    /// Start the private server if it is down. The caller that starts it rebinds the
    /// open sessions before anything else can create a workspace.
    pub(super) fn ensure_server(&self) -> Result<(), Error> {
        let _held = self.lock(SERVER_LOCK)?;
        let pending = self.home.join(RESTORE_PENDING);
        if self.run(&["status", "server", "--json"])?["running"] != true {
            // Mark first: a start that times out can still start the server.
            fs::write(&pending, b"")?;
            self.start_server()?;
        }
        if pending.exists() {
            self.restore_sessions()?;
            fs::remove_file(&pending)?;
        }
        Ok(())
    }
    /// Herdr's panes. Starts the server again when open sessions need it.
    pub(super) fn live_panes(&self) -> Result<Value, Error> {
        let needed = self.records()?.iter().any(restorable);
        if needed && self.home.join(RESTORE_PENDING).exists() {
            self.ensure_server()?;
        }
        match self.run(&["pane", "list"]) {
            Err(_) if needed => {
                self.ensure_server()?;
                self.run(&["pane", "list"])
            }
            listed => listed,
        }
    }
    /// Give each open session the terminal Herdr restored for its pane. A session that
    /// is missing from the saved layout gets a new workspace in its folder.
    fn restore_sessions(&self) -> Result<(), Error> {
        let records = self.records()?;
        if !records.iter().any(restorable) {
            return Ok(());
        }
        let listed = self.run(&["pane", "list"])?;
        let panes = listed["panes"]
            .as_array()
            .or_else(|| listed.as_array())
            .cloned()
            .unwrap_or_default();
        for agent in records.iter().filter(|a| restorable(a)) {
            let live = |p: &&Value| p["pane_id"] == agent.pane_id;
            if panes
                .iter()
                .filter(live)
                .any(|p| p["terminal_id"] == agent.terminal_id)
            {
                continue;
            }
            let pane = match panes
                .iter()
                .filter(live)
                .find(|p| p["workspace_id"] == agent.workspace_id)
            {
                Some(pane) => pane.clone(),
                // Herdr saves its layout a few seconds after a change, so a session
                // created just before the stop can be missing.
                None => match self.run(&[
                    "workspace",
                    "create",
                    "--cwd",
                    &agent.cwd,
                    "--label",
                    &agent.name,
                    "--no-focus",
                ]) {
                    Ok(created) => created["root_pane"].clone(),
                    Err(e) => {
                        self.update(agent, |a| a.launch_error = Some(e.to_string()))?;
                        continue;
                    }
                },
            };
            let (Some(workspace), Some(pane), Some(terminal)) = (
                pane["workspace_id"].as_str(),
                pane["pane_id"].as_str(),
                pane["terminal_id"].as_str(),
            ) else {
                return Err(invalid("Herdr did not return a terminal identity"));
            };
            self.update(agent, |a| {
                a.workspace_id = workspace.into();
                a.pane_id = pane.into();
                a.terminal_id = terminal.into();
                a.launch_error = None;
                a.recovery = Some(Recovery::Pending);
            })?;
        }
        Ok(())
    }
    /// Start the CLIs of restored sessions off the request path. A CLI can take many
    /// seconds to become ready.
    pub(super) fn relaunch_in_background(&self, records: &[Agent]) {
        if !records.iter().any(waiting) {
            return;
        }
        let runtime = self.clone();
        std::thread::spawn(move || {
            let _ = runtime.relaunch();
        });
    }
    fn relaunch(&self) -> Result<(), Error> {
        let file = self.lock_file(RELAUNCH_LOCK)?;
        if file.try_lock_exclusive().is_err() {
            return Ok(());
        }
        let _held = Held(file);
        let ids = self
            .records()?
            .into_iter()
            .filter(waiting)
            .map(|a| a.id)
            .collect::<Vec<_>>();
        for id in ids {
            let mut agent = self.agent(id)?;
            // Start only in the terminal this session was rebound to.
            if !waiting(&agent) || self.pane(&agent).is_err() {
                continue;
            }
            let resume = match agent.conversation_id {
                Some(conversation) => match self.has_conversation(&agent.kind, conversation) {
                    Ok(found) => found,
                    Err(_) => continue,
                },
                None => false,
            };
            // A fresh Claude Code gets an unused id, saved before the CLI can start
            // with it. A fresh Codex thread is recorded once its pane holds it.
            let conversation = (agent.kind == "claude").then(Uuid::new_v4);
            if !resume && conversation != agent.conversation_id {
                agent.conversation_id = conversation;
                self.update(&agent, |a| a.conversation_id = conversation)?;
            }
            let launched = self.start_cli(&agent, resume);
            self.update(&agent, |a| {
                a.recovery = Some(if resume {
                    Recovery::Resumed
                } else {
                    Recovery::Fresh
                });
                a.launch_error = launched.err().map(|e| e.to_string());
            })?;
        }
        Ok(())
    }
    /// Record the thread of each open Codex session from the rollout that its own pane
    /// process holds, so a restarted VM can resume it. Off the request path, at most
    /// once per `THREAD_CHECK`. Codex on the shared server holds no rollout in the pane,
    /// so such a session stays without a thread.
    pub(super) fn remember_threads_in_background(&self, records: &[Agent]) {
        let codex = records
            .iter()
            .filter(|a| a.kind == "codex" && restorable(a) && !waiting(a))
            .cloned()
            .collect::<Vec<_>>();
        if self.guest.is_none() || codex.is_empty() {
            return;
        }
        {
            let mut checked = THREADS_CHECKED
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let now = Instant::now();
            if checked
                .get(&self.home)
                .is_some_and(|at| now.duration_since(*at) < THREAD_CHECK)
            {
                return;
            }
            checked.insert(self.home.clone(), now);
        }
        let runtime = self.clone();
        std::thread::spawn(move || {
            let _ = runtime.remember_threads(&codex);
        });
    }
    fn remember_threads(&self, agents: &[Agent]) -> Result<(), Error> {
        let guest = self
            .guest
            .as_ref()
            .ok_or_else(|| invalid("Select a MicroVM first."))?;
        let mut script = format!(
            "processes() {{ {} pane process-info --pane \"$1\"; }}\n{}",
            self.guest_herdr(),
            include_str!("preview.sh")
        );
        for agent in agents {
            script.push_str(&format!(
                "thread {} {}\n",
                guest::quote(&agent.id.to_string()),
                guest::quote(&agent.pane_id)
            ));
        }
        let output = guest.shell(&script, Duration::from_secs(20))?;
        for line in output.lines() {
            let Some((id, name)) = line
                .strip_prefix("FIRECRAB_THREAD ")
                .and_then(|rest| rest.split_once(' '))
            else {
                continue;
            };
            let (Ok(id), Some(thread)) = (Uuid::parse_str(id), rollout_thread(name)) else {
                continue;
            };
            let Some(agent) = agents.iter().find(|a| a.id == id) else {
                continue;
            };
            if agent.conversation_id != Some(thread) {
                self.update(agent, |a| {
                    if a.recovery != Some(Recovery::Pending) {
                        a.conversation_id = Some(thread);
                    }
                })?;
            }
        }
        Ok(())
    }
    /// Change a record only while it is open and still names the same terminal.
    fn update(&self, agent: &Agent, edit: impl FnOnce(&mut Agent)) -> Result<(), Error> {
        self.change(|records| {
            if let Some(current) = records
                .iter_mut()
                .find(|a| a.id == agent.id && a.terminal_id == agent.terminal_id && !a.closed)
            {
                edit(current);
            }
            Ok(())
        })
    }
    /// Whether the CLI saved messages under this conversation id.
    fn has_conversation(&self, kind: &str, id: Uuid) -> Result<bool, Error> {
        let script = match kind {
            "claude" => transcript_check(id),
            "codex" => rollout_check(id),
            _ => return Ok(false),
        };
        let output = match &self.guest {
            Some(guest) => guest.shell(&script, Duration::from_secs(10))?,
            None => Command::new("sh")
                .args(["-c", &script])
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
                .unwrap_or_default(),
        };
        Ok(output.trim() == "found")
    }
    /// Start the session's coding CLI. Herdr starts an agent only in an idle shell,
    /// so one pane never runs two.
    pub(super) fn start_cli(&self, agent: &Agent, resume: bool) -> Result<Value, Error> {
        let alias = format!("fc-{}", &agent.id.simple().to_string()[..12]);
        let extra = cli_args(agent, resume);
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
        if !extra.is_empty() {
            args.push("--");
            args.extend(extra.iter().map(String::as_str));
        }
        self.run(&args)
    }
}
