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
        let output = guest.shell_capture(
            include_str!("github_repositories.sh"),
            Duration::from_secs(30),
            1024 * 1024,
        )?;
        Ok(json!({"repositories":parse_repositories(&output)?}))
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

/// Repositories from `gh` output: a stream of repository objects, or of
/// arrays of them (one per page), in any JSON formatting.
fn parse_repositories(output: &str) -> Result<Vec<Repository>, Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Rows {
        Page(Vec<Repository>),
        One(Repository),
    }
    let mut seen = std::collections::HashSet::new();
    let mut rows = Vec::new();
    for value in serde_json::Deserializer::from_str(output).into_iter() {
        let page = match value? {
            Rows::Page(page) => page,
            Rows::One(row) => vec![row],
        };
        // Offer only repositories this VM can connect; one unsupported name
        // (e.g. `.github`) must not hide the rest of the account. A repository
        // updated while pages load moves to an earlier page and can repeat.
        rows.extend(page.into_iter().filter(|row| {
            matches!(guest::repository_url(&row.name), Ok(Some(_))) && seen.insert(row.name.clone())
        }));
    }
    Ok(rows)
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

    fn names(rows: &[Repository]) -> Vec<&str> {
        rows.iter().map(|row| row.name.as_str()).collect()
    }

    #[test]
    fn repository_lists_parse_from_every_gh_page_format() {
        // The list script: one compact object per line, across pages.
        let lines = "{\"name\":\"owner/a\",\"private\":true,\"archived\":false}\n\
                     {\"name\":\"owner/b\",\"private\":false,\"archived\":true}\n";
        let rows = parse_repositories(lines).unwrap();
        assert_eq!(names(&rows), ["owner/a", "owner/b"]);
        assert!(rows[0].private && rows[1].archived);
        // `--paginate` with an array filter prints one array per page, and
        // older gh versions may indent objects over several lines.
        let pages = "[{\"name\":\"owner/a\",\"private\":false,\"archived\":false}]\n\
                     [{\"name\":\"owner/b\",\"private\":false,\"archived\":false}]\n\
                     {\n  \"name\": \"owner/c\",\n  \"private\": false,\n  \"archived\": false\n}\n";
        assert_eq!(
            names(&parse_repositories(pages).unwrap()),
            ["owner/a", "owner/b", "owner/c"]
        );
        assert!(parse_repositories("").unwrap().is_empty());
    }

    #[test]
    fn repository_lists_keep_one_row_per_connectable_name() {
        // A repository updated while pages load moves to an earlier page and
        // can appear twice; names this VM cannot clone never reach the list.
        let output = r#"[
            {"name":"owner/app","private":false,"archived":false,"token":"x"},
            {"name":"OpenStudyService/.github","private":false,"archived":false},
            {"name":"../host","private":false,"archived":false},
            {"name":"","private":false,"archived":false}
        ]
        {"name":"owner/app","private":false,"archived":false}
        {"name":"owner/api","private":true,"archived":false}"#;
        let rows = parse_repositories(output).unwrap();
        assert_eq!(names(&rows), ["owner/app", "owner/api"]);
        assert!(!serde_json::to_string(&rows).unwrap().contains("token"));
        for output in [
            "not JSON",
            "{\"name\":\"owner/a\"}",
            "[{\"name\":\"owner/a\",\"private\":false,\"archived\":false}",
        ] {
            assert!(parse_repositories(output).is_err(), "{output}");
        }
    }

    /// Runs the list script with stub `gh` and `timeout` commands for an
    /// account with `total` repositories whose page `fail` (if not 0) errors.
    #[cfg(unix)]
    fn list_repositories(total: usize, fail: usize) -> (std::process::Output, Vec<usize>) {
        use std::os::unix::fs::PermissionsExt;
        let bin = tempfile::tempdir().unwrap();
        for (name, body) in [
            ("timeout", "#!/bin/sh\nshift\nexec \"$@\"\n"),
            (
                "gh",
                r#"#!/bin/sh
page=${2#*&page=}
page=${page%%&*}
echo "$page" >> "$GH_CALLS"
if [ "$page" = "$FAIL_PAGE" ]; then echo 'API rate limit exceeded' >&2; exit 1; fi
i=$(( (page - 1) * 100 + 1 ))
while [ "$i" -le "$TOTAL" ] && [ "$i" -le $(( page * 100 )) ]; do
  printf '{"name":"owner/repo-%s","private":false,"archived":false}\n' "$i"
  i=$((i + 1))
done
"#,
            ),
        ] {
            fs::write(bin.path().join(name), body).unwrap();
            fs::set_permissions(bin.path().join(name), fs::Permissions::from_mode(0o700)).unwrap();
        }
        let calls = bin.path().join("calls");
        let output = Command::new("sh")
            .args(["-c", include_str!("github_repositories.sh")])
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    bin.path().display(),
                    std::env::var("PATH").unwrap()
                ),
            )
            .env("GH_CALLS", &calls)
            .env("TOTAL", total.to_string())
            .env("FAIL_PAGE", fail.to_string())
            .output()
            .unwrap();
        let pages = fs::read_to_string(&calls)
            .unwrap_or_default()
            .lines()
            .map(|page| page.parse().unwrap())
            .collect();
        (output, pages)
    }

    #[cfg(unix)]
    #[test]
    fn repository_pages_load_until_the_last_page_or_one_thousand_rows() {
        for (total, expected_pages, expected_rows) in [
            (0, vec![1], 0),
            (42, vec![1], 42),
            (100, vec![1, 2], 100),
            (250, vec![1, 2, 3], 250),
            (1500, (1..=10).collect(), 1000),
        ] {
            let (output, pages) = list_repositories(total, 0);
            assert!(output.status.success(), "{output:?}");
            assert_eq!(pages, expected_pages, "{total} repositories");
            let rows = parse_repositories(&String::from_utf8(output.stdout).unwrap()).unwrap();
            assert_eq!(rows.len(), expected_rows, "{total} repositories");
            if total > 0 {
                assert_eq!(rows[0].name, "owner/repo-1");
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn only_a_failed_first_page_hides_the_repository_list() {
        let (output, pages) = list_repositories(500, 1);
        assert!(!output.status.success());
        assert_eq!(pages, [1]);
        assert!(String::from_utf8_lossy(&output.stderr).contains("rate limit"));
        // A later page (rate limit, slow API) keeps the newest repositories.
        let (output, pages) = list_repositories(500, 3);
        assert!(output.status.success(), "{output:?}");
        assert_eq!(pages, [1, 2, 3]);
        let rows = parse_repositories(&String::from_utf8(output.stdout).unwrap()).unwrap();
        assert_eq!(rows.len(), 200);
    }
}
