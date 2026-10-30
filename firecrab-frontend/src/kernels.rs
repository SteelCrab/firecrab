use crate::{
    App, Shared, api,
    i18n::t,
    model::{escape, list, text},
    render,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use wasm_bindgen_futures::spawn_local;

#[derive(Default)]
pub struct State {
    pub installs: BTreeMap<String, Value>,
    polling: Option<u64>,
}

pub fn install_version(path: &str) -> Option<&str> {
    path.strip_prefix("/api/kernels/")?.strip_suffix("/install")
}

/// Read server-owned jobs on entry and on each catalog refresh, including jobs
/// started in another browser. Reading a snapshot never starts a new build.
pub fn refresh(app: &Shared) {
    let mut state = app.borrow_mut();
    if state.route != "kernels" || state.kernels.polling == Some(state.generation) {
        return;
    }
    let generation = state.generation;
    let versions: Vec<String> = list(&state.resource)
        .iter()
        .map(|row| text(row, "version").to_owned())
        .collect();
    let needs_images = list(&state.resource).iter().any(|row| row["inUse"] == true);
    state.kernels.polling = Some(generation);
    drop(state);
    let app = app.clone();
    spawn_local(async move {
        let mut changed = false;
        if needs_images {
            let images = api::request("GET", "/api/images", None).await.ok();
            let mut state = app.borrow_mut();
            if state.generation != generation {
                return;
            }
            changed |= state.cache.get("/api/images") != images.as_ref();
            if let Some(images) = images {
                state.cache.insert("/api/images".into(), images);
            } else {
                state.cache.remove("/api/images");
            }
        }
        for version in versions {
            let result = api::request(
                "GET",
                &format!("/api/kernels/{}/install", api::encode(&version)),
                None,
            )
            .await;
            let mut state = app.borrow_mut();
            if state.generation != generation {
                return;
            }
            let snapshot =
                result.unwrap_or_else(|error| json!({"status":"unavailable", "error":error}));
            changed |= state.kernels.installs.get(&version).is_none_or(|previous| {
                previous["status"] != snapshot["status"]
                    || stage(previous) != stage(&snapshot)
                    || previous["downloadedBytes"] != snapshot["downloadedBytes"]
                    || previous["totalBytes"] != snapshot["totalBytes"]
            });
            state.kernels.installs.insert(version, snapshot);
        }
        let mut state = app.borrow_mut();
        state.kernels.polling = None;
        if let Some(version) = state
            .detail
            .as_ref()
            .map(|data| text(data, "version").to_owned())
            && let Some(current) = list(&state.resource)
                .iter()
                .find(|row| row["version"] == version)
                .cloned()
        {
            state.detail = Some(current);
        }
        drop(state);
        if !app.borrow().busy {
            if changed {
                crate::render_content(&app);
            } else {
                crate::render_detail(&app);
            }
        }
    });
}

pub fn status(app: &App, data: &Value) -> &'static str {
    let Some(snapshot) = app.kernels.installs.get(text(data, "version")) else {
        return if data["installed"] == true {
            "Installed"
        } else {
            "Checking status…"
        };
    };
    match text(snapshot, "status") {
        "running" => stage_label(snapshot),
        "failed" => "Installation failed",
        "unavailable" => "Status unavailable",
        "succeeded" => "Installed",
        _ if data["installed"] == true => "Installed",
        "idle" => "Not installed",
        _ => "Checking status…",
    }
}

pub fn can_install(app: &App, data: &Value) -> bool {
    !app.busy
        && app
            .kernels
            .installs
            .get(text(data, "version"))
            .is_some_and(|snapshot| {
                matches!(text(snapshot, "status"), "idle" | "failed" | "succeeded")
            })
}

