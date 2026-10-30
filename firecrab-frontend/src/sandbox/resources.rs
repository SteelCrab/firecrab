//! Environment and preview controls use the shared REST lifecycle.
use super::view::{banner, btn, disabled, field, selected};
use super::*;
use std::fmt::Write;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    Environment,
    Preview,
}
pub(super) struct Editor {
    kind: Kind,
    id: String,
    project: String,
    spec: Value,
    choices: Vec<Value>,
    notes: BTreeMap<String, String>,
    loading: bool,
    pub(super) busy: bool,
    uncertain: bool,
    error: String,
}

pub(super) fn tick(app: &Shared, force: bool) {
    let mut a = app.borrow_mut();
    if a.route != "ai-sandbox"
        || a.sandbox.project.is_empty()
        || a.sandbox.resources_polling
        || (!force && js_sys::Date::now() - a.sandbox.resources_at < 10_000.0)
    {
        return;
    }
    a.sandbox.resources_polling = true;
    let (project, context, generation) =
        (a.sandbox.project.clone(), a.sandbox.context, a.generation);
    drop(a);
    let app = app.clone();
    spawn_local(async move {
        let base = format!("/api/ai-sandbox/projects/{}", api::encode(&project));
        let environments = api::request("GET", &format!("{base}/environments"), None).await;
        let previews = api::request("GET", &format!("{base}/tunnels"), None).await;
        if !live(&app, generation) {
            return;
        }
        let mut a = app.borrow_mut();
        if a.sandbox.context != context {
            return;
        }
        let before = (
            a.sandbox.environments.clone(),
            a.sandbox.previews.clone(),
            a.sandbox.resource_error.clone(),
        );
        a.sandbox.resources_polling = false;
        a.sandbox.resources_at = js_sys::Date::now();
        a.sandbox.resource_error.clear();
        match environments {
            Ok(v) => a.sandbox.environments = list(&v).to_vec(),
            Err(e) => a.sandbox.resource_error = e,
        }
        match previews {
            Ok(v) => a.sandbox.previews = list(&v).to_vec(),
            Err(e) => a.sandbox.resource_error = e,
        }
        let changed = before
            != (
                a.sandbox.environments.clone(),
                a.sandbox.previews.clone(),
                a.sandbox.resource_error.clone(),
            );
        drop(a);
        if changed {
            render(&app);
        }
    });
}

