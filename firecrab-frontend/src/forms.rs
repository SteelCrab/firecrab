mod advanced;
use crate::model::{escape, format_pairs, format_ports, list, pairs, ports, text};
use serde_json::{Value, json};
use std::collections::BTreeMap;

#[derive(Clone)]
pub struct Field {
    pub key: &'static str,
    pub label: &'static str,
    pub kind: &'static str,
    pub value: String,
    pub options: Vec<(String, String)>,
    pub required: bool,
    pub translate_options: bool,
}
impl Field {
    pub(crate) fn new(
        key: &'static str,
        label: &'static str,
        kind: &'static str,
        value: impl ToString,
    ) -> Self {
        Self {
            key,
            label,
            kind,
            value: value.to_string(),
            options: Vec::new(),
            required: true,
            translate_options: true,
        }
    }
    pub(crate) fn optional(mut self) -> Self {
        self.required = false;
        self
    }
    pub(crate) fn choices(mut self, options: &[(&str, &str)]) -> Self {
        self.options = options
            .iter()
            .map(|(key, label)| (key.to_string(), label.to_string()))
            .collect();
        self
    }
    pub fn resources(mut self, values: &Value, key: &str, label: &str) -> Self {
        self.translate_options = false;
        self.options = list(values)
            .iter()
            .map(|value| (text(value, key).into(), text(value, label).into()))
            .collect();
        self
    }
}

