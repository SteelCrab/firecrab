//! Agent skills in a MicroVM, managed by the Vercel Skills CLI (`npx skills`).
//! Skills are installed globally for every coding CLI Firecrab runs, so a CLI installed later
//! also finds them. Node.js is installed only when a change needs the CLI.
use super::*;
use crate::micromanager::skills::registry::{valid_skill, valid_source};

/// Pinned CLI release; `npx -y` fetches exactly this version.
pub const CLI: &str = "skills@1.7.6";
/// The CLI's agent names for every coding CLI in `DRIVERS`.
const AGENTS: [&str; 7] = [
    "claude-code",
    "codex",
    "gemini-cli",
    "opencode",
    "github-copilot",
    "cursor",
    "grok",
];
const NODE_MISSING: &str = "FIRECRAB_NODE_MISSING";

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "lowercase", deny_unknown_fields)]
pub enum Change {
    Install { source: String, skill: String },
    Remove { skill: String },
    Update,
}

/// The CLI needs Node.js 22.20 or later.
const NODE_OK: &str = r#"node_ok() {
  command -v node >/dev/null 2>&1 && command -v npx >/dev/null 2>&1 || return 1
  version=$(node -p 'process.versions.node' 2>/dev/null) || return 1
  major=${version%%.*}
  rest=${version#*.}
  minor=${rest%%.*}
  [ "$major" -gt 22 ] || { [ "$major" -eq 22 ] && [ "$minor" -ge 20 ]; }
}
"#;

fn script(body: &str, args: &[&str]) -> String {
    let args = args
        .iter()
        .map(|a| guest::quote(a))
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "set -eu\nexport HOME=/root npm_config_update_notifier=false npm_config_fund=false npm_config_audit=false npm_config_loglevel=error\ncd /root\n{NODE_OK}{body}npx -y {CLI} {args}\n"
    )
}

/// The JSON value the CLI printed, after any lines npx wrote before it.
fn json_output(output: &str) -> Result<Value, Error> {
    let start = output
        .find(['[', '{'])
        .ok_or_else(|| invalid("The skills CLI returned no result."))?;
    serde_json::from_str(output[start..].trim())
        .map_err(|_| invalid("The skills CLI returned an invalid result."))
}

impl Runtime {
    fn skills_guest(&self) -> Result<&guest::Guest, Error> {
        self.guest
            .as_ref()
            .ok_or_else(|| invalid("Select a MicroVM first."))
    }

    /// Global skills as the CLI reports them. A VM without Node.js has none, and gets no Node.js.
    pub fn installed_skills(&self) -> Result<Value, Error> {
        let output = self.skills_guest()?.shell_capture(
            &script(
                &format!("node_ok || {{ printf '{NODE_MISSING}\\n'; exit 0; }}\n"),
                &["ls", "-g", "--json"],
            ),
            Duration::from_secs(90),
            1024 * 1024,
        )?;
        if output.contains(NODE_MISSING) {
            return Ok(json!({"node": "missing", "skills": []}));
        }
        let skills: Vec<Value> = json_output(&output)?
            .as_array()
            .into_iter()
            .flatten()
            .map(|s| {
                json!({
                    "name": s["name"],
                    "source": s["source"],
                    "agents": s["agents"],
                    "status": s["status"],
                })
            })
            .collect();
        Ok(json!({"node": "ready", "skills": skills}))
    }

    /// Install, remove or update through the CLI, then report the new global list.
    pub fn change_skills(&self, change: Change) -> Result<Value, Error> {
        let guest = self.skills_guest()?;
        let args: Vec<&str> = match &change {
            Change::Install { source, skill } => {
                if !valid_source(source) || !valid_skill(skill) {
                    return Err(invalid(
                        "Choose a skill from skills.sh search results to install.",
                    ));
                }
                let mut args = vec![
                    "add",
                    source.as_str(),
                    "--skill",
                    skill.as_str(),
                    "-g",
                    "-a",
                ];
                args.extend(AGENTS);
                args.extend(["-y", "--json"]);
                args
            }
            Change::Remove { skill } => {
                if !valid_skill(skill) {
                    return Err(invalid("Choose an installed skill to remove."));
                }
                vec!["remove", "-g", "-y", skill.as_str()]
            }
            Change::Update => vec!["update", "-g", "-y"],
        };
        let prepare = format!(
            "{}if ! node_ok; then install_node >&2; fi\nnode_ok || {{ echo 'Node.js 22.20 or later could not be installed in this MicroVM.' >&2; exit 1; }}\n",
            include_str!("skills_node.sh")
        );
        let output = guest.shell_capture(
            &script(&prepare, &args),
            Duration::from_secs(150),
            1024 * 1024,
        )?;
        let result = match change {
            Change::Install { .. } => {
                let result = json_output(&output)?;
                if let Some(failed) = result
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find(|r| r["status"] != "installed" && r["status"] != "skipped")
                {
                    return Err(invalid(format!(
                        "The skill was not installed: {}",
                        failed["error"]
                            .as_str()
                            .or(failed["reason"].as_str())
                            .unwrap_or("unknown error")
                    )));
                }
                result
            }
            _ => Value::Null,
        };
        Ok(json!({"result": result, "installed": self.installed_skills()?}))
    }
}

#[cfg(all(test, unix))]
mod tests;
