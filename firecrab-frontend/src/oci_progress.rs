use crate::{
    i18n::t,
    model::{escape, text},
};
use serde_json::Value;

fn stage(job: &Value) -> usize {
    if job["status"] == "succeeded" {
        return 4;
    }
    let log = text(job, "log");
    if log.contains("pairing host kernel") {
        3
    } else if log.contains("provisioning guest runtime") || log.contains("writing ext4 image") {
        2
    } else if log.contains("reading image process config") || log.contains("decompressing layers") {
        1
    } else {
        0
    }
}

pub fn html(job: &Value) -> String {
    let current = stage(job);
    let status = match text(job, "status") {
        "succeeded" => "Import complete",
        "failed" | "error" => "Import failed",
        "interrupted" | "idle" => "Import status unavailable",
        _ => match current {
            0 => "Downloading image",
            1 => "Preparing image files",
            2 => "Building image",
            _ => "Connecting kernel",
        },
    };
    let steps = ["Download", "Prepare files", "Build image", "Connect kernel"];
    let alias = escape(text(job, "alias"));
    let count = format!("{current} / 4 {}", t("stages complete"));
    let mut bars = String::new();
    let mut labels = String::new();
    for (i, step) in steps.iter().enumerate() {
        let state = if i < current {
            "done"
        } else if i == current {
            "current"
        } else {
            "pending"
        };
        bars.push_str(&format!("<span class=\"{state}\"></span>"));
        labels.push_str(&format!(
            "<li class=\"{state}\"{}>{}</li>",
            if i == current {
                " aria-current=\"step\""
            } else {
                ""
            },
            escape(t(step))
        ));
    }
    let label = escape(t(status));
    let failed = matches!(
        text(job, "status"),
        "failed" | "error" | "interrupted" | "idle"
    );
    let mut html = format!(
        "<section class=\"oci-import-progress kernel-job\" aria-label=\"{}\"><div class=\"oci-import-head\"><strong>{alias}</strong></div>",
        escape(t("Image import progress"))
    );
    if matches!(text(job, "status"), "interrupted" | "idle") {
        html.push_str(&format!(
            "<p class=\"oci-import-note\"><strong>{label}</strong></p>"
        ));
    } else {
        html.push_str(&format!("<div class=\"kernel-progress{}\"><div class=\"kernel-progress-head\"><strong>{label}</strong><span>{count}</span></div><div class=\"kernel-stage-bar\" role=\"progressbar\" aria-label=\"{}\" aria-valuemin=\"0\" aria-valuemax=\"4\" aria-valuenow=\"{current}\" aria-valuetext=\"{label} · {count}\">{bars}</div><ol class=\"kernel-steps\">{labels}</ol></div>", if failed { " failed" } else { "" }, escape(t("Installation stages"))));
    }
    if matches!(text(job, "status"), "interrupted" | "idle") {
        html.push_str(&format!(
            "<p class=\"oci-import-note\">{}</p>",
            escape(t("Check installed images or retry the import."))
        ));
    }
    let log = text(job, "log");
    if !log.is_empty() {
        let title = escape(t("Detailed log"));
        html.push_str(&format!("<details id=\"oci-import-log-details\" data-import-alias=\"{alias}\"><summary>{title}</summary><pre id=\"oci-import-log\" class=\"kernel-install-log\" tabindex=\"0\" role=\"region\" aria-label=\"{title}\">{}</pre></details>", escape(log)));
    }
    html.push_str("</section>");
    html
}
