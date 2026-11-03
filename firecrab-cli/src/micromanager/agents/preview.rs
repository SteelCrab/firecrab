//! The latest input and reply of each CLI session, read from the CLI's own transcript in the VM.
//! Herdr names the processes in each pane. Claude Code registers the session of each process.
//! Codex without its shared daemon keeps its rollouts open in the pane's own process; on the
//! shared daemon a card stays empty. Messages typed in the terminal are included.
use super::*;

#[cfg(all(test, unix))]
mod tests;

/// Characters kept from each message: enough for a board card, never a whole transcript.
const PREVIEW_CHARS: usize = 280;
const SECTION: &str = "FIRECRAB_PREVIEW ";

#[derive(Clone, Copy)]
enum Role {
    Input,
    /// A slash or shell command. Only a command that the CLI answers is a conversation turn.
    Command,
    Response,
}

impl Runtime {
    pub fn previews(&self) -> Result<Value, Error> {
        let guest = self
            .guest
            .as_ref()
            .ok_or_else(|| invalid("Select a MicroVM first."))?;
        let agents = self
            .records()?
            .into_iter()
            .filter(|a| {
                !a.closed
                    && a.role != "authentication"
                    && ["codex", "claude"].contains(&a.kind.as_str())
            })
            .collect::<Vec<_>>();
        if agents.is_empty() {
            return Ok(json!({"previews": []}));
        }
        let mut script = format!(
            "processes() {{ {} pane process-info --pane \"$1\"; }}\n{}",
            self.guest_herdr(),
            include_str!("preview.sh")
        );
        for agent in &agents {
            script.push_str(&format!(
                "preview {} {} {}\n",
                guest::quote(&agent.id.to_string()),
                guest::quote(&agent.kind),
                guest::quote(&agent.pane_id)
            ));
        }
        let output = guest.shell_capture(&script, Duration::from_secs(20), 4 * 1024 * 1024)?;
        let mut sections: Vec<(&str, Vec<&str>)> = Vec::new();
        for line in output.lines() {
            match line.strip_prefix(SECTION) {
                Some(id) => sections.push((id, Vec::new())),
                None => {
                    if let Some((_, lines)) = sections.last_mut() {
                        lines.push(line);
                    }
                }
            }
        }
        let previews = agents
            .iter()
            .map(|agent| {
                let id = agent.id.to_string();
                let lines = sections
                    .iter()
                    .find(|(section, _)| *section == id)
                    .map_or(&[][..], |(_, lines)| lines.as_slice());
                let (input, response) = latest(lines);
                json!({"id": id, "input": input, "response": response})
            })
            .collect::<Vec<_>>();
        Ok(json!({ "previews": previews }))
    }
}

/// The last input, and the reply to it if one followed. An older reply belongs to an older input.
fn latest(lines: &[&str]) -> (Option<String>, Option<String>) {
    let mut input = None;
    let mut response = None;
    let mut command = None;
    for (role, text) in lines.iter().filter_map(|line| message(line)) {
        match role {
            Role::Input => {
                input = Some(text);
                response = None;
                command = None;
            }
            Role::Command => command = Some(text),
            Role::Response => {
                if let Some(command) = command.take() {
                    input = Some(command);
                }
                response = Some(text);
            }
        }
    }
    (input, response)
}

fn message(line: &str) -> Option<(Role, String)> {
    let (role, text) = match serde_json::from_str::<Value>(line) {
        Ok(record) => record_message(&record)?,
        // The guest cuts very long lines. The start of their text is still a preview.
        Err(_) => cut_message(line)?,
    };
    let (role, text) = match role {
        Role::Input => typed(&text)?,
        role => (role, text),
    };
    let text = preview(&text);
    (!text.is_empty()).then_some((role, text))
}

fn record_message(record: &Value) -> Option<(Role, String)> {
    let payload = &record["payload"];
    let item = &payload["item"];
    // A compaction summary is a user record that the user did not type.
    let main = record["isMeta"] != true
        && record["isSidechain"] != true
        && record["isCompactSummary"] != true;
    // Older Claude Code versions name no origin. Newer ones mark notices and injected turns.
    let human = |origin: &Value| origin.as_str().is_none_or(|kind| kind == "human");
    match (record["type"].as_str()?, payload["type"].as_str()) {
        // Codex, legacy history mode.
        ("event_msg", Some("user_message")) => {
            Some((Role::Input, payload["message"].as_str()?.into()))
        }
        ("event_msg", Some("agent_message")) => {
            Some((Role::Response, payload["message"].as_str()?.into()))
        }
        // Codex, paginated history mode.
        ("event_msg", Some("item_completed")) => match item["type"].as_str()? {
            "UserMessage" => Some((Role::Input, texts(&item["content"]))),
            "AgentMessage" => Some((Role::Response, texts(&item["content"]))),
            _ => None,
        },
        // Claude Code also stores tool results, commands and notices as user records.
        ("user", _) if main && human(&record["origin"]["kind"]) && human(&record["turnOrigin"]) => {
            let content = &record["message"]["content"];
            if content
                .as_array()
                .is_some_and(|parts| parts.iter().any(|part| part["type"] == "tool_result"))
            {
                return None;
            }
            let text = content
                .as_str()
                .map_or_else(|| texts(content), str::to_owned);
            Some((Role::Input, text))
        }
        ("assistant", _) if main => Some((Role::Response, texts(&record["message"]["content"]))),
        _ => None,
    }
}

