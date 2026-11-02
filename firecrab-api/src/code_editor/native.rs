use super::*;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use tokio::{io::AsyncWriteExt, net::UnixListener};
pub(super) const VERSION: &str = "4.144.0";
pub(super) async fn run(state: AppState, s: Arc<Session>) {
    let result = tokio::select! {
        value=tokio::time::timeout(Duration::from_secs(600),prepare(&state,&s))=>value,
        _=async{loop{tokio::time::sleep(Duration::from_secs(1)).await;if s.expires<=Instant::now()||!current(&state,&s){break;}}}=>Ok(Err(())),
    };
    if !matches!(result, Ok(Ok(()))) || !current(&state, &s) {
        *s.phase.lock().unwrap_or_else(|p| p.into_inner()) = "failed";
        let _ = s.cancel.send(true);
        return;
    }
    let Ok(listener) = UnixListener::bind(&s.socket) else {
        *s.phase.lock().unwrap_or_else(|p| p.into_inner()) = "failed";
        return;
    };
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(&s.socket, std::fs::Permissions::from_mode(0o600));
    *s.phase.lock().unwrap_or_else(|p| p.into_inner()) = "ready";
    let mut cancelled = s.cancel.subscribe();
    let monitor_state = state.clone();
    let monitor_session = s.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(1));
        let mut stop = monitor_session.cancel.subscribe();
        let mut ticks = 0u8;
        loop {
            tokio::select! {_=stop.changed()=>return,_=interval.tick()=>{}};
            if monitor_session.expires <= Instant::now()
                || !current(&monitor_state, &monitor_session)
            {
                break;
            }
            ticks = (ticks + 1) % 5;
            if ticks == 0
                && !matches!(tokio::time::timeout(Duration::from_secs(5),monitor_session.client.get("http://editor.invalid/healthz").send()).await,Ok(Ok(r)) if r.status().is_success())
            {
                break;
            }
        }
        let _ = monitor_session.cancel.send(true);
    });
    loop {
        tokio::select! {
            _=cancelled.changed()=>break,
            incoming=listener.accept()=>{
                let Ok((socket,_))=incoming else{break};
                tokio::spawn(forward(state.clone(),s.clone(),socket));
            }
        }
    }
    let _ = s.cancel.send(true);
    *s.phase.lock().unwrap_or_else(|p| p.into_inner()) = "failed";
}
async fn forward(state: AppState, s: Arc<Session>, mut socket: tokio::net::UnixStream) {
    let mut stop = s.cancel.subscribe();
    let work = async {
        if !current(&state, &s) {
            return;
        }
        let directory = crate::code_runtime::directory(&state, s.record.runtime.workspace_id);
        let Ok(mut command) = guest::code_transport(&state, &s.binding, &directory, None) else {
            return;
        };
        command.stderr(std::process::Stdio::null());
        let Ok(mut child) = command.spawn() else {
            return;
        };
        let (Some(mut input), Some(mut output)) = (child.stdin.take(), child.stdout.take()) else {
            return;
        };
        let (mut read, mut write) = socket.split();
        let send = async {
            tokio::io::copy(&mut read, &mut input).await?;
            input.shutdown().await
        };
        let receive = tokio::io::copy(&mut output, &mut write);
        tokio::select! {_=send=>{},_=receive=>{}}
        let _ = child.kill().await;
    };
    tokio::select! {_=work=>{},_=stop.changed()=>{}}
}

async fn prepare(state: &AppState, s: &Session) -> Result<(), ()> {
    let _permit = state
        .code_jobs
        .clone()
        .acquire_owned()
        .await
        .map_err(|_| ())?;
    let _claim = crate::code_runtime::lock(state, s.record.runtime.workspace_id).map_err(|_| ())?;
    if !current(state, s) {
        return Err(());
    }
    let payload=STANDARD.encode(serde_json::to_vec(&serde_json::json!({"workspace":s.record.runtime.workspace_id,"commit":s.record.runtime.initial_commit})).map_err(|_|())?);
    let script = format!(
        "timeout -k 5 540 python3 - <<'FC_EDITOR'\n{}\nimport base64\nmain(json.loads(base64.b64decode('{}')))\nFC_EDITOR\n",
        include_str!("guest.py"),
        payload
    );
    let dir = crate::code_runtime::directory(state, s.record.runtime.workspace_id);
    let output = guest::code_bootstrap(state, &s.binding, &dir, script.into_bytes())
        .await
        .map_err(|_| ())?;
    if output.code != 0 || output.stdout != b"editor-ready\n" {
        return Err(());
    }
    Ok(())
}
