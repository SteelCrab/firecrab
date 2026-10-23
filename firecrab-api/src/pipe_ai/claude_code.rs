//! The published Claude Code CLI owns login, credential storage and refresh.
//! Firecrab only runs fixed auth commands and retains connection metadata.
use std::{
    collections::HashMap,
    os::unix::fs::{DirBuilderExt, MetadataExt},
    path::PathBuf,
    process::Stdio,
    sync::{Arc, Mutex, Weak},
    time::{Duration, Instant},
};

pub use super::claude_terminal::login_terminal;
use firecrab_api_types::AiCliState;
use tokio::{io::AsyncReadExt, process::Command, sync::watch};
use uuid::Uuid;

pub const ENDPOINT: &str = "claude-code://api-host";
pub const ATTACH_SECONDS: u64 = 60;
pub const LOGIN_SECONDS: u64 = 600;

#[derive(Debug, Clone, Copy)]
pub enum Error {
    Unavailable,
    Failed,
    Busy,
    Expired,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pipe_ai_claude_tickets_expire_cancel_and_bound_sessions() {
        let cli = ClaudeCode::new(std::env::temp_dir().join(Uuid::new_v4().to_string()));
        let id = Uuid::new_v4();
        let ticket = cli.prepare(id).unwrap();
        assert!(matches!(cli.claim(id, Uuid::new_v4()), Err(Error::Expired)));
        let cancelled = cli.claim(id, ticket).unwrap();
        assert!(matches!(cli.claim(id, ticket), Err(Error::Expired)));
        assert!(matches!(cli.prepare(id), Err(Error::Busy)));
        cli.cancel(id);
        assert!(*cancelled.borrow());
        let next = cli.prepare(id).unwrap();
        cli.finish(id, ticket);
        assert!(cli.claim(id, next).is_ok());
        cli.logins.lock().unwrap().get_mut(&id).unwrap().deadline =
            Instant::now() - Duration::from_secs(1);
        assert!(matches!(cli.claim(id, next), Err(Error::Expired)));
        cli.prepare(id).unwrap();
        for _ in 0..7 {
            cli.prepare(Uuid::new_v4()).unwrap();
        }
        assert!(matches!(cli.prepare(Uuid::new_v4()), Err(Error::Busy)));
    }
}

impl Error {
    pub fn details(self) -> (reqwest::StatusCode, &'static str, &'static str) {
        use reqwest::StatusCode as S;
        match self {
            Self::Unavailable => (
                S::SERVICE_UNAVAILABLE,
                "ai_claude_unavailable",
                "Install the official Claude Code CLI on the API host and configure its executable path",
            ),
            Self::Failed => (
                S::BAD_GATEWAY,
                "ai_claude_failed",
                "Claude Code authentication command failed; check the CLI installation or sign in again",
            ),
            Self::Busy => (
                S::CONFLICT,
                "ai_claude_busy",
                "Claude Code authentication is already in progress",
            ),
            Self::Expired => (
                S::CONFLICT,
                "ai_claude_login_expired",
                "Claude Code login attachment expired or was already used; start again",
            ),
        }
    }
}

#[derive(Debug)]
struct Login {
    ticket: Uuid,
    claimed: bool,
    deadline: Instant,
    cancel: watch::Sender<bool>,
}

#[derive(Debug)]
pub struct ClaudeCode {
    root: PathBuf,
    binary: PathBuf,
    operations: Mutex<HashMap<Uuid, Weak<tokio::sync::Mutex<()>>>>,
    logins: Mutex<HashMap<Uuid, Login>>,
}

impl ClaudeCode {
    pub fn new(root: PathBuf) -> Self {
        let root = if root.is_absolute() {
            root
        } else {
            std::env::current_dir()
                .expect("API working directory")
                .join(root)
        };
        Self {
            root,
            binary: std::env::var_os("FIRECRAB_CLAUDE_CODE_BINARY")
                .map(PathBuf::from)
                .unwrap_or_else(|| "claude".into()),
            operations: Mutex::new(HashMap::new()),
            logins: Mutex::new(HashMap::new()),
        }
    }

    #[cfg(test)]
    pub fn at(root: PathBuf, binary: PathBuf) -> Self {
        Self {
            binary,
            ..Self::new(root)
        }
    }

    pub fn operation(&self, id: Uuid) -> Arc<tokio::sync::Mutex<()>> {
        let mut operations = self.operations.lock().unwrap_or_else(|p| p.into_inner());
        operations.retain(|_, lock| lock.strong_count() > 0);
        if let Some(lock) = operations.get(&id).and_then(Weak::upgrade) {
            return lock;
        }
        let lock = Arc::new(tokio::sync::Mutex::new(()));
        operations.insert(id, Arc::downgrade(&lock));
        lock
    }

