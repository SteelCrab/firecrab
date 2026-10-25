//! Controller service editor and durable browser retry requests.
use super::*;
const SAVED: &str = "firecrab.cluster.service.v1";

pub(super) fn content(app: &App) -> String {
    let state = &app.advanced.cluster;
    let services = app
        .cache
        .get("/api/cluster/services")
        .map(list)
        .unwrap_or_default();
    let mut out = String::from(
        "<h2>Controller services</h2><p>One service ID and pinned image across workload hosts. Updates stop each host's previous release before deploying its replacement.</p>",
    );
    if app
        .cache
        .get("/api/cluster/services")
        .is_some_and(|value| value["unavailable"] == true)
    {
        out.push_str("<p>Controller service status is unavailable. Refresh to retry.</p>");
        return out;
    }
    for service in services {
        let id = text(service, "id");
        out.push_str(&format!("<article class=\"wasm-card\" data-cluster-service=\"{}\"><h3>{}</h3><p>Service: {} · Release: {} · Generation: {}</p><p>Desired: {} · Observed: {} · Pending: {} · Degraded: {}</p><p>Image: {}</p>",
            escape(id),escape(text(&service["spec"]["service"],"name")),escape(id),service["releaseGeneration"],service["generation"],escape(text(service,"desired")),escape(text(service,"observed")),service["pending"],service["degraded"],escape(text(&service["spec"]["service"],"image"))));
        out.push_str(&format!(
            "<details><summary>Application state and credential versions</summary>{}{}</details>",
            crate::render::properties(&service["spec"]["state"]),
            crate::render::properties(&service["spec"]["credentials"])
        ));
        for host in list(&service["hosts"]) {
            out.push_str(&format!(
                "<p>Host: {} · Deployment: {} · Replicas: {} · {}</p>",
                escape(text(host, "hostId")),
                escape(text(&host["intent"], "deploymentId")),
                host["intent"]["spec"]["replicas"],
                escape(text(host, "error"))
            ));
            out.push_str(&format!(
                "<details><summary>Observed resources</summary>{}</details>",
                crate::render::properties(&host["deployment"])
            ));
        }
        out.push_str("<div class=\"cluster-actions\">");
        for (label, action) in [
            ("Edit release", "edit"),
            ("Stop", "stop"),
            ("Reconcile", "reconcile"),
            ("Operations", "operations"),
        ] {
            if state.service_pending.is_some() && matches!(action, "edit" | "stop") {
                continue;
            }
            out.push_str(&button(label, &format!("cluster-service-{action}"), id));
        }
        out.push_str("</div>");
        if let Some(ops) = app
            .cache
            .get(&format!("/api/cluster/services/{id}/operations"))
        {
            out.push_str(&format!(
                "<details open><summary>Operation history</summary>{}</details>",
                crate::render::properties(ops)
            ));
        }
        out.push_str("</article>");
    }
    if let Some(saved) = &state.service_pending {
        out.push_str(&format!("<article class=\"wasm-card\"><p>A service request has no acknowledged result. Retry uses its saved identity and configuration.</p><p>{}</p>{}</article>",escape(text(saved,"path")),button("Retry saved service request","cluster-service-retry","")));
        return out;
    }
    out.push_str(
        "<section class=\"wasm-card cluster-form\" aria-label=\"Controller service form\"><h3>",
    );
    out.push_str(if state.editing_service.is_some() {
        "Update service release"
    } else {
        "New controller service"
    });
    out.push_str("</h3><p>Use an image reference ending in @sha256: followed by its 64-character digest. Keep the same name, hosts, networks and host ports when updating.</p><div class=\"cluster-fields\">");
    for (key, label, default, kind) in [
        ("service-name", "Service name", "", "text"),
        ("service-image", "Pinned OCI image", "", "text"),
        ("service-port", "Application port", "8080", "number"),
        ("service-health", "Health path", "/health", "text"),
        ("service-cpu", "vCPUs per replica", "1", "number"),
        ("service-ram", "Memory per replica (MiB)", "512", "number"),
    ] {
        out.push_str(&field(state, key, label, default, kind));
    }
    out.push_str("</div><details><summary>Application state and configuration</summary><p>Defaults declare stateless HTTP, duplicate-safe requests and no background work. External state references environment variable names for shared session, upload or database URLs. Exclusive work requires an ownership protocol.</p><p>Credentials map environment names to controller PipeSecret references: {&quot;TOKEN&quot;:{&quot;secretId&quot;:&quot;UUID&quot;,&quot;version&quot;:1}}. Values remain in encrypted vaults.</p>");
    for (key, label, default) in [
        (
            "service-state",
            "Application state contract (JSON)",
            "{\"mode\":\"stateless_http\"}",
        ),
        ("service-env", "Environment variables (JSON)", "{}"),
        (
            "service-credentials",
            "Versioned controller credentials (JSON)",
            "{}",
        ),
    ] {
        out.push_str(&field(state, key, label, default, "text"));
    }
    out.push_str("</details><h4>Replica distribution</h4>");
    for host in list(&app.resource) {
        let report = &host["report"];
        let id = text(report, "id");
        let networks = list(&report["networks"]);
        let chosen = value(state, &format!("service-host-{id}"), "false") == "true";
        let network = format!("service-network-{id}");
        out.push_str(&format!("<fieldset><legend><label><input type=\"checkbox\" data-cluster-input=\"service-host-{}\" {}>{}</label></legend><label class=\"field\">Local network<select data-cluster-input=\"{}\">{}</select></label>",
            escape(id),if chosen { "checked" } else { "" },escape(text(report,"name")),escape(&network),options(networks,"id","name",value(state,&network,networks.first().map(|n| text(n,"id")).unwrap_or_default()))));
        out.push_str(&field(
            state,
            &format!("service-replicas-{id}"),
            "Replicas on this host",
            "1",
            "number",
        ));
        out.push_str(&field(
            state,
            &format!("service-hostport-{id}"),
            "First host port",
            "30100",
            "number",
        ));
        out.push_str("</fieldset>");
    }
    out.push_str(&button(
        if state.editing_service.is_some() {
            "Deploy release"
        } else {
            "Create service"
        },
        "cluster-service-submit",
        "",
    ));
    if state.editing_service.is_some() {
        out.push_str(&button("Cancel edit", "cluster-service-cancel", ""));
    }
    out.push_str("</section>");
    out
}

