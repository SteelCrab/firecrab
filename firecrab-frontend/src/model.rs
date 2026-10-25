use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const VIEWS: &[(&str, &str, &str, &str)] = &[
    ("vms", "compute", "MicroVM", "MicroVM"),
    ("pools", "compute", "Pools", "풀"),
    ("schedules", "compute", "Schedules", "스케줄러"),
    ("services", "compute", "MicroService", "MicroService"),
    ("networks", "network", "Networks", "네트워크"),
    ("security-groups", "network", "Security groups", "보안그룹"),
    ("pipe", "network", "Pipe", "Pipe"),
    ("secrets", "data", "Secrets", "Secrets"),
    ("storages", "data", "Storage", "스토리지"),
    ("images", "data", "Images", "이미지"),
    ("kernels", "data", "Kernels", "커널"),
    ("shells", "system", "Shells", "Shell"),
    ("host", "system", "Host", "호스트"),
    ("ai-sandbox", "ai", "AI Sandbox", "AI Sandbox"),
];

#[derive(Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Vm {
    pub id: String,
    pub name: String,
    pub state: String,
    pub template: String,
    pub cpu: u32,
    pub ram: u32,
    pub disk_gb: u32,
    pub ipv4: Option<String>,
    pub cpu_usage_percent: Option<f64>,
    pub memory_used_percent: Option<f64>,
    #[serde(default)]
    pub usage_history: Vec<Value>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}

pub fn lowercase(value: &str) -> String {
    if value.is_ascii() {
        return value.to_ascii_lowercase();
    }
    #[cfg(target_arch = "wasm32")]
    {
        js_sys::JsString::from(value).to_lower_case().into()
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        value.to_lowercase()
    }
}

pub fn uppercase(value: &str) -> String {
    if value.is_ascii() {
        return value.to_ascii_uppercase();
    }
    #[cfg(target_arch = "wasm32")]
    {
        js_sys::JsString::from(value).to_upper_case().into()
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        value.to_uppercase()
    }
}

impl Vm {
    pub fn matches(&self, filter: &str, query: &str) -> bool {
        if filter != "all" && self.state != filter {
            return false;
        }
        let needle = lowercase(query.trim());
        needle.is_empty()
            || [
                &self.name,
                self.ipv4.as_deref().unwrap_or(""),
                &self.template,
            ]
            .iter()
            .any(|text| lowercase(text).contains(&needle))
    }
    pub fn editable(&self) -> bool {
        self.extra.get("purpose").and_then(Value::as_str) != Some("pool")
            && matches!(self.state.as_str(), "created" | "stopped" | "error")
    }
}

pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

pub fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or("")
}
pub fn list(value: &Value) -> &[Value] {
    value.as_array().map(Vec::as_slice).unwrap_or(&[])
}
pub fn pairs(input: &str) -> Result<Value, String> {
    let mut object = serde_json::Map::new();
    for line in input.lines().filter(|line| !line.trim().is_empty()) {
        let (key, value) = line
            .split_once('=')
            .ok_or("Use NAME=value, one per line.")?;
        if key.trim().is_empty() {
            return Err("A variable name is required.".into());
        }
        if object.insert(key.trim().into(), json!(value)).is_some() {
            return Err(format!("Duplicate variable: {}", key.trim()));
        }
    }
    Ok(Value::Object(object))
}
pub fn format_pairs(value: &Value) -> String {
    value
        .as_object()
        .map(|object| {
            object
                .iter()
                .map(|(key, value)| format!("{key}={}", value.as_str().unwrap_or("")))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}
pub fn ports(input: &str) -> Result<Value, String> {
    let mut entries = Vec::new();
    for line in input.lines().filter(|line| !line.trim().is_empty()) {
        let (host, tail) = line
            .trim()
            .split_once(':')
            .ok_or("Use HOST:GUEST/tcp or HOST:GUEST/udp, one per line.")?;
        let (guest, protocol) = tail.split_once('/').unwrap_or((tail, "tcp"));
        let host: u16 = host.parse().map_err(|_| "Host port must be 1–65535.")?;
        let guest: u16 = guest.parse().map_err(|_| "Guest port must be 1–65535.")?;
        if host == 0 || guest == 0 || !matches!(protocol, "tcp" | "udp") {
            return Err("Use ports 1–65535 and tcp or udp.".into());
        }
        entries.push(json!({ "hostPort": host, "guestPort": guest, "protocol": protocol }));
    }
    Ok(json!(entries))
}
pub fn format_ports(value: &Value) -> String {
    list(value)
        .iter()
        .map(|entry| {
            format!(
                "{}:{}/{}",
                entry["hostPort"],
                entry["guestPort"],
                text(entry, "protocol")
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn filter_matches_baseline_tabs_and_case_insensitive_search() {
        let vm = Vm {
            name: "My VM".into(),
            state: "running".into(),
            template: "Ubuntu".into(),
            ipv4: Some("10.0.0.8".into()),
            ..Vm::default()
        };
        assert!(vm.matches("all", "  UBUNTU  "));
        assert!(vm.matches("running", "0.0.8"));
        assert!(!vm.matches("stopped", "VM"));
        assert!(!vm.matches("all", "absent"));
    }
    #[test]
    fn values_cannot_inject_elements_or_attributes() {
        assert_eq!(
            escape("<img src=x onerror='x'>&\""),
            "&lt;img src=x onerror=&#39;x&#39;&gt;&amp;&quot;"
        );
    }
    #[test]
    fn variables_preserve_values_and_reject_duplicates() {
        assert_eq!(
            pairs("URL=https://example.com/?a=b\nEMPTY=").unwrap(),
            json!({"URL":"https://example.com/?a=b", "EMPTY":""})
        );
        assert!(pairs("A=1\nA=2").is_err());
        assert!(pairs("=secret").is_err());
    }
    #[test]
    fn port_rules_enforce_api_limits() {
        assert_eq!(ports("8080:80\n5353:53/udp").unwrap()[1]["protocol"], "udp");
        for invalid in ["0:80", "80:65536", "80:80/icmp", "80"] {
            assert!(ports(invalid).is_err());
        }
    }
}