    /// Operator-owned paths only. No client-supplied executable, argv, cwd or env.
    pub fn command(&self, id: Uuid, action: &str) -> Result<Command, Error> {
        if !matches!(action, "login" | "status" | "logout") {
            return Err(Error::Failed);
        }
        let profile = self.root.join(id.to_string());
        for path in [&self.root, &profile] {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(path)
                .map_err(|_| Error::Failed)?;
            let metadata = std::fs::symlink_metadata(path).map_err(|_| Error::Failed)?;
            if !metadata.is_dir()
                || metadata.uid() != unsafe { libc::geteuid() }
                || metadata.mode() & 0o077 != 0
            {
                return Err(Error::Failed);
            }
        }
        let mut command = Command::new(&self.binary);
        // Do not let a service's shared credential or provider env override this user's login.
        command.env_clear();
        for key in [
            "PATH",
            "HOME",
            "USER",
            "LOGNAME",
            "TMPDIR",
            "LANG",
            "LC_ALL",
            "HTTPS_PROXY",
            "HTTP_PROXY",
            "NO_PROXY",
            "https_proxy",
            "http_proxy",
            "no_proxy",
            "SSL_CERT_FILE",
            "SSL_CERT_DIR",
            "NODE_EXTRA_CA_CERTS",
        ] {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
        command
            .args(["auth", action])
            .current_dir(&profile)
            .env("CLAUDE_CONFIG_DIR", profile)
            .env("TERM", "xterm-256color")
            .kill_on_drop(true);
        Ok(command)
    }

    /// Parse only the public loggedIn boolean. Discard all other output and stderr.
    pub async fn status(&self, id: Uuid) -> Result<AiCliState, Error> {
        let mut child = match self
            .command(id, "status")?
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(child) => child,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(AiCliState::Unavailable);
            }
            Err(_) => return Err(Error::Failed),
        };
        let result = tokio::time::timeout(Duration::from_secs(10), async {
            let mut output = Vec::new();
            child
                .stdout
                .take()
                .ok_or(Error::Failed)?
                .take(8193)
                .read_to_end(&mut output)
                .await
                .map_err(|_| Error::Failed)?;
            if output.len() > 8192 {
                return Err(Error::Failed);
            }
            let exit = child.wait().await.map_err(|_| Error::Failed)?;
            let value: serde_json::Value =
                serde_json::from_slice(&output).map_err(|_| Error::Failed)?;
            match (value["loggedIn"].as_bool(), exit.code()) {
                (Some(true), Some(0)) => Ok(AiCliState::Connected),
                (Some(false), Some(0 | 1)) => Ok(AiCliState::SignedOut),
                _ => Err(Error::Failed),
            }
        })
        .await
        .map_err(|_| Error::Failed)?;
        if result.is_err() {
            let _ = child.kill().await;
        }
        result
    }

    pub async fn logout(&self, id: Uuid) -> Result<(), Error> {
        let mut child = self
            .command(id, "logout")?
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    Error::Unavailable
                } else {
                    Error::Failed
                }
            })?;
        let exit = tokio::time::timeout(Duration::from_secs(10), child.wait())
            .await
            .map_err(|_| Error::Failed)?
            .map_err(|_| Error::Failed)?;
        if !exit.success() || self.status(id).await? != AiCliState::SignedOut {
            return Err(Error::Failed);
        }
        Ok(())
    }

    pub fn prepare(&self, id: Uuid) -> Result<Uuid, Error> {
        let mut logins = self.logins.lock().unwrap_or_else(|p| p.into_inner());
        logins.retain(|_, login| {
            if login.deadline > Instant::now() {
                return true;
            }
            let _ = login.cancel.send(true);
            false
        });
        if logins.get(&id).is_some_and(|login| login.claimed)
            || (logins.len() >= 8 && !logins.contains_key(&id))
        {
            return Err(Error::Busy);
        }
        let ticket = Uuid::new_v4();
        let (cancel, _) = watch::channel(false);
        logins.insert(
            id,
            Login {
                ticket,
                claimed: false,
                deadline: Instant::now() + Duration::from_secs(ATTACH_SECONDS),
                cancel,
            },
        );
        Ok(ticket)
    }

    pub fn claim(&self, id: Uuid, ticket: Uuid) -> Result<watch::Receiver<bool>, Error> {
        let mut logins = self.logins.lock().unwrap_or_else(|p| p.into_inner());
        let login = logins.get_mut(&id).ok_or(Error::Expired)?;
        if login.ticket != ticket || login.claimed || login.deadline <= Instant::now() {
            return Err(Error::Expired);
        }
        login.claimed = true;
        login.deadline = Instant::now() + Duration::from_secs(LOGIN_SECONDS);
        Ok(login.cancel.subscribe())
    }

    pub fn cancel(&self, id: Uuid) {
        let mut logins = self.logins.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(login) = logins.remove(&id) {
            let _ = login.cancel.send(true);
        }
    }

    pub fn finish(&self, id: Uuid, ticket: Uuid) {
        let mut logins = self.logins.lock().unwrap_or_else(|p| p.into_inner());
        if logins.get(&id).is_some_and(|login| login.ticket == ticket) {
            logins.remove(&id);
        }
    }
}