#[derive(Clone)]
pub struct Form {
    pub title: String,
    pub path: String,
    pub method: &'static str,
    pub base: Value,
    pub fields: Vec<Field>,
    pub return_route: String,
    pub controller: Option<String>,
}
impl Form {
    pub fn new(
        title: &str,
        path: String,
        method: &'static str,
        fields: Vec<Field>,
        route: &str,
    ) -> Self {
        Self {
            title: title.into(),
            path,
            method,
            base: json!({}),
            fields,
            return_route: route.into(),
            controller: None,
        }
    }
    pub fn html(&self, busy: bool, errors: &BTreeMap<String, String>) -> String {
        let mut html = format!(
            "<h2>{}</h2><form id=\"editor\" role=\"region\" aria-label=\"{}\" class=\"wasm-form\"><fieldset class=\"wasm-wide wasm-form\" {} style=\"border:0;padding:0\">",
            escape(crate::i18n::t(&self.title)),
            escape(crate::i18n::t(&self.title)),
            if busy { "disabled" } else { "" }
        );

        if self.title == "Deploy MicroService" && !self.base["policyNetwork"].is_null() {
            html = html.replace(
                "id=\"editor\"",
                "id=\"editor\" data-policy-network=\"true\"",
            );
        }
        if self.title == "Deploy MicroService" {
            html.push_str("<p class=\"wasm-wide\">Secret values are injected into every replica. Changes apply on the next deployment.</p>");
            html.push_str(&format!("<p class=\"wasm-wide\">Leave the host port empty for internal access. App port: {}.</p>", self.base["port"].as_u64().unwrap_or(8080)));
        }
        let domain_deploy =
            self.title == "Deploy MicroService" && !text(&self.base, "domain").is_empty();
        if self.path == "/api/pipe/auth/firecrab" {
            html.push_str(&format!("<p class=\"wasm-wide\">{} <a href=\"https://console.firecrab.dev/#account\" target=\"_blank\" rel=\"noreferrer\">console.firecrab.dev</a>. {}</p>",crate::i18n::t("Issue an account integration token at"),crate::i18n::t("Open Account integration. The token verifies your account ID and email.")));
        }
        if domain_deploy {
            html.push_str(&format!("<p class=\"wasm-wide\">{} · Healthy replicas will connect to this domain automatically.</p><details class=\"wasm-wide\"><summary>Advanced settings</summary>",escape(text(&self.base,"domain"))));
        }
        if matches!(
            self.title.as_str(),
            "Restore database snapshot" | "Apply database schema"
        ) {
            html.push_str(&format!(
                "<p class=\"wasm-wide\">{}: <strong>{}</strong></p>",
                crate::i18n::t("Confirm database name"),
                escape(text(&self.base, "databaseName"))
            ));
        }
        let database_form = matches!(
            self.title.as_str(),
            "Connect database" | "Update connection credentials"
        );
        for (index, field) in self.fields.iter().enumerate() {
            if database_form && index == 3 {
                html.push_str(&format!(
                    "<details class=\"wasm-wide\"><summary>{}</summary><div class=\"wasm-form\">",
                    escape(crate::i18n::t("TLS & certificates"))
                ));
            }

            if field.kind == "mounts" {
                html.push_str(&crate::volumes::mount_editor(&field.value, &field.options));
                if let Some(error) = errors.get(field.key) {
                    html.push_str(&format!(
                        "<p class=\"field-error wasm-wide\" role=\"alert\">{}</p>",
                        escape(crate::i18n::t(error))
                    ));
                }
                continue;
            }
            if self.title == "Create a pool" && index == 2 {
                html.push_str("<details class=\"wasm-wide\"><summary>Advanced settings</summary><div class=\"wasm-form\">");
            }
            let key = escape(field.key);
            let field_error = errors.get(field.key).or_else(|| {
                errors
                    .iter()
                    .find(|(key, _)| key.starts_with(&format!("{}.", field.key)))
                    .map(|(_, value)| value)
            });
            let invalid = if field_error.is_some() {
                format!("aria-invalid=\"true\" aria-describedby=\"error-{key}\"")
            } else {
                String::new()
            };
            let required = if field.required { "required" } else { "" };
            html.push_str(&format!(
                "<div class=\"field {}\"><label for=\"field-{key}\">{}</label>",
                if matches!(
                    field.kind,
                    "textarea" | "secret" | "pairs" | "ports" | "lines" | "routes"
                ) {
                    "wasm-wide"
                } else {
                    ""
                },
                escape(crate::i18n::t(field.label))
            ));
            if field.kind == "queue-bindings" {
                html.push_str(&crate::advanced::queues::bindings(field));
            } else if !field.options.is_empty()
                || matches!(field.kind, "select" | "multi" | "nullable-select")
            {
                html.push_str(&format!(
                    "<select id=\"field-{key}\" name=\"{key}\" {required} {invalid} {}>",
                    if field.kind == "multi" {
                        "multiple"
                    } else {
                        ""
                    }
                ));
                if !field.required && field.kind != "multi" {
                    html.push_str(&format!(
                        "<option value=\"\">{}</option>",
                        crate::i18n::t("Default")
                    ));
                }
                if !field.value.is_empty()
                    && field.kind != "multi"
                    && !field.options.iter().any(|(v, _)| *v == field.value)
                {
                    html.push_str(&format!(
                        "<option value=\"{}\" selected>{}</option>",
                        escape(&field.value),
                        escape(&field.value)
                    ));
                }
                for (value, label) in &field.options {
                    html.push_str(&format!(
                        "<option value=\"{}\" {} {}>{}</option>",
                        escape(value),
                        if *value == field.value
                            || (field.kind == "multi"
                                && field.value.lines().any(|selected| selected == value))
                        {
                            "selected"
                        } else {
                            ""
                        },
                        if field.key == "microNetworkId"
                            && crate::model::list(&self.base["disabledNetworks"])
                                .iter()
                                .any(|id| id == value)
                        {
                            "disabled"
                        } else {
                            ""
                        },
                        escape(if field.translate_options {
                            crate::i18n::t(label)
                        } else {
                            label
                        })
                    ));
                }
                html.push_str("</select>");
            } else if field.kind == "checkbox" {
                html.push_str(&format!(
                    "<input type=\"checkbox\" id=\"field-{key}\" name=\"{key}\" value=\"true\" {}>",
                    if field.value == "true" { "checked" } else { "" }
                ));
            } else if field.kind == "secret" {
                html.push_str(&format!("<textarea id=\"field-{key}\" name=\"{key}\" autocomplete=\"off\" autocapitalize=\"off\" spellcheck=\"false\" maxlength=\"16384\" style=\"-webkit-text-security:disc\" {invalid}>{}</textarea><label><input type=\"checkbox\" id=\"secret-show-value\">{}</label>", escape(&field.value), crate::i18n::t("Show entered value")));
            } else if field.kind == "rules" {
                html.push_str(&crate::advanced::security::rules_editor(&field.value));
            } else if field.kind == "days" {
                html.push_str(&format!("<input type=\"hidden\" id=\"field-days\" name=\"days\" value=\"{}\"><div role=\"group\" aria-label=\"Days\">", escape(&field.value)));
                for day in ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"] {
                    html.push_str(&format!("<button type=\"button\" aria-pressed=\"{}\" data-action=\"schedule-day\" data-id=\"{day}\">{day}</button>", field.value.split(',').any(|value| value==day)));
                }
                html.push_str("</div>");
            } else if matches!(
                field.kind,
                "textarea" | "pairs" | "ports" | "lines" | "routes"
            ) {
                html.push_str(&format!("<textarea id=\"field-{key}\" name=\"{key}\" {required} {invalid} spellcheck=\"false\">{}</textarea>", escape(&field.value)));
            } else {
                let constraints = if self.path == "/api/pipe/auth/firecrab" && field.key == "token"
                {
                    "maxlength=\"68\" pattern=\"fca_[a-f0-9]{64}\""
                } else {
                    ""
                };
                let kind = if field.kind == "number" {
                    "number"
                } else if field.kind == "password" {
                    "password"
                } else if ["date", "time"].contains(&field.kind) {
                    field.kind
                } else {
                    "text"
                };
                html.push_str(&format!("<input id=\"field-{key}\" name=\"{key}\" type=\"{kind}\" value=\"{}\" {required} {invalid} {constraints} {} autocomplete=\"off\">", escape(&field.value), if kind == "number" { if field.key == "hostPort" && self.title == "Deploy MicroService" { format!("min=\"0\" max=\"{}\" step=\"1\"", 65536_u64.saturating_sub(self.base["replicas"].as_u64().unwrap_or(1))) } else { "min=\"0\" step=\"1\"".to_string() } } else { String::new() }));
            }
            if let Some(error) = field_error {
                html.push_str(&format!(
                    "<p class=\"field-error\" id=\"error-{key}\" role=\"alert\">{}</p>",
                    escape(crate::i18n::t(error))
                ));
            }
            if database_form && field.key == "uri" {
                html.push_str(&crate::render::button("Show URI", "db-show-uri", ""));
            }
            html.push_str("</div>");
            if database_form && index == 4 {
                html.push_str("</div></details>");
            }
        }
        if database_form {
            html = html.replacen(
                "class=\"wasm-form\"",
                "class=\"wasm-form pipe-database-form\"",
                1,
            );
        }
        if self.title == "Create a pool" {
            html.push_str("</div></details>");
        }
        if domain_deploy {
            html.push_str("</details>");
        }
        for (key, error) in errors
            .iter()
            .filter(|(key, _)| !self.fields.iter().any(|f| f.key == key.as_str()))
        {
            html.push_str(&format!(
                "<p class=\"field-error wasm-wide\" role=\"alert\" data-field=\"{}\">{}</p>",
                escape(key),
                escape(error)
            ));
        }
        let extra = match self.title.as_str() {
            "Connect Cloudflare" => crate::render::button("Load zones", "load-zones", ""),
            "Connect database" | "Update connection credentials" => {
                crate::render::button("Test connection", "database-test", "")
            }
            "Import OCI image" => crate::render::button("Inspect image", "oci-inspect", ""),
            _ => String::new(),
        };
        html.push_str(&extra);
        if matches!(
            self.title.as_str(),
            "New secret" | "Edit secret" | "New connection" | "Edit connection"
        ) {
            let label = crate::i18n::t(
                if matches!(self.title.as_str(), "New secret" | "Edit secret") {
                    "Save secret"
                } else {
                    "Save connection"
                },
            );
            html.push_str(&format!("<div class=\"wasm-actions wasm-wide\"><button class=\"btn primary\" type=\"submit\" aria-label=\"{label}\">{}</button>{}</div></fieldset></form><div id=\"form-result\" aria-live=\"polite\"></div>", crate::i18n::t("Save"), crate::render::icon_button("Cancel", "cancel", "", crate::render::Icon::Close)));
        } else {
            html.push_str(&format!("<div class=\"wasm-actions wasm-wide\"><button class=\"btn primary\" type=\"submit\">{}</button><button class=\"btn\" type=\"button\" data-action=\"cancel\">{}</button></div></fieldset></form><div id=\"form-result\" aria-live=\"polite\"></div>",crate::i18n::t(match self.title.as_str() { "Connect database"|"Update connection credentials"=>"Save connection", "Create volume"=>"Create volume", "Deploy MicroService"=>"Deploy and run", "Deploy update"=>"Deploy update", "New MicroService"|"Edit MicroService"=>"Save configuration", "New schedule"=>"Create schedule", "Edit schedule"=>"Save changes", "Create a pool"=>"Create pool", "Pool settings"=>"Save settings", "New security group"=>"Create security group", "Security group rules"=>"Save rules", "New secret"|"Edit secret"=>"Save secret", "New connection"|"Edit connection"=>"Save connection", _=>"Save" }),crate::i18n::t("Cancel")));
        }
        if database_form
            && (busy
                || self
                    .fields
                    .iter()
                    .any(|f| matches!(f.key, "name" | "uri") && f.value.trim().is_empty()))
        {
            html = html
                .replace("type=\"submit\"", "type=\"submit\" disabled")
                .replace(
                    "data-action=\"database-test\"",
                    "data-action=\"database-test\" disabled",
                );
        }
        if self.base["policyError"] == true {
            html = html.replace("type=\"submit\"", "type=\"submit\" disabled");
            html.push_str(&crate::render::button(
                "Retry loading security groups",
                "security-policy-retry",
                "",
            ));
        }
        if self.title == "Deploy update" {
            for key in ["name", "domain"] {
                html = html.replace(
                    &format!("id=\"field-{key}\""),
                    &format!("id=\"field-{key}\" readonly"),
                );
            }
        }

