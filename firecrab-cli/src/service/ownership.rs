//! Retain root-side ownership admission while the ordinary service command runs.

use super::{Command, Error, env::ServiceEnv, privileged::Privileged};
use sha2::{Digest, Sha256};
use std::{
    io::{BufRead, Write},
    process::{Child, ChildStdin, Stdio},
    time::{Duration, Instant},
};
use uuid::Uuid;

const DEADLINE: Duration = Duration::from_secs(10);

#[derive(Clone)]
pub(super) struct Proof {
    pub(super) operation: Uuid,
    pub(super) input_sha256: String,
}

pub(super) struct Guard {
    child: Child,
    input: Option<ChildStdin>,
    operation: Uuid,
    proof: Proof,
}

impl Drop for Guard {
    fn drop(&mut self) {
        // EOF leaves the original journal busy. Dropping is never successful completion.
        self.input.take();
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.try_wait();
        }
    }
}

impl Guard {
    pub(super) fn begin(
        command: &Command,
        env: &ServiceEnv,
        privileged: &Privileged<'_>,
    ) -> Result<Self, Error> {
        let action = match command {
            Command::Install(_) => "install",
            Command::Reinstall { .. } => "reinstall",
            Command::Uninstall { .. } => "uninstall",
            Command::Start => "start",
            Command::Stop => "stop",
            Command::Restart => "restart",
            Command::Enable => "enable",
            Command::Disable => "disable",
            Command::Dev { .. } => {
                return Err(Error::step(
                    "ownership",
                    "isolated development does not use host-service ownership",
                ));
            }
        };
        let input = serde_json::to_vec(&(command, env))
            .map_err(|_| Error::step("ownership", "invalid native service request"))?;
        let hash = format!("{:x}", Sha256::digest(input));
        let operation = Uuid::new_v4();
        let executable =
            std::env::current_exe().map_err(|error| Error::step("ownership", error.to_string()))?;
        let mut process = if privileged.uses_sudo() {
            let mut process = std::process::Command::new("sudo");
            process.args(["-n", "env"]);
            // Preserve selected installation paths without changing the caller's HOME/cache.
            for (name, value) in [("CONFDIR", &env.confdir), ("UNITDIR", &env.unitdir)] {
                let mut assignment = std::ffi::OsString::from(format!("{name}="));
                assignment.push(value);
                process.arg(assignment);
            }
            process.arg(&executable);
            process
        } else {
            std::process::Command::new(&executable)
        };
        process
            .args([
                "iac",
                "native-maintenance",
                "--operation-id",
                &operation.to_string(),
                "--action",
                action,
                "--input-sha256",
                &hash,
            ])
            .env("CONFDIR", &env.confdir)
            .env("UNITDIR", &env.unitdir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        let mut child = process
            .spawn()
            .map_err(|error| Error::step("ownership", error.to_string()))?;
        let input = child
            .stdin
            .take()
            .ok_or_else(|| Error::step("ownership", "native maintenance input is unavailable"))?;
        let output = child
            .stdout
            .take()
            .ok_or_else(|| Error::step("ownership", "native maintenance output is unavailable"))?;
        let guard = Self {
            child,
            input: Some(input),
            operation,
            proof: Proof {
                operation,
                input_sha256: hash,
            },
        };
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        std::thread::spawn(move || {
            use std::io::Read;
            let mut line = Vec::new();
            let result = std::io::BufReader::new(output)
                .take(4097)
                .read_until(b'\n', &mut line)
                .map(|_| line);
            let _ = sender.send(result);
        });
        let line = receiver
            .recv_timeout(DEADLINE)
            .map_err(|_| {
                Error::step(
                    "ownership",
                    format!("native admission is unavailable; original operation {operation}"),
                )
            })?
            .map_err(|_| {
                Error::step(
                    "ownership",
                    format!("native admission was interrupted; original operation {operation}"),
                )
            })?;
        let reply: serde_json::Value = serde_json::from_slice(&line).map_err(|_| {
            Error::step(
                "ownership",
                format!("native admission was rejected; original operation {operation}"),
            )
        })?;
        if reply.get("operationId").and_then(serde_json::Value::as_str)
            != Some(operation.to_string().as_str())
            || reply.get("state").and_then(serde_json::Value::as_str) != Some("ready")
        {
            return Err(Error::step(
                "ownership",
                format!(
                    "native admission did not authorize execution; original operation {operation}"
                ),
            ));
        }
        Ok(guard)
    }

    pub(super) fn proof(&self) -> Proof {
        self.proof.clone()
    }

    pub(super) fn complete(mut self) -> Result<(), Error> {
        let mut input = self
            .input
            .take()
            .ok_or_else(|| Error::step("ownership", "native lease input disappeared"))?;
        input.write_all(b"complete\n").map_err(|_| {
            Error::step(
                "ownership",
                format!(
                    "native completion is unknown; recover operation {}",
                    self.operation
                ),
            )
        })?;
        drop(input);
        let deadline = Instant::now() + DEADLINE;
        loop {
            match self
                .child
                .try_wait()
                .map_err(|error| Error::step("ownership", error.to_string()))?
            {
                Some(status) if status.success() => return Ok(()),
                Some(_) => {
                    return Err(Error::step(
                        "ownership",
                        format!(
                            "native completion failed; recover operation {}",
                            self.operation
                        ),
                    ));
                }
                None if Instant::now() >= deadline => {
                    return Err(Error::step(
                        "ownership",
                        format!(
                            "native completion is unknown; recover operation {}",
                            self.operation
                        ),
                    ));
                }
                None => std::thread::sleep(Duration::from_millis(25)),
            }
        }
    }
}