pub fn panel(app: &App, data: &Value) -> String {
    let snapshot = app.kernels.installs.get(text(data, "version"));
    let mut summary = json!({"architecture": data["architecture"]});
    if !snapshot
        .is_some_and(|value| matches!(text(value, "status"), "running" | "failed" | "succeeded"))
    {
        summary["status"] = t(status(app, data)).into();
    }
    let mut html = render::properties(&summary);
    if data["installed"] == true && data["inUse"] == true {
        let aliases = list(app.cache.get("/api/images").unwrap_or(&Value::Null))
            .iter()
            .filter(|image| {
                image["installed"] == true && text(image, "kernelVersion") == text(data, "version")
            })
            .map(|image| text(image, "alias"))
            .filter(|name| !name.is_empty())
            .map(escape)
            .collect::<Vec<_>>();
        let names = if aliases.is_empty() {
            String::new()
        } else {
            format!(
                "<p><strong>{}</strong> {}</p>",
                if app.locale == "ko" {
                    "사용 중인 이미지:"
                } else {
                    "Images using this kernel:"
                },
                aliases.join(", ")
            )
        };
        let message = if app.locale == "ko" {
            "이미지가 이 커널을 사용 중이다. 이미지의 커널을 변경하거나 이미지를 삭제한 뒤 커널을 삭제할 수 있다."
        } else {
            "An image uses this kernel. Change its kernel or delete the image before deleting this kernel."
        };
        let link = if app.locale == "ko" {
            "이미지 관리"
        } else {
            "Manage images"
        };
        html.push_str(&format!(
            "<div class=\"kernel-in-use\"><p>{}</p>{names}<a href=\"#/images\">{link}</a></div>",
            escape(message)
        ));
    }
    if let Some(snapshot) = snapshot {
        if matches!(text(snapshot, "status"), "running" | "failed" | "succeeded") {
            html.push_str(&progress(snapshot));
        }
        if let Some(error) = snapshot["error"].as_str() {
            html.push_str(&format!("<p role=\"alert\">{}</p>", escape(error)));
        }
        let log = text(snapshot, "log");
        if !log.is_empty() {
            let label = escape(t("Detailed log"));
            html.push_str(&format!("<details id=\"kernel-log-details\" class=\"kernel-install\"><summary>{label}</summary><pre id=\"kernel-install-log\" class=\"kernel-install-log\" tabindex=\"0\" role=\"region\" aria-label=\"{label}\">{}</pre></details>", escape(log)));
        }
    }
    html
}

const STEPS: [&str; 4] = ["Download", "Waiting", "Build", "Verify"];

// These milestones are emitted by the kernel builder. Compiler line counts
// cannot measure the remaining build time, so the bar counts stages instead.
fn stage(snapshot: &Value) -> usize {
    if snapshot["status"] == "succeeded" {
        return STEPS.len();
    }
    let log = text(snapshot, "log");
    if log.contains("verifying built kernel") || log.contains("verified and cached") {
        3
    } else if log.contains("building Linux ") {
        2
    } else if log.contains("waiting for the host kernel builder") {
        1
    } else {
        0
    }
}

fn stage_label(snapshot: &Value) -> &'static str {
    if snapshot["status"] == "failed" {
        return "Installation failed";
    }
    match stage(snapshot) {
        4 => "Installed",
        3 => "Verifying kernel",
        2 => "Building kernel",
        1 => "Waiting for build",
        _ if text(snapshot, "log").contains("downloading and verifying ") => "Downloading source",
        _ => "Preparing kernel",
    }
}

fn progress(snapshot: &Value) -> String {
    let current = stage(snapshot);
    let failed = snapshot["status"] == "failed";
    let label = escape(t(stage_label(snapshot)));
    let count = format!("{current} / {} {}", STEPS.len(), t("stages complete"));
    let mut html = format!(
        "<div class=\"kernel-progress{}\"><div class=\"kernel-progress-head\"><strong>{label}</strong><span>{count}</span></div><div class=\"kernel-stage-bar\" role=\"progressbar\" aria-label=\"{}\" aria-valuemin=\"0\" aria-valuemax=\"4\" aria-valuenow=\"{current}\" aria-valuetext=\"{label} · {count}\">",
        if failed { " failed" } else { "" },
        escape(t("Installation stages"))
    );
    for index in 0..STEPS.len() {
        let state = if index < current {
            "done"
        } else if index == current {
            "current"
        } else {
            "pending"
        };
        html.push_str(&format!("<span class=\"{state}\"></span>"));
    }
    html.push_str("</div><ol class=\"kernel-steps\">");
    for (index, name) in STEPS.iter().enumerate() {
        let state = if index < current {
            "done"
        } else if index == current {
            "current"
        } else {
            "pending"
        };
        let marker = if index < current {
            "✓".to_owned()
        } else {
            (index + 1).to_string()
        };
        html.push_str(&format!(
            "<li class=\"{state}\"{}><span aria-hidden=\"true\">{marker}</span>{}</li>",
            if index == current {
                " aria-current=\"step\""
            } else {
                ""
            },
            escape(t(name))
        ));
    }
    html.push_str("</ol>");
    if current == 0
        && let Some(bytes) = snapshot["downloadedBytes"].as_u64()
    {
        let received = bytes;
        let bytes = render::human_bytes(received);
        if let Some(total) = snapshot["totalBytes"].as_u64().filter(|total| *total > 0) {
            let percent = (received as f64 / total as f64 * 100.0).min(100.0);
            html.push_str(&format!("<div class=\"kernel-download\"><span>{bytes} / {} · {percent:.0}%</span><progress aria-label=\"{}\" max=\"100\" value=\"{percent:.2}\"></progress></div>", render::human_bytes(total), escape(t("Source download"))));
        } else {
            html.push_str(&format!(
                "<p class=\"kernel-download\">{} {bytes}</p>",
                escape(t("Downloaded"))
            ));
        }
    }
    html.push_str("</div>");
    html
}

