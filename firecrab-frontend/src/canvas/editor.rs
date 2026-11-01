//! Only a confirmed private snapshot permits disposing this browser's editor surface.
use super::*;
use wasm_bindgen_futures::JsFuture;

#[derive(Clone)]
pub(super) struct Client {
    pub id: String,
    pub instance: String,
    pub directory: String,
    pub url: String,
    pub pending: Option<Value>,
    pub window_proof: Option<String>,
}
#[derive(Clone, Copy, PartialEq)]
pub(super) enum Close {
    Tab,
    Editor,
}
fn matching<'a>(a: &'a mut App, identity: &Identity, client: &str) -> Option<&'a mut Tab> {
    a.canvas.tabs.iter_mut().find(|t| {
        t.identity.id == identity.id
            && t.identity.project == identity.project
            && t.editor_client.as_ref().is_some_and(|c| c.id == client)
    })
}
fn path(identity: &Identity, client: &str) -> String {
    format!(
        "{}/editors/{}/clients/{client}",
        base(&identity.project),
        identity.id
    )
}
fn validate(value: Value, client: &Client) -> Result<Value, String> {
    if value["id"] != client.id
        || value["instance"] != client.instance
        || value["revision"].as_u64().is_none()
    {
        return Err("The editor browser identity changed; keep its current document".into());
    }
    Ok(value)
}
async fn state(identity: &Identity, client: &Client) -> Result<Value, String> {
    validate(
        workspace_request("GET", &path(identity, &client.id), None).await?,
        client,
    )
}
async fn command(
    app: &Shared,
    identity: &Identity,
    client: &Client,
    state: &Value,
    action: &str,
) -> Result<Value, String> {
    let body = {
        let mut a = app.borrow_mut();
        let t = matching(&mut a, identity, &client.id)
            .ok_or("The canvas or editor changed; keep its current document")?;
        let c = t
            .editor_client
            .as_mut()
            .ok_or("Editor client is unavailable")?;
        let revision = state["revision"]
            .as_u64()
            .ok_or("Invalid browser close revision")?;
        let reuse = c
            .pending
            .as_ref()
            .filter(|p| {
                p["action"] == action
                    && p["expectedRevision"] == revision
                    && p["windowProof"].as_str() == client.window_proof.as_deref()
            })
            .cloned();
        let body = reuse.unwrap_or_else(
            || json!({"operationId":uuid(),"expectedRevision":revision,"action":action,"windowProof":client.window_proof}),
        );
        c.pending = Some(body.clone());
        body
    };
    let result = validate(
        workspace_request(
            "PUT",
            &format!("{}/control", path(identity, &client.id)),
            Some(&body),
        )
        .await?,
        client,
    )?;
    if let Some(t) = matching(&mut app.borrow_mut(), identity, &client.id) {
        t.editor_client.as_mut().unwrap().pending = None;
    }
    Ok(result)
}
fn surface(action: &str, identity: &Identity, client: &Client, value: bool) -> Result<(), String> {
    let result = pty::binding(
        action,
        JsValue::from_str(&json!({"id":identity.id,"client":client.id,"value":value,"windowProof":client.window_proof}).to_string()),
    );
    if result.as_bool() != Some(true) {
        return Err("The editor surface changed; its browser document was retained".into());
    }
    Ok(())
}
fn window_client(app: &Shared, identity: &Identity, mut client: Client) -> Result<Client, String> {
    let proof = pty::binding(
        "editor-proof",
        JsValue::from_str(&json!({"id":identity.id,"client":client.id}).to_string()),
    )
    .as_string()
    .ok_or(
        "The editor window is still connecting. Its document is retained; retry when it is ready.",
    )?;
    client.window_proof = Some(proof.clone());
    if let Some(t) = matching(&mut app.borrow_mut(), identity, &client.id) {
        t.editor_client.as_mut().unwrap().window_proof = Some(proof);
    }
    Ok(client)
}
/// Persist and confirm a close before removing the iframe. Native sessions stay retained.
pub(super) async fn detach(app: &Shared, identity: &Identity) -> Result<(), String> {
    let client = app
        .borrow()
        .canvas
        .tabs
        .iter()
        .find(|t| t.identity.id == identity.id && t.identity.project == identity.project)
        .and_then(|t| t.editor_client.clone());
    let Some(client) = client else {
        let opened = app
            .borrow()
            .canvas
            .tabs
            .iter()
            .find(|t| t.identity.id == identity.id && t.identity.project == identity.project)
            .is_some_and(|t| t.editor_opened);
        if !opened {
            return Ok(());
        }
        let exists = pty::binding(
            "has-editor",
            JsValue::from_str(&json!({"id":identity.id}).to_string()),
        )
        .as_bool();
        return if exists == Some(false) {
            Ok(())
        } else {
            Err("This earlier editor cannot confirm a safe browser close. Keep it open and preserve its drafts before reopening it.".into())
        };
    };
    surface("freeze-editor", identity, &client, true)?;
    let client = window_client(app, identity, client)?;
    let mut current = state(identity, &client).await?;
    if current["command"] != "close" && current["command"] != "detach"
        || current["status"] == "error"
        || current["windowProof"].as_str() != client.window_proof.as_deref()
    {
        current = command(app, identity, &client, &current, "close").await?;
    }
    let deadline = js_sys::Date::now() + 20000.;
    loop {
        if matching(&mut app.borrow_mut(), identity, &client.id).is_none() {
            return Err("The canvas changed while its editor was closing".into());
        }
        if current["command"] == "detach" && current["status"] == "detached" {
            break;
        }
        if current["command"] != "close" {
            return Err("The browser close command changed; keep the editor and retry".into());
        }
        if current["status"] == "ready" {
            current = command(app, identity, &client, &current, "detach").await?;
            continue;
        }
        if current["status"] == "error" {
            return Err(text(&current, "error").to_string());
        }
        if js_sys::Date::now() > deadline {
            return Err("Private draft saving is not confirmed. The editor remains open. Retry closing or keep editing.".into());
        }
        let promise = js_sys::Promise::new(&mut |resolve, _| {
            let _ = window().set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, 250);
        });
        JsFuture::from(promise)
            .await
            .map_err(|_| "Browser close polling was interrupted")?;
        current = state(identity, &client).await?;
    }
    surface("detach-editor", identity, &client, true)?;
    if let Some(t) = matching(&mut app.borrow_mut(), identity, &client.id) {
        t.editor_client = None;
        t.editor = None;
        t.editor_opened = false;
    }
    Ok(())
}
pub(super) fn start(app: &Shared, id: &str, kind: Close) {
    let identity = {
        let mut a = app.borrow_mut();
        let Some(t) = a.canvas.tabs.iter_mut().find(|t| t.identity.id == id) else {
            return;
        };
        if t.busy {
            return;
        }
        if kind == Close::Tab && (t.changed != t.saved || t.pending.is_some()) {
            t.error = "Save this canvas layout before closing it".into();
            drop(a);
            render(app);
            return;
        }
        t.busy = true;
        t.closing = Some(kind);
        t.error.clear();
        t.identity.clone()
    };
    render(app);
    let app = app.clone();
    spawn_local(async move {
        let result = detach(&app, &identity).await;
        if let Some(t) = app
            .borrow_mut()
            .canvas
            .tabs
            .iter_mut()
            .find(|t| t.identity.id == identity.id && t.identity.project == identity.project)
        {
            t.busy = false;
            match &result {
                Ok(()) => {
                    t.closing = None;
                    t.error.clear();
                }
                Err(error) => t.error = error.clone(),
            }
        }
        if result.is_ok() && kind == Close::Tab {
            remove_tab(&app, &identity);
        } else {
            render(&app);
        }
    });
}
pub(super) fn keep(app: &Shared, id: &str) {
    let (identity, client) = {
        let mut a = app.borrow_mut();
        let Some(t) = a.canvas.tabs.iter_mut().find(|t| t.identity.id == id) else {
            return;
        };
        if t.busy {
            return;
        }
        let Some(client) = t.editor_client.clone() else {
            t.closing = None;
            drop(a);
            render(app);
            return;
        };
        t.busy = true;
        (t.identity.clone(), client)
    };
    render(app);
    let app = app.clone();
    spawn_local(async move {
        let result = async {
            let current_client = window_client(&app, &identity, client.clone())?;
            let mut current = state(&identity, &current_client).await?;
            if current["command"] != "cancel" || current["windowProof"].as_str() != current_client.window_proof.as_deref() {
                current = command(&app, &identity, &current_client, &current, "cancel").await?;
            }
            let deadline = js_sys::Date::now() + 20000.;
            while current["status"] != "active" || current["command"] != "cancel" || current["windowProof"].as_str() != current_client.window_proof.as_deref() {
                if js_sys::Date::now() > deadline { return Err("The editor has not confirmed that editing resumed. Keep its document and retry.".to_string()); }
                let promise = js_sys::Promise::new(&mut |resolve, _| { let _=window().set_timeout_with_callback_and_timeout_and_arguments_0(&resolve,250); });
                JsFuture::from(promise).await.map_err(|_| "Browser resume polling was interrupted")?;
                current = state(&identity, &current_client).await?;
            }
            surface("freeze-editor", &identity, &current_client, false)
        }.await;
        if let Some(t) = matching(&mut app.borrow_mut(), &identity, &client.id) {
            t.busy = false;
            match result {
                Ok(()) => {
                    t.closing = None;
                    t.error.clear();
                }
                Err(error) => t.error = error,
            }
        }
        render(&app);
    });
}