fn value(node: &Element) -> Option<String> {
    node.dyn_ref::<HtmlInputElement>()
        .map(|n| n.value())
        .or_else(|| node.dyn_ref::<HtmlSelectElement>().map(|n| n.value()))
        .or_else(|| node.dyn_ref::<HtmlTextAreaElement>().map(|n| n.value()))
}
pub(super) fn input(app: &Shared, node: &Element) -> bool {
    let id = node.id();
    if !id.starts_with("sb-resource-") {
        return false;
    }
    let Some(value) = value(node) else {
        return false;
    };
    let mut a = app.borrow_mut();
    let replacement = a
        .sandbox
        .previews
        .iter()
        .find(|p| p["id"] == value)
        .cloned();
    let Some(e) = &mut a.sandbox.resource_editor else {
        return false;
    };
    if e.busy {
        return true;
    }
    if let Some(index) = node
        .get_attribute("data-role-index")
        .and_then(|v| v.parse::<usize>().ok())
    {
        let key = node.get_attribute("data-role-field").unwrap_or_default();
        if key == "image" {
            if let Some(pin) = e.choices.iter().find(|p| text(p, "name") == value) {
                e.spec["roles"][index]["template"] = pin.clone();
            }
        } else if key == "mode" {
            e.spec["roles"][index]["executionMode"] = if value == "persistent_workspace" {
                json!({"kind":"persistent_workspace"})
            } else {
                json!({"kind":"agent_sandbox","runtime":value,"image":""})
            };
        } else if key == "containerImage" {
            e.spec["roles"][index]["executionMode"]["image"] = json!(value);
        } else if matches!(key.as_str(), "cpu" | "memoryMib" | "diskGb") {
            e.spec["roles"][index][&key] = json!(value.parse::<u64>().unwrap_or(0));
        } else if matches!(key.as_str(), "services" | "dependsOn") {
            e.notes.insert(format!("{index}-{key}"), value);
        } else if key == "name" {
            e.spec["roles"][index]["name"] = json!(value);
        }
    } else {
        match id.as_str() {
            "sb-resource-subnet" => e.spec["subnetCidr"] = json!(value),
            "sb-resource-allow" | "sb-resource-ttl" => {
                e.notes.insert(id, value);
            }
            "sb-resource-address" => e.spec["subdomain"] = json!(value),
            "sb-resource-replaces" => {
                if let Some(previous) = replacement {
                    e.notes.insert("replaces".into(), value);
                    e.spec["subdomain"] = previous["spec"]["subdomain"].clone();
                } else {
                    e.notes.remove("replaces");
                    e.spec["subdomain"] = e
                        .choices
                        .iter()
                        .find(|c| c["claimedBy"].is_null())
                        .map(|c| c["subdomain"].clone())
                        .unwrap_or(json!(""));
                }
            }
            "sb-resource-target" => {
                let runtime = e
                    .notes
                    .get("runtime")
                    .and_then(|v| serde_json::from_str::<Value>(v).ok())
                    .unwrap_or(Value::Null);
                e.spec["target"] = if value == "environment_service" {
                    let environment = e
                        .notes
                        .get("environment")
                        .and_then(|v| serde_json::from_str::<Value>(v).ok())
                        .unwrap_or(Value::Null);
                    let services = e
                        .notes
                        .get("services")
                        .and_then(|v| serde_json::from_str::<Value>(v).ok())
                        .unwrap_or(json!([]));
                    json!({"kind":"environment_service","environment":environment,"service":list(&services).first().map(|s|s["name"].clone()).unwrap_or(json!(""))})
                } else if value == "nginx" {
                    json!({"kind":"nginx","host":""})
                } else {
                    json!({"kind":"vm_app","runtime":runtime,"port":8080})
                };
            }
            "sb-resource-port" => {
                e.spec["target"]["port"] = json!(value.parse::<u16>().unwrap_or(0))
            }
            "sb-resource-host" => e.spec["target"]["host"] = json!(value),
            "sb-resource-service" => e.spec["target"]["service"] = json!(value),
            "sb-resource-check-path" | "sb-resource-check-digest" => {
                e.notes.insert(id, value);
            }
            "sb-resource-web-socket" => {
                e.notes.insert(
                    id,
                    node.dyn_ref::<HtmlInputElement>()
                        .is_some_and(|n| n.checked())
                        .to_string(),
                );
            }
            _ => return false,
        }
    }
    dirty(&mut a);
    drop(a);
    if node.dyn_ref::<HtmlSelectElement>().is_some() {
        render_after_event(app);
    }
    true
}
fn role(pin: Value, index: usize) -> Value {
    json!({"name":if index == 0 {"workspace".into()}else{format!("role-{index}")},"template":pin,"cpu":1,"memoryMib":512,"diskGb":4,"dependsOn":[],"services":[],"executionMode":{"kind":"persistent_workspace"}})
}
fn open(app: &Shared, kind: Kind, reference: &str) {
    let mut a = app.borrow_mut();
    if a.sandbox.busy || a.sandbox.resource_editor.is_some() {
        return;
    }
    let Ok(id) = uuid() else {
        return;
    };
    let mut notes = BTreeMap::new();
    let (id, spec) = if kind == Kind::Environment {
        (
            id,
            json!({"version":1,"subnetCidr":"172.29.80.0/24","roles":[],"allow":[],"retention":{"ttlSeconds":null}}),
        )
    } else if let Some(saved) = a.sandbox.previews.iter().find(|p| p["id"] == reference) {
        notes.insert("existing".into(), "true".into());
        notes.insert("runtime".into(), saved["runtime"].to_string());
        (reference.to_owned(), saved["spec"].clone())
    } else {
        let task = a
            .sandbox
            .review
            .as_ref()
            .and_then(|r| r.task.as_ref())
            .filter(|t| t["id"] == reference)
            .or_else(|| a.sandbox.tasks.iter().find(|t| t["id"] == reference));
        let Some(task) = task else {
            return;
        };
        if !task["runtime"].is_object() {
            return;
        }
        notes.insert("runtime".into(), task["runtime"].to_string());
        (
            id,
            json!({"taskId":reference,"subdomain":"","target":{"kind":"vm_app","runtime":task["runtime"],"port":8080}}),
        )
    };
    let task_id = text(&spec, "taskId").to_owned();
    let (generation, project) = (a.generation, a.sandbox.project.clone());
    a.sandbox.opener = document()
        .active_element()
        .map(|n| n.id())
        .unwrap_or_default();
    a.sandbox.review = None;
    a.sandbox.resource_editor = Some(Editor {
        kind,
        id: id.clone(),
        project: project.clone(),
        spec,
        choices: vec![],
        notes,
        loading: true,
        busy: false,
        uncertain: false,
        error: String::new(),
    });
    dirty(&mut a);
    drop(a);
    render(app);
    let app = app.clone();
    spawn_local(async move {
        let path = if kind == Kind::Environment {
            format!(
                "/api/ai-sandbox/projects/{}/environment-images",
                api::encode(&project)
            )
        } else {
            "/api/ai-sandbox/tunnel-addresses".into()
        };
        let result = api::request("GET", &path, None).await;
        let task = if kind == Kind::Preview {
            api::request(
                "GET",
                &format!("/api/ai-sandbox/tasks/{}", api::encode(&task_id)),
                None,
            )
            .await
            .ok()
        } else {
            None
        };
        if !live(&app, generation) {
            return;
        }
        let mut a = app.borrow_mut();
        let services = task
            .as_ref()
            .and_then(|t| {
                a.sandbox
                    .environments
                    .iter()
                    .find(|env| env["id"] == t["environment"]["id"])
            })
            .and_then(|env| {
                list(&env["spec"]["roles"]).iter().find(|r| {
                    task.as_ref()
                        .is_some_and(|t| r["name"] == t["environment"]["role"])
                })
            })
            .map(|r| r["services"].clone());
        let Some(e) = &mut a.sandbox.resource_editor else {
            return;
        };
        if e.id != id || e.project != project {
            return;
        }
        e.loading = false;
        if let Some(task) = task
            && task["environment"].is_object()
        {
            e.notes
                .insert("environment".into(), task["environment"].to_string());
            let services = services.unwrap_or(json!([]));
            e.notes.insert("services".into(), services.to_string());
            if !e.notes.contains_key("existing") {
                e.spec["target"] = json!({"kind":"environment_service","environment":task["environment"],"service":list(&services).first().map(|s|s["name"].clone()).unwrap_or(json!(""))});
            }
        }
        match result {
            Ok(v) => {
                e.choices = list(&v).to_vec();
                if kind == Kind::Environment {
                    if let Some(pin) = e.choices.first() {
                        e.spec["roles"] = json!([role(pin.clone(), 0)]);
                    }
                } else if text(&e.spec, "subdomain").is_empty() {
                    e.spec["subdomain"] = e
                        .choices
                        .iter()
                        .find(|c| c["claimedBy"].is_null())
                        .map(|c| c["subdomain"].clone())
                        .unwrap_or(json!(""));
                }
            }
            Err(error) => e.error = error,
        }
        drop(a);
        render(&app);
    });
}