        html
    }
    pub fn choose(&mut self, key: &str, items: &Value, id: &str, label: &str) {
        if let Some(field) = self.fields.iter_mut().find(|field| field.key == key) {
            field.options = list(items)
                .iter()
                .map(|value| (text(value, id).into(), text(value, label).into()))
                .collect();
            field.translate_options = false;
        }
    }
    pub fn validate(&self, values: &BTreeMap<String, String>) -> BTreeMap<String, String> {
        let mut errors = BTreeMap::new();
        for field in &self.fields {
            let value = values.get(field.key).map(String::as_str).unwrap_or("");
            let error = if field.required && value.trim().is_empty() {
                Some(if crate::i18n::t("Name") == "이름" {
                    "필수 항목이다.".into()
                } else {
                    "This field is required.".into()
                })
            } else if field.key == "hostPort"
                && self.title == "Deploy MicroService"
                && value.trim().is_empty()
            {
                None
            } else {
                match field.kind {
                    "number" => match value.parse::<u32>() {
                        Ok(number) => {
                            let valid = match field.key {
                                "cpu" => (1..=32).contains(&number),
                                "ram" => [128, 256, 512, 1024, 2048, 4096, 8192, 16384, 32768]
                                    .contains(&number),
                                "hostPort" if self.title == "Deploy MicroService" => {
                                    number <= 65535
                                }
                                "hostPort" | "port" => (1..=65535).contains(&number),
                                "replicas" => (1..=100).contains(&number),
                                "sizeGib" => (1..=65535).contains(&number),
                                "minReady" => true,
                                "startupTimeout" => (1..=3600).contains(&number),
                                "drainTimeout" => number <= 3600,
                                _ => number > 0,
                            };
                            (!valid).then(|| "Value is outside the allowed range.".into())
                        }
                        Err(_) => Some("Enter a non-negative integer.".into()),
                    },
                    "pairs" => pairs(value).err(),
                    "ports" => ports(value).err(),
                    "mounts" => crate::volumes::parse_mounts(value).err(),
                    _ => None,
                }
            };
            if let Some(error) = error {
                errors.insert(field.key.into(), error);
            }
        }
        if self.path == "/api/pipe/auth/firecrab"
            && values.get("token").is_none_or(|value| {
                value.strip_prefix("fca_").is_none_or(|token| {
                    token.len() != 64
                        || !token
                            .bytes()
                            .all(|ch| ch.is_ascii_digit() || (b'a'..=b'f').contains(&ch))
                })
            })
        {
            errors.insert(
                "token".into(),
                "Enter a valid account integration token (fca_ and 64 lowercase hex characters)."
                    .into(),
            );
        }
        if ["Quick tunnel", "Firecrab address"].contains(&self.title.as_str()) {
            let apps = values.get("targetType").is_none_or(|kind| kind == "apps");
            let key = if apps { "upstreams" } else { "host" };
            if (apps || values.get("targetType").is_some_and(|v| v == "routes"))
                && values.get(key).is_none_or(|value| value.trim().is_empty())
            {
                errors.insert(key.into(), "Select a connection target.".into());
            }
            if self.title == "Firecrab address"
                && self.base["subdomain"].is_null()
                && values.get("token").is_none_or(|value| {
                    value.len() != 64 || !value.bytes().all(|ch| ch.is_ascii_hexdigit())
                })
            {
                errors.insert(
                    "token".into(),
                    "Enter the 64-character connection token.".into(),
                );
            }
        }
        errors
    }
    pub fn payload(&self, values: &BTreeMap<String, String>) -> Result<Value, String> {
        let mut body = self.base.clone();
        for field in &self.fields {
            let value = values.get(field.key).map(String::as_str).unwrap_or("");
            if field.required && value.trim().is_empty() {
                return Err(format!("{} is required.", field.label));
            }
            body[field.key] = match field.kind {
                "number"
                    if field.key == "hostPort"
                        && self.title == "Deploy MicroService"
                        && value.trim().is_empty() =>
                {
                    json!(0)
                }
                "number" => json!(
                    value
                        .parse::<u32>()
                        .map_err(|_| format!("{} must be a non-negative integer.", field.label))?
                ),
                "queue-bindings" => crate::advanced::queues::binding_payload(value)?,
                "pairs" => pairs(value)?,
                "ports" => ports(value)?,
                "mounts" => crate::volumes::parse_mounts(value)?,
                "lines" | "multi" => json!(
                    value
                        .lines()
                        .map(str::trim)
                        .filter(|line| !line.is_empty())
                        .collect::<Vec<_>>()
                ),
                "bool" | "checkbox" => json!(value == "true"),
                "nullable" | "nullable-select" if value.is_empty() => Value::Null,
                "routes" => {
                    let mut routes = Vec::new();
                    for line in value.lines().filter(|line| !line.trim().is_empty()) {
                        let parts: Vec<_> = line.split('|').map(str::trim).collect();
                        if parts.len() != 3 || !parts[1].starts_with('/') {
                            return Err(
                                "Use HOST | /PATH | ADDRESS:PORT, one route per line.".into()
                            );
                        }
                        routes.push(json!({"host": parts[0], "pathPrefix": parts[1], "upstreams": parts[2].split(',').map(str::trim).collect::<Vec<_>>() }));
                    }
                    json!(routes)
                }
                _ => json!(value),
            };
        }
        if let Some(result) = crate::databases::edit::payload(self, &body) {
            return result;
        }
        match self.title.as_str() {
            "Create volume" => {
                body["name"] = json!(text(&body, "name").trim());
                Ok(body)
            }
            "New network" if body["ipv6AddressMode"].is_null() => {
                body.as_object_mut().unwrap().remove("ipv6AddressMode");
                body.as_object_mut().unwrap().remove("ipv6Cidr");
                Ok(body)
            }
            "Deploy MicroService" => {
                let mut result = json!({"microNetworkId":if self.base["policyNetwork"].is_null(){body["microNetworkId"].clone()}else{self.base["policyNetwork"].clone()}});
                if text(&self.base, "domain").is_empty() {
                    result["hostPort"] = body["hostPort"].clone();
                }
                Ok(result)
            }
            "Connect database" => {
                if text(&body, "caCertificate").trim().is_empty() {
                    body.as_object_mut().unwrap().remove("caCertificate");
                }
                Ok(body)
            }
            "New MicroService" | "Edit MicroService" | "Deploy update" => {
                if let Some(policy) = self.base.get("autoDeploy") {
                    body["autoDeploy"] = policy.clone();
                }
                if body["queues"].as_object().is_some_and(|q| q.is_empty()) {
                    body.as_object_mut().unwrap().remove("queues");
                }
                if body["storageConnectionId"].is_null() {
                    body.as_object_mut().unwrap().remove("storageConnectionId");
                }
                if let Some(domain) = body["domain"].as_str() {
                    body["domain"] = json!(normalize_domain(domain));
                }
                if self.title == "Deploy update" {
                    let object = body.as_object_mut().expect("service configuration");
                    let operation = object.remove("operationId").unwrap_or(Value::Null);
                    let expected = object.remove("expectedReleaseId").unwrap_or(Value::Null);
                    let strategy = object.remove("strategy").unwrap_or(json!("rolling"));
                    return Ok(
                        json!({"operationId":operation,"expectedReleaseId":expected,"spec":body,"strategy":strategy}),
                    );
                }
                Ok(body)
            }
            "Routing" => Ok(json!({"config": body, "credentials": null})),
            "Quick tunnel" | "Firecrab address" => {
                let target = if body["targetType"] == "apps" {
                    json!({"type":"apps","upstreams":body["upstreams"]})
                } else if body["targetType"] == "nginx" && body["host"].is_null() {
                    json!({"type":"nginx"})
                } else {
                    json!({"type":body["targetType"],"host":body["host"]})
                };
                if self.title == "Quick tunnel" {
                    Ok(json!({"id":body["id"],"name":body["name"],"target":target}))
                } else if !body["subdomain"].is_null() {
                    Ok(json!({"subdomain":body["subdomain"],"target":target}))
                } else {
                    Ok(json!({"token":body["token"],"target":target}))
                }
            }
            _ => advanced::payload(&self.title, body),
        }
    }
}

