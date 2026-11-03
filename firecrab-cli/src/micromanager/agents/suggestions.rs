//! Task suggestions for the New task form. The selected CLI reads the VM project once,
//! non-interactively and read-only, and proposes tasks as JSON. Its reply is untrusted:
//! only validated, trimmed fields reach the browser, and it is never logged.
use super::*;
use std::sync::Mutex;

const MAX_TITLE: usize = 80;
const MAX_REASON: usize = 160;
const MAX_TASK: usize = 2000;
const MAX_SUGGESTIONS: usize = 5;
/// Five full suggestions in Korean stay well under this; a longer reply fails to parse.
const MAX_OUTPUT: usize = 60_000;
/// App runs of the project workspace; see `automation_root`.
const LAST_RUN: &str = r#"root=/root/.local/share/firecrab/automation/project
[ -f "$root/latest" ] || exit 0
job=$(head -c 64 "$root/latest")
case "$job" in *[!a-f0-9-]*|'') exit 0;; esac
for field in state phase exit; do
  if [ -f "$root/$job/$field" ]; then printf '%s:' "$field"; head -c 32 "$root/$job/$field" | tr -d '\n'; printf '\n'; fi
done
"#;
const PHASES: &[&str] = &["starting", "tests", "build", "preview"];
const EFFORTS: &[&str] = &["low", "medium", "high", "xhigh", "max"];

#[derive(Deserialize, Default, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    #[default]
    En,
    Ko,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Suggest {
    pub cli: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub effort: String,
    #[serde(default)]
    pub language: Language,
}

/// One suggestion run per VM at a time; a second request gets `busy` instead of a second run.
struct Running(Uuid);
static RUNNING: Mutex<Vec<Uuid>> = Mutex::new(Vec::new());
impl Running {
    fn start(vm: Uuid) -> Option<Self> {
        let mut running = RUNNING.lock().unwrap_or_else(|p| p.into_inner());
        if running.contains(&vm) {
            return None;
        }
        running.push(vm);
        Some(Self(vm))
    }
}
impl Drop for Running {
    fn drop(&mut self) {
        RUNNING
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .retain(|vm| *vm != self.0);
    }
}

/// A short hint when the project's last App run failed; anything unexpected gives none.
pub(super) fn last_run_hint(text: &str) -> Option<String> {
    let field = |name: &str| {
        text.lines()
            .find_map(|line| line.strip_prefix(name)?.strip_prefix(':'))
            .map(str::trim)
    };
    let phase = field("phase").filter(|phase| PHASES.contains(phase))?;
    match field("state")? {
        "failed" => Some(match field("exit").and_then(|v| v.parse::<i32>().ok()) {
            Some(code) => format!(
                "The last App check of this project failed in its \"{phase}\" step with exit code {code}."
            ),
            None => format!("The last App check of this project failed in its \"{phase}\" step."),
        }),
        _ => None,
    }
}

pub(super) fn prompt(language: Language, hint: Option<&str>) -> String {
    let language = match language {
        Language::Ko => "Korean (한국어)",
        Language::En => "English",
    };
    let mut prompt = format!(
        "You suggest the next coding tasks for the repository in the current folder.\n\
         Look at the repository briefly with read-only actions. Do not create, change or delete any file. \
         Do not run tests, builds, installs, servers or network commands. Finish within one minute.\n\
         Return 3 to 5 suggestions as one JSON array and nothing else: no code fence, no explanation.\n\
         Each item is an object with exactly these string fields:\n\
         - \"title\": one line, at most 80 characters.\n\
         - \"reason\": one line, at most 160 characters, why this task helps this repository now.\n\
         - \"task\": the full instructions for a coding agent, at most 2000 characters.\n\
         Prefer small, concrete tasks that fit the repository's current state. \
         If the repository is empty, suggest a first small app and its tests.\n\
         Write every title, reason and task in {language}.\n"
    );
    if let Some(hint) = hint {
        prompt.push_str(&format!(
            "{hint} Make the first suggestion a task that finds and fixes that failure.\n"
        ));
    }
    prompt
}

/// The CLI runs once in the project folder, from a prompt on stdin, and prints nothing but
/// `STATUS:<code>` and, on success, its final reply. Its own logs never leave the VM.
pub(super) fn suggest_command(cli: &str, model: &str, effort: &str) -> String {
    let mut flags = String::new();
    let run = if cli == "claude" {
        if !model.is_empty() {
            flags.push_str(&format!(" --model {}", guest::quote(model)));
        }
        if !effort.is_empty() {
            flags.push_str(&format!(" --effort {}", guest::quote(effort)));
        }
        // An older Claude Code without these flags must not run with its default tools.
        format!(
            r#"help=$(claude --help 2>/dev/null || true)
for flag in '--tools <' '--permission-mode <' --strict-mcp-config --no-session-persistence; do
  printf '%s' "$help" | grep -q -e "$flag" || {{ printf 'STATUS:outdated\n'; exit 0; }}
done
timeout --kill-after=5 120 claude -p --output-format text --tools Read,Grep,Glob --permission-mode plan --strict-mcp-config --no-session-persistence{flags} < "$work/prompt.txt" > "$work/out.txt" 2>/dev/null || status=$?"#
        )
    } else {
        if !model.is_empty() {
            flags.push_str(&format!(" -m {}", guest::quote(model)));
        }
        if !effort.is_empty() {
            flags.push_str(&format!(
                " -c {}",
                guest::quote(&format!("model_reasoning_effort=\"{effort}\""))
            ));
        }
        format!(
            r#"timeout --kill-after=5 120 codex exec --sandbox read-only -c 'approval_policy="never"' --ignore-user-config --ignore-rules --ephemeral --skip-git-repo-check --color never -o "$work/out.txt"{flags} - < "$work/prompt.txt" >/dev/null 2>&1 || status=$?"#
        )
    };
    format!(
        r#"set -u
export DISABLE_AUTOUPDATER=1
work=$(mktemp -d /tmp/firecrab-suggest.XXXXXX) || exit 1
trap 'rm -rf "$work"' EXIT
cat > "$work/prompt.txt"
cd {workspace} || exit 1
status=0
{run}
printf 'STATUS:%s\n' "$status"
if [ "$status" -eq 0 ] && [ -f "$work/out.txt" ]; then head -c {MAX_OUTPUT} "$work/out.txt"; fi
"#,
        workspace = guest::quote(project::WORKSPACE),
    )
}

