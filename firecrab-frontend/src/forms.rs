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
    /// Rarely used: folded into the form's "Advanced settings".
    pub advanced: bool,
    /// Optional for the form's own check, but required whenever it shows (another field decides
    /// when, and a rule elsewhere rejects it empty). It gets no "Optional" tag.
    pub conditional: bool,
    /// Example text in an empty box (translated).
    pub placeholder: Option<&'static str>,
    /// A quiet suffix inside the box, English and Korean. Empty text shows nothing.
    pub unit: Option<(&'static str, &'static str)>,
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
            advanced: false,
            conditional: false,
            placeholder: None,
            unit: None,
        }
    }
    pub(crate) fn optional(mut self) -> Self {
        self.required = false;
        self
    }
    pub(crate) fn conditional(mut self) -> Self {
        self.required = false;
        self.conditional = true;
        self
    }
    pub(crate) fn advanced(mut self) -> Self {
        self.advanced = true;
        self
    }
    pub(crate) fn placeholder(mut self, text: &'static str) -> Self {
        self.placeholder = Some(text);
        self
    }
    pub(crate) fn unit(mut self, english: &'static str, korean: &'static str) -> Self {
        self.unit = Some((english, korean));
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

/// A titled group of fields (left: title and description, right: the fields in `keys` order).
#[derive(Clone)]
pub struct FormSection {
    pub title: &'static str,
    pub description: &'static str,
    pub keys: &'static [&'static str],
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
    pub sections: Vec<FormSection>,
    /// Free memory of the host in MiB, for the line under the memory sizes of the New MicroVM
    /// page. The page only shows it: it is not part of `base`, so it never reaches a request.
    pub host_free_mib: Option<f64>,
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
            sections: Vec::new(),
            host_free_mib: None,
        }
    }
    /// The visible heading, for the pages that show the form without a panel head.
    pub fn heading(&self) -> String {
        format!("<h2>{}</h2>", escape(crate::i18n::t(&self.title)))
    }
    /// The form without a heading: the panel head, or the page that embeds it, shows the title.
    pub fn html(&self, busy: bool, errors: &BTreeMap<String, String>) -> String {
        self.html_with(busy, errors, "")
    }
    /// `html` with `body`: markup that belongs to the form but is not a field (the steps of a
    /// setup). It goes after the fields, above the result area and the footer, so the footer
    /// stays the one bar at the bottom of the form.
    pub fn html_with(&self, busy: bool, errors: &BTreeMap<String, String>, body: &str) -> String {
        let database_form = self.is_database_form();
        let mut html = format!(
            "<form id=\"editor\"{} role=\"region\" aria-label=\"{}\" class=\"wasm-form{}\"><fieldset class=\"wasm-wide wasm-form\" {} style=\"border:0;padding:0\">",
            if self.title == "Deploy MicroService" && !self.base["policyNetwork"].is_null() {
                " data-policy-network=\"true\""
            } else {
                ""
            },
            escape(crate::i18n::t(&self.title)),
            if database_form {
                " pipe-database-form"
            } else {
                ""
            },
            if busy { "disabled" } else { "" }
        );

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
            html.push_str(&format!("<p class=\"wasm-wide\">{} · Healthy replicas will connect to this domain automatically.</p><details class=\"form-advanced wasm-wide\"><summary>{}</summary>",escape(text(&self.base,"domain")),crate::i18n::t("Advanced settings")));
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
        // Sections first, then the fields no section names, then the folded advanced fields.
        let mut placed = Vec::new();
        for section in &self.sections {
            html.push_str(&format!(
                "<section class=\"form-section wasm-wide\"><div class=\"form-section-head\"><h3>{}</h3><p>{}</p></div><div class=\"form-section-fields\">",
                escape(crate::i18n::t(section.title)),
                escape(crate::i18n::t(section.description))
            ));
            for key in section.keys {
                if let Some(index) = self
                    .fields
                    .iter()
                    .position(|field| field.key == *key && !field.advanced)
                {
                    placed.push(index);
                    html.push_str(&self.field_html(index, &self.fields[index], errors));
                }
            }
            html.push_str("</div></section>");
        }
        for (index, field) in self.fields.iter().enumerate() {
            if !field.advanced && !placed.contains(&index) {
                html.push_str(&self.field_html(index, field, errors));
            }
        }
        if self.title == "Create a pool" {
            html.push_str("</div></details>");
        }
        if domain_deploy {
            html.push_str("</details>");
        }
        let advanced: Vec<_> = self
            .fields
            .iter()
            .enumerate()
            .filter(|(_, field)| field.advanced)
            .collect();
        if !advanced.is_empty() {
            html.push_str(&format!(
                "<details class=\"form-advanced wasm-wide\"{}><summary>{}<span class=\"form-advanced-list\">{}</span></summary><div class=\"wasm-form\">",
                if advanced
                    .iter()
                    .any(|(_, field)| field_error(field, errors).is_some())
                {
                    " open"
                } else {
                    ""
                },
                crate::i18n::t("Advanced settings"),
                advanced
                    .iter()
                    .map(|(_, field)| escape(crate::i18n::t(field.label)))
                    .collect::<Vec<_>>()
                    .join(" · ")
            ));
            for (index, field) in advanced {
                html.push_str(&self.field_html(index, field, errors));
            }
            html.push_str("</div></details>");
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
        html.push_str(body);
        // Without the policies a service cannot be saved: the way to ask again is part of the form.
        if self.base["policyError"] == true {
            html.push_str(&crate::render::button(
                "Retry loading security groups",
                "security-policy-retry",
                "",
            ));
        }
        html.push_str(RESULT);
        html.push_str(&self.footer_html());
        html.push_str("</fieldset></form>");
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
    /// The setup forms of a MicroService: steps with Back and Continue instead of one Save.
    pub fn is_wizard(&self) -> bool {
        matches!(
            self.title.as_str(),
            "New MicroService" | "Edit MicroService" | "Deploy update"
        )
    }
    /// The one bar at the bottom of a form: Cancel (ghost) at the left, the way on at the right.
    /// A setup form puts its Back (ghost) and Continue (primary) in the same row, before the
    /// submit button that takes Continue's place on the last step.
    fn footer_html(&self) -> String {
        let cancel = format!(
            "<button class=\"btn ghost\" type=\"button\" data-action=\"cancel\">{}</button>",
            crate::i18n::t("Cancel")
        );
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
            return format!(
                "<div class=\"wasm-actions wasm-wide form-footer\">{cancel}<button class=\"btn primary\" type=\"submit\" aria-label=\"{label}\">{}</button></div>",
                crate::i18n::t("Save")
            );
        }
        let wizard = self.is_wizard();
        format!(
            "<div class=\"wasm-actions wasm-wide form-footer{}\">{}{cancel}{}<button class=\"btn primary\" type=\"submit\">{}</button></div>",
            if wizard { " wizard-footer" } else { "" },
            self.summary_html(),
            if wizard {
                format!(
                    "<button class=\"btn ghost\" type=\"button\" data-action=\"service-back\">{}</button><button class=\"btn primary\" type=\"button\" data-action=\"service-next\">{}</button>",
                    crate::i18n::t("Back"),
                    crate::i18n::t("Next")
                )
            } else {
                String::new()
            },
            crate::i18n::t(match self.title.as_str() {
                "New MicroVM" => "Create MicroVM",
                "Connect database" | "Update connection credentials" => "Save connection",
                "Create volume" => "Create volume",
                "Deploy MicroService" => "Deploy and run",
                "Deploy update" => "Deploy update",
                "New MicroService" => "Create MicroService",
                "Edit MicroService" => "Save configuration",
                "New schedule" => "Create schedule",
                "Edit schedule" => "Save changes",
                "Create a pool" => "Create pool",
                "Pool settings" => "Save settings",
                "New security group" => "Create security group",
                "Security group rules" => "Save rules",
                "New secret" | "Edit secret" => "Save secret",
                "New connection" | "Edit connection" => "Save connection",
                _ => "Save",
            })
        )
    }
    /// The page form of a MicroVM says in its footer what it is about to create. The page
    /// script rewrites the text as the fields change.
    fn summary_html(&self) -> String {
        if self.title != "New MicroVM" {
            return String::new();
        }
        let values = self
            .fields
            .iter()
            .map(|field| (field.key.to_string(), field.value.clone()))
            .collect();
        format!(
            "<span class=\"form-summary\" id=\"form-summary\">{}</span>",
            escape(&vm_create_summary(&values))
        )
    }
    /// "Free host memory 2.8 GiB", or nothing while the host has not said (or said nonsense).
    pub fn memory_hint(&self) -> String {
        self.host_free_mib
            .filter(|mib| mib.is_finite() && *mib >= 0.0)
            .map_or_else(String::new, |mib| {
                format!(
                    "{} {} GiB",
                    crate::i18n::t("Free host memory"),
                    crate::render::gib(mib as u64)
                )
            })
    }
    /// A line of help under a control. The memory sizes of the New MicroVM page always have the
    /// slot, empty until the host has answered; the page fills it without a new render.
    fn hint(&self, field: &Field) -> Option<String> {
        (self.title == "New MicroVM" && field.key == "ram").then(|| self.memory_hint())
    }
    /// The unit of a field in the language of the page; none when it is empty.
    fn unit_text(field: &Field) -> Option<&'static str> {
        field
            .unit
            .map(|(english, korean)| {
                if crate::i18n::korean() {
                    korean
                } else {
                    english
                }
            })
            .filter(|unit| !unit.is_empty())
    }
    fn is_database_form(&self) -> bool {
        matches!(
            self.title.as_str(),
            "Connect database" | "Update connection credentials"
        )
    }
    /// One field: its label row, its control and its error. The ad-hoc `<details>` blocks of the
    /// database and pool forms open and close around fields by their position in `fields`.
    fn field_html(&self, index: usize, field: &Field, errors: &BTreeMap<String, String>) -> String {
        let database_form = self.is_database_form();
        let mut html = String::new();
        if database_form && index == 3 {
            html.push_str(&format!(
                "<details class=\"form-advanced wasm-wide\"><summary>{}</summary><div class=\"wasm-form\">",
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
            return html;
        }
        if self.title == "Create a pool" && index == 2 {
            html.push_str(&format!(
                "<details class=\"form-advanced wasm-wide\"><summary>{}</summary><div class=\"wasm-form\">",
                crate::i18n::t("Advanced settings")
            ));
        }
        let key = escape(field.key);
        let field_error = field_error(field, errors);
        // The tag is not part of the label, so the control keeps its name. The control is
        // described by it, so a screen reader still says "Optional".
        let tagged = Self::shows_optional_tag(field);
        // These editors have no single control to carry the aria attributes below.
        let describable = !matches!(field.kind, "queue-bindings" | "rules" | "days");
        let hint = self.hint(field);
        let unit = Self::unit_text(field);
        let described = [
            (tagged && describable).then(|| format!("optional-{key}")),
            unit.map(|_| format!("unit-{key}")),
            hint.as_ref().map(|_| format!("hint-{key}")),
            field_error.map(|_| format!("error-{key}")),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" ");
        let invalid = format!(
            "{}{}",
            if field_error.is_some() {
                "aria-invalid=\"true\" "
            } else {
                ""
            },
            if described.is_empty() {
                String::new()
            } else {
                format!("aria-describedby=\"{described}\"")
            }
        );
        let required = if field.required { "required" } else { "" };
        let placeholder = field.placeholder.map_or_else(String::new, |text| {
            format!("placeholder=\"{}\"", escape(crate::i18n::t(text)))
        });
        // The database form is a dedicated editor: it keeps its dropdowns (spec §6.1).
        let style = if database_form || field.kind == "queue-bindings" {
            None
        } else {
            choice_style(field)
        };
        let label = escape(crate::i18n::t(field.label));
        html.push_str(&format!(
            "<div class=\"field {}\"><div class=\"field-head\">{}{}</div>",
            if matches!(
                field.kind,
                "textarea" | "secret" | "pairs" | "ports" | "lines" | "routes"
            ) {
                "wasm-wide"
            } else {
                ""
            },
            if style.is_some() {
                format!("<span class=\"field-label\" id=\"label-{key}\">{label}</span>")
            } else {
                format!("<label for=\"field-{key}\">{label}</label>")
            },
            if tagged {
                format!(
                    "<span class=\"field-optional\"{}>{}</span>",
                    if describable {
                        format!(" id=\"optional-{key}\"")
                    } else {
                        String::new()
                    },
                    crate::i18n::t("Optional")
                )
            } else {
                String::new()
            }
        ));
        if field.kind == "queue-bindings" {
            html.push_str(&crate::advanced::queues::bindings(field));
        } else if let Some(style) = style {
            html.push_str(&choice_html(field, style, &invalid, required));
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
            html.push_str(&format!("<textarea id=\"field-{key}\" name=\"{key}\" {required} {invalid} {placeholder} spellcheck=\"false\">{}</textarea>", escape(&field.value)));
        } else {
            let constraints = if self.path == "/api/pipe/auth/firecrab" && field.key == "token" {
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
            let input = format!(
                "<input id=\"field-{key}\" name=\"{key}\" type=\"{kind}\" value=\"{}\" {required} {invalid} {constraints} {placeholder} {} autocomplete=\"off\">",
                escape(&field.value),
                if kind == "number" {
                    if field.key == "hostPort" && self.title == "Deploy MicroService" {
                        format!(
                            "min=\"0\" max=\"{}\" step=\"1\"",
                            65536_u64.saturating_sub(self.base["replicas"].as_u64().unwrap_or(1))
                        )
                    } else {
                        "min=\"0\" step=\"1\"".to_string()
                    }
                } else {
                    String::new()
                }
            );
            match unit {
                Some(unit) => html.push_str(&format!(
                    "<span class=\"input-unit\">{input}<span class=\"field-unit\" id=\"unit-{key}\">{}</span></span>",
                    escape(unit)
                )),
                None => html.push_str(&input),
            }
        }
        if let Some(error) = field_error {
            html.push_str(&format!(
                "<p class=\"field-error\" id=\"error-{key}\" role=\"alert\">{}</p>",
                escape(crate::i18n::t(error))
            ));
        }
        if let Some(hint) = hint {
            html.push_str(&format!(
                "<p class=\"field-hint\" id=\"hint-{key}\">{}</p>",
                escape(&hint)
            ));
        }
        if database_form && field.key == "uri" {
            html.push_str(&crate::render::button("Show URI", "db-show-uri", ""));
        }
        html.push_str("</div>");
        if database_form && index == 4 {
            html.push_str("</div></details>");
        }
        html
    }
    /// "Optional" next to the label of a field that may stay empty. Not for a checkbox (always
    /// optional) or a conditional field (required whenever it shows).
    fn shows_optional_tag(field: &Field) -> bool {
        !field.required && !field.conditional && field.kind != "checkbox"
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

/// The text of an error in the form, for a field or for one of its parts (`key.part`).
fn field_error<'a>(field: &Field, errors: &'a BTreeMap<String, String>) -> Option<&'a String> {
    errors.get(field.key).or_else(|| {
        errors
            .iter()
            .find(|(key, _)| key.starts_with(&format!("{}.", field.key)))
            .map(|(_, value)| value)
    })
}

/// Result area for a check or a test. It sits right above the footer so a message stays in view
/// on a tall form.
const RESULT: &str = "<div id=\"form-result\" class=\"wasm-wide\" aria-live=\"polite\"></div>";

/// How a fixed list shows: two choices as a segmented control, up to seven (the "Default" choice
/// of an optional field counts) as chips. Memory has nine sizes and still shows as chips. Lists
/// from the API (`resources`, `choose`), multi-selects and longer lists stay dropdowns.
fn choice_style(field: &Field) -> Option<&'static str> {
    if !field.translate_options || field.kind == "multi" || field.options.is_empty() {
        return None;
    }
    match field.options.len() + usize::from(!field.required) {
        2 => Some("segmented"),
        3..=7 => Some("chips"),
        _ if field.key == "ram" => Some("chips"),
        _ => None,
    }
}

/// A choice field as one radio group. `#field-{key}` is the group, as it was the dropdown.
fn choice_html(field: &Field, style: &str, invalid: &str, required: &str) -> String {
    let key = escape(field.key);
    let mut options = Vec::new();
    if !field.required {
        options.push((String::new(), crate::i18n::t("Default").to_string()));
    }
    if !field.value.is_empty() && !field.options.iter().any(|(value, _)| *value == field.value) {
        options.push((field.value.clone(), field.value.clone()));
    }
    options.extend(
        field
            .options
            .iter()
            .map(|(value, label)| (value.clone(), crate::i18n::t(label).to_string())),
    );
    // A dropdown shows its first option when nothing matches. A group has to say it.
    let selected = options
        .iter()
        .position(|(value, _)| *value == field.value)
        .unwrap_or(0);
    let mut html = format!(
        "<div role=\"radiogroup\" id=\"field-{key}\" aria-labelledby=\"label-{key}\" class=\"choice {style}\" {invalid}>"
    );
    for (position, (value, label)) in options.iter().enumerate() {
        html.push_str(&format!(
            "<label class=\"choice-option\"><input type=\"radio\" name=\"{key}\" value=\"{}\" {} {required}><span>{}</span></label>",
            escape(value),
            if position == selected { "checked" } else { "" },
            escape(label)
        ));
    }
    html.push_str("</div>");
    html
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
    // The select shows its first image when none is chosen. The form says so too, so the
    // summary in the footer names the image the select shows.
    let first_image = list(&images)
        .first()
        .map_or("", |image| text(image, "alias"));
    let mut form = Form::new(
        "New MicroVM",
        "/api/vms".into(),
        "POST",
        vec![
            Field::new("name", "Name", "text", ""),
            Field::new("template", "Image", "select", first_image)
                .resources(&images, "alias", "alias"),
            Field::new("cpu", "vCPU", "number", 2).unit("", "개"),
            Field::new("ram", "Memory", "number", 1024).choices(&[
                ("128", "128 MiB"),
                ("256", "256 MiB"),
                ("512", "512 MiB"),
                ("1024", "1 GiB"),
                ("2048", "2 GiB"),
                ("4096", "4 GiB"),
                ("8192", "8 GiB"),
                ("16384", "16 GiB"),
                ("32768", "32 GiB"),
            ]),
            Field::new("diskGb", "Disk", "number", 8).unit("GB", "GB"),
            Field::new("egressPolicy", "Internet access", "select", "internet")
                .choices(&[("internet", "Allow internet"), ("isolated", "Isolated")]),
            Field::new("microNetworkId", "MicroNetwork", "select", "").resources(
                get("/api/micro-networks"),
                "id",
                "name",
            ),
            Field::new("storageRoot", "Storage", "nullable", "")
                .optional()
                .advanced()
                .resources(get("/api/storage"), "path", "path"),
            Field::new("shellIds", "Shells", "multi", "")
                .optional()
                .advanced()
                .resources(get("/api/shells"), "id", "name"),
            Field::new("env", "Environment", "pairs", "")
                .optional()
                .advanced()
                .placeholder("NAME=value"),
            Field::new("portForwards", "Port forwarding", "ports", "")
                .optional()
                .placeholder("e.g. 8080:80/tcp"),
            Field::new("blockStorages", "Data volumes", "mounts", "[]")
                .optional()
                .advanced()
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
    );
    form.sections = vec![
        FormSection {
            title: "Basics",
            description: "Name and the image to boot.",
            keys: &["name", "template"],
        },
        FormSection {
            title: "Resources",
            description: "vCPU, memory and disk for this MicroVM.",
            keys: &["cpu", "diskGb", "ram"],
        },
        FormSection {
            title: "Network",
            description: "Isolated MicroVMs talk only inside their MicroNetwork.",
            keys: &["microNetworkId", "egressPolicy", "portForwards"],
        },
    ];
    form
}

/// The footer line of the New MicroVM page: what the form is about to create. A box that holds
/// nothing yet reads "–" and the image is left out until there is one.
/// The size of a MicroVM or a replica: "2 vCPU · 1 GiB". `ram` is in MiB; whole GiB read as GiB.
/// A box that is empty or half typed reads "–", so the line never lies about a number.
pub fn size_label(cpu: &str, ram: &str) -> String {
    let cpu = match cpu.trim() {
        "" => "– vCPU".to_owned(),
        number => format!("{number} vCPU"),
    };
    let memory = match ram.trim().parse::<u32>() {
        Ok(mib) if mib >= 1024 && mib % 1024 == 0 => format!("{} GiB", mib / 1024),
        Ok(mib) => format!("{mib} MiB"),
        Err(_) => "–".into(),
    };
    format!("{cpu} · {memory}")
}

pub fn vm_create_summary(values: &BTreeMap<String, String>) -> String {
    let get = |key: &str| values.get(key).map_or("", |value| value.trim());
    let disk = match get("diskGb") {
        "" => "– GB".to_owned(),
        number => format!("{number} GB"),
    };
    let mut parts = vec![size_label(get("cpu"), get("ram")), disk];
    if !get("template").is_empty() {
        parts.push(get("template").into());
    }
    parts.join(" · ")
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
                    Field::new("ipv6Cidr", "IPv6 subnet", "nullable", "").optional(),
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
                    Field::new("hostPort", "First host port", "number", 30100).optional(),
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
                        "S3 connection",
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
                    Field::new("domain", "Domain", "nullable", value("domain")).optional(),
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
                Field::new("caCertificate", "CA certificate (PEM)", "textarea", "").optional(),
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
                .conditional(),
            );
            // A Quick tunnel needs a host for a named route; a Firecrab address may leave it empty.
            let host = Field::new(
                "host",
                "Route host",
                "nullable-select",
                text(target, "host"),
            );
            fields.push(if firecrab {
                host.optional()
            } else {
                host.conditional()
            });
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
            .conditional(),
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

    fn one(field: Field) -> Form {
        Form::new("Test", "/x".into(), "POST", vec![field], "vms")
    }
    fn numbered(key: &'static str, count: usize) -> Field {
        let mut field = Field::new(key, "Size", "select", "0");
        field.options = (0..count).map(|i| (i.to_string(), i.to_string())).collect();
        field
    }

    #[test]
    fn two_static_choices_render_as_a_segmented_radio_group() {
        let html = one(
            Field::new("egressPolicy", "Internet access", "select", "internet")
                .choices(&[("internet", "Allow internet"), ("isolated", "Isolated")]),
        )
        .html(false, &BTreeMap::new());
        assert!(html.contains("role=\"radiogroup\" id=\"field-egressPolicy\""));
        assert!(html.contains("class=\"choice segmented\""));
        assert!(html.contains("type=\"radio\" name=\"egressPolicy\" value=\"internet\" checked"));
        assert!(!html.contains("<select id=\"field-egressPolicy\""));
    }

    #[test]
    fn a_choice_group_is_named_by_its_label_and_marks_errors_on_the_group() {
        let field = Field::new("egressPolicy", "Internet access", "select", "internet")
            .choices(&[("internet", "Allow internet"), ("isolated", "Isolated")]);
        let errors = BTreeMap::from([("egressPolicy".to_string(), "Invalid".to_string())]);
        let html = one(field).html(false, &errors);
        assert!(html.contains(
            "<span class=\"field-label\" id=\"label-egressPolicy\">Internet access</span>"
        ));
        assert!(html.contains("aria-labelledby=\"label-egressPolicy\""));
        assert!(html.contains("aria-invalid=\"true\" aria-describedby=\"error-egressPolicy\""));
        assert!(html.contains("<p class=\"field-error\" id=\"error-egressPolicy\""));
        assert!(!html.contains("<label for=\"field-egressPolicy\""));
    }

    #[test]
    fn resource_lists_and_long_lists_stay_selects() {
        let resources = one(
            Field::new("microNetworkId", "Network", "select", "").resources(
                &json!([{"id": "a", "name": "A"}, {"id": "b", "name": "B"}]),
                "id",
                "name",
            ),
        )
        .html(false, &BTreeMap::new());
        assert!(resources.contains("<select id=\"field-microNetworkId\""));
        let mut long = Field::new("size", "Size", "select", "0");
        long.options = (0..8).map(|i| (i.to_string(), i.to_string())).collect();
        assert!(
            one(long)
                .html(false, &BTreeMap::new())
                .contains("<select id=\"field-size\"")
        );
    }

    #[test]
    fn the_style_follows_the_option_count_with_the_default_choice() {
        assert_eq!(choice_style(&numbered("a", 1)), None);
        assert_eq!(choice_style(&numbered("a", 2)), Some("segmented"));
        assert_eq!(choice_style(&numbered("a", 3)), Some("chips"));
        assert_eq!(choice_style(&numbered("a", 7)), Some("chips"));
        assert_eq!(choice_style(&numbered("a", 8)), None);
        // An optional field also offers "Default": one more choice.
        assert_eq!(
            choice_style(&numbered("a", 1).optional()),
            Some("segmented")
        );
        assert_eq!(choice_style(&numbered("a", 6).optional()), Some("chips"));
        assert_eq!(choice_style(&numbered("a", 7).optional()), None);
        // Multi-selects keep their list; so do fields with no fixed list.
        let mut multi = numbered("a", 3);
        multi.kind = "multi";
        assert_eq!(choice_style(&multi), None);
        assert_eq!(choice_style(&Field::new("a", "A", "select", "")), None);
    }

    #[test]
    fn an_optional_choice_offers_default_and_an_unknown_value_gets_its_own_option() {
        let optional = one(Field::new("provider", "Provider", "nullable-select", "")
            .optional()
            .choices(&[("harbor", "Harbor")]))
        .html(false, &BTreeMap::new());
        assert!(optional.contains("class=\"choice segmented\""));
        assert!(optional.contains("name=\"provider\" value=\"\" checked"));
        assert!(!optional.contains(" required"));
        let stale = one(
            Field::new("egressPolicy", "Internet access", "select", "legacy")
                .choices(&[("internet", "Allow internet"), ("isolated", "Isolated")]),
        )
        .html(false, &BTreeMap::new());
        assert!(stale.contains("name=\"egressPolicy\" value=\"legacy\" checked"));
        assert!(!stale.contains("value=\"internet\" checked"));
    }

    #[test]
    fn a_required_choice_with_no_value_checks_the_first_option_like_a_dropdown_does() {
        let html = one(Field::new("egressPolicy", "Internet access", "select", "")
            .choices(&[("internet", "Allow internet"), ("isolated", "Isolated")]))
        .html(false, &BTreeMap::new());
        // Compare with the whitespace squeezed: a missing `checked` leaves a gap in the tag.
        let html = html.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(html.contains("name=\"egressPolicy\" value=\"internet\" checked required>"));
        assert!(html.contains("name=\"egressPolicy\" value=\"isolated\" required>"));
    }

    #[test]
    fn memory_always_uses_chips() {
        let html = vm_create(&Default::default()).html(false, &BTreeMap::new());
        assert!(html.contains("role=\"radiogroup\" id=\"field-ram\""));
        assert!(html.contains("class=\"choice chips\""));
    }

    #[test]
    fn the_database_form_keeps_its_dropdowns() {
        let html = resource_form("database", &Value::Null, "")
            .unwrap()
            .html(false, &BTreeMap::new());
        assert!(html.contains("<select id=\"field-provider\""));
        assert!(html.contains("<select id=\"field-tlsMode\""));
        assert!(!html.contains("radiogroup"));
    }

    #[test]
    fn advanced_fields_fold_and_open_on_error() {
        let form = Form::new(
            "Test",
            "/x".into(),
            "POST",
            vec![
                Field::new("name", "Name", "text", ""),
                Field::new("env", "Environment", "pairs", "")
                    .optional()
                    .advanced(),
            ],
            "vms",
        );
        assert!(
            form.html(false, &BTreeMap::new())
                .contains("<details class=\"form-advanced wasm-wide\">")
        );
        let errors = BTreeMap::from([("env".to_string(), "Invalid".to_string())]);
        assert!(
            form.html(false, &errors)
                .contains("<details class=\"form-advanced wasm-wide\" open>")
        );
        assert!(
            form.html(false, &BTreeMap::new())
                .contains("<span class=\"field-optional\" id=\"optional-env\">Optional</span>")
        );
    }

    #[test]
    fn advanced_fields_come_last_and_the_fold_lists_their_names() {
        let form = Form::new(
            "Test",
            "/x".into(),
            "POST",
            vec![
                Field::new("env", "Environment", "pairs", "")
                    .optional()
                    .advanced(),
                Field::new("name", "Name", "text", ""),
                Field::new("ports", "Ports", "ports", "")
                    .optional()
                    .advanced(),
            ],
            "vms",
        );
        let html = form.html(false, &BTreeMap::new());
        assert!(
            html.find("id=\"field-name\"").unwrap()
                < html.find("<details class=\"form-advanced").unwrap()
        );
        assert!(
            html.find("<details class=\"form-advanced").unwrap()
                < html.find("id=\"field-env\"").unwrap()
        );
        assert!(html.contains("Advanced settings<span class=\"form-advanced-list\">Environment · Ports</span></summary>"));
        assert_eq!(html.matches("id=\"field-env\"").count(), 1);
    }

    /// The rendered `.field` blocks of a form, keyed by field key.
    fn fields_of(form: &Form) -> BTreeMap<String, String> {
        let html = form.html(false, &BTreeMap::new());
        form.fields
            .iter()
            .filter_map(|field| {
                let start = html
                    .find(&format!("for=\"field-{}\"", field.key))
                    .or_else(|| html.find(&format!("id=\"label-{}\"", field.key)))?;
                let end = html[start..].find("</div>")? + start;
                Some((field.key.to_string(), html[start..end].to_string()))
            })
            .collect()
    }
    fn tagged(rows: &BTreeMap<String, String>, key: &str) -> bool {
        rows[key].contains("class=\"field-optional\"")
    }

    #[test]
    fn fields_that_may_stay_empty_say_so_and_the_tag_describes_the_control() {
        let form = one(Field::new("env", "Environment", "pairs", "").optional());
        let html = form.html(false, &BTreeMap::new());
        assert!(html.contains("<label for=\"field-env\">Environment</label><span class=\"field-optional\" id=\"optional-env\">Optional</span>"));
        assert!(html.contains("aria-describedby=\"optional-env\""));
        let errors = BTreeMap::from([("env".to_string(), "Invalid".to_string())]);
        let html = form.html(false, &errors);
        assert!(html.contains("aria-invalid=\"true\" aria-describedby=\"optional-env error-env\""));
        // A required field has no tag; a checkbox is always optional and has none either.
        assert!(
            !one(Field::new("name", "Name", "text", ""))
                .html(false, &BTreeMap::new())
                .contains("field-optional")
        );
        let checkbox = one(Field::new("write", "Allow writes", "checkbox", "false").optional());
        assert!(
            !checkbox
                .html(false, &BTreeMap::new())
                .contains("field-optional")
        );
    }

    #[test]
    fn an_optional_choice_group_is_described_by_its_tag() {
        let field = || {
            Field::new(
                "ipv6AddressMode",
                "IPv6 address mode",
                "nullable-select",
                "",
            )
            .optional()
            .choices(&[("slaac", "SLAAC"), ("dhcpv6", "DHCPv6")])
        };
        let html = one(field()).html(false, &BTreeMap::new());
        assert!(html.contains(
            "<span class=\"field-optional\" id=\"optional-ipv6AddressMode\">Optional</span>"
        ));
        assert!(html.contains("<div role=\"radiogroup\" id=\"field-ipv6AddressMode\" aria-labelledby=\"label-ipv6AddressMode\" class=\"choice chips\" aria-describedby=\"optional-ipv6AddressMode\">"), "{html}");
        let errors = BTreeMap::from([("ipv6AddressMode".to_string(), "Invalid".to_string())]);
        assert!(one(field()).html(false, &errors).contains("class=\"choice chips\" aria-invalid=\"true\" aria-describedby=\"optional-ipv6AddressMode error-ipv6AddressMode\">"));
    }

    #[test]
    fn editors_without_one_control_have_no_orphaned_tag_id() {
        let queues =
            one(Field::new("queues", "Queue connections", "queue-bindings", "").optional())
                .html(false, &BTreeMap::new());
        assert!(
            queues.contains("<span class=\"field-optional\">Optional</span>"),
            "{queues}"
        );
        let rules = resource_form("security-rules", &json!({"rules": []}), "g")
            .unwrap()
            .html(false, &BTreeMap::new());
        assert!(
            rules.contains("<span class=\"field-optional\">Optional</span>"),
            "{rules}"
        );
        for html in [queues, rules] {
            assert!(!html.contains("id=\"optional-"), "{html}");
            assert!(!html.contains("aria-describedby=\"optional-"), "{html}");
        }
    }

    #[test]
    fn fields_required_when_shown_get_no_optional_tag() {
        // `conditional` is optional for the form's own check and required whenever it shows.
        let field = Field::new("host", "Host", "text", "").conditional();
        assert!(!field.required && field.conditional);
        assert!(
            !one(field)
                .html(false, &BTreeMap::new())
                .contains("field-optional")
        );
        // Quick tunnel: app addresses and the route host. A Firecrab address may leave the host empty.
        let quick = fields_of(&resource_form("quick-tunnel", &Value::Null, "").unwrap());
        assert!(!tagged(&quick, "upstreams") && !tagged(&quick, "host"));
        let firecrab = fields_of(&resource_form("firecrab-tunnel", &Value::Null, "").unwrap());
        assert!(!tagged(&firecrab, "upstreams") && tagged(&firecrab, "host"));
        // S3: the keys are required with separate credentials; folder, name and session token are not.
        let s3 = fields_of(&resource_form("pipe-storage", &Value::Null, "").unwrap());
        assert!(!tagged(&s3, "accessKeyId") && !tagged(&s3, "secretAccessKey"));
        assert!(tagged(&s3, "sessionToken") && tagged(&s3, "prefix") && tagged(&s3, "name"));
        // The schedule's time fields depend on the repeat; its name does not.
        let schedule = fields_of(&resource_form("schedules", &Value::Null, "").unwrap());
        for key in ["time", "date", "days", "expression", "timeZone"] {
            assert!(!tagged(&schedule, key), "{key}");
        }
        assert!(tagged(&schedule, "name"));
        // Secret connection: base URL and region show with their provider; the keys are required
        // for a new connection only (an edit keeps the saved ones); prefix and tokens never.
        let new = fields_of(&resource_form("secret-connection", &Value::Null, "").unwrap());
        for key in ["baseUrl", "region", "accessKeyId", "secretAccessKey"] {
            assert!(!tagged(&new, key), "{key}");
        }
        for key in ["prefix", "token", "sessionToken"] {
            assert!(tagged(&new, key), "{key}");
        }
        let edit =
            fields_of(&resource_form("secret-connection", &json!({"id": "c"}), "c").unwrap());
        assert!(
            !tagged(&edit, "baseUrl")
                && tagged(&edit, "accessKeyId")
                && tagged(&edit, "secretAccessKey")
        );
        // The bridged network interface only shows with bridged mode and is then required.
        let manager = fields_of(&manager_form(&json!({"settings": {}}), "/x".into()));
        assert!(!tagged(&manager, "networkInterface"));
    }

    #[test]
    fn the_labels_that_lost_their_suffix_and_the_tag_read_in_korean() {
        // The language is per thread, so this does not touch the other tests.
        crate::i18n::set("ko");
        let render = |kind: &str| {
            resource_form(kind, &Value::Null, "")
                .unwrap()
                .html(false, &BTreeMap::new())
        };
        for (kind, labels) in [
            ("pipe-storage", &["폴더", "세션 토큰"][..]),
            (
                "secret-connection",
                &["이름 접두사", "Bearer 토큰", "AWS 세션 토큰"],
            ),
            ("services", &["도메인", "S3 연결"]),
            ("networks", &["IPv6 서브넷"]),
            ("database", &["CA 인증서 (PEM)"]),
        ] {
            let html = render(kind);
            for label in labels {
                assert!(
                    html.contains(&format!(">{label}</label>")),
                    "{kind}: {label}"
                );
            }
            assert!(html.contains(">선택</span>"), "{kind}");
        }
    }

    #[test]
    fn no_label_carries_its_own_optional_suffix() {
        let kinds = [
            "volumes",
            "networks",
            "storages",
            "shells",
            "service-deploy",
            "services",
            "vm-resources",
            "vm-egress",
            "vm-env",
            "vm-ports",
            "vm-shells",
            "vm-storage",
            "oci",
            "image-kernel",
            "database",
            "firecrab",
            "github",
            "aws",
            "registry",
            "registry-pull",
            "route",
            "quick-tunnel",
            "firecrab-tunnel",
            "named-tunnel",
            "pipe-storage",
            "pools",
            "security-groups",
            "security-rules",
            "schedules",
            "secrets",
            "secret-connection",
        ];
        // The database operations are built in databases/edit.rs and render through `Form::html` too.
        let operations = [
            "db-binding",
            "db-trigger",
            "db-external",
            "db-backup",
            "db-migration",
            "db-restore",
        ];
        let all_forms = || {
            let app = crate::App::default();
            let mut forms: Vec<Form> = kinds
                .iter()
                .map(|kind| {
                    resource_form(kind, &Value::Null, "").unwrap_or_else(|| panic!("{kind}"))
                })
                .collect();
            forms.push(vm_create(&Default::default()));
            forms.push(manager_form(&json!({"settings": {}}), "/x".into()));
            forms.extend(operations.iter().map(|action| {
                crate::databases::edit::operation_form(&app, action, "request")
                    .unwrap_or_else(|| panic!("{action}"))
            }));
            forms
        };
        // In English, then in Korean (the language is per thread): the tag says it once.
        for korean in [false, true] {
            crate::i18n::set(if korean { "ko" } else { "en" });
            for form in all_forms() {
                for field in &form.fields {
                    let label = crate::i18n::t(field.label);
                    assert!(
                        !label.to_lowercase().contains("optional") && !label.contains("선택)"),
                        "{}: {label}",
                        form.title
                    );
                }
            }
        }
    }

    #[test]
    fn sections_group_fields_in_key_order() {
        let mut form = Form::new(
            "Test",
            "/x".into(),
            "POST",
            vec![
                Field::new("a", "A", "text", ""),
                Field::new("b", "B", "text", ""),
                Field::new("c", "C", "text", ""),
            ],
            "vms",
        );
        assert!(form.sections.is_empty());
        form.sections = vec![
            FormSection {
                title: "First",
                description: "The first part.",
                keys: &["c", "a"],
            },
            FormSection {
                title: "Second",
                description: "The rest.",
                keys: &["b"],
            },
        ];
        let html = form.html(false, &BTreeMap::new());
        assert!(html.contains("<section class=\"form-section wasm-wide\"><div class=\"form-section-head\"><h3>First</h3><p>The first part.</p></div><div class=\"form-section-fields\">"));
        let order = ["field-c", "field-a", "Second", "field-b"].map(|key| html.find(key).unwrap());
        assert!(order.windows(2).all(|pair| pair[0] < pair[1]), "{html}");
        assert_eq!(html.matches("id=\"field-a\"").count(), 1);
    }

    #[test]
    fn the_form_has_no_heading_of_its_own() {
        let form = one(Field::new("name", "Name", "text", ""));
        assert!(!form.html(false, &BTreeMap::new()).contains("<h2"));
        assert_eq!(form.heading(), "<h2>Test</h2>");
    }

    #[test]
    fn a_size_reads_cpu_and_memory_and_dashes_what_is_missing() {
        assert_eq!(size_label("2", "1024"), "2 vCPU · 1 GiB");
        assert_eq!(size_label("1", "512"), "1 vCPU · 512 MiB");
        assert_eq!(size_label(" 4 ", " 4096 "), "4 vCPU · 4 GiB");
        // Whole GiB only: anything else stays in MiB.
        assert_eq!(size_label("2", "1536"), "2 vCPU · 1536 MiB");
        assert_eq!(size_label("", ""), "– vCPU · –");
        assert_eq!(size_label("2", "lots"), "2 vCPU · –");
    }

    #[test]
    fn a_setup_form_has_its_wizard_buttons_in_the_footer_row() {
        for title in ["New MicroService", "Edit MicroService", "Deploy update"] {
            let form = Form::new(title, "/x".into(), "POST", vec![], "services");
            assert!(form.is_wizard(), "{title}");
            let html = form.html(false, &BTreeMap::new());
            assert_eq!(html.matches("form-footer").count(), 1, "{title}: {html}");
            let footer = html
                .split("form-footer wizard-footer\">")
                .nth(1)
                .unwrap_or_else(|| panic!("{title}: {html}"));
            // Cancel (ghost), then Back (ghost), then Next (primary), then the submit button.
            let at = |needle: &str| {
                footer
                    .find(needle)
                    .unwrap_or_else(|| panic!("{needle}: {footer}"))
            };
            let cancel = at(
                "<button class=\"btn ghost\" type=\"button\" data-action=\"cancel\">Cancel</button>",
            );
            let back = at(
                "<button class=\"btn ghost\" type=\"button\" data-action=\"service-back\">Back</button>",
            );
            let next = at(
                "<button class=\"btn primary\" type=\"button\" data-action=\"service-next\">Next</button>",
            );
            let submit = at("<button class=\"btn primary\" type=\"submit\">");
            assert!(
                cancel < back && back < next && next < submit,
                "{title}: {footer}"
            );
            // The footer is the last thing in the form, and the form is all there is.
            assert!(
                html.ends_with("</button></div></fieldset></form>"),
                "{title}: {html}"
            );
            assert_eq!(html.matches("service-next").count(), 1, "{title}");
        }
    }

    #[test]
    fn other_forms_have_no_wizard_buttons() {
        for title in ["New network", "Deploy MicroService", "New MicroVM", "Test"] {
            let form = Form::new(title, "/x".into(), "POST", vec![], "services");
            assert!(!form.is_wizard(), "{title}");
            let html = form.html(false, &BTreeMap::new());
            assert!(
                !html.contains("service-next")
                    && !html.contains("service-back")
                    && !html.contains("wizard-footer"),
                "{title}: {html}"
            );
        }
    }

    #[test]
    fn a_body_goes_after_the_fields_and_before_the_result_and_the_footer() {
        let form = one(Field::new("name", "Name", "text", ""));
        let html = form.html_with(false, &BTreeMap::new(), "<section id=\"extra\"></section>");
        let at = |needle: &str| {
            html.find(needle)
                .unwrap_or_else(|| panic!("{needle}: {html}"))
        };
        assert!(at("id=\"field-name\"") < at("<section id=\"extra\">"));
        assert!(at("<section id=\"extra\">") < at("id=\"form-result\""));
        assert!(at("id=\"form-result\"") < at("form-footer"));
        assert!(at("</section>") < html.find("</form>").unwrap());
        assert_eq!(
            form.html(false, &BTreeMap::new()),
            form.html_with(false, &BTreeMap::new(), "")
        );
    }

    #[test]
    fn the_result_area_sits_inside_the_form_right_above_the_footer() {
        let html = one(Field::new("name", "Name", "text", "")).html(false, &BTreeMap::new());
        let result = html
            .find("<div id=\"form-result\" class=\"wasm-wide\" aria-live=\"polite\"></div>")
            .expect("result");
        let footer = html
            .find("<div class=\"wasm-actions wasm-wide form-footer\">")
            .expect("footer");
        assert!(result < footer && footer < html.find("</form>").unwrap());
        assert_eq!(
            &html[result..footer],
            "<div id=\"form-result\" class=\"wasm-wide\" aria-live=\"polite\"></div>"
        );
        assert!(html.ends_with("</form>"));
    }

    #[test]
    fn the_database_form_class_is_set_on_the_form_itself() {
        let html = resource_form("database", &Value::Null, "")
            .unwrap()
            .html(false, &BTreeMap::new());
        assert!(html.contains("role=\"region\" aria-label=\"Connect database\" class=\"wasm-form pipe-database-form\">"));
        assert_eq!(html.matches("pipe-database-form").count(), 1);
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
        let mut form = vm_create(&BTreeMap::new());
        // The page shows the host's free memory; the request never carries it, and
        // `CreateVmRequest` rejects unknown fields.
        form.host_free_mib = Some(8192.0);
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
        let mut keys: Vec<_> = payload.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        assert_eq!(
            keys,
            [
                "blockStorages",
                "cpu",
                "diskGb",
                "egressPolicy",
                "env",
                "microNetworkId",
                "name",
                "portForwards",
                "ram",
                "shellIds",
                "storageRoot",
                "template"
            ]
        );
    }
    fn summary_values(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect()
    }

    #[test]
    fn summarizes_the_chosen_resources() {
        let values = |pairs: &[(&str, &str)]| {
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect::<BTreeMap<_, _>>()
        };
        assert_eq!(
            vm_create_summary(&values(&[
                ("cpu", "2"),
                ("ram", "1024"),
                ("diskGb", "8"),
                ("template", "traefik/whoami:latest")
            ])),
            "2 vCPU · 1 GiB · 8 GB · traefik/whoami:latest"
        );
        assert_eq!(
            vm_create_summary(&values(&[
                ("cpu", "1"),
                ("ram", "512"),
                ("diskGb", "4"),
                ("template", "")
            ])),
            "1 vCPU · 512 MiB · 4 GB"
        );
    }

    #[test]
    fn the_summary_keeps_odd_sizes_in_mib_and_survives_half_typed_numbers() {
        let summary = |ram: &str| {
            vm_create_summary(&summary_values(&[
                ("cpu", "2"),
                ("ram", ram),
                ("diskGb", "8"),
                ("template", " alpine "),
            ]))
        };
        assert_eq!(summary("32768"), "2 vCPU · 32 GiB · 8 GB · alpine");
        assert_eq!(summary("1536"), "2 vCPU · 1536 MiB · 8 GB · alpine");
        assert_eq!(summary("128"), "2 vCPU · 128 MiB · 8 GB · alpine");
        // A box the user is still typing in shows what it holds; nothing panics.
        assert_eq!(
            vm_create_summary(&summary_values(&[
                ("cpu", ""),
                ("ram", "x"),
                ("diskGb", "")
            ])),
            "– vCPU · – · – GB"
        );
    }

    #[test]
    fn new_microvm_is_three_sections_with_the_rare_fields_folded() {
        let form = vm_create(&Default::default());
        assert_eq!(
            form.sections
                .iter()
                .map(|section| (section.title, section.keys.to_vec()))
                .collect::<Vec<_>>(),
            vec![
                ("Basics", vec!["name", "template"]),
                ("Resources", vec!["cpu", "diskGb", "ram"]),
                (
                    "Network",
                    vec!["microNetworkId", "egressPolicy", "portForwards"]
                ),
            ]
        );
        assert_eq!(
            form.sections
                .iter()
                .map(|section| section.description)
                .collect::<Vec<_>>(),
            vec![
                "Name and the image to boot.",
                "vCPU, memory and disk for this MicroVM.",
                "Isolated MicroVMs talk only inside their MicroNetwork."
            ]
        );
        let folded: Vec<_> = form
            .fields
            .iter()
            .filter(|field| field.advanced)
            .map(|field| field.key)
            .collect();
        assert_eq!(folded, ["storageRoot", "shellIds", "env", "blockStorages"]);
        let html = form.html(false, &BTreeMap::new());
        let positions: Vec<_> = ["<h3>Basics</h3>", "<h3>Resources</h3>", "<h3>Network</h3>"]
            .iter()
            .map(|title| html.find(title).expect(title))
            .collect();
        assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));
        assert_eq!(html.matches("<details class=\"form-advanced").count(), 1);
        assert!(!html.contains("<details class=\"form-advanced wasm-wide\" open"));
        assert!(html.contains("<span class=\"form-advanced-list\">Storage · Shells · Environment · Data volumes</span>"));
    }

    #[test]
    fn new_microvm_memory_and_internet_choices_read_in_plain_words() {
        let html = vm_create(&Default::default())
            .html(false, &BTreeMap::new())
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let memory = html
            .split("id=\"field-ram\"")
            .nth(1)
            .and_then(|rest| rest.split("</div>").next())
            .expect("memory group");
        let labels: Vec<_> = memory
            .split("<span>")
            .skip(1)
            .map(|part| part.split("</span>").next().unwrap())
            .collect();
        assert_eq!(
            labels,
            [
                "128 MiB", "256 MiB", "512 MiB", "1 GiB", "2 GiB", "4 GiB", "8 GiB", "16 GiB",
                "32 GiB"
            ]
        );
        assert!(memory.contains("value=\"1024\" checked"));
        assert!(html.contains("<span class=\"field-label\" id=\"label-ram\">Memory</span>"));
        assert!(html.contains(
            "name=\"egressPolicy\" value=\"internet\" checked required><span>Allow internet</span>"
        ));
        assert!(
            html.contains(
                "name=\"egressPolicy\" value=\"isolated\" required><span>Isolated</span>"
            )
        );
        // Other forms keep their wording.
        let live = resource_form("vm-egress", &json!({"egressPolicy": "internet"}), "vm")
            .unwrap()
            .html(false, &BTreeMap::new());
        assert!(live.contains("<span>Internet</span>") && !live.contains("Allow internet"));
    }

    #[test]
    fn new_microvm_is_created_not_saved() {
        let html = vm_create(&Default::default()).html(false, &BTreeMap::new());
        assert!(
            html.contains("<button class=\"btn primary\" type=\"submit\">Create MicroVM</button>")
        );
        assert!(!html.contains(">Save</button>"));
    }

    #[test]
    fn the_footer_starts_with_the_summary_of_the_form() {
        let cache = BTreeMap::from([(
            "/api/images".to_string(),
            json!([{"alias": "alpine", "installed": true}, {"alias": "ubuntu", "installed": true}]),
        )]);
        let html = vm_create(&cache).html(false, &BTreeMap::new());
        assert!(html.contains("<div class=\"wasm-actions wasm-wide form-footer\"><span class=\"form-summary\" id=\"form-summary\">2 vCPU · 1 GiB · 8 GB · alpine</span><button class=\"btn ghost\" type=\"button\" data-action=\"cancel\">Cancel</button>"), "{html}");
        // The summary follows the values the form holds, not its defaults.
        let mut form = vm_create(&cache);
        for field in &mut form.fields {
            match field.key {
                "cpu" => field.value = "4".into(),
                "ram" => field.value = "4096".into(),
                "template" => field.value = "ubuntu".into(),
                _ => {}
            }
        }
        assert!(
            form.html(false, &BTreeMap::new())
                .contains(">4 vCPU · 4 GiB · 8 GB · ubuntu</span>")
        );
        // The footer of any other form has no summary.
        let other = resource_form("vm-egress", &json!({"egressPolicy": "internet"}), "vm")
            .unwrap()
            .html(false, &BTreeMap::new());
        assert!(!other.contains("form-summary"));
    }

    #[test]
    fn memory_shows_the_free_host_memory_when_it_is_known() {
        let mut form = vm_create(&Default::default());
        // Until the host has answered the line is an empty slot; the page fills it later.
        let none = form.html(false, &BTreeMap::new());
        assert!(
            none.contains("<p class=\"field-hint\" id=\"hint-ram\"></p>"),
            "{none}"
        );
        assert_eq!(form.memory_hint(), "");
        form.host_free_mib = Some(2867.0);
        let html = form.html(false, &BTreeMap::new());
        assert!(
            html.contains("<p class=\"field-hint\" id=\"hint-ram\">Free host memory 2.8 GiB</p>"),
            "{html}"
        );
        assert_eq!(form.memory_hint(), "Free host memory 2.8 GiB");
        assert_eq!(html.matches("field-hint").count(), 1);
        // The group says it is described by the hint, so a screen reader reads it.
        assert!(
            html.contains("class=\"choice chips\" aria-describedby=\"hint-ram\""),
            "{html}"
        );
        form.host_free_mib = Some(6144.0);
        assert!(
            form.html(false, &BTreeMap::new())
                .contains("Free host memory 6 GiB</p>")
        );
        // A value that is not a size says nothing.
        form.host_free_mib = Some(f64::NAN);
        assert_eq!(form.memory_hint(), "");
        form.host_free_mib = Some(-1.0);
        assert_eq!(form.memory_hint(), "");
    }

    #[test]
    fn only_the_new_microvm_form_has_the_memory_hint_slot() {
        let live = resource_form("vm-resources", &json!({"ram": 512}), "vm")
            .unwrap()
            .html(false, &BTreeMap::new());
        assert!(!live.contains("field-hint") && !live.contains("hint-ram"));
    }

    #[test]
    fn new_microvm_labels_placeholders_and_units_follow_the_page_design() {
        let form = vm_create(&Default::default());
        let label = |key: &str| form.fields.iter().find(|f| f.key == key).unwrap().label;
        assert_eq!(label("cpu"), "vCPU");
        assert_eq!(label("diskGb"), "Disk");
        assert_eq!(label("microNetworkId"), "MicroNetwork");
        assert_eq!(label("portForwards"), "Port forwarding");
        assert_eq!(label("env"), "Environment");
        let html = form.html(false, &BTreeMap::new());
        assert!(
            html.contains("<label for=\"field-cpu\">vCPU</label>"),
            "{html}"
        );
        assert!(html.contains("<label for=\"field-diskGb\">Disk</label>"));
        assert!(html.contains("<label for=\"field-microNetworkId\">MicroNetwork</label>"));
        assert!(html.contains("<label for=\"field-portForwards\">Port forwarding</label>"));
        assert!(html.contains("<label for=\"field-env\">Environment</label>"));
        let tag = |id: &str| {
            let at = html.find(&format!("id=\"{id}\"")).unwrap();
            html[at..].split('>').next().unwrap().to_string()
        };
        assert!(tag("field-portForwards").contains("placeholder=\"e.g. 8080:80/tcp\""));
        assert!(tag("field-env").contains("placeholder=\"NAME=value\""));
        // The unit sits inside the box and describes the input. English vCPU has none.
        assert!(html.contains("<span class=\"input-unit\"><input id=\"field-diskGb\""));
        assert!(html.contains("<span class=\"field-unit\" id=\"unit-diskGb\">GB</span></span>"));
        assert!(html.contains("aria-describedby=\"unit-diskGb\""));
        assert!(
            !html.contains("unit-cpu") && !html.contains("input-unit\"><input id=\"field-cpu\"")
        );
        assert_eq!(html.matches("class=\"input-unit\"").count(), 1);
        // The live edit forms keep their long labels.
        let live = resource_form(
            "vm-resources",
            &json!({"cpu": 2, "ram": 512, "diskGb": 8}),
            "vm",
        )
        .unwrap()
        .html(false, &BTreeMap::new());
        assert!(live.contains(">CPU</label>") && live.contains(">Disk (GB)</label>"));
        assert!(!live.contains("input-unit") && !live.contains("placeholder"));
        let env = resource_form("vm-env", &json!({"env": {}}), "vm")
            .unwrap()
            .html(false, &BTreeMap::new());
        assert!(env.contains("Environment (NAME=value)"), "{env}");
    }

    #[test]
    fn units_and_placeholders_read_in_korean() {
        crate::i18n::set("ko");
        let html = vm_create(&Default::default()).html(false, &BTreeMap::new());
        let heading = resource_form("vm-resources", &json!({"ram": 512}), "vm")
            .unwrap()
            .heading();
        let egress = resource_form("vm-egress", &json!({"egressPolicy": "internet"}), "vm")
            .unwrap()
            .html(false, &BTreeMap::new());
        crate::i18n::set("en");
        for text in [
            "<label for=\"field-cpu\">vCPU</label>",
            "<span class=\"field-unit\" id=\"unit-cpu\">개</span></span>",
            "<label for=\"field-diskGb\">디스크</label>",
            "<span class=\"field-unit\" id=\"unit-diskGb\">GB</span></span>",
            "<label for=\"field-microNetworkId\">MicroNetwork</label>",
            "<label for=\"field-portForwards\">포트 포워딩</label>",
            "placeholder=\"예: 8080:80/tcp\"",
            "<label for=\"field-env\">환경 변수</label>",
            "placeholder=\"이름=값\"",
            "id=\"label-egressPolicy\">외부 통신</span>",
        ] {
            assert!(html.contains(text), "{text}: {html}");
        }
        // The same words name the live forms, and the title agrees with the menu item.
        assert!(egress.contains("외부 통신"), "{egress}");
        assert_eq!(heading, "<h2>VM 사양</h2>");
    }

    #[test]
    fn new_microvm_reads_in_korean() {
        crate::i18n::set("ko");
        let mut form = vm_create(&Default::default());
        form.host_free_mib = Some(2867.0);
        let html = form.html(false, &BTreeMap::new());
        for text in [
            "<h3>기본</h3><p>이름과 시작할 이미지를 정합니다.</p>",
            "<h3>사양</h3><p>이 MicroVM의 vCPU, 메모리, 디스크입니다.</p>",
            "<h3>네트워크</h3><p>격리하면 같은 MicroNetwork 안에서만 통신합니다.</p>",
            "id=\"label-ram\">메모리</span>",
            "<span>인터넷 허용</span>",
            "<span>격리</span>",
            "호스트 여유 메모리 2.8 GiB</p>",
            "<summary>고급 설정<span class=\"form-advanced-list\">스토리지 · Shell · 환경 변수 · 데이터 볼륨</span>",
            ">MicroVM 만들기</button>",
        ] {
            assert!(html.contains(text), "{text}: {html}");
        }
        crate::i18n::set("en");
    }

    #[test]
    fn the_first_installed_image_is_the_default_the_select_shows() {
        let cache = BTreeMap::from([(
            "/api/images".to_string(),
            json!([{"alias": "old", "installed": false}, {"alias": "alpine", "installed": true}]),
        )]);
        let form = vm_create(&cache);
        let template = form.fields.iter().find(|f| f.key == "template").unwrap();
        assert_eq!(template.value, "alpine");
        assert!(
            vm_create(&Default::default())
                .fields
                .iter()
                .all(|f| f.key != "template" || f.value.is_empty())
        );
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
