use crate::{
    App, Shared, api,
    model::{escape, list, text},
    render::{button, request},
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
pub fn content(app: &App) -> String {
    let filter = super::query("vm").unwrap_or_default();
    let mut out = String::from(
        "<label for=\"schedule-filter\">Filter by VM</label><select id=\"schedule-filter\"><option value=\"\">All MicroVMs</option>",
    );
    for vm in app.vms.iter().filter(|v| {
        !matches!(
            v.extra.get("purpose").and_then(Value::as_str),
            Some("pool" | "queue")
        )
    }) {
        out.push_str(&format!(
            "<option value=\"{}\" {}>{}</option>",
            escape(&vm.id),
            if vm.id == filter { "selected" } else { "" },
            escape(&vm.name)
        ));
    }
    out.push_str("</select><ul class=\"schedule-list\">");
    let mut count = 0;
    for row in list(&app.resource)
        .iter()
        .filter(|row| filter.is_empty() || row["vmId"] == filter)
    {
        count += 1;
        let id = text(row, "id");
        let vm = app
            .vms
            .iter()
            .find(|vm| vm.id == text(row, "vmId"))
            .map(|vm| vm.name.as_str())
            .unwrap_or("Missing VM");
        let enabled = row["enabled"] == true;
        out.push_str(&format!("<li class=\"wasm-card\" aria-label=\"{}\"><button class=\"schedule-row-main btn\" data-action=\"detail\" data-id=\"{}\" aria-label=\"{}\">{}</button><p>{} · {} · {}</p><a class=\"btn\" href=\"#/vms?vm={}\">{}</a><button class=\"btn\" role=\"switch\" aria-label=\"Enable {}\" aria-checked=\"{enabled}\" data-action=\"schedule-enable\" data-id=\"{}\" {}>{}</button></li>",escape(text(row,"name")),escape(id),escape(&if app.locale=="ko" {format!("{} 예약 보기",text(row,"name"))}else{format!("View schedule {}",text(row,"name"))}),escape(text(row,"name")),escape(&timing(&row["timing"])),escape(text(&row["timing"],"timeZone")),escape(text(row,"action")),api::encode(text(row,"vmId")),escape(vm),escape(text(row,"name")),escape(id),if consumed(row){"disabled"}else{""},if enabled{"Pause schedule"}else{"Enable schedule"}));
    }
    out.push_str("</ul>");
    if count == 0 {
        out.push_str(&button("Create your first schedule", "create", "schedules"));
    }
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
pub fn detail(_app: &App, data: &Value) -> String {
    let mut out = format!(
        "<p>{} · {}</p><p>Next run: {}</p>",
        escape(&timing(&data["timing"])),
        escape(text(&data["timing"], "timeZone")),
        data["nextRunAtMs"]
    );
    for run in list(&data["runs"]) {
        out.push_str(&format!(
            "<article><p>{}</p><p>{}</p></article>",
            escape(text(run, "status")),
            escape(text(run, "error"))
        ));
    }
    if list(&data["runs"]).is_empty() && !data["lastRun"].is_null() {
        out.push_str(&format!(
            "<p>{}</p>",
            escape(text(&data["lastRun"], "error"))
        ));
    }
    out
}
pub fn vm_schedules(app: &App, vm: &Value) -> String {
    if ["pool", "queue"].contains(&text(vm, "purpose")) {
        return String::new();
    }
    let mut out = format!(
        "<section role=\"region\" aria-label=\"{}\"><h3>{}</h3>",
        if app.locale == "ko" {
            "예약"
        } else {
            "Schedules"
        },
        if app.locale == "ko" {
            "예약"
        } else {
            "Schedules"
        }
    );
    for row in list(app.cache.get("/api/schedules").unwrap_or(&Value::Null))
        .iter()
        .filter(|r| r["vmId"] == vm["id"])
    {
        out.push_str(&format!(
            "<a class=\"btn\" href=\"#/schedules?vm={}&schedule={}\"><span>{}</span> {}</a>",
            api::encode(text(vm, "id")),
            api::encode(text(row, "id")),
            escape(&timing(&row["timing"])),
            escape(text(row, "action"))
        ));
    }
    out.push_str("</section>");
    out
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