/// Single-line fields lose every control character; the task keeps its line breaks.
fn clean(text: &str, limit: usize, multiline: bool) -> Option<String> {
    let bidi = |c: char| matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}');
    let text: String = text
        .chars()
        .filter_map(|c| match c {
            c if bidi(c) || c == '\r' => None,
            '\n' if multiline => Some('\n'),
            '\n' | '\t' => Some(' '),
            c if c.is_control() => None,
            c => Some(c),
        })
        .collect();
    let text: String = text.trim().chars().take(limit).collect();
    let text = text.trim_end();
    (!text.is_empty()).then(|| text.to_owned())
}

/// The CLI's reply as at most five valid suggestions, or `None` when nothing usable came back.
pub(super) fn parse_suggestions(text: &str) -> Option<Vec<Value>> {
    let text = text.trim();
    let items: Vec<Value> = serde_json::from_str(text).ok().or_else(|| {
        // Tolerate a code fence or a sentence around the array.
        let start = text.find('[')?;
        let end = text.rfind(']')?;
        serde_json::from_str(text.get(start..=end)?).ok()
    })?;
    let suggestions: Vec<Value> = items
        .iter()
        .filter_map(|item| {
            let field =
                |name: &str, limit, multiline| clean(item.get(name)?.as_str()?, limit, multiline);
            Some(json!({
                "title": field("title", MAX_TITLE, false)?,
                "reason": field("reason", MAX_REASON, false)?,
                "task": field("task", MAX_TASK, true)?,
            }))
        })
        .take(MAX_SUGGESTIONS)
        .collect();
    (!suggestions.is_empty()).then_some(suggestions)
}

impl Runtime {
    pub fn suggest_tasks(&self, input: Suggest) -> Result<Value, Error> {
        let guest = self
            .guest
            .as_ref()
            .ok_or_else(|| invalid("Select a MicroVM first."))?;
        let name = match input.cli.as_str() {
            "codex" => "Codex",
            "claude" => "Claude Code",
            _ => {
                return Err(invalid(
                    "Task suggestions work only with Codex and Claude Code.",
                ));
            }
        };
        if !guest.available.contains(&input.cli) || !guest.authenticated.contains(&input.cli) {
            return Err(invalid(format!(
                "Sign in to {name} in this MicroVM before getting suggestions."
            )));
        }
        if input.model.len() > 128
            || input.model.chars().any(char::is_control)
            || !(input.effort.is_empty() || EFFORTS.contains(&input.effort.as_str()))
        {
            return Err(invalid("Invalid suggestion request."));
        }
        if self.vm_project()?["state"] != "ready" {
            return Err(invalid("Prepare the repository in Workspace first."));
        }
        let Some(_running) = Running::start(guest.id) else {
            return Ok(json!({"state": "busy", "cli": input.cli, "name": name}));
        };
        // Codex runs without the user's config (MCP servers, hooks, approval rules), so its
        // configured default model and effort are passed explicitly instead.
        let (mut model, mut effort) = (input.model, input.effort);
        if input.cli == "codex" && (model.is_empty() || effort.is_empty()) {
            let (default_model, default_effort) = guest.cli_defaults("codex");
            if model.is_empty() {
                model = default_model;
            }
            if effort.is_empty() && EFFORTS.contains(&default_effort.as_str()) {
                effort = default_effort;
            }
        }
        let hint = guest
            .shell(LAST_RUN, Duration::from_secs(8))
            .ok()
            .and_then(|text| last_run_hint(&text));
        let output = guest.shell_with_input(
            &suggest_command(&input.cli, &model, &effort),
            prompt(input.language, hint.as_deref()).as_bytes(),
            Duration::from_secs(150),
        )?;
        let (status, reply) = output.split_once('\n').unwrap_or((&output, ""));
        let state = match status.strip_prefix("STATUS:") {
            Some("0") => match parse_suggestions(reply) {
                Some(suggestions) => {
                    return Ok(json!({
                        "state": "ready",
                        "cli": input.cli,
                        "name": name,
                        "suggestions": suggestions,
                    }));
                }
                None => "invalid",
            },
            Some("124" | "137") => "timeout",
            Some("outdated") => "outdated",
            _ => "failed",
        };
        Ok(json!({"state": state, "cli": input.cli, "name": name, "suggestions": []}))
    }
}