pub fn vm_create(cache: &BTreeMap<String, Value>) -> Form {
    let empty = json!([]);
    let get = |key: &str| cache.get(key).unwrap_or(&empty);
    let images = json!(
        list(get("/api/images"))
            .iter()
            .filter(|image| image["installed"] == true)
            .cloned()
            .collect::<Vec<_>>()
    );
    Form::new(
        "New MicroVM",
        "/api/vms".into(),
        "POST",
        vec![
            Field::new("name", "Name", "text", ""),
            Field::new("template", "Image", "select", "").resources(&images, "alias", "alias"),
            Field::new("cpu", "CPU", "number", 2),
            Field::new("ram", "Memory (MiB)", "number", 1024).choices(&[
                ("128", "128"),
                ("256", "256"),
                ("512", "512"),
                ("1024", "1024"),
                ("2048", "2048"),
                ("4096", "4096"),
                ("8192", "8192"),
                ("16384", "16384"),
                ("32768", "32768"),
            ]),
            Field::new("diskGb", "Disk (GB)", "number", 8),
            Field::new("egressPolicy", "Internet access", "select", "internet")
                .choices(&[("internet", "Internet"), ("isolated", "Isolated")]),
            Field::new("microNetworkId", "Network", "select", "").resources(
                get("/api/micro-networks"),
                "id",
                "name",
            ),
            Field::new("storageRoot", "Storage", "nullable", "")
                .optional()
                .resources(get("/api/storage"), "path", "path"),
            Field::new("shellIds", "Shells", "multi", "")
                .optional()
                .resources(get("/api/shells"), "id", "name"),
            Field::new("env", "Environment (NAME=value, one per line)", "pairs", "").optional(),
            Field::new(
                "portForwards",
                "Port forwarding (HOST:GUEST/tcp, one per line)",
                "ports",
                "",
            )
            .optional(),
            Field::new("blockStorages", "Data volumes", "mounts", "[]")
                .optional()
                .resources(
                    &json!(
                        list(get(crate::volumes::ENDPOINT))
                            .iter()
                            .filter(|volume| volume["state"] == "available"
                                && volume["attachedVmId"].is_null())
                            .cloned()
                            .collect::<Vec<_>>()
                    ),
                    "id",
                    "name",
                ),
        ],
        "vms",
    )
}