fn environment_spec(a: &App, e: &Editor) -> Result<Value, String> {
    let mut spec = e.spec.clone();
    for index in 0..list(&spec["roles"]).len() {
        let depends = e
            .notes
            .get(&format!("{index}-dependsOn"))
            .map(String::as_str)
            .unwrap_or("");
        spec["roles"][index]["dependsOn"] = json!(depends.split_whitespace().collect::<Vec<_>>());
        let mut services = Vec::new();
        for line in e
            .notes
            .get(&format!("{index}-services"))
            .map(String::as_str)
            .unwrap_or("")
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
        {
            let (name, port) = line.split_once(':').ok_or(tr(
                a,
                "Services use name:port, one per line",
                "서비스는 이름:포트 형식으로 한 줄에 하나씩 입력한다",
            ))?;
            let port_error = tr(
                a,
                "Service ports must be between 1 and 65535",
                "서비스 포트는 1부터 65535 사이여야 한다",
            );
            let port: u16 = port.parse().map_err(|_| port_error)?;
            if port == 0 {
                return Err(port_error.into());
            }
            services.push(json!({"name":name.trim(),"port":port,"readiness":true}));
        }
        spec["roles"][index]["services"] = json!(services);
    }
    let mut allow = Vec::new();
    for line in e
        .notes
        .get("sb-resource-allow")
        .map(String::as_str)
        .unwrap_or("")
        .lines()
        .filter(|l| !l.trim().is_empty())
    {
        let words = line.split_whitespace().collect::<Vec<_>>();
        if words.len() != 3 {
            return Err(tr(
                a,
                "Allowed connections use: source destination port",
                "허용할 연결은 출발역할 도착역할 포트 형식으로 입력한다",
            )
            .into());
        }
        let port_error = tr(
            a,
            "Allowed connection ports must be between 1 and 65535",
            "허용할 연결의 포트는 1부터 65535 사이여야 한다",
        );
        let port: u16 = words[2].parse().map_err(|_| port_error)?;
        if port == 0 {
            return Err(port_error.into());
        }
        allow.push(json!({"from":words[0],"to":words[1],"port":port}));
    }
    spec["allow"] = json!(allow);
    let ttl = e
        .notes
        .get("sb-resource-ttl")
        .map(String::as_str)
        .unwrap_or("")
        .trim();
    spec["retention"]["ttlSeconds"] = if ttl.is_empty() {
        Value::Null
    } else {
        json!(ttl.parse::<u64>().map_err(|_| tr(
            a,
            "Lifetime must be a positive number of seconds",
            "보존 시간은 양의 정수(초)로 입력한다",
        ))?)
    };
    Ok(spec)
}
pub(super) fn save(app: &Shared) {
    let mut a = app.borrow_mut();
    let generation = a.generation;
    let Some(e) = &a.sandbox.resource_editor else {
        return;
    };
    if e.busy || e.loading || e.uncertain {
        return;
    }
    let spec = if e.kind == Kind::Environment {
        environment_spec(&a, e)
    } else {
        Ok(e.spec.clone())
    };
    let Some(e) = &mut a.sandbox.resource_editor else {
        return;
    };
    let spec = match spec {
        Ok(v) => v,
        Err(error) => {
            e.error = error;
            drop(a);
            render(app);
            return;
        }
    };
    e.busy = true;
    e.error.clear();
    let (kind, id, project) = (e.kind, e.id.clone(), e.project.clone());
    let replaces = e.notes.get("replaces").cloned();
    drop(a);
    render(app);
    let app = app.clone();
    spawn_local(async move {
        let group = if kind == Kind::Environment {
            "environments"
        } else {
            "tunnels"
        };
        let path = format!(
            "/api/ai-sandbox/projects/{}/{group}/{}",
            api::encode(&project),
            api::encode(&id)
        );
        let (method, path, body) = if let Some(previous) = replaces {
            (
                "POST",
                format!(
                    "/api/ai-sandbox/tunnels/{}/retarget",
                    api::encode(&previous)
                ),
                json!({"id":id,"taskId":spec["taskId"],"target":spec["target"]}),
            )
        } else {
            ("PUT", path, spec)
        };
        let result = api::request_detailed(method, &path, Some(&body), None).await;
        if !live(&app, generation) {
            return;
        }
        let mut a = app.borrow_mut();
        if a.sandbox
            .resource_editor
            .as_ref()
            .is_none_or(|e| e.id != id || e.project != project)
        {
            return;
        }
        match result {
            Ok(_) => {
                a.sandbox.resource_editor = None;
                dirty(&mut a);
            }
            Err(error) => {
                let e = a.sandbox.resource_editor.as_mut().unwrap();
                e.busy = false;
                e.uncertain = error.status.is_none() || error.status.is_some_and(|s| s >= 500);
                e.error = error.to_string();
            }
        }
        drop(a);
        render(&app);
        tick(&app, true);
    });
}