fn spec(app: &App) -> Result<Value, String> {
    let state = &app.advanced.cluster;
    let number = |key: &str, default| {
        value(state, key, default)
            .parse::<u64>()
            .map_err(|_| format!("Enter a valid {key}."))
    };
    let mut targets = Vec::new();
    let mut count = 0u64;
    for host in list(&app.resource) {
        let report = &host["report"];
        let id = text(report, "id");
        if value(state, &format!("service-host-{id}"), "false") != "true" {
            continue;
        }
        let replicas = number(&format!("service-replicas-{id}"), "1")?;
        count = count
            .checked_add(replicas)
            .ok_or("Replica count is too large.")?;
        let network = value(
            state,
            &format!("service-network-{id}"),
            list(&report["networks"])
                .first()
                .map(|n| text(n, "id"))
                .unwrap_or_default(),
        );
        targets.push(json!({"hostId":id,"replicas":replicas,"request":{"microNetworkId":network,"hostPort":number(&format!("service-hostport-{id}"),"30100")?}}));
    }
    let mut spec = json!({"mode":"replicated","service":{"name":value(state,"service-name",""),"image":value(state,"service-image",""),
        "port":number("service-port","8080")?,"healthPath":value(state,"service-health","/health"),"cpu":number("service-cpu","1")?,"ram":number("service-ram","512")?,"replicas":count},"targets":targets});
    if let Some((id, _)) = &state.editing_service
        && let Some(current) = app
            .cache
            .get("/api/cluster/services")
            .map(list)
            .unwrap_or_default()
            .iter()
            .find(|s| text(s, "id") == id)
    {
        let mut service = current["spec"]["service"].clone();
        if let Some(fields) = spec["service"].as_object() {
            for (key, value) in fields {
                service[key] = value.clone();
            }
        }
        spec["service"] = service;
    }
    for (key, default, target) in [
        ("service-state", "{\"mode\":\"stateless_http\"}", "state"),
        ("service-credentials", "{}", "credentials"),
        ("service-env", "{}", "env"),
    ] {
        let parsed: Value = serde_json::from_str(value(state, key, default))
            .map_err(|_| format!("Enter a JSON object for {target}."))?;
        if !parsed.is_object() {
            return Err(format!("Enter a JSON object for {target}."));
        }
        if target == "env" {
            spec["service"]["env"] = parsed;
        } else {
            spec[target] = parsed;
        }
    }
    Ok(spec)
}

