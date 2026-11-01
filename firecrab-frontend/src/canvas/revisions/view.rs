use super::*;

fn button(action: &str, id: &str, label: &str, disabled: bool) -> String {
    format!(
        "<button type=\"button\" data-action=\"{action}\" data-id=\"{}\" {}>{}</button>",
        escape(id),
        if disabled { "disabled" } else { "" },
        escape(label)
    )
}
fn preview(a: &App, file: &Value) -> String {
    let metadata = &file["version"];
    let encoded = text(file, "contentBase64");
    let bytes: Vec<u8> = window()
        .atob(encoded)
        .unwrap_or_default()
        .chars()
        .map(|c| c as u8)
        .collect();
    let content = match std::str::from_utf8(&bytes) {
        Ok(content) if !content.contains('\0') => {
            let mut end = content.len().min(65536);
            while !content.is_char_boundary(end) {
                end -= 1;
            }
            format!(
                "<pre>{}</pre>{}",
                escape(&content[..end]),
                if end < content.len() {
                    tr(
                        a,
                        "Preview limited to 64 KiB",
                        "미리보기는 64 KiB까지 표시한다",
                    )
                } else {
                    ""
                }
            )
        }
        _ => escape(tr(
            a,
            "Binary file: compare size and SHA-256",
            "바이너리 파일: 크기와 SHA-256으로 비교한다",
        )),
    };
    format!(
        "<section><strong>v{} · {} B</strong><small>{}</small>{content}<a download=\"{}\" href=\"data:application/octet-stream;base64,{}\">{}</a></section>",
        metadata["id"],
        metadata["byteLength"],
        escape(text(metadata, "sha256")),
        escape(&text(metadata, "path").replace('/', "_")),
        escape(encoded),
        tr(a, "Download this version", "이 버전 다운로드")
    )
}
pub(in crate::canvas) fn card(a: &App, t: &Tab) -> String {
    let s = &t.revisions;
    let mut result = button(
        "cw-history",
        "",
        tr(a, "Load retained history", "저장된 기록 불러오기"),
        s.busy || t.history_loading,
    );
    if let Some(notice) = &s.message {
        let message = match notice {
            Notice::Restored => tr(a, "File restored as a new version", "새 버전으로 복원했다"),
            Notice::Recovered => tr(a, "Saved outcome recovered", "저장 결과를 확인했다"),
        };
        result += &format!("<p role=\"status\">{}</p>", escape(message));
    }
    if let Some(pending) = &s.pending {
        result += &format!(
            "<div class=\"cw-restore-pending\"><p>{}: {}</p>{}{}</div>",
            tr(a, "Unconfirmed restore", "복원 결과 확인 필요"),
            escape(&pending.path),
            button(
                "cw-restore-retry",
                "",
                tr(a, "Retry the same restore", "같은 복원 요청 재시도"),
                s.busy
            ),
            button(
                "cw-file-reconcile",
                text(&pending.body, "operationId"),
                tr(a, "Check saved outcome", "저장 결과 확인"),
                s.busy
            )
        );
    }
    for op in s
        .operations
        .iter()
        .filter(|op| text(op, "status") == "pending")
    {
        result += &format!(
            "<div class=\"cw-restore-pending\">{} · {} {}</div>",
            tr(a, "Pending file operation", "확인되지 않은 파일 작업"),
            escape(text(op, "path")),
            button(
                "cw-file-reconcile",
                text(op, "id"),
                tr(a, "Reconcile", "결과 확인"),
                s.busy
            )
        );
    }
    if let Some(diff) = &s.comparison {
        let blocked = s.busy
            || !s.catalog_ready
            || s.pending.is_some()
            || s.operations
                .iter()
                .any(|op| text(op, "status") == "pending");
        result += &format!(
            "<div class=\"cw-comparison\"><h3>{}</h3><p>{}</p><div class=\"cw-diff-columns\">{}{}</div>{}{}</div>",
            escape(text(&diff["before"]["version"], "path")),
            tr(
                a,
                "Selected version → current saved version. Restore creates a new version. Existing history and private drafts remain.",
                "선택한 버전 → 현재 저장된 버전. 복원하면 새 버전을 만든다. 기존 기록과 개인 초안은 유지한다."
            ),
            preview(a, &diff["before"]),
            preview(a, &diff["after"]),
            button(
                "cw-restore",
                "",
                tr(a, "Restore selected version", "선택한 버전 복원"),
                blocked
            ),
            button(
                "cw-compare-close",
                "",
                tr(a, "Close comparison", "비교 닫기"),
                s.busy
            )
        );
    }
    result += "<ul class=\"cw-history\">";
    for r in &t.history {
        result += &format!(
            "<li><span>v{}</span><strong>{}</strong><small>{} B</small>{}</li>",
            r["id"],
            escape(text(r, "path")),
            r["byteLength"],
            button(
                "cw-compare",
                &r["id"].to_string(),
                tr(a, "Compare", "비교"),
                s.busy
            )
        );
    }
    result += "</ul>";
    if t.history_more {
        result += &button(
            "cw-history-older",
            "",
            tr(a, "Load older history", "이전 기록 더 보기"),
            t.history_loading,
        );
    }
    result
}