pub fn resource_form(kind: &str, data: &Value, id: &str) -> Option<Form> {
    let value = |key| text(data, key).to_string();
    let number = |key, default| data[key].as_u64().unwrap_or(default).to_string();
    let editing = !id.is_empty();
    let method = if editing { "PUT" } else { "POST" };
    let suffix = if editing {
        format!("/{}", crate::api::encode(id))
    } else {
        String::new()
    };
    let form = match kind {
        "volumes" => Form::new(
            "Create volume",
            crate::volumes::ENDPOINT.into(),
            "POST",
            vec![
                Field::new("name", "Name", "text", ""),
                Field::new("sizeGib", "Size (GiB)", "number", 10),
                Field::new("storageRoot", "Storage", "nullable-select", "").optional(),
            ],
            "volumes",
        ),
        "networks" => Form::new(
            if editing {
                "Network settings"
            } else {
                "New network"
            },
            format!("/api/micro-networks{suffix}"),
            if editing { "PATCH" } else { "POST" },
            if editing {
                vec![
                    Field::new(
                        "internetEnabled",
                        "Internet enabled",
                        "bool",
                        data["internetEnabled"].as_bool().unwrap_or(true),
                    )
                    .choices(&[("true", "Enabled"), ("false", "Blocked")]),
                    Field::new(
                        "uplink",
                        "Uplink (empty restores default)",
                        "text",
                        value("uplink"),
                    )
                    .optional(),
                ]
            } else {
                vec![
                    Field::new("name", "Name", "text", ""),
                    Field::new("subnetCidr", "IPv4 subnet (CIDR)", "text", "172.31.20.0/24"),
                    Field::new("internetEnabled", "Internet enabled", "bool", "true")
                        .choices(&[("true", "Enabled"), ("false", "Blocked")]),
                    Field::new("uplink", "Uplink", "nullable", "").optional(),
                    Field::new("ipv6Cidr", "IPv6 subnet (optional)", "nullable", "").optional(),
                    Field::new(
                        "ipv6AddressMode",
                        "IPv6 address mode",
                        "nullable-select",
                        "",
                    )
                    .optional()
                    .choices(&[("slaac", "SLAAC"), ("dhcpv6", "DHCPv6")]),
                ]
            },
            "networks",
        ),
        "storages" => Form::new(
            "New storage",
            "/api/micro-storages".into(),
            "POST",
            vec![
                Field::new("name", "Name", "text", ""),
                Field::new("path", "Directory", "text", ""),
            ],
            "storages",
        ),
        "shells" => Form::new(
            if editing {
                "Publish shell revision"
            } else {
                "New shell"
            },
            if editing {
                format!("/api/shells{suffix}/revisions")
            } else {
                "/api/shells".into()
            },
            "POST",
            if editing {
                vec![Field::new(
                    "content",
                    "Shell script",
                    "textarea",
                    value("content"),
                )]
            } else {
                vec![
                    Field::new("name", "Name", "text", ""),
                    Field::new("description", "Description", "text", "").optional(),
                    Field::new(
                        "content",
                        "Shell script",
                        "textarea",
                        "#!/bin/sh\nset -eu\n",
                    ),
                ]
            },
            "shells",
        ),
        "service-deploy" => {
            let mut form = Form::new(
                "Deploy MicroService",
                format!("/api/micro-services{suffix}/deployment"),
                "POST",
                vec![
                    Field::new("hostPort", "First host port (optional)", "number", 30100)
                        .optional(),
                    Field::new("microNetworkId", "Network", "nullable-select", "").optional(),
                ],
                "services",
            );
            form.base = data.clone();
            if !text(data, "domain").is_empty() {
                form.fields.retain(|f| f.key != "hostPort");
            }
            form
        }
        "services" => {
            let mut form = Form::new(
                if editing {
                    "Edit MicroService"
                } else {
                    "New MicroService"
                },
                format!("/api/micro-services{suffix}"),
                method,
                vec![
                    Field::new(
                        "queues",
                        "Queue connections",
                        "queue-bindings",
                        crate::advanced::queues::binding_draft(&data["queues"]),
                    )
                    .optional(),
                    Field::new(
                        "storageConnectionId",
                        "S3 connection (optional)",
                        "nullable-select",
                        value("storageConnectionId"),
                    )
                    .optional(),
                    Field::new(
                        "securityGroupId",
                        "Security group",
                        "nullable-select",
                        value("securityGroupId"),
                    )
                    .optional(),
                    Field::new("name", "Name", "text", value("name")),
                    Field::new("image", "OCI image", "text", value("image")),
                    Field::new("port", "Port", "number", number("port", 8080)),
                    Field::new(
                        "healthPath",
                        "Health path",
                        "text",
                        if editing {
                            value("healthPath")
                        } else {
                            "/health".into()
                        },
                    ),
                    Field::new("domain", "Domain (optional)", "nullable", value("domain"))
                        .optional(),
                    Field::new("ram", "Memory (MiB)", "number", number("ram", 512)),
                    Field::new("cpu", "CPU", "number", number("cpu", 1)),
                    Field::new("replicas", "Replicas", "number", number("replicas", 1)),
                    Field::new(
                        "startupTimeout",
                        "Startup timeout (seconds)",
                        "number",
                        number("startupTimeout", 300),
                    ),
                    Field::new(
                        "drainTimeout",
                        "Drain timeout (seconds)",
                        "number",
                        number("drainTimeout", 30),
                    ),
                    Field::new(
                        "env",
                        "Environment (NAME=value)",
                        "pairs",
                        format_pairs(&data["env"]),
                    )
                    .optional(),
                    Field::new(
                        "secrets",
                        "Secret references (NAME=CrabSecrets ID)",
                        "pairs",
                        format_pairs(&data["secrets"]),
                    )
                    .optional(),
                ],
                "services",
            );
            form.base = json!({"runtime":"vm"});
            form
        }
        "vm-resources" => Form::new(
            "VM resources",
            format!("/api/vms{suffix}"),
            "PUT",
            vec![
                Field::new("cpu", "CPU", "number", number("cpu", 2)),
                Field::new("ram", "Memory (MiB)", "number", number("ram", 1024)),
                Field::new("diskGb", "Disk (GB)", "number", number("diskGb", 8)),
                Field::new(
                    "egressPolicy",
                    "Internet access",
                    "select",
                    value("egressPolicy"),
                )
                .choices(&[("internet", "Internet"), ("isolated", "Isolated")]),
            ],
            "vms",
        ),
        "vm-egress" => Form::new(
            "VM egress",
            format!("/api/vms{suffix}"),
            "PUT",
            vec![
                Field::new(
                    "egressPolicy",
                    "Internet access",
                    "select",
                    value("egressPolicy"),
                )
                .choices(&[("internet", "Internet"), ("isolated", "Isolated")]),
            ],
            "vms",
        ),
        "vm-env" => Form::new(
            "VM environment",
            format!("/api/vms{suffix}"),
            "PUT",
            vec![
                Field::new(
                    "env",
                    "Environment (NAME=value)",
                    "pairs",
                    format_pairs(&data["env"]),
                )
                .optional(),
            ],
            "vms",
        ),
        "vm-ports" => Form::new(
            "VM port forwarding",
            format!("/api/vms{suffix}/port-forwards"),
            "PUT",
            vec![
                Field::new(
                    "portForwards",
                    "HOST:GUEST/tcp or HOST:GUEST/udp",
                    "ports",
                    format_ports(&data["portForwards"]),
                )
                .optional(),
            ],
            "vms",
        ),
        "vm-shells" => Form::new(
            "VM shells",
            format!("/api/vms{suffix}/shells"),
            "PUT",
            vec![
                Field::new(
                    "shellIds",
                    "Shell IDs (one per line)",
                    "lines",
                    list(&data["shellRefs"])
                        .iter()
                        .map(|v| text(v, "shellId"))
                        .collect::<Vec<_>>()
                        .join("\n"),
                )
                .optional(),
            ],
            "vms",
        ),
        "vm-storage" => Form::new(
            "VM storage",
            format!("/api/vms{suffix}/storage"),
            "PUT",
            vec![Field::new(
                "storageRoot",
                "Storage path",
                "text",
                value("storageRoot"),
            )],
            "vms",
        ),
        "oci" => Form::new(
            "Import OCI image",
            "/api/oci/import".into(),
            "POST",
            vec![Field::new(
                "reference",
                "OCI reference",
                "text",
                "nginx:1.27",
            )],
            "images",
        ),
        "image-kernel" => Form::new(
            "Image kernel",
            format!("/api/images{suffix}/kernel"),
            "PUT",
            vec![Field::new(
                "kernelVersion",
                "Installed kernel version",
                "text",
                value("kernelVersion"),
            )],
            "images",
        ),
        "database" => Form::new(
            "Connect database",
            "/api/pipe/databases".into(),
            "POST",
            vec![
                Field::new("name", "Connection name", "text", ""),
                Field::new("provider", "Database source", "select", "local").choices(&[
                    ("local", "Local"),
                    ("supabase", "Supabase"),
                    ("rds", "RDS"),
                ]),
                Field::new("uri", "Database URI", "password", ""),
                Field::new("tlsMode", "TLS", "select", "")
                    .choices(&[
                        ("disable", "Disabled"),
                        ("require", "Required"),
                        ("verify_full", "Verify certificate"),
                    ])
                    .optional(),
                Field::new(
                    "caCertificate",
                    "CA certificate (PEM, optional)",
                    "textarea",
                    "",
                )
                .optional(),
                Field::new("writeEnabled", "Allow writes", "checkbox", "false").optional(),
            ],
            "pipe/database",
        ),
        "firecrab" => {
            let mut f = Form::new(
                "Connect Firecrab",
                "/api/pipe/auth/firecrab".into(),
                "PUT",
                vec![Field::new(
                    "token",
                    "Account integration token",
                    "password",
                    "",
                )],
                "pipe/auth",
            );
            f.base = json!({"provider":"firecrab"});
            f
        }
        "github" => {
            let mut f = Form::new(
                "Connect GitHub",
                "/api/pipe/auth/github".into(),
                "PUT",
                vec![Field::new("token", "Token", "password", "")],
                "pipe/auth",
            );
            f.base = json!({"provider":"github"});
            f
        }
        "aws" => {
            let mut f = Form::new(
                "Connect AWS",
                "/api/pipe/auth/aws".into(),
                "PUT",
                vec![
                    Field::new("accessKeyId", "Access key ID", "password", ""),
                    Field::new("secretAccessKey", "Secret access key", "password", ""),
                    Field::new("sessionToken", "Session token", "password", "").optional(),
                    Field::new("region", "Region", "text", "ap-northeast-2"),
                ],
                "pipe/auth",
            );
            f.base = json!({"provider":"aws"});
            f
        }
        "registry" => Form::new(
            "Connect registry",
            "/api/pipe/registries/".into(),
            "PUT",
            vec![
                Field::new(
                    "registry",
                    "Registry host",
                    "text",
                    if editing {
                        value("registry")
                    } else {
                        "ghcr.io".into()
                    },
                ),
                Field::new("username", "Username", "text", value("username")),
                Field::new("secret", "Password / token", "password", ""),
                Field::new("provider", "Provider", "nullable-select", value("provider"))
                    .optional()
                    .choices(&[("harbor", "Harbor")]),
                Field::new("accountName", "Account name", "text", value("accountName")).optional(),
            ],
            "pipe/registry",
        ),
        "registry-pull" => {
            let mut f = Form::new(
                "Pull registry image",
                "/api/pipe/registry-transfers".into(),
                "POST",
                vec![Field::new("reference", "Image reference", "text", "")],
                "pipe/registry",
            );
            f.base = json!({"action":"pull"});
            f
        }
        "route" => {
            let mut f = Form::new(
                "Routing",
                "/api/pipe".into(),
                "PUT",
                vec![
                    Field::new(
                        "listen",
                        "Listen address",
                        "text",
                        if data["listen"].is_string() {
                            value("listen")
                        } else {
                            "127.0.0.1:8080".into()
                        },
                    ),
                    Field::new(
                        "routes",
                        "HOST | /PATH | ADDRESS:PORT (one per line)",
                        "routes",
                        list(&data["routes"])
                            .iter()
                            .filter(|route| route["owner"].is_null())
                            .map(|r| {
                                format!(
                                    "{} | {} | {}",
                                    text(r, "host"),
                                    text(r, "pathPrefix"),
                                    list(&r["upstreams"])
                                        .iter()
                                        .filter_map(Value::as_str)
                                        .collect::<Vec<_>>()
                                        .join(",")
                                )
                            })
                            .collect::<Vec<_>>()
                            .join("\n"),
                    )
                    .optional(),
                ],
                "pipe/route",
            );
            f.base = if data.is_object() {
                data.clone()
            } else {
                json!({"tunnel":null})
            };
            f
        }
        "quick-tunnel" | "firecrab-tunnel" => {
            let firecrab = kind == "firecrab-tunnel";
            let target = &data["target"];
            let target_type = if text(target, "type").is_empty() {
                "apps"
            } else {
                text(target, "type")
            };
            let mut fields = Vec::new();
            if !firecrab {
                fields.push(Field::new("name", "Name", "text", value("name")));
            }
            if firecrab && !editing {
                fields.push(Field::new("token", "Ownership token", "password", ""));
            }
            fields.push(
                Field::new("targetType", "Target", "select", target_type).choices(&[
                    ("apps", "Applications"),
                    (if firecrab { "nginx" } else { "routes" }, "Named route"),
                ]),
            );
            fields.push(
                Field::new(
                    "upstreams",
                    "App addresses (one per line)",
                    "multi",
                    list(&target["upstreams"])
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join("\n"),
                )
                .optional(),
            );
            fields.push(
                Field::new(
                    "host",
                    "Route host",
                    "nullable-select",
                    text(target, "host"),
                )
                .optional(),
            );
            let mut form = Form::new(
                if firecrab {
                    "Firecrab address"
                } else {
                    "Quick tunnel"
                },
                if firecrab {
                    "/api/pipe/firecrab-tunnels".into()
                } else {
                    "/api/pipe/quick-tunnels".into()
                },
                "POST",
                fields,
                if firecrab {
                    "pipe/firecrab-tunnel"
                } else {
                    "pipe/quick-tunnel"
                },
            );
            form.base = if firecrab && editing {
                json!({"subdomain":id})
            } else {
                json!({"id":if editing{json!(id)}else{Value::Null}})
            };
            form
        }
        "named-tunnel" => {
            let mut f = Form::new(
                "Connect Cloudflare",
                "/api/pipe/cloudflare/connect".into(),
                "POST",
                vec![
                    Field::new("apiToken", "API token", "password", ""),
                    Field::new("zoneId", "Zone", "select", ""),
                    Field::new("hostname", "Hostname", "select", ""),
                ],
                "pipe/named-tunnel",
            );
            f.base = json!({"config":data});
            f
        }
        _ => return advanced::resource(kind, data, id),
    };
    let mut form = form;
    if kind == "vm-env" || kind == "vm-egress" {
        form.base = json!({"cpu": data["cpu"], "ram": data["ram"], "diskGb": data["diskGb"], "egressPolicy": data["egressPolicy"]});
    }
    Some(form)
}

