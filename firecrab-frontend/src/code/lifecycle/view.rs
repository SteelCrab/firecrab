use super::*;
use crate::model::escape;
use std::fmt::Write;

fn button(app: &App, id: &str, en: &str, ko: &str, disabled: bool, primary: bool) -> String {
    format!(
        "<button class=\"btn{}\" type=\"button\" data-action=\"code-runtime-{id}\" {}>{}</button>",
        if primary { " primary" } else { "" },
        if disabled { "disabled" } else { "" },
        tr(app, en, ko)
    )
}
fn select(app: &App, key: &str, en: &str, ko: &str, options: String) -> String {
    format!(
        "<div class=\"code-setup-field\"><label for=\"code-placement-{key}\">{}</label><select id=\"code-placement-{key}\">{options}</select></div>",
        tr(app, en, ko)
    )
}
fn options<'a>(
    app: &App,
    values: impl Iterator<Item = (&'a str, String)>,
    selected: &str,
) -> String {
    let mut found = false;
    let mut out = String::new();
    for (value, label) in values {
        if value == selected {
            found = true
        }
        out.push_str(&accounts::option(value, &label, selected));
    }
    if !found {
        out.push_str(&accounts::option(
            selected,
            tr(
                app,
                "Choice unavailable — reload placement",
                "선택 항목 사용 불가 — 배치 정보 다시 불러오기",
            ),
            selected,
        ));
    }
    out
}
pub fn content(app: &App, w: &Workspace) -> String {
    let s = &app.code.lifecycle;
    let mut out = String::from("<div class=\"code-runtime-controls\">");
    if let Some(p) = &s.request {
        let _ = write!(
            out,
            "<p role=\"status\" class=\"code-hint\">{}</p>",
            if s.busy {
                tr(
                    app,
                    "Submitting or checking the runtime operation…",
                    "실행 환경 작업을 요청하거나 확인하는 중…",
                )
            } else {
                tr(
                    app,
                    "A runtime request needs confirmation. Check the saved operation before another action.",
                    "실행 환경 요청을 확인해야 합니다. 다른 작업 전에 저장된 작업을 확인해 주세요.",
                )
            }
        );
        out.push_str("<div class=\"code-account-actions\">");
        out.push_str(&button(
            app,
            "check",
            "Check operation",
            "작업 확인",
            s.busy,
            true,
        ));
        if s.retryable {
            out.push_str(&button(
                app,
                "retry",
                "Retry same request",
                "같은 요청 재시도",
                s.busy,
                false,
            ));
        }
        out.push_str(&button(
            app,
            "dismiss",
            "Dismiss request notice",
            "요청 알림 닫기",
            s.busy,
            false,
        ));
        out.push_str("</div>");
        let _ = write!(
            out,
            "<p class=\"code-hint\">{} <code>{}</code></p>",
            tr(
                app,
                "Dismissing the notice does not stop or delete the MicroVM. Workspace:",
                "알림을 닫아도 MicroVM을 중지하거나 삭제하지 않습니다. 워크스페이스:"
            ),
            escape(&p.id)
        );
    } else {
        match &app.code.runtime {
            RuntimeState::Absent => {
                if s.loading {
                    let _ = write!(
                        out,
                        "<p role=\"status\">{}</p>",
                        tr(
                            app,
                            "Loading MicroVM placement…",
                            "MicroVM 배치 정보 불러오는 중…"
                        )
                    );
                }
                if s.unavailable {
                    let _ = write!(
                        out,
                        "<p role=\"alert\" class=\"code-message\">{}</p>",
                        tr(
                            app,
                            "Could not load images, networks or storage. Reload placement to retry.",
                            "이미지, 네트워크 또는 스토리지를 불러오지 못했습니다. 배치 정보를 다시 불러와 주세요."
                        )
                    );
                }
                if let Some(c) = &s.catalog {
                    let chosen = placement(app, w).unwrap_or(Placement {
                        template: String::new(),
                        micro_network_id: String::new(),
                        storage_root: String::new(),
                    });
                    out.push_str("<fieldset class=\"code-placement\">");
                    out.push_str(&select(
                        app,
                        "template",
                        "Installed image",
                        "설치된 이미지",
                        options(
                            app,
                            c.images.iter().filter(|i| eligible_image(i, w)).map(|i| {
                                (i.alias.as_str(), format!("{} · {}", i.alias, i.version))
                            }),
                            &chosen.template,
                        ),
                    ));
                    out.push_str(&select(
                        app,
                        "network",
                        "Internet-enabled network",
                        "인터넷 연결 네트워크",
                        options(
                            app,
                            c.networks
                                .iter()
                                .filter(|n| n.internet_enabled && uuid(&n.id))
                                .map(|n| (n.id.as_str(), n.name.clone())),
                            &chosen.micro_network_id,
                        ),
                    ));
                    out.push_str(&select(
                        app,
                        "storage",
                        "Storage root",
                        "스토리지 루트",
                        options(
                            app,
                            c.storage.iter().map(|r| (r.id.as_str(), r.name.clone())),
                            &chosen.storage_root,
                        ),
                    ));
                    out.push_str("</fieldset>");
                    if !valid_placement(app, w, &chosen) {
                        let _ = write!(
                            out,
                            "<p class=\"code-hint\">{}</p>",
                            tr(
                                app,
                                "Install an image that fits the saved disk, and create an internet-enabled network and a storage root before provisioning.",
                                "저장된 디스크 크기에 맞는 이미지를 설치하고, 인터넷 연결 네트워크와 스토리지 루트를 준비해 주세요."
                            )
                        );
                    }
                }
                if w.connections.github != "present_unverified" {
                    let _ = write!(
                        out,
                        "<p class=\"code-hint\">{}</p>",
                        tr(
                            app,
                            "Reconnect the selected GitHub account before the first checkout.",
                            "처음 저장소를 가져오기 전에 선택한 GitHub 계정을 다시 연결해 주세요."
                        )
                    );
                }
                out.push_str("<div class=\"code-account-actions\">");
                out.push_str(&button(
                    app,
                    "create",
                    "Create and start MicroVM",
                    "MicroVM 생성 및 시작",
                    !available(app, w, &Action::Create),
                    true,
                ));
                out.push_str(&button(
                    app,
                    "catalog",
                    "Reload placement",
                    "배치 정보 다시 불러오기",
                    s.loading,
                    false,
                ));
                out.push_str("</div>");
                let _ = write!(
                    out,
                    "<p class=\"code-hint\">{}</p>",
                    tr(
                        app,
                        "The selected repository branch is cloned into /home/fc-code/project on first start. No editor or agent starts.",
                        "처음 시작할 때 선택한 저장소 브랜치를 /home/fc-code/project로 가져옵니다. 편집기나 에이전트는 실행하지 않습니다."
                    )
                );
            }
            RuntimeState::Current(r) => {
                if pending(r) {
                    let _ = write!(
                        out,
                        "<p role=\"status\" class=\"code-hint\">{}</p>",
                        tr(
                            app,
                            "Operation in progress. Status refreshes automatically; another lifecycle action is disabled.",
                            "작업 중입니다. 상태를 자동으로 새로 불러옵니다. 다른 실행 환경 작업은 비활성 상태입니다."
                        )
                    );
                }
                out.push_str("<div class=\"code-account-actions\">");
                out.push_str(&button(
                    app,
                    "start",
                    if r.phase == "failed" || r.phase == "reconcile_required" {
                        "Retry / recover MicroVM"
                    } else {
                        "Start MicroVM"
                    },
                    if r.phase == "failed" || r.phase == "reconcile_required" {
                        "MicroVM 재시도 / 복구"
                    } else {
                        "MicroVM 시작"
                    },
                    !available(app, w, &Action::Start),
                    true,
                ));
                out.push_str(&button(
                    app,
                    "stop",
                    if r.error.as_deref() == Some("sync_failed") {
                        "Retry stop"
                    } else {
                        "Stop MicroVM"
                    },
                    if r.error.as_deref() == Some("sync_failed") {
                        "중지 재시도"
                    } else {
                        "MicroVM 중지"
                    },
                    !available(app, w, &Action::Stop),
                    false,
                ));
                out.push_str("</div>");
            }
            _ => {}
        }
    }
    if s.notice {
        let _ = write!(
            out,
            "<p class=\"code-runtime-notice\" role=\"status\">{}</p>",
            tr(
                app,
                "Runtime operation observed. Readiness is shown by the current status.",
                "실행 환경 작업을 확인했습니다. 현재 상태에서 준비 여부를 확인합니다."
            )
        );
    }
    if !s.error.is_empty() {
        let message = match s.error {
            "conflict" => tr(
                app,
                "The workspace or runtime changed. Review its current state; the old request will not be replayed.",
                "워크스페이스 또는 실행 환경이 바뀌었습니다. 현재 상태를 확인해 주세요. 이전 요청은 자동으로 다시 보내지 않습니다.",
            ),
            "unchanged" => tr(
                app,
                "No new operation was observed. You can retry the same request after checking again.",
                "새 작업을 확인하지 못했습니다. 다시 확인한 뒤 같은 요청을 재시도할 수 있습니다.",
            ),
            "busy" => tr(
                app,
                "Another lifecycle operation is active. Review the current status before retrying.",
                "다른 실행 환경 작업이 진행 중입니다. 재시도 전에 현재 상태를 확인해 주세요.",
            ),
            "placement_bound" => tr(
                app,
                "This workspace already has different placement. Inspect its retained runtime before changing it.",
                "이 워크스페이스에 다른 배치 정보가 이미 연결돼 있습니다. 변경 전에 유지된 실행 환경을 확인해 주세요.",
            ),
            "rejected" => tr(
                app,
                "The operation was rejected. Check operator access, image capacity, network and storage, then reload.",
                "작업이 거부됐습니다. 운영자 권한, 이미지 용량, 네트워크와 스토리지를 확인하고 다시 불러와 주세요.",
            ),
            _ => tr(
                app,
                "The request result is unknown. Check the saved operation before retrying. Its original target and request are retained.",
                "요청 결과를 확인할 수 없습니다. 재시도 전에 저장된 작업을 확인해 주세요. 기존 대상과 요청은 유지됩니다.",
            ),
        };
        let _ = write!(
            out,
            "<p class=\"code-message\" role=\"alert\">{message}</p>"
        );
    }
    out.push_str("</div>");
    out
}