fn mutate(app: &Shared, kind: Kind, id: &str, operation: &str) {
    let mut a = app.borrow_mut();
    if a.sandbox.busy {
        return;
    }
    let project = a.sandbox.project.clone();
    let mut body = (kind == Kind::Preview && operation == "verify-shutdown")
        .then(|| json!({"mode":"shutdown","path":"/"}));
    if operation == "delete" {
        let workspace = kind == Kind::Environment
            && a.sandbox
                .environments
                .iter()
                .find(|e| e["id"] == id)
                .is_some_and(|e| {
                    list(&e["spec"]["roles"])
                        .iter()
                        .any(|r| r["executionMode"]["kind"] == "persistent_workspace")
                });
        let message = if workspace {
            tr(
                &a,
                "Permanently delete this environment's retained Workspace disks after export?",
                "결과를 내보낸 뒤 이 환경의 보존된 Workspace 디스크를 영구 삭제할까요?",
            )
        } else {
            tr(
                &a,
                "Delete this owned resource? Remote Tunnel retirement must be completed in the connection dashboard.",
                "이 소유 리소스를 삭제할까요? 원격 Tunnel 폐기는 연결 대시보드에서 완료해야 한다.",
            )
        };
        if !window().confirm_with_message(message).unwrap_or(false) {
            return;
        }
        if kind == Kind::Environment {
            body = Some(json!({"authorizeWorkspaceDeletion":workspace}));
        }
    }
    let (generation, context) = (a.generation, a.sandbox.context);
    a.sandbox.busy = true;
    drop(a);
    render(app);
    let (app, id, operation) = (app.clone(), id.to_owned(), operation.to_owned());
    spawn_local(async move {
        let base = if kind == Kind::Environment {
            format!(
                "/api/ai-sandbox/projects/{}/environments/{}",
                api::encode(&project),
                api::encode(&id)
            )
        } else {
            format!("/api/ai-sandbox/tunnels/{}", api::encode(&id))
        };
        let path = if operation == "delete" {
            base
        } else if operation == "verify-shutdown" {
            format!("{base}/verify")
        } else {
            format!("{base}/{operation}")
        };
        let result = api::request(
            if operation == "delete" {
                "DELETE"
            } else {
                "POST"
            },
            &path,
            body.as_ref(),
        )
        .await;
        if !live(&app, generation) {
            return;
        }
        let mut a = app.borrow_mut();
        if a.sandbox.context != context {
            return;
        }
        a.sandbox.busy = false;
        match result {
            Ok(_) => a.sandbox.resource_error.clear(),
            Err(error) => a.sandbox.resource_error = error,
        }
        drop(a);
        render(&app);
        tick(&app, true);
    });
}
fn inspect_pending(app: &Shared) {
    let mut a = app.borrow_mut();
    let generation = a.generation;
    let Some(e) = &mut a.sandbox.resource_editor else {
        return;
    };
    if e.busy {
        return;
    }
    let (id, project, kind) = (e.id.clone(), e.project.clone(), e.kind);
    e.busy = true;
    drop(a);
    render(app);
    let app = app.clone();
    spawn_local(async move {
        let path = if kind == Kind::Environment {
            format!(
                "/api/ai-sandbox/projects/{}/environments/{}",
                api::encode(&project),
                api::encode(&id)
            )
        } else {
            format!("/api/ai-sandbox/tunnels/{}", api::encode(&id))
        };
        let result = api::request_detailed("GET", &path, None, None).await;
        if !live(&app, generation) {
            return;
        }
        let mut a = app.borrow_mut();
        let Some(e) = &mut a.sandbox.resource_editor else {
            return;
        };
        if e.id != id {
            return;
        }
        match result {
            Ok(_) => {
                a.sandbox.resource_editor = None;
                dirty(&mut a);
            }
            Err(error) => {
                e.busy = false;
                e.error = error.to_string();
                if error.status == Some(404) {
                    e.uncertain = false;
                }
            }
        }
        drop(a);
        render(&app);
        tick(&app, true);
    });
}
pub(super) fn action(app: &Shared, name: &str, id: &str) -> bool {
    match name {
        "sb-env-new" => open(app, Kind::Environment, ""),
        "sb-preview-new" | "sb-preview-edit" => open(app, Kind::Preview, id),
        "sb-resource-inspect" => inspect_pending(app),
        "sb-resource-verify" => verify_current(app),
        "sb-env-add-role" => {
            let mut a = app.borrow_mut();
            if let Some(e) = &mut a.sandbox.resource_editor
                && !e.busy
                && let Some(pin) = e.choices.first()
            {
                let mut roles = list(&e.spec["roles"]).to_vec();
                roles.push(role(pin.clone(), roles.len()));
                e.spec["roles"] = json!(roles);
            }
            drop(a);
            render(app);
        }
        "sb-env-remove-role" => {
            let mut a = app.borrow_mut();
            if let Some(e) = &mut a.sandbox.resource_editor
                && !e.busy
                && let Ok(index) = id.parse::<usize>()
            {
                let mut roles = list(&e.spec["roles"]).to_vec();
                if index < roles.len() {
                    roles.remove(index);
                    e.spec["roles"] = json!(roles);
                    let previous = std::mem::take(&mut e.notes);
                    for (key, value) in previous {
                        if let Some((number, field)) = key.split_once('-')
                            && let Ok(number) = number.parse::<usize>()
                        {
                            if number == index {
                                continue;
                            }
                            e.notes.insert(
                                format!(
                                    "{}-{field}",
                                    if number > index { number - 1 } else { number }
                                ),
                                value,
                            );
                        } else {
                            e.notes.insert(key, value);
                        }
                    }
                }
            }
            drop(a);
            render(app);
        }
        _ if name.starts_with("sb-env-") => mutate(
            app,
            Kind::Environment,
            id,
            name.trim_start_matches("sb-env-"),
        ),
        _ if name.starts_with("sb-preview-") => mutate(
            app,
            Kind::Preview,
            id,
            name.trim_start_matches("sb-preview-"),
        ),
        _ => return false,
    }
    true
}
fn verify_current(app: &Shared) {
    let mut a = app.borrow_mut();
    let generation = a.generation;
    let Some(e) = &mut a.sandbox.resource_editor else {
        return;
    };
    if e.busy || !e.notes.contains_key("existing") {
        return;
    }
    let id = e.id.clone();
    let path = e
        .notes
        .get("sb-resource-check-path")
        .cloned()
        .unwrap_or("/health".into());
    let digest = e
        .notes
        .get("sb-resource-check-digest")
        .filter(|s| !s.trim().is_empty())
        .cloned();
    let request = json!({"path":path,"bodySha256":digest,"webSocket":e.notes.get("sb-resource-web-socket").is_some_and(|v|v=="true")});
    e.busy = true;
    e.error.clear();
    drop(a);
    render(app);
    let app = app.clone();
    spawn_local(async move {
        let result = api::request(
            "POST",
            &format!("/api/ai-sandbox/tunnels/{}/verify", api::encode(&id)),
            Some(&request),
        )
        .await;
        if !live(&app, generation) {
            return;
        }
        let mut a = app.borrow_mut();
        let check_failed = tr(
            &a,
            "The public check did not succeed. Inspect connector health and the expected response.",
            "공개 검증이 성공하지 않았다. 연결 상태와 예상 응답을 확인한다.",
        )
        .to_owned();
        let Some(e) = &mut a.sandbox.resource_editor else {
            return;
        };
        if e.id != id {
            return;
        }
        e.busy = false;
        match result {
            Ok(binding) => {
                if binding["errorCode"].is_string() {
                    e.error = check_failed;
                }
                a.sandbox.previews.retain(|p| p["id"] != id);
                a.sandbox.previews.push(binding);
            }
            Err(error) => e.error = error,
        }
        drop(a);
        render(&app);
    });
}

