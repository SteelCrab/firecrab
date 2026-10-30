use crate::{
    App, Shared, api,
    model::{escape, list, text},
    render::{Icon, button, empty_state, icon_link, request, status_dot},
};
use serde_json::{Value, json};
pub fn timing(data: &Value) -> String {
    if data["kind"] == "once" {
        return text(data, "at").into();
    }
    let expression = text(data, "expression");
    let parts: Vec<_> = expression.split_whitespace().collect();
    if parts.len() == 5
        && parts[1].parse::<u8>().is_ok()
        && parts[0].parse::<u8>().is_ok()
        && parts[2] == "*"
        && parts[3] == "*"
    {
        let days = match parts[4] {
            "Mon-Fri" => crate::i18n::t("Weekdays"),
            "*" => crate::i18n::t("Daily"),
            d => d,
        };
        format!(
            "{} {:02}:{:02}",
            days.replace(',', "·"),
            parts[1].parse::<u8>().unwrap_or(0),
            parts[0].parse::<u8>().unwrap_or(0)
        )
    } else {
        expression.into()
    }
}
pub fn consumed(row: &Value) -> bool {
    row["timing"]["kind"] == "once" && !row["lastRun"].is_null()
}
/// "MM/DD HH:mm" in the browser's time zone.
fn when_label(ms: f64) -> String {
    let date = js_sys::Date::new(&wasm_bindgen::JsValue::from_f64(ms));
    format!(
        "{:02}/{:02} {:02}:{:02}",
        date.get_month() + 1,
        date.get_date(),
        date.get_hours(),
        date.get_minutes()
    )
}
/// The name of a schedule with the MicroVM it acts on, as a link to that VM, under it.
fn schedule_name(name: &str, vm: &str, vm_id: &str) -> String {
    format!(
        "<div class=\"cell-name\"><b class=\"\">{}</b><span class=\"cell-sub\" data-tone=\"\"><a href=\"#/vms?vm={}\">{}</a></span></div>",
        escape(name),
        api::encode(vm_id),
        escape(vm)
    )
}
/// The tone and label of the last run of a schedule.
fn last_result(row: &Value, ko: bool) -> String {
    if row["lastRun"].is_null() {
        return "—".into();
    }
    match text(&row["lastRun"], "status") {
        "succeeded" | "success" | "ok" => status_dot("ok", if ko { "성공" } else { "Succeeded" }),
        "failed" | "error" => status_dot("error", if ko { "실패" } else { "Failed" }),
        other => status_dot("busy", other),
    }
}
pub fn content(app: &App) -> String {
    let ko = app.locale == "ko";
    let vms: Vec<_> = app
        .vms
        .iter()
        .filter(|v| {
            !matches!(
                v.extra.get("purpose").and_then(Value::as_str),
                Some("pool" | "queue")
            )
        })
        .collect();
    let filter = super::query("vm")
        .filter(|id| vms.iter().any(|vm| &vm.id == id))
        .unwrap_or_default();
    let mut out = String::new();
    if !vms.is_empty() {
        out.push_str(&format!(
            "<div class=\"schedule-toolbar\"><label for=\"schedule-filter\">{}</label><select id=\"schedule-filter\"><option value=\"\">{}</option>",
            if ko { "VM 필터" } else { "Filter by VM" },
            if ko { "모든 MicroVM" } else { "All MicroVMs" }
        ));
        for vm in vms {
            out.push_str(&format!(
                "<option value=\"{}\" {}>{}</option>",
                escape(&vm.id),
                if vm.id == filter { "selected" } else { "" },
                escape(&vm.name)
            ));
        }
        out.push_str("</select></div>");
    }
    let rows: Vec<&Value> = list(&app.resource)
        .iter()
        .filter(|row| filter.is_empty() || row["vmId"] == filter)
        .collect();
    if rows.is_empty() {
        out.push_str(&empty_state(
            "schedules",
            if ko {
                "아직 스케줄이 없습니다"
            } else {
                "No schedules yet"
            },
            if ko {
                "MicroVM을 정한 시간에 켜고 끕니다."
            } else {
                "Start and stop MicroVMs at set times."
            },
            &button("New schedule", "create", "schedules"),
        ));
        return out;
    }
    out.push_str(&format!(
        "<div class=\"table-scroll\"><table class=\"wasm-table schedule-table\" aria-label=\"{}\"><thead><tr><th>{}</th><th class=\"col-secondary\">{}</th><th class=\"col-secondary\">{}</th><th class=\"col-secondary\">{}</th><th class=\"col-secondary\">{}</th><th class=\"row-actions\"><span class=\"sr-only\">{}</span></th></tr></thead><tbody>",
        if ko { "스케줄" } else { "Schedules" },
        if ko { "이름" } else { "Name" },
        if ko { "일정" } else { "When" },
        if ko { "동작" } else { "Action" },
        if ko { "다음 실행" } else { "Next run" },
        if ko { "최근 결과" } else { "Last result" },
        crate::i18n::t("Actions")
    ));
    for row in rows {
        let id = text(row, "id");
        let vm = app
            .vms
            .iter()
            .find(|vm| vm.id == text(row, "vmId"))
            .map(|vm| vm.name.as_str())
            .unwrap_or("Missing VM");
        let enabled = row["enabled"] == true;
        let selected = app.detail.as_ref().is_some_and(|d| d["id"] == id);
        let next = row["nextRunAtMs"]
            .as_f64()
            .filter(|_| enabled)
            .map_or("—".to_string(), when_label);
        let name = text(row, "name");
        let toggle = if enabled {
            if ko {
                "일시 중지"
            } else {
                "Pause schedule"
            }
        } else if ko {
            "켜기"
        } else {
            "Enable schedule"
        };
        out.push_str(&format!(
            "<tr data-id=\"{}\" aria-selected=\"{selected}\"><td>{}</td><td class=\"col-secondary\">{} · {}</td><td class=\"col-secondary\">{}</td><td class=\"col-secondary\">{next}</td><td class=\"col-secondary\">{}</td><td class=\"row-actions\"><button type=\"button\" class=\"icon-btn\" role=\"switch\" aria-checked=\"{enabled}\" aria-label=\"{} {}\" title=\"{toggle}\" data-action=\"schedule-enable\" data-id=\"{}\"{}>{}</button></td></tr>",
            escape(id),
            schedule_name(name, vm, text(row, "vmId")),
            escape(&timing(&row["timing"])),
            escape(text(&row["timing"], "timeZone")),
            escape(text(row, "action")),
            last_result(row, ko),
            if ko { "켜기" } else { "Enable" },
            escape(name),
            escape(id),
            if consumed(row) { " disabled" } else { "" },
            if enabled { Icon::Pause.svg() } else { Icon::Start.svg() }
        ));
    }
    out.push_str("</tbody></table></div>");
    out
}
pub fn actions(data: &Value) -> String {
    button("Edit schedule", "edit", "schedules")
        + &request(
            &format!("Delete {}", text(data, "name")),
            "DELETE",
            &format!("/api/schedules/{}", api::encode(text(data, "id"))),
            Value::Null,
        )
        + &format!(
            "<a class=\"btn\" href=\"#/vms?vm={}\">Open VM</a>",
            api::encode(text(data, "vmId"))
        )
}
pub fn detail(app: &App, data: &Value) -> String {
    let ko = app.locale == "ko";
    let vm = app
        .vms
        .iter()
        .find(|vm| vm.id == text(data, "vmId"))
        .map_or("—", |vm| vm.name.as_str());
    let schedule = crate::details::section(
        "Schedule",
        "",
        &crate::render::properties(&json!({
            "MicroVM": vm,
            "When": timing(&data["timing"]),
            "Time zone": text(&data["timing"], "timeZone"),
            "Next run": data["nextRunAtMs"].as_f64().map_or("—".to_string(), when_label),
        })),
    );
    let mut runs = String::new();
    for run in list(&data["runs"]) {
        let failed = matches!(text(run, "status"), "failed" | "error");
        runs.push_str(&format!(
            "<li><span class=\"detail-list-main\">{}</span>{}</li>",
            escape(text(run, "error")),
            status_dot(
                if failed { "error" } else { "ok" },
                if failed {
                    if ko { "실패" } else { "Failed" }
                } else if ko {
                    "성공"
                } else {
                    "Succeeded"
                }
            )
        ));
    }
    if runs.is_empty() && !data["lastRun"].is_null() {
        runs.push_str(&format!(
            "<li><span class=\"detail-list-main\">{}</span>{}</li>",
            escape(text(&data["lastRun"], "error")),
            last_result(data, ko)
        ));
    }
    let runs = if runs.is_empty() {
        "<p class=\"detail-note\">No runs yet.</p>".to_string()
    } else {
        format!("<ul class=\"detail-list\">{runs}</ul>")
    };
    schedule + &crate::details::section("Recent runs", "", &runs)
}
pub fn vm_schedules(app: &App, vm: &Value) -> String {
    if ["pool", "queue"].contains(&text(vm, "purpose")) {
        return String::new();
    }
    let mut rows = String::new();
    for row in list(app.cache.get("/api/schedules").unwrap_or(&Value::Null))
        .iter()
        .filter(|r| r["vmId"] == vm["id"])
    {
        rows.push_str(&format!(
            "<li><a class=\"detail-list-main\" href=\"#/schedules?vm={}&schedule={}\">{}</a><span class=\"detail-list-sub\">{}</span></li>",
            api::encode(text(vm, "id")),
            api::encode(text(row, "id")),
            escape(&timing(&row["timing"])),
            escape(text(row, "action"))
        ));
    }
    let add = icon_link(
        "Add schedule",
        &format!("#/schedules?vm={}&new=1", api::encode(text(vm, "id"))),
        false,
        Icon::Add,
    );
    let body = if rows.is_empty() {
        String::new()
    } else {
        format!("<ul class=\"detail-list\">{rows}</ul>")
    };
    crate::details::section(
        if app.locale == "ko" {
            "예약"
        } else {
            "Schedules"
        },
        &add,
        &body,
    )
}
pub fn dispatch(app: &Shared, action: &str, id: &str) -> bool {
    if action != "schedule-enable" {
        return false;
    }
    let row = list(&app.borrow().resource)
        .iter()
        .find(|r| r["id"] == id)
        .cloned();
    if let Some(row) = row
        && !consumed(&row)
    {
        crate::mutate(
            app.clone(),
            "PUT".into(),
            format!("/api/schedules/{}/enabled", api::encode(id)),
            Some(json!({"enabled":row["enabled"]!=true})),
            None,
        );
    }
    true
}
