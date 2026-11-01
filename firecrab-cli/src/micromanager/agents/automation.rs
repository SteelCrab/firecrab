//! Shared workspace verification locks used by agent terminal control.
use super::*;

impl Runtime {
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
}
