//! The VM repository's AGENTS.md: read, agent-refined proposals, and approved writes.
//! An agent only proposes one block; nothing is written until the user approves it.
pub mod blocks;

use super::*;
use base64::{Engine, engine::general_purpose::STANDARD};

pub const RULES_PATH: &str = "/workspace/firecrab/project/AGENTS.md";
const CLAUDE_MD: &str = "/root/.claude/CLAUDE.md";
const GEMINI_SETTINGS: &str = "/root/.gemini/settings.json";
const IMPORT: &str = "@/workspace/firecrab/project/AGENTS.md";
const MAX_RULES: usize = 64 * 1024;
const MAX_INPUT: usize = 16 * 1024;
const MAX_PROPOSAL: usize = 8 * 1024;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Add,
    Edit,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Refine {
    pub mode: Mode,
    #[serde(default)]
    pub block: Option<usize>,
    pub input: String,
    pub cli: String,
    #[serde(default)]
    pub model: String,
    pub digest: String,
    #[serde(default)]
    pub previous: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Apply {
    pub mode: Mode,
    #[serde(default)]
    pub block: Option<usize>,
    pub text: String,
    pub digest: String,
}

/// The ready document; any other state is read-only with a reason.
struct Document {
    exists: bool,
    text: String,
    digest: String,
}

fn sha256(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}

fn unavailable(reason: &str) -> Value {
    json!({"state":"unavailable","reason":reason,"exists":false,"text":"","digest":"","blocks":[]})
}

/// Strip surrounding whitespace and one wrapping code fence; reject empty or oversized output.
fn clean_proposal(output: &str) -> Result<String, Error> {
    let mut text = output.trim();
    if let Some(inner) = text.strip_prefix("```")
        && let Some((_, body)) = inner.split_once('\n')
        && let Some(body) = body.trim_end().strip_suffix("```")
        && !body.contains("\n```")
    {
        text = body.trim();
    }
    if text.is_empty() {
        return Err(invalid("The agent returned no rule. Try again."));
    }
    if text.len() > MAX_PROPOSAL {
        return Err(invalid("The agent's rule is longer than 8 KiB."));
    }
    Ok(text.to_owned())
}

fn prompt(mode: Mode, before: &str, previous: &str, input: &str, text: &str) -> String {
    let mut prompt = String::from(
        "You edit one rule in an AGENTS.md file for coding agents.\nReturn only the rule text in Markdown. No explanation, no code fence.\nKeep it short, clear and imperative. Keep the user's language.\n",
    );
    prompt.push_str(match mode {
        Mode::Add => "Mode: add\n",
        Mode::Edit => "Mode: edit\n",
    });
    if mode == Mode::Edit {
        prompt.push_str(&format!("Current rule:\n{before}\n"));
    }
    if !previous.trim().is_empty() {
        prompt.push_str(&format!("Previous proposal to improve:\n{previous}\n"));
    }
    prompt.push_str(&format!(
        "User request:\n{input}\nWhole AGENTS.md for context (do not repeat it):\n{text}\n"
    ));
    prompt
}

/// Run the selected CLI once, without tools on the repository, from an empty temporary folder.
fn refine_command(cli: &str, model: &str) -> String {
    let model = |flag: &str| {
        if model.is_empty() {
            String::new()
        } else {
            format!(" {flag} {}", guest::quote(model))
        }
    };
    let run = match cli {
        "claude" => format!(
            "timeout 120 claude -p --output-format text{} < prompt.txt",
            model("--model")
        ),
        _ => format!(
            "timeout 120 codex exec --sandbox read-only --skip-git-repo-check --ephemeral -o \"$work/out.txt\"{} - < prompt.txt >/dev/null\ncat \"$work/out.txt\"",
            model("-m")
        ),
    };
    format!(
        "set -eu\n# firecrab-refine\nexport DISABLE_AUTOUPDATER=1\nwork=$(mktemp -d /tmp/firecrab-refine.XXXXXX)\ntrap 'rm -rf \"$work\"' EXIT\ncd \"$work\"\ncat > prompt.txt\n{run}\n"
    )
}

impl Runtime {
    fn guest_ref(&self) -> Result<&guest::Guest, Error> {
        self.guest
            .as_ref()
            .ok_or_else(|| invalid("Select a MicroVM first."))
    }

    /// `None` when the file does not exist; the content may be one byte over `limit`.
    fn read_guest_file(&self, path: &str, limit: usize) -> Result<Option<Vec<u8>>, Error> {
        let quoted = guest::quote(path);
        // base64 needs 4 bytes per 3, plus room for the one-byte-over marker.
        let output = self.guest_ref()?.shell_capture(
            &format!(
                "if [ -f {quoted} ]; then head -c {} {path} | base64 -w0; else printf missing; fi",
                limit + 1
            ),
            Duration::from_secs(10),
            (limit as u64 + 4) / 3 * 4 + 16,
        )?;
        let output = output.trim();
        if output == "missing" {
            return Ok(None);
        }
        STANDARD
            .decode(output)
            .map(Some)
            .map_err(|_| invalid("Invalid MicroVM file response."))
    }

    fn write_guest_file(&self, path: &str, bytes: &[u8]) -> Result<(), Error> {
        let directory = Path::new(path)
            .parent()
            .and_then(Path::to_str)
            .ok_or_else(|| invalid("Invalid MicroVM file path."))?;
        let temporary = format!("{path}.firecrab-tmp");
        self.guest_ref()?.shell_with_input(
            &format!(
                "mkdir -p {} && cat > {} && mv {} {}",
                guest::quote(directory),
                guest::quote(&temporary),
                guest::quote(&temporary),
                guest::quote(path)
            ),
            bytes,
            Duration::from_secs(20),
        )?;
        Ok(())
    }

    fn document(&self) -> Result<Result<Document, Value>, Error> {
        if self.vm_project()?["state"] != "ready" {
            return Ok(Err(unavailable(
                "Prepare the repository in Workspace first.",
            )));
        }
        let bytes = self.read_guest_file(RULES_PATH, MAX_RULES)?;
        let exists = bytes.is_some();
        let bytes = bytes.unwrap_or_default();
        if bytes.len() > MAX_RULES {
            return Ok(Err(unavailable(
                "AGENTS.md is larger than 64 KiB. Edit it in the repository.",
            )));
        }
        let Ok(text) = String::from_utf8(bytes) else {
            return Ok(Err(unavailable(
                "AGENTS.md is not UTF-8 text. Edit it in the repository.",
            )));
        };
        Ok(Ok(Document {
            exists,
            digest: sha256(&text),
            text,
        }))
    }

    /// A ready document, or the reason it cannot be edited as an error.
    fn editable(&self, digest: &str) -> Result<Document, Error> {
        let document = self.document()?.map_err(|value| {
            invalid(
                value["reason"]
                    .as_str()
                    .unwrap_or("AGENTS.md is unavailable."),
            )
        })?;
        if document.digest != digest {
            return Err(invalid("The file changed. Reload it and refine again."));
        }
        Ok(document)
    }

    pub fn rules(&self) -> Result<Value, Error> {
        Ok(match self.document()? {
            Ok(document) => json!({
                "state": "ready",
                "reason": null,
                "exists": document.exists,
                "blocks": blocks::split(&document.text),
                "digest": document.digest,
                "text": document.text,
            }),
            Err(value) => value,
        })
    }

    pub fn refine_rules(&self, input: Refine) -> Result<Value, Error> {
        let guest = self.guest_ref()?;
        let (name, command) = match input.cli.as_str() {
            "codex" => ("Codex", "codex"),
            "claude" => ("Claude Code", "claude"),
            _ => return Err(invalid("Select Codex or Claude Code to refine rules.")),
        };
        if !guest.available.contains(command) || !guest.authenticated.contains(&input.cli) {
            return Err(invalid(format!(
                "Connect {name} in this MicroVM before refining rules."
            )));
        }
        if input.input.trim().is_empty() || input.input.len() > MAX_INPUT {
            return Err(invalid("Enter a rule request up to 16 KiB."));
        }
        if input.previous.len() > MAX_PROPOSAL
            || input.model.len() > 128
            || input.model.chars().any(|c| c.is_control() || c == '\'')
        {
            return Err(invalid("Invalid refine request."));
        }
        let document = self.editable(&input.digest)?;
        let before = match (input.mode, input.block) {
            (Mode::Edit, Some(index)) => {
                let block = *blocks::split(&document.text)
                    .get(index)
                    .ok_or_else(|| invalid("That rule no longer exists. Reload the file."))?;
                blocks::text_of(&document.text, block)
            }
            (Mode::Edit, None) => return Err(invalid("Select the rule to edit.")),
            (Mode::Add, _) => String::new(),
        };
        let output = guest.shell_with_input(
            &refine_command(&input.cli, &input.model),
            prompt(
                input.mode,
                &before,
                &input.previous,
                &input.input,
                &document.text,
            )
            .as_bytes(),
            Duration::from_secs(150),
        )?;
        Ok(json!({
            "mode": input.mode,
            "block": if input.mode == Mode::Edit { input.block } else { None },
            "before": before,
            "after": clean_proposal(&output)?,
            "cli": input.cli,
            "name": name,
            "model": input.model,
            "digest": document.digest,
        }))
    }

    /// Write an approved proposal. Broken CLI wiring is reported, never a reason to drop the rule.
    pub fn apply_rules(&self, input: Apply) -> Result<Value, Error> {
        if input.text.trim().is_empty() || input.text.len() > MAX_PROPOSAL {
            return Err(invalid("A rule must be 1 byte to 8 KiB."));
        }
        let document = self.editable(&input.digest)?;
        let updated = match input.mode {
            Mode::Edit => blocks::replace(
                &document.text,
                input
                    .block
                    .ok_or_else(|| invalid("Select the rule to edit."))?,
                &input.text,
            )?,
            Mode::Add => blocks::append(&document.text, &input.text),
        };
        let warning = self.ensure_rule_wiring().err().map(|e| e.to_string());
        self.write_guest_file(RULES_PATH, updated.as_bytes())?;
        let mut value = self.rules()?;
        value["warning"] = json!(warning);
        Ok(value)
    }

    /// Claude Code imports the repository AGENTS.md; Gemini CLI lists it as a context file.
    pub(super) fn ensure_rule_wiring(&self) -> Result<(), Error> {
        let claude = self
            .read_guest_file(CLAUDE_MD, MAX_RULES)?
            .unwrap_or_default();
        let claude = String::from_utf8(claude)
            .map_err(|_| invalid("~/.claude/CLAUDE.md is not UTF-8 text."))?;
        if !claude.lines().any(|l| l.trim() == IMPORT) {
            let mut updated = claude.clone();
            if !updated.is_empty() {
                if !updated.ends_with('\n') {
                    updated.push('\n');
                }
                updated.push('\n');
            }
            updated.push_str(&format!(
                "<!-- firecrab:agents-md -->\n{IMPORT}\n<!-- /firecrab:agents-md -->\n"
            ));
            self.write_guest_file(CLAUDE_MD, updated.as_bytes())?;
        }
        let raw = self
            .read_guest_file(GEMINI_SETTINGS, MAX_RULES)?
            .unwrap_or_default();
        let mut settings: Value = if raw.iter().all(u8::is_ascii_whitespace) {
            json!({})
        } else {
            serde_json::from_slice(&raw)
                .map_err(|_| invalid("~/.gemini/settings.json is not valid JSON."))?
        };
        let object = settings
            .as_object_mut()
            .ok_or_else(|| invalid("~/.gemini/settings.json is not a JSON object."))?;
        let context = object.entry("context").or_insert_with(|| json!({}));
        let context = context
            .as_object_mut()
            .ok_or_else(|| invalid("~/.gemini/settings.json context is not an object."))?;
        let names = match context.get("fileName") {
            None => Some(json!(["AGENTS.md", "GEMINI.md"])),
            Some(Value::String(name)) if name == "AGENTS.md" => None,
            Some(Value::String(name)) => Some(json!([name, "AGENTS.md"])),
            Some(Value::Array(names)) if names.iter().any(|n| n == "AGENTS.md") => None,
            Some(Value::Array(names)) => {
                let mut names = names.clone();
                names.push(json!("AGENTS.md"));
                Some(Value::Array(names))
            }
            Some(_) => {
                return Err(invalid(
                    "~/.gemini/settings.json context.fileName is not a string or list.",
                ));
            }
        };
        if let Some(names) = names {
            context.insert("fileName".into(), names);
            self.write_guest_file(GEMINI_SETTINGS, &serde_json::to_vec_pretty(&settings)?)?;
        }
        Ok(())
    }
}

#[cfg(all(test, unix))]
mod tests;