pub(super) fn status<'a>(a: &App, state: &'a str) -> &'a str {
    match state {
        "ready" => tr(a, "Ready", "준비됨"),
        "pending" => tr(a, "Pending", "확인 대기"),
        "unsupported" => tr(a, "Unsupported", "지원하지 않음"),
        "provisioning" => tr(a, "Preparing", "준비 중"),
        "failed" => tr(a, "Failed", "실패"),
        "stopped" => tr(a, "Stopped", "중지됨"),
        "cleanup_pending" => tr(a, "Cleanup pending", "정리 대기"),
        "deleted" => tr(a, "Deleted", "삭제됨"),
        "superseded" => tr(a, "Replaced", "후속 연결로 교체됨"),
        "waiting" => tr(a, "Waiting", "대기"),
        "connecting" => tr(a, "Connecting", "연결 중"),
        "connected" => tr(a, "Connected", "연결됨"),
        "stopping" => tr(a, "Stopping", "중지 중"),
        "deleting" => tr(a, "Deleting", "삭제 중"),
        "needs_reconciliation" => tr(a, "Needs inspection", "상태 확인 필요"),
        "unverified" => tr(a, "Unverified", "미검증"),
        "verified" => tr(a, "Verified", "검증됨"),
        _ => state,
    }
}
fn short(value: &str) -> String {
    value.chars().take(8).collect()
}
pub(super) fn content(a: &App, out: &mut String) {
    let s = &a.sandbox;
    let _ = write!(
        out,
        "<details id=\"sb-resources\" class=\"sb-resources\"><summary>{}</summary>{}<div class=\"sb-resource-toolbar\">{}</div>",
        tr(a, "Environments & previews", "환경·미리보기"),
        banner(&s.resource_error),
        btn(
            tr(a, "New environment", "새 환경"),
            "sb-env-new",
            "",
            "plus",
            "btn quiet",
            s.busy
        )
    );
    if s.environments.is_empty() {
        let _ = write!(
            out,
            "<p class=\"sb-hint\">{}</p>",
            tr(
                a,
                "Create an isolated environment, or use a borrowed MicroVM from project settings.",
                "격리된 환경을 만들거나 프로젝트 설정에서 기존 MicroVM을 선택한다."
            )
        );
    }
    for e in &s.environments {
        let id = text(e, "id");
        let deleted = e["state"] == "deleted";
        let _ = write!(
            out,
            "<article class=\"sb-resource-card\"><header><strong>{} {}</strong><span>{}</span></header>",
            tr(a, "Environment", "환경"),
            escape(&short(id)),
            escape(status(a, text(e, "state")))
        );
        for role in list(&e["roles"]) {
            let _ = write!(
                out,
                "<p><strong>{}</strong> · {}: {} · {}: {} · {}: {}</p>",
                escape(text(role, "role")),
                tr(a, "Agent", "에이전트"),
                escape(status(a, text(role, "agent"))),
                tr(a, "Network", "네트워크"),
                escape(status(a, text(role, "network"))),
                tr(a, "App", "앱"),
                escape(status(a, text(role, "application")))
            );
            for svc in list(&role["services"]) {
                let _ = write!(
                    out,
                    "<p class=\"sb-hint\">{} · {}:{} · {}</p>",
                    escape(text(svc, "name")),
                    escape(text(svc, "address")),
                    svc["port"],
                    if svc["ready"] == true {
                        tr(a, "Ready", "준비됨")
                    } else {
                        tr(a, "Waiting", "대기")
                    }
                );
            }
        }
        if !text(e, "error").is_empty() {
            out.push_str(&banner(text(e, "error")));
        }
        let _ = write!(
            out,
            "<p class=\"sb-hint\">{}: {} · {}: {}</p><div class=\"sb-row sb-resource-actions\">",
            tr(a, "Result export", "결과 내보내기"),
            if e["exportedAt"].is_number() {
                tr(a, "Verified", "검증됨")
            } else {
                tr(a, "Required before cleanup", "정리 전에 필요")
            },
            tr(a, "Lifetime", "보존 기간"),
            if e["expiresAt"].is_number() {
                tr(a, "Temporary", "임시")
            } else {
                tr(a, "Retained", "보존")
            }
        );
        for (action, en, ko, glyph) in [
            ("reconcile", "Inspect & reconcile", "상태 확인", "refresh"),
            ("stop", "Stop", "중지", "square"),
            ("resume", "Resume", "재개", "send"),
            ("export", "Export results", "결과 내보내기", "download"),
            ("delete", "Delete", "삭제", "trash"),
        ] {
            out.push_str(&btn(
                tr(a, en, ko),
                &format!("sb-env-{action}"),
                id,
                glyph,
                "btn quiet",
                s.busy || deleted,
            ));
        }
        out.push_str("</div></article>");
    }
    for preview in &s.previews {
        preview_card(a, preview, out);
    }
    out.push_str("</details>");
}
fn preview_card(a: &App, p: &Value, out: &mut String) {
    let id = text(p, "id");
    let deleted = matches!(text(p, "state"), "deleted" | "superseded");
    let _ = write!(
        out,
        "<article class=\"sb-resource-card\"><header><strong>{}</strong><span>{}</span></header>",
        escape(text(&p["spec"], "subdomain")),
        escape(status(a, text(p, "state")))
    );
    if p["helperConnected"] == true
        && p["appReady"] == true
        && let Some(url) = p["publicUrl"]
            .as_str()
            .filter(|u| **u == format!("https://{}.firecrab.dev", text(&p["spec"], "subdomain")))
    {
        let _ = write!(
            out,
            "<a href=\"{}\" target=\"_blank\" rel=\"noopener noreferrer\">{}{}</a>",
            escape(url),
            icon("arrow"),
            escape(url)
        );
    }
    let _ = write!(
        out,
        "<p class=\"sb-hint\">{}: {} · HTTPS: {} · WebSocket: {} · {}: {}</p>",
        tr(a, "Connector", "연결기"),
        if p["helperConnected"] == true {
            tr(a, "Connected", "연결됨")
        } else {
            tr(a, "Disconnected", "연결되지 않음")
        },
        escape(status(a, text(p, "publicHttps"))),
        escape(status(a, text(p, "webSocketHandshake"))),
        tr(a, "Public shutdown", "공개 종료"),
        escape(status(a, text(p, "publicShutdown")))
    );
    if p["remoteRetirement"] == "operator_required" {
        let _ = write!(
            out,
            "<p class=\"sb-hint\">{} <a href=\"https://console.firecrab.dev/\" target=\"_blank\" rel=\"noopener noreferrer\">{}</a></p>",
            tr(
                a,
                "Retire the token in the connection dashboard.",
                "연결 대시보드에서 토큰을 폐기한다."
            ),
            tr(a, "Connection dashboard", "연결 대시보드")
        );
    }
    if let Some(code) = p["errorCode"].as_str() {
        let _ = write!(out, "<p class=\"sb-hint\">{}</p>", escape(code));
    }
    out.push_str("<div class=\"sb-row sb-resource-actions\">");
    for (action, en, ko, glyph) in [
        ("edit", "Edit target", "대상 편집", "settings"),
        ("publish", "Publish / reconnect", "공개·재연결", "send"),
        ("reconcile", "Inspect", "상태 확인", "refresh"),
        ("stop", "Stop", "중지", "square"),
        (
            "delete",
            "Delete local connection",
            "로컬 연결 삭제",
            "trash",
        ),
    ] {
        out.push_str(&btn(
            tr(a, en, ko),
            &format!("sb-preview-{action}"),
            id,
            glyph,
            "btn quiet",
            a.sandbox.busy || deleted,
        ));
    }
    if matches!(text(p, "state"), "stopped" | "deleted") {
        out.push_str(&btn(
            tr(a, "Verify public shutdown", "공개 종료 검증"),
            "sb-preview-verify-shutdown",
            id,
            "shield",
            "btn quiet",
            a.sandbox.busy,
        ));
    }
    out.push_str("</div></article>");
}

