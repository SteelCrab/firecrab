//! Browser device authorization through GitHub CLI inside the selected MicroVM.
use super::*;
use base64::{Engine, engine::general_purpose::STANDARD};

#[cfg(all(test, unix))]
mod transport_tests;

impl Runtime {
    pub fn github_status(&self) -> Result<Value, Error> {
        let guest = self
            .guest
            .as_ref()
            .ok_or_else(|| invalid("Select a MicroVM first."))?;
        if guest.authenticated.contains("github") {
            return Ok(json!({"state":"connected"}));
        }
        if !guest.available.contains("gh") {
            return Ok(json!({"state":"unavailable"}));
        }
        let output = guest.shell(include_str!("github_status.sh"), Duration::from_secs(8))?;
        Ok(parse_status(&output))
    }

    pub fn connect_github(&self) -> Result<Value, Error> {
        let status = self.github_status()?;
        if status["state"] == "connected" || status["state"] == "pending" {
            return Ok(status);
        }
        if status["state"] == "unavailable" {
            return Err(invalid("Prepare GitHub tools in this MicroVM first."));
        }
        let guest = self.guest.as_ref().unwrap();
        let script = format!(
            "set -eu\numask 077\nroot=/root/.local/share/firecrab/github-auth\nmkdir -p \"$root\"\nrun={}\nmkdir \"$root/$run\"\nprintf '%s' \"$run\" > \"$root/current.tmp\"\nmv -T \"$root/current.tmp\" \"$root/current\"\ndate +%s > \"$root/$run/created\"\nnohup /bin/sh -c {} -- \"$root/$run\" </dev/null >\"$root/$run/log\" 2>&1 &\n",
            Uuid::new_v4(),
            guest::quote(include_str!("github_login.sh")),
        );
        guest.shell(&script, Duration::from_secs(8))?;
        self.github_status()
    }

    pub fn github_repositories(&self) -> Result<Value, Error> {
        let guest = self
            .guest
            .as_ref()
            .ok_or_else(|| invalid("Select a MicroVM first."))?;
        if !guest.authenticated.contains("github") {
            return Err(invalid("Connect this MicroVM's GitHub account first."));
        }
        // Return only repository identities. Credentials never leave the VM.
        let output = guest.shell(
            "GH_PROMPT_DISABLED=1 timeout 25 gh api 'user/repos?sort=updated&per_page=100&affiliation=owner,collaborator,organization_member' --jq '[.[] | {name: .full_name, private: .private, archived: .archived}]'",
            Duration::from_secs(30),
        )?;
        let rows: Vec<Repository> = serde_json::from_str(&output)?;
        for row in &rows {
            guest::repository_url(&row.name)?;
        }
        Ok(json!({"repositories":rows}))
    }

    pub fn create_github_repository(&self, input: CreateRepository) -> Result<Value, Error> {
        let guest = self
            .guest
            .as_ref()
            .ok_or_else(|| invalid("Select a MicroVM first."))?;
        if !guest.authenticated.contains("github") {
            return Err(invalid("Connect this MicroVM's GitHub account first."));
        }
        if input.id.is_nil()
            || input.name.is_empty()
            || input.name.len() > 100
            || !input
                .name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
            || input.name.to_ascii_lowercase().ends_with(".git")
            || guest::repository_url(&format!("owner/{}", input.name)).is_err()
        {
            return Err(invalid(
                "Use a repository name of up to 100 letters, digits, '.', '_' or '-'; do not start with '.', include '..', or end with '.git'.",
            ));
        }
        if input.description.chars().count() > 350
            || input.description.chars().any(char::is_control)
        {
            return Err(invalid(
                "Repository description must be one line of up to 350 characters.",
            ));
        }
        let project = self.vm_project()?;
        if matches!(project["state"].as_str(), Some("ready" | "preparing")) {
            return Err(invalid(
                "This MicroVM already has a project. Select a new workspace before creating a repository.",
            ));
        }
        let request = STANDARD.encode(serde_json::to_vec(&input)?);
        let payload = STANDARD.encode(serde_json::to_vec(&json!({
            "name":&input.name,"description":&input.description,
            "private":input.private,"auto_init":input.initialize,
        }))?);
        let script = format!(
            "/bin/sh -c {} -- {} {} {}",
            guest::quote(include_str!("github_create.sh")),
            guest::quote(&input.id.to_string()),
            guest::quote(&request),
            guest::quote(&payload)
        );
        let output = guest.shell(&script, Duration::from_secs(65))?;
        let created: Repository = serde_json::from_str(&output)?;
        guest::repository_url(&created.name)?;
        if created.name.split('/').nth(1) != Some(input.name.as_str())
            || created.private != input.private
            || created.archived
        {
            return Err(invalid(
                "The created GitHub repository identity does not match the request. Check GitHub before retrying.",
            ));
        }
        serde_json::to_value(created).map_err(|error| invalid(error.to_string()))
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateRepository {
    pub id: Uuid,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub private: bool,
    pub initialize: bool,
}

#[derive(Serialize, Deserialize)]
struct Repository {
    name: String,
    private: bool,
    archived: bool,
}

fn parse_status(output: &str) -> Value {
    let pending = output.lines().any(|line| line == "PENDING");
    let code = output
        .lines()
        .filter_map(|line| line.strip_prefix("CODE:"))
        .find(|code| {
            code.len() == 9
                && code.as_bytes()[4] == b'-'
                && code
                    .bytes()
                    .enumerate()
                    .all(|(i, c)| i == 4 || c.is_ascii_uppercase() || c.is_ascii_digit())
        });
    if pending {
        return json!({"state":"pending","userCode":code,"verificationUri":"https://github.com/login/device"});
    }
    if output.lines().any(|line| line == "FAILED") {
        return json!({"state":"failed","error":"GitHub authorization ended or expired. Start again to get a new code."});
    }
    json!({"state":"disconnected"})
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_pending_device_codes_are_exposed() {
        let status =
            parse_status("PENDING\nCODE:AB12-CD34\nTOKEN:secret\nURL:https://evil.invalid\n");
        assert_eq!(status["userCode"], "AB12-CD34");
        assert_eq!(status["verificationUri"], "https://github.com/login/device");
        assert!(!status.to_string().contains("secret"));
        for output in [
            "FAILED\nCODE:AB12-CD34",
            "CODE:AB12-CD34",
            "PENDING\nCODE:abcd-1234",
            "PENDING\nCODE:<script>",
        ] {
            assert!(parse_status(output)["userCode"].is_null());
        }
    }
}
