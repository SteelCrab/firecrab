use crate::{
    App, Shared, api,
    model::{escape, list, text},
    render::{button, request},
    render_messages, set_html,
};
use serde_json::{Value, json};
use wasm_bindgen_futures::spawn_local;
pub fn content(app: &App) -> String {
    let mut out = String::from("<section class=\"security-groups-screen\">");
    if list(&app.resource).is_empty() {
        out.push_str("<p>No security groups yet.</p>");
    }
    for group in list(&app.resource) {
        let name = text(group, "name");
        let id = text(group, "id");
        out.push_str(&format!("<article class=\"wasm-card\"><h2>{}</h2><p>Network: {}</p>{}{}<div id=\"connections-{}\">{}</div></article>",escape(name),escape(text(group,"microNetworkId")),button(&if app.locale=="ko"{format!("{name} 규칙 편집")}else{format!("Edit rules for {name}")},"security-edit",id),request(&format!("Delete {name}"),"DELETE",&format!("/api/security-groups/{}",api::encode(id)),Value::Null),escape(id),connections(app,group)));
    }
    out.push_str("</section>");
    out
}
fn connections(app: &App, group: &Value) -> String {
    let empty = Value::Null;
    let data = app.cache.get("security-connections").unwrap_or(&empty);
    let id = text(group, "id");
    let mut out = format!(
        "<figure aria-label=\"{}\" class=\"sg-connections\">",
        escape(&if app.locale == "ko" {
            format!("{} 연결 관계", text(group, "name"))
        } else {
            format!("Connections for {}", text(group, "name"))
        })
    );
    if data["serviceError"] == true {
        out.push_str("<p role=\"alert\">Connections are unavailable.</p>");
        out.push_str(&button("Retry connections", "security-retry", ""));
        out.push_str("</figure>");
        return out;
    }
    let services = list(&data["services"]);
    let mut known = 0;
    let mut unknown = false;
    let mut count = 0;
    let mut body = String::new();
    let mut unassigned_failure = false;
    for service in services {
        let sid = text(service, "id");
        let deployment = &data["deployments"][sid];
        let saved = service["securityGroupId"] == id;
        let deployed = deployment["spec"]["securityGroupId"] == id;
        let failure = data["errors"][sid] == true;
        if !saved && !deployed {
            if failure {
                unassigned_failure = true;
            }
            continue;
        }
        count += 1;
        let instances = if deployed {
            list(&deployment["instances"])
        } else {
            &[]
        };
        known += instances.len();
        unknown |= failure;
        body.push_str(&format!(
            "<section><h3>{}</h3><p>Configured: {} replicas</p>",
            escape(text(service, "name")),
            service["replicas"]
        ));
        if deployed && !saved {
            body.push_str("<p>Previous deployment</p>");
        }
        if saved && !deployed && !deployment.is_null() {
            body.push_str("<p>Applies on the next deployment</p>");
        }
        if failure {
            body.push_str("<p role=\"alert\">VM status unavailable.</p>");
        } else if instances.is_empty() {
            body.push_str("<p>No deployed VMs</p>");
        }
        for instance in instances {
            let vm = list(&data["vms"])
                .iter()
                .find(|vm| vm["id"] == instance["vmId"]);
            let group_id = vm.map(|v| text(v, "securityGroupId")).unwrap_or("");
            let status = if group_id.is_empty() {
                "Group unverified"
            } else if group_id == id {
                "Inherited"
            } else {
                "Group mismatch"
            };
            let name = vm
                .map(|v| text(v, "name"))
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| text(instance, "vmId"));
            body.push_str(&format!("<div class=\"sg-connection-circle wasm-card\"><p>{}</p><p>{}</p><span>{}</span></div>",escape(name),escape(text(instance,"ipv4")),crate::i18n::t(status)));
        }
        body.push_str("</section>");
    }
    if unknown {
        out.push_str(&format!(
            "<figcaption>{count} services · VM count unknown</figcaption>"
        ));
    } else {
        out.push_str(&format!(
            "<figcaption>{count} services · {known} replica VMs</figcaption>"
        ));
    }
    if unassigned_failure {
        out.push_str("<p role=\"alert\">Some deployments could not be checked.</p>");
    } else if count == 0 {
        out.push_str("<p>No MicroServices connected.</p>");
    }
    out.push_str(&body);
    out.push_str("</figure>");
    out
}
pub fn load(app: &Shared) {
    if app.borrow().advanced.security_loading || app.borrow().route != "security-groups" {
        return;
    }
    app.borrow_mut().advanced.security_loading = true;
    let a = app.clone();
    let generation = app.borrow().generation;
    spawn_local(async move {
        let services = api::request("GET", "/api/micro-services", None).await;
        let vms = api::request("GET", "/api/vms", None).await;
        let mut data = json!({"services":[],"vms":[],"deployments":{},"errors":{}});
        if let Ok(vms) = vms {
            data["vms"] = vms;
        }
        match services {
            Ok(services) => {
                for service in list(&services) {
                    let id = text(service, "id");
                    match api::request(
                        "GET",
                        &format!("/api/micro-services/{}/deployment", api::encode(id)),
                        None,
                    )
                    .await
                    {
                        Ok(v) => data["deployments"][id] = v,
                        Err(_) => data["errors"][id] = json!(true),
                    }
                }
                data["services"] = services;
            }
            Err(_) => data["serviceError"] = json!(true),
        }
        let mut s = a.borrow_mut();
        s.advanced.security_loading = false;
        if s.generation != generation || s.route != "security-groups" {
            return;
        }
        s.cache.insert("security-connections".into(), data);
        for group in list(&s.resource) {
            set_html(
                &format!("connections-{}", text(group, "id")),
                &connections(&s, group),
            );
        }
    });
}
pub fn dispatch(app: &Shared, action: &str, id: &str) -> bool {
    match action {
        "security-add-rule" | "security-remove-rule" => {
            use wasm_bindgen::JsCast;
            sync_rules();
            let value = crate::element("field-rules")
                .unwrap()
                .unchecked_into::<web_sys::HtmlTextAreaElement>()
                .value();
            let mut lines: Vec<_> = value.lines().map(str::to_string).collect();
            if action == "security-add-rule" {
                lines.push("ingress | tcp | 80-80 | 0.0.0.0/0".into());
            } else if let Ok(index) = id.parse::<usize>()
                && index < lines.len()
            {
                lines.remove(index);
            }
            if let Some(parent) = crate::element("field-rules").and_then(|el| el.parent_element()) {
                parent.set_inner_html(&rules_editor(&lines.join("\n")));
            }
            app.borrow_mut().dirty = true;
            true
        }
        "security-retry" => {
            load(app);
            true
        }
        "security-edit" => {
            crate::show_detail(app.clone(), id.into());
            crate::open_form(app.clone(), "security-rules", id);
            true
        }
        "security-policy-retry" => {
            policy(app);
            true
        }
        _ => false,
    }
}
pub fn policy(app: &Shared) {
    let a = app.clone();
    let generation = app.borrow().generation;
    let selected = app
        .borrow()
        .detail
        .as_ref()
        .map(|d| text(d, "securityGroupId").to_string())
        .unwrap_or_default();
    let deploy = app
        .borrow()
        .form
        .as_ref()
        .is_some_and(|f| f.title == "Deploy MicroService");
    spawn_local(async move {
        let pipe = if deploy {
            api::request("GET", "/api/pipe", None).await.ok()
        } else {
            None
        };
        let result = if deploy && !selected.is_empty() {
            api::request(
                "GET",
                &format!("/api/security-groups/{}", api::encode(&selected)),
                None,
            )
            .await
            .map(|v| json!([v]))
        } else {
            api::request("GET", "/api/security-groups", None).await
        };
        super::capture_form(&a);
        super::capture_form(&a);
        let mut s = a.borrow_mut();
        if s.generation != generation || s.form.is_none() {
            return;
        }
        if let Some(pipe) = pipe {
            s.cache.insert("/api/pipe".into(), pipe);
        }
        match result {
            Ok(groups) => {
                s.error.clear();
                s.cache
                    .insert("/api/security-groups".into(), groups.clone());
                if let Some(form) = s.form.as_mut() {
                    form.base.as_object_mut().map(|b| b.remove("policyError"));
                    form.choose("securityGroupId", &groups, "id", "name");
                    if deploy
                        && !selected.is_empty()
                        && let Some(group) = list(&groups).first()
                    {
                        for field in &mut form.fields {
                            if field.key == "microNetworkId" {
                                field.value = text(group, "microNetworkId").into();
                                field.label = "Network";
                            }
                        }
                        form.base["policyNetwork"] = group["microNetworkId"].clone();
                    }
                }
            }
            Err(e) => {
                s.error = e;
                if let Some(form) = s.form.as_mut() {
                    form.base["policyError"] = json!(true);
                }
            }
        };
        drop(s);
        crate::render_editor(&a);
        super::sync_form();
        render_messages(&a);
    });
}