pub(super) fn coordination(a: &App, out: &mut String) {
    let s = &a.sandbox;
    let c = &s.coordination;
    let _ = write!(
        out,
        "<details id=\"sb-task-options\" class=\"sb-task-options\"><summary>{}</summary><div class=\"sb-resource-grid\">{}{}",
        tr(a, "Task options", "작업 옵션"),
        field(
            tr(a, "Assigned agent", "담당 에이전트"),
            "sb-assigned-agent",
            text(c, "assignedAgent"),
            "maxlength=\"120\"",
            s.busy
        ),
        field(
            tr(a, "Retry limit", "재시도 한도"),
            "sb-retry-limit",
            &c["retryLimit"].as_u64().unwrap_or(3).to_string(),
            "type=\"number\" min=\"0\" max=\"3\"",
            s.busy
        )
    );
    if let Some(p) = project(s)
        && let Some(e) = s
            .environments
            .iter()
            .find(|e| e["id"] == p["spec"]["environmentId"])
    {
        let _ = write!(
            out,
            "<label for=\"sb-task-role\">{}<select id=\"sb-task-role\"><option value=\"\">{}</option>",
            tr(a, "Environment role", "환경 역할"),
            tr(a, "Choose role", "역할 선택")
        );
        for r in list(&e["roles"]) {
            let role = text(r, "role");
            let _ = write!(
                out,
                "<option value=\"{}\"{}>{}</option>",
                escape(role),
                selected(s.environment_role == role),
                escape(role)
            );
        }
        out.push_str("</select></label>");
    }
    out.push_str("</div>");
    let candidates = s
        .tasks
        .iter()
        .filter(|t| matches!(text(t, "state"), "result" | "merged"))
        .collect::<Vec<_>>();
    if !candidates.is_empty() {
        let _ = write!(
            out,
            "<fieldset class=\"sb-dependencies\"><legend>{}</legend>",
            tr(a, "Run after these results", "이 결과가 완료된 뒤 실행")
        );
        for t in candidates {
            let id = text(t, "id");
            let _ = write!(
                out,
                "<label class=\"sb-check\"><input type=\"checkbox\" id=\"sb-dependency-{}\" data-sb-dependency=\"{}\"{}{}>{}</label>",
                escape(id),
                escape(id),
                if list(&c["dependencies"]).iter().any(|v| *v == id) {
                    " checked"
                } else {
                    ""
                },
                disabled(s.busy),
                escape(&format!("{} · {}", short(id), text(&t["request"], "input")))
            );
        }
        out.push_str("</fieldset>");
    }
    out.push_str("</details>");
}
pub(super) fn result(a: &App, task: &Value, out: &mut String) {
    let tests = list(&task["tests"]);
    let artifacts = list(&task["artifacts"]);
    if !tests.is_empty() || !artifacts.is_empty() {
        let _ = write!(
            out,
            "<section class=\"sb-retained-results\"><h3>{}</h3>",
            tr(a, "Retained results", "보존된 결과")
        );
        for t in tests {
            let _ = write!(
                out,
                "<p>{} {} · {} {}</p>",
                icon(if t["passed"] == true {
                    "circle-check"
                } else {
                    "circle-x"
                }),
                escape(text(t, "name")),
                tr(a, "Event", "기록"),
                t["eventSequence"]
            );
        }
        for artifact in artifacts {
            let path = format!(
                "/api/ai-sandbox/tasks/{}/artifacts/{}",
                api::encode(text(task, "id")),
                api::encode(text(artifact, "id"))
            );
            let _ = write!(
                out,
                "<a href=\"{}\" download>{}{} · {} bytes</a>",
                escape(&path),
                icon("download"),
                escape(text(artifact, "path")),
                artifact["size"]
            );
        }
        out.push_str("</section>");
    }
    for p in a
        .sandbox
        .previews
        .iter()
        .filter(|p| p["taskId"] == task["id"])
    {
        preview_card(a, p, out);
    }
    if task["runtime"].is_object() {
        out.push_str(&btn(
            tr(a, "Create app preview", "앱 미리보기 만들기"),
            "sb-preview-new",
            text(task, "id"),
            "arrow",
            "btn quiet",
            a.sandbox.busy || active(task),
        ));
    }
}