pub fn manager_form(manager: &Value, path: String) -> Form {
    let settings = &manager["settings"];
    let mut form = Form::new(
        "microManager resources",
        path,
        "PUT",
        vec![
            Field::new(
                "cpu",
                "CPU",
                "number",
                settings["cpu"].as_u64().unwrap_or(2),
            ),
            Field::new(
                "memoryMib",
                "Memory (MiB)",
                "number",
                settings["memoryMib"].as_u64().unwrap_or(4096),
            ),
            Field::new(
                "diskGib",
                "Disk (GiB)",
                "number",
                settings["diskGib"].as_u64().unwrap_or(64),
            ),
            Field::new(
                "networkMode",
                "Network mode",
                "select",
                text(settings, "networkMode"),
            )
            .choices(&[
                ("nat", "NAT"),
                ("bridged", "Bridged"),
                ("mirrored", "Mirrored"),
            ]),
            Field::new(
                "networkInterface",
                "Network interface",
                "nullable-select",
                text(settings, "networkInterface"),
            )
            .optional(),
        ],
        "host",
    );
    if let Some(interface) = form
        .fields
        .iter_mut()
        .find(|field| field.key == "networkInterface")
    {
        interface.options = list(&manager["host"]["networkInterfaces"])
            .iter()
            .filter_map(Value::as_str)
            .map(|name| (name.into(), name.into()))
            .collect();
        interface.translate_options = false;
    }
    let modes = list(&manager["host"]["networkModes"]);
    if !modes.is_empty()
        && let Some(mode) = form
            .fields
            .iter_mut()
            .find(|field| field.key == "networkMode")
    {
        mode.options
            .retain(|(key, _)| modes.iter().any(|mode| mode == key));
    }
    form
}