pub fn queue(app: &App) -> String {
    let mut jobs: Vec<_> = app
        .kernels
        .installs
        .iter()
        .filter(|(_, job)| {
            job["startedAtMs"].as_u64().is_some()
                && matches!(text(job, "status"), "running" | "failed" | "succeeded")
        })
        .collect();
    if jobs.is_empty() {
        return String::new();
    }
    jobs.sort_by_key(|(version, job)| {
        (
            job["status"] == "running" && stage(job) == 1,
            job["startedAtMs"].as_u64(),
            *version,
        )
    });
    let waiting = jobs
        .iter()
        .filter(|(_, job)| job["status"] == "running" && stage(job) == 1)
        .count();
    let active = jobs
        .iter()
        .filter(|(_, job)| job["status"] == "running")
        .count()
        - waiting;
    let counts = if active + waiting > 0 {
        format!("{} {active} · {} {waiting}", t("In progress"), t("Waiting"))
    } else {
        let installed = jobs
            .iter()
            .filter(|(_, job)| job["status"] == "succeeded")
            .count();
        let failed = jobs.len() - installed;
        let mut counts = Vec::new();
        if installed > 0 {
            counts.push(format!("{} {installed}", t("Installed")));
        }
        if failed > 0 {
            counts.push(format!("{} {failed}", t("Installation failed")));
        }
        counts.join(" · ")
    };
    let mut html = format!(
        "<section class=\"kernel-queue\" aria-label=\"{}\"><div class=\"kernel-queue-head\"><h2>{}</h2><span>{}</span></div>",
        escape(t("Kernel installation queue")),
        escape(t("Kernel installations")),
        escape(&counts)
    );
    for (version, snapshot) in jobs {
        html.push_str(&format!("<article class=\"kernel-job\" data-kernel-version=\"{}\"><div class=\"kernel-job-head\">{}</div>{}</article>", escape(version), render::button(version, "detail", version), progress(snapshot)));
    }
    html.push_str("</section>");
    html
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observed_milestones_advance_the_stage_bar_without_guessing_build_percent() {
        let mut job = json!({"status":"running", "log":"resolving Linux source checksum"});
        assert_eq!(stage_label(&job), "Preparing kernel");
        for (line, expected) in [
            (
                "downloading and verifying https://cdn.kernel.org/source.tar.xz",
                0,
            ),
            ("waiting for the host kernel builder", 1),
            ("building Linux 7.2.13 for aarch64", 2),
            ("verifying built kernel", 3),
        ] {
            job["log"] = format!("{}\n[12:00:00] {line}", text(&job, "log")).into();
            assert_eq!(stage(&job), expected);
        }
        assert!(!progress(&job).contains('%'));
        job["status"] = "succeeded".into();
        assert_eq!(stage(&job), 4);
        assert!(progress(&job).contains("aria-valuenow=\"4\""));
        assert!(!progress(&job).contains("aria-current=\"step\""));
    }

    #[test]
    fn a_failure_retains_the_failed_stage_instead_of_marking_it_complete() {
        let job = json!({"status":"failed", "log":"building Linux 7.2.13 for aarch64\nkernel install failed: compiler error"});
        let html = progress(&job);
        assert_eq!(stage_label(&job), "Installation failed");
        assert!(html.contains("kernel-progress failed"));
        assert!(html.contains("aria-valuenow=\"2\""));
        assert!(html.contains("class=\"current\" aria-current=\"step\""));
    }

    #[test]
    fn only_known_download_totals_produce_a_real_download_percentage() {
        let mut job = json!({"status":"running", "log":"downloading and verifying source", "downloadedBytes":25 * 1024 * 1024});
        assert!(!progress(&job).contains('%'));
        job["totalBytes"] = (100 * 1024 * 1024).into();
        let html = progress(&job);
        assert!(html.contains("25 MiB / 100 MiB · 25%"));
        assert!(html.contains("value=\"25.00\""));
    }

    #[test]
    fn the_queue_distinguishes_working_and_waiting_jobs_and_omits_idle_catalog_entries() {
        let mut app = App::default();
        app.kernels.installs.insert("7.2.12".into(), json!({"status":"running", "startedAtMs":1, "log":"waiting for the host kernel builder"}));
        app.kernels.installs.insert(
            "7.2.13".into(),
            json!({"status":"running", "startedAtMs":2, "log":"building Linux 7.2.13 for aarch64"}),
        );
        app.kernels
            .installs
            .insert("7.2.11".into(), json!({"status":"idle"}));
        let html = queue(&app);
        assert_eq!(html.matches("<article ").count(), 2);
        assert!(html.find("7.2.13").unwrap() < html.find("7.2.12").unwrap());
        assert!(html.contains("In progress 1 · Waiting 1"));
        assert!(html.contains("Waiting for build"));
        assert!(!html.contains("7.2.11"));
        app.kernels.installs.get_mut("7.2.13").unwrap()["status"] = "succeeded".into();
        app.kernels.installs.get_mut("7.2.12").unwrap()["status"] = "failed".into();
        let finished = queue(&app);
        assert!(finished.contains("Installed 1 · Installation failed 1"));
    }
}