pub fn rules_editor(value: &str) -> String {
    let mut out = format!(
        "<textarea hidden id=\"field-rules\" name=\"rules\">{}</textarea><div id=\"rule-rows\">",
        escape(value)
    );
    if value.trim().is_empty() {
        out.push_str("<p>No allow rules. All new peer connections are denied.</p>");
    }
    for (i, line) in value.lines().filter(|s| !s.trim().is_empty()).enumerate() {
        let p: Vec<_> = line.split('|').map(str::trim).collect();
        if p.len() != 4 {
            continue;
        }
        let (from, to) = p[2].split_once('-').unwrap_or((p[2], p[2]));
        out.push_str(&rule_row(i, p[0], p[1], from, to, p[3]));
    }
    out.push_str("</div>");
    out.push_str(&button("Add rule", "security-add-rule", ""));
    out
}
fn rule_row(i: usize, direction: &str, protocol: &str, from: &str, to: &str, peer: &str) -> String {
    let n = i + 1;
    let select = |key: &str, label: &str, value: &str, items: &[&str]| {
        format!(
            "<div><label for=\"rule-{i}-{key}\">{}</label><select id=\"rule-{i}-{key}\" data-rule=\"{key}\">{}</select></div>",
            crate::i18n::t(label),
            items
                .iter()
                .map(|v| format!(
                    "<option value=\"{v}\" {}>{v}</option>",
                    if *v == value { "selected" } else { "" }
                ))
                .collect::<String>()
        )
    };
    let input = |key: &str, label: &str, value: &str| {
        format!(
            "<label>{}<input data-rule=\"{key}\" value=\"{}\" autocomplete=\"off\"></label>",
            crate::i18n::t(label),
            escape(value)
        )
    };
    format!(
        "<fieldset class=\"wasm-form wasm-wide sg-rule\" role=\"group\" aria-label=\"Rule {n}\"><legend>Rule {n}</legend>{}{}{}{}{}{}</fieldset>",
        select("direction", "Direction", direction, &["ingress", "egress"]),
        select("protocol", "Protocol", protocol, &["tcp", "udp"]),
        input("from", "Port from", from),
        input("to", "Port to", to),
        input(
            "peer",
            if direction == "egress" {
                "Destination IPv4 CIDR"
            } else {
                "Source IPv4 CIDR"
            },
            peer
        ),
        button(
            &format!("Remove rule {n}"),
            "security-remove-rule",
            &i.to_string()
        )
    )
}
pub fn sync_rules() {
    use wasm_bindgen::JsCast;
    let Some(hidden) = crate::element("field-rules")
        .and_then(|el| el.dyn_into::<web_sys::HtmlTextAreaElement>().ok())
    else {
        return;
    };
    let mut rules = Vec::new();
    if let Ok(rows) = crate::document().query_selector_all(".sg-rule") {
        for i in 0..rows.length() {
            if let Some(row) = rows
                .item(i)
                .and_then(|el| el.dyn_into::<web_sys::Element>().ok())
            {
                let read = |key: &str| {
                    row.query_selector(&format!("[data-rule={key}]"))
                        .ok()
                        .flatten()
                        .map(|el| {
                            if let Some(v) = el.dyn_ref::<web_sys::HtmlInputElement>() {
                                v.value()
                            } else {
                                el.unchecked_into::<web_sys::HtmlSelectElement>().value()
                            }
                        })
                        .unwrap_or_default()
                };
                rules.push(format!(
                    "{} | {} | {}-{} | {}",
                    read("direction"),
                    read("protocol"),
                    read("from"),
                    read("to"),
                    read("peer")
                ));
            }
        }
    }
    hidden.set_value(&rules.join("\n"));
}