#[cfg(test)]
mod tests {
    use super::*;
    use firecrab_api_types::{
        CreateVmRequest, DeployMicroServiceRequest, MicroServiceSpec, UpdateVmResourcesRequest,
    };

    fn values(form: &Form) -> BTreeMap<String, String> {
        form.fields
            .iter()
            .map(|field| (field.key.into(), field.value.clone()))
            .collect()
    }

    #[test]
    fn database_omits_an_absent_ca_bundle_and_preserves_explicit_pem() {
        let form = resource_form("database", &Value::Null, "").unwrap();
        let mut input = values(&form);
        input.insert("name".into(), "DB".into());
        input.insert(
            "uri".into(),
            "postgres://reader:fixture@localhost/db".into(),
        );
        let body = form.payload(&input).unwrap();
        assert!(body.get("caCertificate").is_none());
        assert!(body.get("tlsMode").is_none());
        input.insert(
            "caCertificate".into(),
            "-----BEGIN CERTIFICATE-----\nfixture\n-----END CERTIFICATE-----".into(),
        );
        assert_eq!(
            form.payload(&input).unwrap()["caCertificate"],
            input["caCertificate"]
        );
    }
    #[test]
    fn vm_create_matches_the_backend_wire_type() {
        let form = vm_create(&BTreeMap::new());
        let mut input = values(&form);
        input.insert("name".into(), "wasm-vm".into());
        input.insert("template".into(), "ubuntu-24.04".into());
        input.insert(
            "microNetworkId".into(),
            "00000000-0000-4000-8000-000000000000".into(),
        );
        let payload = form.payload(&input).unwrap();
        let _: CreateVmRequest = serde_json::from_value(payload.clone()).unwrap();
        assert_eq!(payload["ram"], 1024);
        assert_eq!(payload["storageRoot"], Value::Null);
    }
    #[test]
    fn live_env_updates_match_the_backend_and_preserve_resources() {
        let vm =
            json!({"cpu":2,"ram":1024,"diskGb":8,"egressPolicy":"isolated","env":{"OLD":"value"}});
        let form = resource_form("vm-env", &vm, "a/b").unwrap();
        let mut input = values(&form);
        input.insert("env".into(), "NEW=value".into());
        let payload = form.payload(&input).unwrap();
        let _: UpdateVmResourcesRequest = serde_json::from_value(payload.clone()).unwrap();
        assert_eq!(payload["egressPolicy"], "isolated");
        assert_eq!(payload["env"], json!({"NEW":"value"}));
        assert_eq!(form.path, "/api/vms/a%2Fb");
    }
    #[test]
    fn service_and_deployment_forms_match_the_backend_types() {
        let form = resource_form("services", &Value::Null, "").unwrap();
        let mut input = values(&form);
        input.insert("name".into(), "web".into());
        input.insert("image".into(), "nginx:1.27".into());
        let _: MicroServiceSpec = serde_json::from_value(form.payload(&input).unwrap()).unwrap();
        let deploy = resource_form("service-deploy", &Value::Null, "service-1").unwrap();
        let _: DeployMicroServiceRequest =
            serde_json::from_value(deploy.payload(&values(&deploy)).unwrap()).unwrap();
        let mut private = values(&deploy);
        private.insert("hostPort".into(), String::new());
        assert!(deploy.validate(&private).is_empty());
        let request: DeployMicroServiceRequest =
            serde_json::from_value(deploy.payload(&private).unwrap()).unwrap();
        assert_eq!(request.host_port, 0);
    }
    #[test]
    fn routing_editor_leaves_service_owned_routes_to_backend_reconciliation() {
        let config = json!({"listen":"127.0.0.1:8080", "routes":[
            {"host":"manual.example.com", "pathPrefix":"/", "upstreams":["127.0.0.1:9000"]},
            {"host":"web.internal", "pathPrefix":"/", "upstreams":["172.30.0.2:80"],
             "owner":{"serviceId":"service", "deploymentId":"release"}}
        ], "tunnel":null});
        let form = resource_form("route", &config, "").unwrap();
        let input = values(&form);
        assert!(!input["routes"].contains("web.internal"));
        let payload = form.payload(&input).unwrap();
        assert_eq!(payload["config"]["routes"].as_array().unwrap().len(), 1);
        assert_eq!(payload["config"]["routes"][0]["host"], "manual.example.com");
    }
    #[test]
    fn running_update_payload_keeps_release_fencing_outside_the_service_spec() {
        let data = json!({"name":"web","image":"ghcr.io/team/web:v2","port":8080,"healthPath":"/health","ram":256,"cpu":1,"domain":"web.example.com","env":{"MODE":"prod"},"secrets":{},"queues":{"JOBS":{"queueId":"33333333-3333-4333-8333-333333333333","access":"consume"}}});
        let mut form = resource_form("services", &data, "service").unwrap();
        form.title = "Deploy update".into();
        form.base["operationId"] = json!("11111111-1111-4111-8111-111111111111");
        form.base["expectedReleaseId"] = json!("22222222-2222-4222-8222-222222222222");
        form.fields.push(Field::new(
            "strategy",
            "Deployment strategy",
            "select",
            "blue_green",
        ));
        let body = form.payload(&values(&form)).unwrap();
        let request: firecrab_api_types::UpdateServiceRequest =
            serde_json::from_value(body.clone()).unwrap();
        assert!(request.expected_release_id.is_some());
        assert_eq!(
            request.strategy,
            firecrab_api_types::DeployStrategy::BlueGreen
        );
        assert_eq!(request.spec.env["MODE"], "prod");
        assert_eq!(
            request.spec.queues["JOBS"].access,
            firecrab_api_types::QueueAccess::Consume
        );
        assert!(body["spec"].get("operationId").is_none());
        assert!(body["spec"].get("expectedReleaseId").is_none());
        assert!(body["spec"].get("strategy").is_none());
    }
    #[test]
    fn unicode_ids_are_encoded_as_one_path_component() {
        assert_eq!(
            crate::api::encode("hello/한글?x=1"),
            "hello%2F%ED%95%9C%EA%B8%80%3Fx%3D1"
        );
    }
    #[test]
    fn ipv4_only_network_omits_ipv6_mode_but_selected_ipv6_survives() {
        let form = resource_form("networks", &Value::Null, "").unwrap();
        let mut input = values(&form);
        input.insert("name".into(), "network".into());
        let body = form.payload(&input).unwrap();
        assert!(body.get("ipv6AddressMode").is_none());
        let _: firecrab_api_types::CreateMicroNetworkRequest =
            serde_json::from_value(body).unwrap();
        input.insert("ipv6AddressMode".into(), "dhcpv6".into());
        assert_eq!(form.payload(&input).unwrap()["ipv6AddressMode"], "dhcpv6");
    }
    #[test]
    fn fixed_address_requires_token_only_for_creation_and_keeps_saved_identity() {
        let new = resource_form("firecrab-tunnel", &Value::Null, "").unwrap();
        let mut input = values(&new);
        input.insert("upstreams".into(), "10.0.0.2:80".into());
        assert!(new.validate(&input).contains_key("token"));
        input.insert("token".into(), "a".repeat(64));
        assert!(new.validate(&input).is_empty());
        let body = new.payload(&input).unwrap();
        assert_eq!(
            body["target"],
            json!({"type":"apps","upstreams":["10.0.0.2:80"]})
        );
        assert!(body.get("id").is_none());
        let edit = resource_form(
            "firecrab-tunnel",
            &json!({"target":{"type":"nginx","host":"app.example.com"}}),
            "saved",
        )
        .unwrap();
        let body = edit.payload(&values(&edit)).unwrap();
        assert_eq!(
            body,
            json!({"subdomain":"saved","target":{"type":"nginx","host":"app.example.com"}})
        );
    }
}

fn normalize_domain(input: &str) -> String {
    let input = input.trim().to_lowercase();
    if let Some(tail) = input
        .strip_prefix("https://")
        .or_else(|| input.strip_prefix("http://"))
    {
        let host = tail.trim_end_matches('/');
        if !host.contains(['/', '?', '#', ':', '@']) {
            return host.trim_end_matches('.').into();
        }
    }
    input.trim_end_matches('.').into()
}