pub(super) fn restore(app: &Shared) {
    if app.borrow().advanced.cluster.service_pending.is_some() {
        return;
    }
    if let Ok(Some(storage)) = window().session_storage()
        && let Ok(Some(saved)) = storage.get_item(SAVED)
        && let Ok(saved) = serde_json::from_str::<Value>(&saved)
        && text(&saved, "path").starts_with("/api/cluster/services")
        && !text(&saved, "key").is_empty()
        && saved["body"].is_object()
    {
        app.borrow_mut().advanced.cluster.service_pending = Some(saved);
    }
}

fn edit(app: &Shared, id: &str) {
    let service = app
        .borrow()
        .cache
        .get("/api/cluster/services")
        .map(list)
        .unwrap_or_default()
        .iter()
        .find(|s| text(s, "id") == id)
        .cloned();
    let Some(service) = service else {
        return;
    };
    let mut app = app.borrow_mut();
    let state = &mut app.advanced.cluster;
    state.values.retain(|key, _| !key.starts_with("service-"));
    state.editing_service = Some((
        id.into(),
        service["generation"].as_u64().unwrap_or_default(),
    ));
    for (key, value) in [
        ("service-state", &service["spec"]["state"]),
        ("service-credentials", &service["spec"]["credentials"]),
        ("service-env", &service["spec"]["service"]["env"]),
    ] {
        if value.is_object() {
            state.values.insert(key.into(), value.to_string());
        }
    }
    for (key, field) in [
        ("service-name", "name"),
        ("service-image", "image"),
        ("service-port", "port"),
        ("service-health", "healthPath"),
        ("service-cpu", "cpu"),
        ("service-ram", "ram"),
    ] {
        let value = &service["spec"]["service"][field];
        state.values.insert(
            key.into(),
            value
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| value.to_string()),
        );
    }
    for target in list(&service["spec"]["targets"]) {
        let host = text(target, "hostId");
        for (key, value) in [
            (format!("service-host-{host}"), "true".into()),
            (
                format!("service-replicas-{host}"),
                target["replicas"].to_string(),
            ),
            (
                format!("service-network-{host}"),
                text(&target["request"], "microNetworkId").into(),
            ),
            (
                format!("service-hostport-{host}"),
                target["request"]["hostPort"].to_string(),
            ),
        ] {
            state.values.insert(key, value);
        }
    }
}

fn new_request(app: &App, action: &str, id: &str) -> Result<Value, String> {
    let (path, body) = if action == "cluster-service-stop" {
        let service = app
            .cache
            .get("/api/cluster/services")
            .map(list)
            .unwrap_or_default()
            .iter()
            .find(|s| text(s, "id") == id)
            .ok_or("Refresh the service before stopping it.")?;
        (
            format!("/api/cluster/services/{}/stop", api::encode(id)),
            json!({"expectedGeneration":service["generation"]}),
        )
    } else {
        let spec = spec(app)?;
        if let Some((id, generation)) = &app.advanced.cluster.editing_service {
            (
                format!("/api/cluster/services/{}/update", api::encode(id)),
                json!({"expectedGeneration":generation,"spec":spec}),
            )
        } else {
            ("/api/cluster/services".into(), spec)
        }
    };
    let mut bytes = [0u8; 16];
    window()
        .crypto()
        .map_err(api::error)?
        .get_random_values_with_u8_array(&mut bytes)
        .map_err(api::error)?;
    Ok(
        json!({"path":path,"body":body,"key":bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()}),
    )
}

