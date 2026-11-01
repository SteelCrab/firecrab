//! One persistent repository and working folder per MicroVM.
use super::*;
use base64::{Engine, engine::general_purpose::STANDARD};

pub const WORKSPACE: &str = "/workspace/firecrab/project";
pub const MANIFEST: &str = "/root/.local/share/firecrab/vm-repository.json";

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VmProject {
    pub vm_id: Uuid,
    pub repository: Option<String>,
    pub branch: String,
    pub path: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Setup {
    #[serde(default)]
    pub repository: String,
    #[serde(default)]
    pub branch: String,
}

#[derive(Serialize, Deserialize)]
struct Record {
    #[serde(flatten)]
    project: VmProject,
    state: String,
    pid: u32,
    error: Option<String>,
}

impl VmProject {
    fn check(&self, vm: Uuid) -> Result<(), Error> {
        if self.vm_id != vm
            || self.path != WORKSPACE
            || self.repository.as_deref().is_some_and(
                |url| !matches!(guest::repository_url(url), Ok(Some(valid)) if valid == url),
            )
            || self.branch.len() > 128
            || self.branch.starts_with('-')
            || self.branch.chars().any(char::is_control)
        {
            return Err(invalid(
                "The MicroVM repository identity could not be verified.",
            ));
        }
        Ok(())
    }
    pub fn accepts(&self, repository: &str, branch: &str) -> Result<(), Error> {
        let requested = guest::repository_url(repository)?;
        if requested.as_ref().is_some_and(|url| {
            !self
                .repository
                .as_ref()
                .is_some_and(|saved| saved.eq_ignore_ascii_case(url))
        }) || (!branch.is_empty() && branch != self.branch)
        {
            return Err(invalid(
                "This MicroVM already has a project. Select another MicroVM for a different repository or initial branch.",
            ));
        }
        Ok(())
    }
}

impl Runtime {
    fn project_record(&self) -> PathBuf {
        self.home.join("vm-repository.json")
    }

    pub fn vm_project(&self) -> Result<Value, Error> {
        let Some(guest) = &self.guest else {
            return Ok(Value::Null);
        };
        let saved: Option<Record> = match fs::read(self.project_record()) {
            Ok(bytes) => Some(serde_json::from_slice(&bytes)?),
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(e) => return Err(e.into()),
        };
        if let Some(mut record) = saved {
            record.project.check(guest.id)?;
            if record.state == "ready"
                || (record.state == "preparing" && record.pid == std::process::id())
            {
                return serde_json::to_value(record).map_err(|e| invalid(e.to_string()));
            }
            // A completed guest clone survives browser and native-service restarts.
            if let Some(project) = self.guest_project()? {
                return self.save_project(project, "ready", None);
            }
            if record.state == "preparing" {
                record.state = "interrupted".into();
                record.error = Some(
                    "Repository setup was interrupted. Check the VM folder before retrying.".into(),
                );
            }
            return serde_json::to_value(record).map_err(|e| invalid(e.to_string()));
        }
        match self.guest_project()? {
            Some(project) => self.save_project(project, "ready", None),
            None => Ok(Value::Null),
        }
    }
    fn guest_project(&self) -> Result<Option<VmProject>, Error> {
        let guest = self
            .guest
            .as_ref()
            .ok_or_else(|| invalid("Select a MicroVM first."))?;
        let value = guest.shell(
            &format!("if [ -f {MANIFEST} ]; then head -c 8192 {MANIFEST} | base64 -w0; fi"),
            Duration::from_secs(10),
        )?;
        if value.trim().is_empty() {
            return Ok(None);
        }
        let bytes = STANDARD
            .decode(value.trim())
            .map_err(|_| invalid("Invalid MicroVM repository metadata."))?;
        let project: VmProject = serde_json::from_slice(&bytes)?;
        project.check(guest.id)?;
        Ok(Some(project))
    }
    fn save_project(
        &self,
        project: VmProject,
        state: &str,
        error: Option<String>,
    ) -> Result<Value, Error> {
        let record = Record {
            project,
            state: state.into(),
            pid: std::process::id(),
            error,
        };
        resource::write(&self.project_record(), &record)?;
        serde_json::to_value(record).map_err(|e| invalid(e.to_string()))
    }
    pub fn setup_project(&self, input: Setup) -> Result<Value, Error> {
        let guest = self
            .guest
            .as_ref()
            .ok_or_else(|| invalid("Select a MicroVM first."))?;
        let project = VmProject {
            vm_id: guest.id,
            repository: guest::repository_url(&input.repository)?,
            branch: input.branch,
            path: WORKSPACE.into(),
        };
        project.check(guest.id)?;
        let current = self.vm_project()?;
        if current["state"] == "ready" {
            let saved: Record = serde_json::from_value(current.clone())?;
            saved.project.accepts(&input.repository, &project.branch)?;
            return Ok(current);
        }
        if current["state"] == "preparing" {
            return Ok(current);
        }
        self.save_project(project.clone(), "preparing", None)?;
        if let Err(error) = guest.workspace(&project) {
            self.save_project(project, "failed", Some(error.to_string()))?;
            return Err(error);
        }
        self.save_project(project, "ready", None)
    }
    pub(super) fn task_project(
        &self,
        input: &TaskStart,
    ) -> Result<(String, Option<String>, String), Error> {
        // Keep existing running sessions and their retry identity in their original folders.
        if let Some(agent) = self
            .records()?
            .into_iter()
            .find(|a| a.id == input.id && a.cwd != WORKSPACE)
        {
            return Ok((
                agent.cwd,
                guest::repository_url(&input.repository)?,
                input.branch.clone(),
            ));
        }
        let status = self.vm_project()?;
        let value = if status.is_null() {
            self.setup_project(Setup {
                repository: input.repository.clone(),
                branch: input.branch.clone(),
            })?
        } else if status["state"] == "ready" {
            status
        } else {
            return Err(invalid(
                "Finish this MicroVM's repository setup before starting a task.",
            ));
        };
        let saved: Record = serde_json::from_value(value)?;
        saved.project.accepts(&input.repository, &input.branch)?;
        self.guest.as_ref().unwrap().shell(
            &format!("test -d {}", guest::quote(&saved.project.path)),
            Duration::from_secs(10),
        )?;
        Ok((
            saved.project.path,
            saved.project.repository,
            saved.project.branch,
        ))
    }
}

#[cfg(test)]
mod qa {
    use super::*;
    #[test]
    #[ignore = "Requires the explicitly owned qa-github-auth VM; never authenticates an account or launches a model"]
    fn real_vm_reuses_repository_without_resetting_files() {
        let marker: Value =
            serde_json::from_slice(&fs::read("/private/tmp/firecrab-github-auth-qa.json").unwrap())
                .unwrap();
        assert_eq!(marker["createdBy"], "github-auth-QA");
        assert_eq!(marker["deleted"], false);
        let id: Uuid = marker["id"].as_str().unwrap().parse().unwrap();
        let client = reqwest::blocking::Client::new();
        let vm: Value = client
            .get(format!("http://127.0.0.1:31100/api/vms/{id}"))
            .send()
            .unwrap()
            .json()
            .unwrap();
        assert_eq!(vm["name"], marker["name"]);
        assert!(vm["name"].as_str().unwrap().starts_with("qa-github-auth-"));
        let home = PathBuf::from(
            std::env::var("FIRECRAB_GITHUB_QA_HOME").expect("Private QA managed home"),
        );
        let runtime = Runtime::for_vm(&home, id, true).unwrap();
        let configured = runtime
            .setup_project(Setup {
                repository: "octocat/Hello-World".into(),
                branch: String::new(),
            })
            .unwrap();
        assert_eq!(configured["path"], WORKSPACE);
        let guest = runtime.guest.as_ref().unwrap();
        guest
            .shell(
                "printf QA_KEEP_EDIT > /workspace/firecrab/project/.firecrab-qa-edit",
                Duration::from_secs(5),
            )
            .unwrap();
        runtime
            .setup_project(Setup {
                repository: "octocat/Hello-World".into(),
                branch: String::new(),
            })
            .unwrap();
        assert_eq!(
            guest
                .shell(
                    "cat /workspace/firecrab/project/.firecrab-qa-edit",
                    Duration::from_secs(5)
                )
                .unwrap(),
            "QA_KEEP_EDIT"
        );
        assert!(
            runtime
                .setup_project(Setup {
                    repository: "octocat/other".into(),
                    branch: String::new()
                })
                .is_err()
        );
        fs::remove_file(runtime.project_record()).unwrap();
        assert_eq!(runtime.vm_project().unwrap()["state"], "ready");
        println!(
            "QA VM: repository cloned once, local edit preserved, guest binding recovered; no account authorization or model execution"
        );
    }
}
