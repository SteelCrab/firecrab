//! "View locally": the VM's test app on this computer's loopback, through one pinned SSH
//! forward per VM workspace app. Nothing is published to the internet.
use super::*;
use std::collections::{BTreeMap, BTreeSet};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::process::Child;
use std::sync::{Mutex, PoisonError};

/// Keyed by VM and workspace, as the app is. Each ssh lives while this process holds its
/// stdin, so the forwards end with the resource server however it exits.
static VIEWS: Mutex<BTreeMap<(Uuid, String), View>> = Mutex::new(BTreeMap::new());

struct View {
    ssh: Child,
    app_port: u16,
    host_port: u16,
    check: Value,
}
impl View {
    fn alive(&mut self) -> bool {
        matches!(self.ssh.try_wait(), Ok(None))
    }
}
impl Drop for View {
    fn drop(&mut self) {
        let _ = self.ssh.kill();
        let _ = self.ssh.wait();
    }
}

fn views() -> std::sync::MutexGuard<'static, BTreeMap<(Uuid, String), View>> {
    VIEWS.lock().unwrap_or_else(PoisonError::into_inner)
}
pub(super) fn close_local_view(key: &(Uuid, String)) {
    views().remove(key);
}

impl Runtime {
    pub(super) fn local_key(&self, id: Uuid) -> Result<(Uuid, String), Error> {
        let guest = self
            .guest
            .as_ref()
            .ok_or_else(|| invalid("Select a MicroVM first."))?;
        Ok((guest.id, self.workspace_key(id)?))
    }
    /// The local view as the status shows it. A forward whose ssh ended (the VM stopped or
    /// restarted), whose app stopped or whose app now uses another port is closed here.
    pub(super) fn local_view(&self, id: Uuid, run: &Value) -> Result<Value, Error> {
        let key = self.local_key(id)?;
        let mut views = views();
        let Some(view) = views.get_mut(&key) else {
            return Ok(Value::Null);
        };
        if !view.alive()
            || run["appReady"] != true
            || run["appRunning"] != true
            || run["config"]["port"].as_u64() != Some(view.app_port.into())
        {
            views.remove(&key);
            return Ok(Value::Null);
        }
        Ok(json!({
            "url": format!("http://127.0.0.1:{}/", view.host_port),
            "hostPort": view.host_port,
            "appPort": view.app_port,
            "check": view.check,
        }))
    }
    pub(super) fn start_local_view(&self, id: Uuid) -> Result<Value, Error> {
        let (_, guest) = self.automation_workspace(id)?;
        let mut state = self.automation_status(id)?;
        let run = &state["run"];
        if run["state"] != "passed" || run["appReady"] != true || run["appRunning"] != true {
            return Err(invalid(
                "Pass verification and start the test app before viewing it locally.",
            ));
        }
        let app_port = run["config"]["port"]
            .as_u64()
            .and_then(|port| u16::try_from(port).ok())
            .filter(|port| *port != 0)
            .ok_or_else(|| invalid("The test app port is unavailable."))?;
        let key = self.local_key(id)?;
        let log = self
            .home
            .join("automation")
            .join(format!("{}.local.log", key.1));
        let host_port = {
            let mut views = views();
            let reusable = views
                .get_mut(&key)
                .filter(|view| view.app_port == app_port)
                .is_some_and(View::alive);
            if !reusable {
                views.remove(&key);
                let view = start(guest, app_port, &log)?;
                views.insert(key.clone(), view);
            }
            views[&key].host_port
        };
        // Outside the lock: a slow app must not hold up other workspaces' status.
        let check = check(host_port);
        if let Some(view) = views().get_mut(&key)
            && view.host_port == host_port
        {
            view.check = check;
        }
        state["local"] = self.local_view(id, &state["run"])?;
        Ok(state)
    }
}

fn start(guest: &guest::Guest, app_port: u16, log: &Path) -> Result<View, Error> {
    let host_port = host_port(app_port, &trusted_ports(guest))?;
    fs::create_dir_all(log.parent().unwrap())?;
    let output = fs::File::create(log)?;
    let ssh = guest
        .held_forward(host_port, app_port)
        .stdout(Stdio::null())
        .stderr(Stdio::from(output))
        .spawn()?;
    let mut view = View {
        ssh,
        app_port,
        host_port,
        check: Value::Null,
    };
    for _ in 0..100 {
        if !view.alive() {
            let reason = fs::read_to_string(log).unwrap_or_default();
            let reason = reason.lines().rfind(|l| !l.trim().is_empty()).unwrap_or("");
            return Err(invalid(format!(
                "The local SSH connection to the app could not be opened. {reason}"
            )));
        }
        if super::super::editor::listener_owned(view.ssh.id(), host_port) {
            return Ok(view);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err(invalid("The local app port did not become ready."))
}

/// Ports whose `http://127.0.0.1:<port>` origin the resource server trusts. The VM's app
/// must never be served there, even while that dashboard is stopped.
fn trusted_ports(guest: &guest::Guest) -> BTreeSet<u16> {
    use crate::micromanager::dev::{dashboard, instance};
    let mut ports = BTreeSet::from([5523, 5524, 8080]);
    if let Some(home) = guest.controller_home() {
        if let Ok(Some(state)) = instance::load(home) {
            ports.extend([state.api_port, state.resource_port]);
        }
        if let Ok(port) = dashboard::port(home) {
            ports.insert(port);
        }
    }
    ports
}

/// The app's own port when nothing on this computer uses it, else a free loopback port.
/// The connect probe also finds a listener on another address that a bind would not.
fn host_port(app_port: u16, trusted: &BTreeSet<u16>) -> Result<u16, Error> {
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, app_port));
    if !trusted.contains(&app_port)
        && TcpStream::connect_timeout(&address, Duration::from_millis(300)).is_err()
        && TcpListener::bind(address).is_ok()
    {
        return Ok(app_port);
    }
    for _ in 0..8 {
        let port = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?
            .local_addr()?
            .port();
        if !trusted.contains(&port) {
            return Ok(port);
        }
    }
    Err(invalid("No free local port is available for the app."))
}

/// Any HTTP status means the app answers through the forward.
fn check(host_port: u16) -> Value {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(5))
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .build();
    let started = Instant::now();
    match client.and_then(|c| c.get(format!("http://127.0.0.1:{host_port}/")).send()) {
        Ok(response) => json!({
            "reachable": true,
            "status": response.status().as_u16(),
            "latencyMs": started.elapsed().as_millis(),
        }),
        Err(_) => json!({
            "reachable": false,
            "error": "The app did not answer through the local connection. Check the app log, then retry.",
        }),
    }
}
