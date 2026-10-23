use super::{Field, Form};
use crate::model::{list, text};
use serde_json::{Value, json};

pub fn resource(kind: &str, data: &Value, id: &str) -> Option<Form> {
    let edit = !id.is_empty();
    let value = |key| text(data, key).to_string();
    let number = |key, default| data[key].as_u64().unwrap_or(default);
    let method = if edit { "PUT" } else { "POST" };
    let suffix = if edit {
        format!("/{}", crate::api::encode(id))
    } else {
        String::new()
    };
    Some(match kind {
        "pools" => {
            let mut fields = if edit {
                Vec::new()
            } else {
                vec![
                    Field::new("name", "Name", "text", ""),
                    Field::new("template", "Image", "select", ""),
                    Field::new("cpu", "CPU", "number", 1),
                    Field::new("ram", "Memory (MiB)", "number", 512),
                    Field::new("diskGb", "Disk (GB)", "number", 2),
                    Field::new("egressPolicy", "Internet access", "select", "internet")
                        .choices(&[("internet", "Internet"), ("isolated", "Isolated")]),
                    Field::new("microNetworkId", "Network", "select", ""),
                    Field::new("storageRoot", "Storage", "nullable", "").optional(),
                ]
            };
            fields.extend([
                Field::new("minReady", "Keep ready", "number", number("minReady", 2)),
                Field::new("maxSize", "Maximum VMs", "number", number("maxSize", 4)),
                Field::new(
                    "leaseMinutes",
                    "Session duration (minutes)",
                    "number",
                    number("leaseTtlSeconds", 600) / 60,
                ),
            ]);
            Form::new(
                if edit {
                    "Pool settings"
                } else {
                    "Create a pool"
                },
                format!("/api/pools{suffix}"),
                if edit { "PATCH" } else { "POST" },
                fields,
                "pools",
            )
        }
        "security-groups" => Form::new(
            "New security group",
            "/api/security-groups".into(),
            "POST",
            vec![
                Field::new("name", "Name", "text", ""),
                Field::new("microNetworkId", "MicroNetwork", "select", ""),
            ],
            "security-groups",
        ),
        "security-rules" => Form::new(
            "Security group rules",
            format!("/api/security-groups{suffix}/rules"),
            "PUT",
            vec![
                Field::new(
                    "rules",
                    "Rules",
                    "rules",
                    list(&data["rules"])
                        .iter()
                        .map(|rule| {
                            format!(
                                "{} | {} | {}-{} | {}",
                                text(rule, "direction"),
                                text(rule, "protocol"),
                                rule["portFrom"],
                                rule["portTo"],
                                text(rule, "peerCidr")
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n"),
                )
                .optional(),
            ],
            "security-groups",
        ),
        "schedules" => {
            let timing = &data["timing"];
            let draft = timing_draft(timing);
            let mut form = Form::new(
                if edit {
                    "Edit schedule"
                } else {
                    "New schedule"
                },
                format!("/api/schedules{suffix}"),
                method,
                vec![
                    Field::new("name", "Name (optional)", "text", value("name")).optional(),
                    Field::new("vmId", "MicroVM", "select", value("vmId")),
                    Field::new(
                        "action",
                        "Action",
                        "select",
                        if value("action").is_empty() {
                            "start".into()
                        } else {
                            value("action")
                        },
                    )
                    .choices(&[("start", "Start"), ("stop", "Stop")]),
                    Field::new(
                        "repeat",
                        "Repeat",
                        "select",
                        draft["repeat"].as_str().unwrap_or("weekdays"),
                    )
                    .choices(&[
                        ("weekdays", "Weekdays"),
                        ("daily", "Daily"),
                        ("weekly", "Weekly"),
                        ("once", "Once"),
                        ("cron", "Custom cron"),
                    ]),
                    Field::new(
                        "time",
                        "Time",
                        "time",
                        draft["time"].as_str().unwrap_or("09:00"),
                    )
                    .optional(),
                    Field::new("date", "Date", "date", draft["date"].as_str().unwrap_or(""))
                        .optional(),
                    Field::new(
                        "days",
                        "Days",
                        "days",
                        draft["days"].as_str().unwrap_or("Mon"),
                    )
                    .optional(),
                    Field::new(
                        "expression",
                        "Cron expression",
                        "text",
                        text(timing, "expression"),
                    )
                    .optional(),
                    Field::new(
                        "timeZone",
                        "Time zone",
                        "text",
                        if text(timing, "timeZone").is_empty() {
                            local_zone()
                        } else {
                            text(timing, "timeZone").into()
                        },
                    )
                    .optional(),
                    Field::new(
                        "enabled",
                        "Enabled",
                        "bool",
                        data["enabled"].as_bool().unwrap_or(true),
                    )
                    .choices(&[("true", "Yes"), ("false", "No")]),
                ],
                "schedules",
            );
            form.base = json!({"original":timing,"draft":draft});
            form
        }
        "secrets" => {
            let mut form = Form::new(
                if edit { "Edit secret" } else { "New secret" },
                format!("/api/secrets{suffix}"),
                method,
                vec![
                    Field::new("name", "Secret name", "text", value("name")),
                    Field::new("description", "Description", "text", value("description"))
                        .optional(),
                    if edit {
                        Field::new("value", "Replacement value (optional)", "password", "")
                            .optional()
                    } else {
                        Field::new("value", "Secret value", "password", "")
                    },
                ],
                "secrets",
            );
            if edit {
                form.base = json!({"version":data["version"]});
            }
            form
        }
        "secret-connection" => {
            let settings = &data["settings"];
            let mut fields = vec![
                Field::new("name", "Connection name", "text", value("name")),
                Field::new(
                    "provider",
                    "Provider",
                    "select",
                    if edit {
                        value("provider")
                    } else {
                        "firecrab".into()
                    },
                )
                .choices(&[
                    ("firecrab", "Firecrab · PipeSecrets"),
                    ("infisical", "Infisical"),
                    ("awsSecretsManager", "AWS Secrets Manager"),
                ]),
            ];
            for (key, label, default) in [
                ("baseUrl", "Base URL", ""),
                ("projectId", "Project ID", ""),
                ("environment", "Environment", "prod"),
                ("secretPath", "Secret path", "/"),
                ("region", "Region", "us-east-1"),
                ("prefix", "Name prefix (optional)", ""),
            ] {
                fields.push(
                    Field::new(
                        key,
                        label,
                        "text",
                        if text(settings, key).is_empty() {
                            default
                        } else {
                            text(settings, key)
                        },
                    )
                    .optional(),
                );
            }
            for (key, label) in [
                ("token", "Access token"),
                ("accessKeyId", "AWS access key ID"),
                ("secretAccessKey", "AWS secret access key"),
                ("sessionToken", "AWS session token (optional)"),
            ] {
                fields.push(Field::new(key, label, "password", "").optional());
            }
            let mut form = Form::new(
                if edit {
                    "Edit connection"
                } else {
                    "New connection"
                },
                format!("/api/secrets/connections{suffix}"),
                method,
                fields,
                "secrets",
            );
            if edit {
                form.base = json!({"version":data["version"]});
            }
            form
        }
        _ => return None,
    })
}

pub fn payload(title: &str, mut body: Value) -> Result<Value, String> {
    match title {
        "Create a pool" | "Pool settings" => {
            let minutes = body["leaseMinutes"].as_u64().unwrap_or_default();
            if minutes == 0
                || body["minReady"].as_u64().unwrap_or_default()
                    > body["maxSize"].as_u64().unwrap_or_default()
            {
                return Err("Check the pool size and session duration.".into());
            }
            body["leaseTtlSeconds"] = json!(
                minutes
                    .checked_mul(60)
                    .ok_or("Session duration is too large.")?
            );
            body.as_object_mut().unwrap().remove("leaseMinutes");
            Ok(body)
        }
        "Security group rules" => {
            let mut rules = Vec::new();
            for line in text(&body, "rules")
                .lines()
                .filter(|line| !line.trim().is_empty())
            {
                let parts: Vec<_> = line.split('|').map(str::trim).collect();
                if parts.len() != 4
                    || !["ingress", "egress"].contains(&parts[0])
                    || !["tcp", "udp"].contains(&parts[1])
                {
                    return Err(
                        "Use ingress or egress, tcp or udp, a port range, and an IPv4 CIDR.".into(),
                    );
                }
                let (from, to) = parts[2].split_once('-').unwrap_or((parts[2], parts[2]));
                let from = from
                    .trim()
                    .parse::<u16>()
                    .map_err(|_| "Invalid first port.")?;
                let to = to.trim().parse::<u16>().map_err(|_| "Invalid last port.")?;
                if from == 0 || from > to {
                    return Err("Ports must be between 1 and 65535 in ascending order.".into());
                }
                let (ip, prefix) = parts[3].split_once('/').ok_or("Enter an IPv4 CIDR.")?;
                if ip.parse::<std::net::Ipv4Addr>().is_err()
                    || prefix.parse::<u8>().ok().is_none_or(|n| n > 32)
                {
                    return Err("Enter an IPv4 CIDR.".into());
                }
                rules.push(json!({"direction":parts[0],"protocol":parts[1],"portFrom":from,"portTo":to,"peerCidr":parts[3]}));
            }
            Ok(json!({"rules":rules}))
        }
        "New schedule" | "Edit schedule" => {
            let timing = build_timing(&body)?;
            let name = if text(&body, "name").trim().is_empty() {
                let vm = body["vmNames"][text(&body, "vmId")]
                    .as_str()
                    .unwrap_or("MicroVM");
                let mut name = format!(
                    "{vm} · {}",
                    crate::i18n::t(if body["action"] == "stop" {
                        "Stop"
                    } else {
                        "Start"
                    })
                );
                let mut end = name.len().min(64);
                while !name.is_char_boundary(end) {
                    end -= 1;
                }
                name.truncate(end);
                name
            } else {
                text(&body, "name").trim().into()
            };
            Ok(
                json!({"name":name,"vmId":body["vmId"],"action":body["action"],"enabled":body["enabled"],"timing":timing}),
            )
        }
        "New secret" | "Edit secret" => {
            if title == "Edit secret" && text(&body, "value").is_empty() {
                body.as_object_mut().unwrap().remove("value");
            }
            Ok(body)
        }
        "New connection" | "Edit connection" => {
            let mut settings = json!({});
            for key in [
                "baseUrl",
                "projectId",
                "environment",
                "secretPath",
                "region",
                "prefix",
            ] {
                settings[key] = body[key].clone();
            }
            let mut credentials = json!({});
            for key in ["token", "accessKeyId", "secretAccessKey", "sessionToken"] {
                if !text(&body, key).is_empty() {
                    credentials[key] = body[key].clone();
                }
            }
            let mut result = json!({"name":body["name"],"provider":body["provider"],"settings":settings,"credentials":credentials});
            if !body["version"].is_null() {
                result["version"] = body["version"].clone();
            }
            Ok(result)
        }
        _ => Ok(body),
    }
}

pub fn timing_draft(timing: &Value) -> Value {
    let mut result = json!({"repeat":"weekdays","time":"09:00","date":"","days":"Mon"});
    if timing["kind"] == "once" {
        #[cfg(target_arch = "wasm32")]
        {
            let date = js_sys::Date::new(&wasm_bindgen::JsValue::from_str(text(timing, "at")));
            result["repeat"] = json!("once");
            result["date"] = json!(format!(
                "{:04}-{:02}-{:02}",
                date.get_full_year(),
                date.get_month() + 1,
                date.get_date()
            ));
            result["time"] = json!(format!("{:02}:{:02}", date.get_hours(), date.get_minutes()));
        }
        return result;
    }
    let expression = text(timing, "expression");
    if expression.is_empty() {
        return result;
    }
    result["repeat"] = json!("cron");
    let parts: Vec<_> = expression.split_whitespace().collect();
    if parts.len() != 5 || parts[2] != "*" || parts[3] != "*" {
        return result;
    }
    let (Ok(minute), Ok(hour)) = (parts[0].parse::<u8>(), parts[1].parse::<u8>()) else {
        return result;
    };
    if minute > 59 || hour > 23 {
        return result;
    }
    let days = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
    let selected: Option<Vec<_>> = parts[4]
        .split(',')
        .map(|part| {
            days.iter()
                .find(|day| day.eq_ignore_ascii_case(part))
                .copied()
        })
        .collect();
    let repeat = if parts[4] == "*" {
        "daily"
    } else if parts[4].eq_ignore_ascii_case("Mon-Fri") {
        "weekdays"
    } else if let Some(selected) = selected {
        result["days"] = json!(selected.join(","));
        "weekly"
    } else {
        return result;
    };
    result["repeat"] = json!(repeat);
    result["time"] = json!(format!("{hour:02}:{minute:02}"));
    result
}
fn build_timing(body: &Value) -> Result<Value, String> {
    let repeat = text(body, "repeat");
    if repeat == "cron" {
        return Ok(
            json!({"kind":"cron","expression":text(body,"expression").trim(),"timeZone":text(body,"timeZone").trim()}),
        );
    }
    let (hour, minute) = text(body, "time")
        .split_once(':')
        .ok_or("Check the time.")?;
    let hour = hour.parse::<u8>().map_err(|_| "Check the time.")?;
    let minute = minute.parse::<u8>().map_err(|_| "Check the time.")?;
    if hour > 23 || minute > 59 {
        return Err("Check the time.".into());
    }
    if repeat == "once" {
        if body["draft"]["date"] == body["date"]
            && body["draft"]["time"] == body["time"]
            && body["original"]["kind"] == "once"
        {
            return Ok(body["original"].clone());
        }
        #[cfg(target_arch = "wasm32")]
        {
            let date = js_sys::Date::new(&wasm_bindgen::JsValue::from_str(&format!(
                "{}T{hour:02}:{minute:02}:00",
                text(body, "date")
            )));
            if !date.get_time().is_finite()
                || format!(
                    "{:04}-{:02}-{:02}",
                    date.get_full_year(),
                    date.get_month() + 1,
                    date.get_date()
                ) != text(body, "date")
                || date.get_hours() != u32::from(hour)
                || date.get_minutes() != u32::from(minute)
            {
                return Err("Check the date and time. This local time may not exist during a daylight saving change.".into());
            }
            return Ok(
                json!({"kind":"once","at":date.to_iso_string().as_string().unwrap_or_default()}),
            );
        }
        #[cfg(not(target_arch = "wasm32"))]
        return Err("One-time schedules require the browser time zone.".into());
    }
    let selected: Vec<_> = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"]
        .into_iter()
        .filter(|day| text(body, "days").split(',').any(|chosen| chosen == *day))
        .collect();
    if repeat == "weekly" && selected.is_empty() {
        return Err("Choose at least one day.".into());
    }
    let days = if repeat == "weekdays" {
        "Mon-Fri".into()
    } else if repeat == "daily" {
        "*".into()
    } else {
        selected.join(",")
    };
    Ok(
        json!({"kind":"cron","expression":format!("{minute} {hour} * * {days}"),"timeZone":text(body,"timeZone").trim()}),
    )
}
fn local_zone() -> String {
    #[cfg(target_arch = "wasm32")]
    {
        use wasm_bindgen::{JsCast, JsValue};
        let result = (|| -> Result<JsValue, JsValue> {
            let intl = js_sys::Reflect::get(&js_sys::global(), &"Intl".into())?;
            let constructor = js_sys::Reflect::get(&intl, &"DateTimeFormat".into())?
                .dyn_into::<js_sys::Function>()?;
            let formatter = js_sys::Reflect::construct(&constructor, &js_sys::Array::new())?;
            let options = js_sys::Reflect::get(&formatter, &"resolvedOptions".into())?
                .dyn_into::<js_sys::Function>()?
                .call0(&formatter)?;
            js_sys::Reflect::get(&options, &"timeZone".into())
        })();
        result
            .ok()
            .and_then(|value| value.as_string())
            .unwrap_or("UTC".into())
    }
    #[cfg(not(target_arch = "wasm32"))]
    "UTC".into()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn schedule_presets_keep_the_api_contract_and_reject_invalid_days() {
        let mut body = json!({"repeat":"weekdays","time":"18:30","timeZone":"Asia/Seoul","days":"Fri,Mon,Wed"});
        assert_eq!(
            build_timing(&body).unwrap(),
            json!({"kind":"cron","expression":"30 18 * * Mon-Fri","timeZone":"Asia/Seoul"})
        );
        body["repeat"] = json!("weekly");
        assert_eq!(
            build_timing(&body).unwrap()["expression"],
            "30 18 * * Mon,Wed,Fri"
        );
        body["days"] = json!("");
        assert!(build_timing(&body).is_err());
        body["time"] = json!("25:00");
        assert!(build_timing(&body).is_err());
    }
    #[test]
    fn schedule_edits_keep_imported_cron_and_exact_one_time_instants() {
        for expression in [
            "0 9 * * Mon-Fri",
            "15 23 * * *",
            "30 8 * * Mon,Wed,Fri",
            "*/15 9-18 * * Mon-Fri",
            "0 9 1 * *",
            "0 9 * * 1-5",
        ] {
            let original =
                json!({"kind":"cron","expression":expression,"timeZone":"America/New_York"});
            let mut draft = timing_draft(&original);
            draft["expression"] = json!(expression);
            draft["timeZone"] = json!("America/New_York");
            assert_eq!(build_timing(&draft).unwrap(), original);
        }
        let body = json!({"repeat":"once","date":"2030-01-01","time":"18:00","original":{"kind":"once","at":"2030-01-01T09:00:00.123Z"},"draft":{"date":"2030-01-01","time":"18:00"}});
        assert_eq!(
            build_timing(&body).unwrap()["at"],
            "2030-01-01T09:00:00.123Z"
        );
    }
    #[test]
    fn security_group_rules_validate_before_sending_and_allow_an_empty_policy() {
        assert!(
            payload(
                "Security group rules",
                json!({"rules":"ingress | tcp | 90-22 | 0.0.0.0/0"})
            )
            .is_err()
        );
        assert!(
            payload(
                "Security group rules",
                json!({"rules":"ingress | tcp | 22 | ::/0"})
            )
            .is_err()
        );
        assert_eq!(
            payload("Security group rules", json!({"rules":""})).unwrap(),
            json!({"rules":[]})
        );
    }
}