/// Text parts only: images, reasoning and tool calls are left out.
fn texts(content: &Value) -> String {
    content
        .as_array()
        .into_iter()
        .flatten()
        .filter(|part| {
            part["type"]
                .as_str()
                .is_some_and(|kind| kind.eq_ignore_ascii_case("text"))
        })
        .filter_map(|part| part["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

/// What the user typed, without context or command output the CLI stored with it.
fn typed(text: &str) -> Option<(Role, String)> {
    let mut text = text.to_owned();
    for tag in [
        "system-reminder",
        "bash-stdout",
        "bash-stderr",
        "local-command-stdout",
        "local-command-stderr",
    ] {
        text = without(&text, tag);
    }
    let inner = |name: &str| {
        let (_, rest) = text.split_once(&format!("<{name}>"))?;
        let (value, _) = rest.split_once(&format!("</{name}>"))?;
        Some(value.trim())
    };
    if let Some(command) = inner("command-name") {
        let command = match inner("command-args") {
            Some(args) if !args.is_empty() => format!("{command} {args}"),
            _ => command.to_owned(),
        };
        return Some((Role::Command, command));
    }
    if let Some(command) = inner("bash-input") {
        return Some((Role::Command, format!("!{command}")));
    }
    // A line cut inside a command leaves an unclosed tag.
    let notices = [
        "<command-",
        "<bash-",
        "<local-command-",
        "<task-notification>",
        "[Request interrupted by user",
    ];
    let start = text.trim_start();
    (!notices.iter().any(|notice| start.starts_with(notice))).then(|| (Role::Input, text.clone()))
}

/// The text without any `<tag>` block. An unclosed block runs to the end of the text.
fn without(text: &str, tag: &str) -> String {
    let (open, close) = (format!("<{tag}>"), format!("</{tag}>"));
    let mut kept = String::new();
    let mut rest = text;
    while let Some(start) = rest.find(&open) {
        kept.push_str(&rest[..start]);
        match rest[start..].find(&close) {
            Some(end) => rest = &rest[start + end + close.len()..],
            None => return kept,
        }
    }
    kept.push_str(rest);
    kept
}

/// The role and the start of the text of a line the guest cut at its length limit.
fn cut_message(line: &str) -> Option<(Role, String)> {
    let has = |kinds: &[&str]| {
        kinds
            .iter()
            .any(|kind| line.contains(&format!("\"type\":\"{kind}\"")))
    };
    // Claude Code may write the record type after a long message, but its role comes first.
    let role =
        if has(&["user", "user_message", "UserMessage"]) || line.contains("\"role\":\"user\"") {
            Role::Input
        } else if has(&["assistant", "agent_message", "AgentMessage"])
            || line.contains("\"role\":\"assistant\"")
        {
            Role::Response
        } else {
            return None;
        };
    let start = ["\"text\":", "\"message\":", "\"content\":"]
        .iter()
        .filter_map(|key| line.find(&format!("{key}\"")).map(|at| at + key.len()))
        .min()?;
    let text = string_prefix(&line[start..])?;
    // A cut can split a multi-byte character, which the output decoder replaced.
    Some((role, text.trim_end_matches('\u{fffd}').to_owned()))
}

/// Decode a JSON string literal that may be cut short, without a partial escape at the end.
fn string_prefix(literal: &str) -> Option<String> {
    let mut escaped = false;
    for (at, byte) in literal.bytes().enumerate().skip(1) {
        match byte {
            _ if escaped => escaped = false,
            b'\\' => escaped = true,
            b'"' => return serde_json::from_str(&literal[..=at]).ok(),
            _ => {}
        }
    }
    // The longest escape is a 12-byte UTF-16 surrogate pair.
    (literal.len().saturating_sub(12)..=literal.len())
        .rev()
        .filter(|&end| literal.is_char_boundary(end))
        .find_map(|end| serde_json::from_str(&format!("{}\"", &literal[..end])).ok())
}

/// One short line of plain text: terminal escapes and control characters removed.
fn preview(text: &str) -> String {
    let mut plain = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' => match chars.next() {
                Some('[') => {
                    chars.find(|c| ('@'..='~').contains(c));
                }
                // An operating system command ends with BEL or with ESC \.
                Some(']') if chars.find(|c| matches!(c, '\u{7}' | '\u{1b}')) == Some('\u{1b}') => {
                    chars.next();
                }
                _ => {}
            },
            c if c.is_whitespace() => plain.push(' '),
            c if c.is_control() => {}
            c => plain.push(c),
        }
    }
    let words = plain.split_whitespace().collect::<Vec<_>>().join(" ");
    match words.char_indices().nth(PREVIEW_CHARS) {
        Some((end, _)) => format!("{}…", words[..end].trim_end()),
        None => words,
    }
}