pub(super) fn dispatch(app: &Shared, action: &str, id: &str) -> bool {
    if !action.starts_with("cluster-service-") {
        return false;
    }
    if app.borrow().busy {
        return true;
    }
    if app.borrow().advanced.cluster.service_pending.is_some()
        && matches!(
            action,
            "cluster-service-submit" | "cluster-service-stop" | "cluster-service-edit"
        )
    {
        app.borrow_mut().error =
            "Retry the saved service request before starting another mutation.".into();
        render_messages(app);
        return true;
    }
    if action == "cluster-service-edit" {
        edit(app, id);
        render_content(app);
        return true;
    }
    if action == "cluster-service-cancel" {
        app.borrow_mut().advanced.cluster.editing_service = None;
        app.borrow_mut()
            .advanced
            .cluster
            .values
            .retain(|key, _| !key.starts_with("service-"));
        render_content(app);
        return true;
    }
    let saved = if matches!(
        action,
        "cluster-service-submit" | "cluster-service-stop" | "cluster-service-retry"
    ) {
        let request = app
            .borrow()
            .advanced
            .cluster
            .service_pending
            .clone()
            .map(Ok)
            .unwrap_or_else(|| new_request(&app.borrow(), action, id));
        match request.and_then(|saved| {
            let storage = window()
                .session_storage()
                .map_err(api::error)?
                .ok_or("Session storage is required for safe service retries.")?;
            storage
                .set_item(SAVED, &saved.to_string())
                .map_err(api::error)?;
            Ok(saved)
        }) {
            Ok(saved) => {
                app.borrow_mut().advanced.cluster.service_pending = Some(saved.clone());
                Some(saved)
            }
            Err(error) => {
                app.borrow_mut().error = error;
                render_messages(app);
                return true;
            }
        }
    } else {
        None
    };
    let action = action.to_string();
    let id = id.to_string();
    let app = app.clone();
    app.borrow_mut().busy = true;
    let generation = app.borrow().generation;
    spawn_local(async move {
        let history = action == "cluster-service-operations";
        let path = format!(
            "/api/cluster/services/{}/{}",
            api::encode(&id),
            if history { "operations" } else { "reconcile" }
        );
        let (result, clear) = if let Some(saved) = &saved {
            let result =
                api::post_idempotent(text(saved, "path"), &saved["body"], text(saved, "key")).await;
            let clear = result.is_ok()
                || result
                    .as_ref()
                    .err()
                    .is_some_and(|e| matches!(e.status, Some(400 | 409)));
            (result.map_err(|e| e.to_string()), clear)
        } else {
            (
                api::request(if history { "GET" } else { "POST" }, &path, None).await,
                false,
            )
        };
        if clear {
            if let Ok(Some(storage)) = window().session_storage() {
                let _ = storage.remove_item(SAVED);
            }
            let mut state = app.borrow_mut();
            state.advanced.cluster.service_pending = None;
            if result.is_ok() {
                state.advanced.cluster.editing_service = None;
            }
        }
        let mut state = app.borrow_mut();
        state.busy = false;
        if state.generation != generation {
            return;
        }
        match result {
            Ok(value) => {
                if history {
                    state.cache.insert(path, value);
                }
                state.error.clear();
                state.notice = "Controller service operation recorded.".into();
            }
            Err(error) => state.error = error,
        }
        drop(state);
        render_content(&app);
        crate::refresh(app.clone());
    });
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn editor_distributes_replicas_and_uses_the_shared_wire_contract() {
        let mut app = App {
            resource: json!([{"report":{"id":"00000000-0000-0000-0000-000000000001","name":"host","networks":[{"id":"00000000-0000-0000-0000-000000000002","name":"private"}]}}]),
            ..App::default()
        };
        for (key, value) in [
            ("service-name", "web"),
            (
                "service-image",
                &format!("ghcr.io/team/app@sha256:{}", "a".repeat(64)),
            ),
            ("service-host-00000000-0000-0000-0000-000000000001", "true"),
        ] {
            app.advanced.cluster.values.insert(key.into(), value.into());
        }
        let spec: firecrab_api_types::ClusterServiceSpec =
            serde_json::from_value(spec(&app).unwrap()).unwrap();
        assert_eq!(spec.service.replicas, 1);
        assert_eq!(spec.targets[0].request.host_port, 30100);
        let html = content(&app);
        assert!(html.contains("Controller service form"));
        app.advanced.cluster.service_pending =
            Some(json!({"path":"/api/cluster/services/<unsafe>"}));
        let html = content(&app);
        assert!(html.contains("Retry saved service request"));
        assert!(html.contains("&lt;unsafe&gt;"));
        assert!(!html.contains("Controller service form"));
    }
}
