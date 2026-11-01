//! Resource sections stay within their exact Stack or Terraform state owner.
use super::{App, Value, escape, list, text, tr};
use crate::render::navigation_icon;
use serde_json::json;
use std::collections::BTreeMap;

fn kind_label<'a>(app: &App, kind: &'a str) -> (&'static str, &'a str) {
    match kind {
        "kernel" => ("kernels", tr(app, "Kernels", "커널")),
        "image" => ("images", tr(app, "Images", "이미지")),
        "storage_root" => ("storages", tr(app, "Storage roots", "스토리지 루트")),
        "volume" => ("volumes", tr(app, "Volumes", "볼륨")),
        "volume_attachment" => ("volumes", tr(app, "Volume attachments", "볼륨 연결")),
        "network" => ("networks", tr(app, "Networks", "네트워크")),
        "security_group" => ("security-groups", tr(app, "Security groups", "보안그룹")),
        "shell" => ("shells", tr(app, "Shells", "셸")),
        "vm" => ("vms", "MicroVMs"),
        "pool" => ("pools", tr(app, "Pools", "풀")),
        "schedule" => ("schedules", tr(app, "Schedules", "스케줄")),
        "secret" => ("secrets", tr(app, "Secrets", "시크릿")),
        "secret_connection" => ("secrets", tr(app, "Secret connections", "시크릿 연결")),
        "auth" => ("security", tr(app, "Auth", "인증")),
        "registry" => ("images", tr(app, "Registries", "레지스트리")),
        "service" => ("services", "MicroServices"),
        "service_auto_deploy" => ("services", tr(app, "Auto deploy", "자동 배포")),
        "service_otlp" => ("services", tr(app, "Telemetry", "관측 설정")),
        "service_deploy_token" => ("secrets", tr(app, "Deploy tokens", "배포 토큰")),
        "ingress" => ("networks", tr(app, "Ingress", "인그레스")),
        "quick_tunnel" => ("pipe", tr(app, "Quick tunnels", "빠른 터널")),
        "named_tunnel" => ("pipe", tr(app, "Named tunnels", "이름 있는 터널")),
        "firecrab_tunnel" => ("pipe", "Firecrab Tunnels"),
        "database_connection" => (
            "storages",
            tr(app, "Database connections", "데이터베이스 연결"),
        ),
        "database_binding" => (
            "storages",
            tr(app, "Database bindings", "데이터베이스 바인딩"),
        ),
        "database_events" => (
            "storages",
            tr(app, "Database events", "데이터베이스 이벤트"),
        ),
        "database_rabbitmq" => ("queues", "RabbitMQ"),
        "database_migration" => (
            "storages",
            tr(app, "Database migrations", "데이터베이스 마이그레이션"),
        ),
        "storage_connection" => ("storages", tr(app, "Storage connections", "스토리지 연결")),
        "queue_engine" => ("queues", tr(app, "Queue engines", "큐 엔진")),
        "queue" => ("queues", tr(app, "Queues", "큐")),
        "ai_connection" => ("secrets", tr(app, "AI connections", "AI 연결")),
        "ai_model" => ("services", tr(app, "AI models", "AI 모델")),
        "sandbox_project" => ("services", tr(app, "Sandbox projects", "Sandbox 프로젝트")),
        "sandbox_access" => ("security", tr(app, "Sandbox access", "Sandbox 접근 권한")),
        "sandbox_environment" => ("pools", tr(app, "Sandbox environments", "Sandbox 환경")),
        "sandbox_tunnel" => ("pipe", tr(app, "Sandbox tunnels", "Sandbox 터널")),
        "cluster_host" => ("host", tr(app, "Cluster hosts", "클러스터 호스트")),
        "cluster_placement" => ("cluster", tr(app, "Cluster placements", "클러스터 배치")),
        "cluster_service" => ("cluster", tr(app, "Cluster services", "클러스터 서비스")),
        "stack_source" => ("stacks", tr(app, "Git sources", "Git 소스")),
        "management_settings" => ("host", tr(app, "Management settings", "관리 설정")),
        "host_configuration" => ("host", tr(app, "Host configuration", "호스트 설정")),
        _ => ("stacks", kind),
    }
}

pub(super) fn native_records(row: &Value, owners: &[Value]) -> Vec<Value> {
    let id = text(&row["stack"], "id");
    let mut records: Vec<_> = owners
        .iter()
        .filter(|record| {
            !id.is_empty()
                && text(&record["owner"], "manager") == "stack"
                && text(&record["owner"], "id") == id
        })
        .cloned()
        .collect();
    // Legacy bindings remain visible when the ownership endpoint is unavailable.
    for binding in list(&row["stack"]["bindings"]) {
        if owners.iter().any(|record| {
            text(&record["resource"], "kind") == text(binding, "kind")
                && text(&record["resource"], "id") == text(binding, "resourceId")
        }) {
            continue;
        }
        records.push(
            json!({"resource":{"kind":binding["kind"],"id":binding["resourceId"]},
            "owner":{"key":binding["key"]}}),
        );
    }
    records
}

pub(super) fn groups(app: &App, records: &[&Value]) -> String {
    let mut kinds: BTreeMap<&str, Vec<&Value>> = BTreeMap::new();
    for record in records {
        kinds
            .entry(text(&record["resource"], "kind"))
            .or_default()
            .push(record);
    }
    if kinds.is_empty() {
        return String::new();
    }
    let mut out = String::from("<div class=\"stack-resource-groups\">");
    for (kind, mut records) in kinds {
        records.sort_by_key(|record| text(&record["owner"], "key"));
        let (icon, label) = kind_label(app, kind);
        out.push_str(&format!(
            "<section class=\"stack-resource-group\" data-kind=\"{}\"><header class=\"stack-resource-heading\"><span class=\"stack-resource-icon\">{}</span><h3>{}</h3><span class=\"stack-resource-count\">{}</span></header>",
            escape(kind), navigation_icon(icon), escape(label), records.len()
        ));
        for record in records {
            out.push_str(&format!(
                "<article class=\"stack-resource\" data-resource-id=\"{}\"><h4 class=\"mono\">{}</h4><dl class=\"stack-status stack-resource-properties\"><dt>{}</dt><dd class=\"mono\">{}</dd>",
                escape(text(&record["resource"], "id")), escape(text(&record["owner"], "key")),
                tr(app, "Resource ID", "리소스 ID"), escape(text(&record["resource"], "id"))
            ));
            if let Some(generation) = record["generation"].as_u64() {
                out.push_str(&format!(
                    "<dt>{}</dt><dd>{generation}</dd>",
                    tr(app, "Ownership generation", "소유권 세대")
                ));
            }
            out.push_str("</dl></article>");
        }
        out.push_str("</section>");
    }
    out.push_str("</div>");
    out
}