pub(super) fn editor(a: &App, e: &Editor, out: &mut String) {
    let environment = e.kind == Kind::Environment;
    let _ = write!(
        out,
        "<dialog id=\"sb-resource-dialog\" class=\"sb-dialog sb-resource-dialog\" aria-labelledby=\"sb-resource-title\"><header class=\"sb-dialog-header\"><h2 id=\"sb-resource-title\">{}{}</h2>{}</header><form id=\"sb-resource-form\" class=\"sb-project-form\">{}",
        icon(if environment { "terminal" } else { "arrow" }),
        if environment {
            tr(a, "New environment", "새 환경")
        } else {
            tr(a, "App preview", "앱 미리보기")
        },
        btn(
            tr(a, "Close", "닫기"),
            "sb-close",
            "resource",
            "close",
            "icon-btn",
            e.busy
        ),
        banner(&e.error)
    );
    if e.loading {
        let _ = write!(
            out,
            "<p role=\"status\">{}</p>",
            tr(
                a,
                "Loading saved choices…",
                "저장된 선택 항목을 불러오는 중…"
            )
        );
    }
    if environment {
        environment_editor(a, e, out);
    } else {
        preview_editor(a, e, out);
    }
    let _ = write!(
        out,
        "<footer class=\"sb-dialog-footer\">{}{}<button type=\"submit\" class=\"btn primary\"{}>{}</button></footer></form></dialog>",
        btn(
            tr(a, "Cancel", "취소"),
            "sb-close",
            "resource-cancel",
            "close",
            "btn",
            e.busy
        ),
        if e.uncertain {
            btn(
                tr(a, "Inspect saved request", "저장된 요청 확인"),
                "sb-resource-inspect",
                "",
                "refresh",
                "btn",
                e.busy,
            )
        } else {
            String::new()
        },
        disabled(
            e.busy
                || e.loading
                || e.uncertain
                || e.choices.is_empty()
                || (e.kind == Kind::Preview && text(&e.spec, "subdomain").is_empty())
        ),
        if e.busy {
            tr(a, "Saving…", "저장 중…")
        } else if environment {
            tr(a, "Create environment", "환경 생성")
        } else {
            tr(a, "Publish preview", "미리보기 공개")
        }
    );
}
fn textarea(a: &App, e: &Editor, id: &str, en: &str, ko: &str, extra: &str, out: &mut String) {
    let _ = write!(
        out,
        "<label for=\"{id}\">{}<textarea id=\"{id}\" rows=\"3\" {extra}{}>{}</textarea></label>",
        tr(a, en, ko),
        disabled(e.busy),
        escape(e.notes.get(id).map(String::as_str).unwrap_or(""))
    );
}
fn role_field(e: &Editor, index: usize, key: &str, label: &str, extra: &str, out: &mut String) {
    let id = format!("sb-resource-role-{index}-{key}");
    let value = &e.spec["roles"][index][key];
    let value = value
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| value.to_string());
    out.push_str(&field(
        label,
        &id,
        &value,
        &format!("data-role-index=\"{index}\" data-role-field=\"{key}\" {extra}"),
        e.busy,
    ));
}
fn environment_editor(a: &App, e: &Editor, out: &mut String) {
    out.push_str(&field(
        tr(a, "Private subnet", "전용 서브넷"),
        "sb-resource-subnet",
        text(&e.spec, "subnetCidr"),
        "required placeholder=\"172.29.80.0/24\"",
        e.busy,
    ));
    for (index, r) in list(&e.spec["roles"]).iter().enumerate() {
        let _ = write!(
            out,
            "<fieldset class=\"sb-fieldset\"><legend>{} {}</legend><div class=\"sb-resource-grid\">",
            tr(a, "Role", "역할"),
            index + 1
        );
        role_field(
            e,
            index,
            "name",
            tr(a, "Role name", "역할 이름"),
            "required maxlength=\"32\"",
            out,
        );
        let _ = write!(
            out,
            "<label for=\"sb-resource-image-{index}\">{}<select id=\"sb-resource-image-{index}\" data-role-index=\"{index}\" data-role-field=\"image\"{}>",
            tr(a, "Pinned image", "고정 이미지"),
            disabled(e.busy)
        );
        for p in &e.choices {
            let name = text(p, "name");
            let _ = write!(
                out,
                "<option value=\"{}\"{}>{} · {}</option>",
                escape(name),
                selected(r["template"]["name"] == name),
                escape(name),
                escape(text(p, "version"))
            );
        }
        out.push_str("</select></label>");
        for (key, en, ko, extra) in [
            (
                "cpu",
                "vCPUs",
                "vCPU 수",
                "type=\"number\" min=\"1\" max=\"32\" required",
            ),
            (
                "memoryMib",
                "Memory (MiB)",
                "메모리 (MiB)",
                "type=\"number\" min=\"128\" required",
            ),
            (
                "diskGb",
                "Disk (GiB)",
                "디스크 (GiB)",
                "type=\"number\" min=\"1\" required",
            ),
        ] {
            role_field(e, index, key, tr(a, en, ko), extra, out);
        }
        let mode = &r["executionMode"];
        let _ = write!(
            out,
            "<label for=\"sb-resource-mode-{index}\">{}<select id=\"sb-resource-mode-{index}\" data-role-index=\"{index}\" data-role-field=\"mode\"{}>",
            tr(a, "Execution & retention", "실행·보존"),
            disabled(e.busy)
        );
        for (id, en, ko) in [
            (
                "persistent_workspace",
                "Persistent Workspace",
                "보존형 Workspace",
            ),
            (
                "crun",
                "Agent Sandbox · containerd + crun (recommended)",
                "에이전트 Sandbox · containerd + crun (권장)",
            ),
            (
                "docker",
                "Agent Sandbox · Docker",
                "에이전트 Sandbox · Docker",
            ),
            (
                "nspawn",
                "Agent Sandbox · nspawn",
                "에이전트 Sandbox · nspawn",
            ),
        ] {
            let chosen = if id == "persistent_workspace" {
                mode["kind"] == id
            } else {
                mode["runtime"] == id
            };
            let _ = write!(
                out,
                "<option value=\"{id}\"{}>{}</option>",
                selected(chosen),
                tr(a, en, ko)
            );
        }
        out.push_str("</select></label></div>");
        if mode["kind"] == "agent_sandbox" {
            out.push_str(&field(tr(a,"Container image digest","컨테이너 이미지 해시"),&format!("sb-resource-container-{index}"),text(mode,"image"),&format!("data-role-index=\"{index}\" data-role-field=\"containerImage\" required placeholder=\"repo@sha256:…\""),e.busy));
            if mode["runtime"] == "crun" {
                let _ = write!(
                    out,
                    "<p class=\"sb-hint\">{}</p>",
                    tr(
                        a,
                        "Install containerd, nerdctl and crun in the MicroVM. Prepare the image in the firecrab-sandbox namespace.",
                        "MicroVM에 containerd, nerdctl, crun을 설치한다. firecrab-sandbox 네임스페이스에 이미지를 준비한다."
                    )
                );
            }
        }
        for (key, en, ko, placeholder) in [
            (
                "services",
                "Services (name:port, one per line)",
                "서비스 (이름:포트, 한 줄에 하나)",
                "api:8080",
            ),
            (
                "dependsOn",
                "Start after roles (space separated)",
                "먼저 준비할 역할 (공백으로 구분)",
                "db backend",
            ),
        ] {
            let id = format!("sb-resource-role-{index}-{key}");
            let _ = write!(
                out,
                "<label for=\"{id}\">{}<textarea id=\"{id}\" rows=\"2\" data-role-index=\"{index}\" data-role-field=\"{key}\" placeholder=\"{placeholder}\"{}>{}</textarea></label>",
                tr(a, en, ko),
                disabled(e.busy),
                escape(
                    e.notes
                        .get(&format!("{index}-{key}"))
                        .map(String::as_str)
                        .unwrap_or("")
                )
            );
        }
        out.push_str(&btn(
            tr(a, "Remove role", "역할 제거"),
            "sb-env-remove-role",
            &index.to_string(),
            "trash",
            "btn quiet",
            e.busy || list(&e.spec["roles"]).len() == 1,
        ));
        out.push_str("</fieldset>");
    }
    out.push_str(&btn(
        tr(a, "Add role", "역할 추가"),
        "sb-env-add-role",
        "",
        "plus",
        "btn quiet",
        e.busy || e.choices.is_empty(),
    ));
    textarea(
        a,
        e,
        "sb-resource-allow",
        "Allowed TCP connections (source destination port)",
        "허용할 TCP 연결 (출발역할 도착역할 포트)",
        "placeholder=\"debug backend 8080&#10;backend db 5432\"",
        out,
    );
    out.push_str(&field(
        tr(
            a,
            "Temporary lifetime (seconds, optional)",
            "임시 보존 시간 (초, 선택)",
        ),
        "sb-resource-ttl",
        e.notes
            .get("sb-resource-ttl")
            .map(String::as_str)
            .unwrap_or(""),
        "type=\"number\" min=\"1\"",
        e.busy,
    ));
    let _ = write!(
        out,
        "<p class=\"sb-hint\">{}</p>",
        tr(
            a,
            "All other connections and Internet egress are blocked. Persistent Workspaces retain disks until explicit deletion. Temporary cleanup waits for result export.",
            "그 밖의 연결과 인터넷 통신을 차단한다. 보존형 Workspace는 명시적으로 삭제할 때까지 디스크를 유지한다. 임시 리소스는 결과를 내보낸 뒤 정리한다."
        )
    );
}
fn preview_editor(a: &App, e: &Editor, out: &mut String) {
    if !e.notes.contains_key("existing") {
        let _ = write!(
            out,
            "<label for=\"sb-resource-replaces\">{}<select id=\"sb-resource-replaces\"{}><option value=\"\">{}</option>",
            tr(
                a,
                "Reconnect an existing fixed URL",
                "기존 고정 URL 다시 연결"
            ),
            disabled(e.busy),
            tr(
                a,
                "Use an unclaimed saved address",
                "사용 중이 아닌 저장 주소 사용"
            )
        );
        for previous in a.sandbox.previews.iter().filter(|p| {
            !matches!(text(p, "state"), "deleted" | "deleting" | "superseded")
                && p["taskId"] != e.spec["taskId"]
        }) {
            let id = text(previous, "id");
            let _ = write!(
                out,
                "<option value=\"{}\"{}>{} · {}</option>",
                escape(id),
                selected(e.notes.get("replaces").is_some_and(|v| v == id)),
                escape(text(&previous["spec"], "subdomain")),
                escape(&short(text(previous, "taskId")))
            );
        }
        out.push_str("</select></label>");
    }
    let _ = write!(
        out,
        "<label for=\"sb-resource-address\">{}<select id=\"sb-resource-address\"{}>",
        tr(a, "Saved fixed address", "저장된 고정 주소"),
        disabled(e.busy || e.notes.contains_key("existing"))
    );
    for c in &e.choices {
        if c["claimedBy"].is_string()
            && c["claimedBy"] != e.id
            && !e
                .notes
                .get("replaces")
                .is_some_and(|id| c["claimedBy"] == *id)
        {
            continue;
        }
        let name = text(c, "subdomain");
        let _ = write!(
            out,
            "<option value=\"{}\"{}>{}</option>",
            escape(name),
            selected(e.spec["subdomain"] == name),
            escape(text(c, "publicUrl"))
        );
    }
    out.push_str("</select></label>");
    if e.choices.is_empty() {
        let _ = write!(
            out,
            "<p class=\"sb-hint\">{} <a href=\"#/pipe/firecrab-tunnel\">Pipe → Firecrab Tunnel</a></p>",
            tr(
                a,
                "Register a connection token once in Pipe, then select its saved address here.",
                "Pipe에서 연결 토큰을 한 번 등록한 뒤 여기에서 저장된 주소를 선택한다."
            )
        );
    }
    let target = &e.spec["target"];
    let _ = write!(
        out,
        "<label for=\"sb-resource-target\">{}<select id=\"sb-resource-target\"{}><option value=\"vm_app\"{}>{}</option><option value=\"nginx\"{}>{}</option>{}</select></label>",
        tr(a, "Application target", "앱 대상"),
        disabled(e.busy),
        selected(target["kind"] == "vm_app"),
        tr(a, "Task MicroVM app", "작업 MicroVM 앱"),
        selected(target["kind"] == "nginx"),
        tr(a, "Saved Nginx routes", "저장된 Nginx 경로"),
        if e.notes.contains_key("environment") {
            format!(
                "<option value=\"environment_service\"{}>{}</option>",
                selected(target["kind"] == "environment_service"),
                tr(a, "Declared environment service", "선언된 환경 서비스")
            )
        } else {
            String::new()
        }
    );
    if target["kind"] == "nginx" {
        out.push_str(&field(
            tr(a, "Saved route hostname", "저장된 경로 호스트 이름"),
            "sb-resource-host",
            text(target, "host"),
            "required placeholder=\"app.example\"",
            e.busy,
        ));
    } else if target["kind"] == "environment_service" {
        let services = e
            .notes
            .get("services")
            .and_then(|v| serde_json::from_str::<Value>(v).ok())
            .unwrap_or(json!([]));
        let _ = write!(
            out,
            "<label for=\"sb-resource-service\">{}<select id=\"sb-resource-service\"{}>",
            tr(a, "Declared service", "선언된 서비스"),
            disabled(e.busy)
        );
        for service in list(&services) {
            let name = text(service, "name");
            let _ = write!(
                out,
                "<option value=\"{}\"{}>{} · TCP {}</option>",
                escape(name),
                selected(target["service"] == name),
                escape(name),
                service["port"]
            );
        }
        out.push_str("</select></label>");
    } else {
        out.push_str(&field(
            tr(a, "Application TCP port", "앱 TCP 포트"),
            "sb-resource-port",
            &target["port"].as_u64().unwrap_or(8080).to_string(),
            "type=\"number\" min=\"1\" max=\"65535\" required",
            e.busy,
        ));
    }
    let _ = write!(
        out,
        "<p class=\"sb-hint\">{}</p>",
        tr(
            a,
            "Publishing makes this ready application reachable at the selected public HTTPS address. Its task and VM generation stay fixed. No existing Quick Tunnel is required.",
            "공개하면 준비된 앱에 선택한 HTTPS 주소로 접속할 수 있다. 작업과 VM 실행 세대는 고정된다. 기존 Quick Tunnel은 필요하지 않다."
        )
    );
    if e.notes.contains_key("existing") {
        let _ = write!(
            out,
            "<details id=\"sb-public-checks\" class=\"sb-fieldset\"><summary>{}</summary>{}{}<label class=\"sb-check\"><input type=\"checkbox\" id=\"sb-resource-web-socket\"{}{}>{}</label>{}<p class=\"sb-hint\">{}</p></details>",
            tr(a, "Public checks", "공개 검증"),
            field(
                tr(a, "Application check path", "앱 검증 경로"),
                "sb-resource-check-path",
                e.notes
                    .get("sb-resource-check-path")
                    .map(String::as_str)
                    .unwrap_or("/health"),
                "maxlength=\"1024\"",
                e.busy
            ),
            field(
                tr(a, "Expected HTTP body SHA-256", "예상 HTTP 본문 SHA-256"),
                "sb-resource-check-digest",
                e.notes
                    .get("sb-resource-check-digest")
                    .map(String::as_str)
                    .unwrap_or(""),
                "maxlength=\"64\"",
                e.busy
            ),
            if e.notes
                .get("sb-resource-web-socket")
                .is_some_and(|v| v == "true")
            {
                " checked"
            } else {
                ""
            },
            disabled(e.busy),
            tr(
                a,
                "Check WebSocket upgrade instead",
                "WebSocket 업그레이드 검증"
            ),
            btn(
                tr(a, "Verify public connection", "공개 연결 검증"),
                "sb-resource-verify",
                "",
                "shield",
                "btn",
                e.busy
            ),
            tr(
                a,
                "HTTPS requires the exact expected body hash. WebSocket checks confirm the upgrade handshake; application-frame tests remain separate.",
                "HTTPS는 예상 본문의 정확한 해시가 필요하다. WebSocket 검증은 업그레이드 응답을 확인한다. 앱 메시지 검증은 별도이다."
            )
        );
    }
}

#[cfg(test)]
#[path = "../../tests/unit/sandbox/resources.rs"]
mod tests;
