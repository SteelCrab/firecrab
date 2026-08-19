pub mod checks;

use serde::Serialize;

/// Outcome of one check. Mirrors the bash script's three-way PASS/FAIL/SKIP
/// (there is no "warn" — an inconclusive check reports `Skip`, not a status
/// of its own).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// Check succeeded; silent in `print_human` (only failures/skips print).
    Pass,
    /// Check found a real problem; drives [`Report::exit_code`] nonzero.
    Fail,
    /// Check could not run to a conclusion on this host (e.g. permission
    /// denied reading a path) — not the same as `Fail`, and does not affect
    /// `exit_code`.
    Skip,
}

/// One line of doctor output — mirrors the bash script's flat `REPORT`
/// array entries (`fail "title" "detail" "fix"` / `skip ...` / `pass`).
/// A single check function may return more than one of these (e.g. `ufw`
/// reports one failure per firecrab bridge).
#[derive(Debug, Clone, Serialize)]
pub struct CheckResult {
    /// Short, one-line description of the check and its outcome.
    pub title: String,
    pub status: Status,
    /// Extra context explaining the failure/skip; `None` for `Pass`.
    pub detail: Option<String>,
    /// Suggested remediation command or doc pointer; `None` for `Pass`.
    pub fix: Option<String>,
}

impl CheckResult {
    /// A passing check never carries detail/fix text — nothing to explain.
    pub fn pass(title: impl Into<String>) -> CheckResult {
        CheckResult {
            title: title.into(),
            status: Status::Pass,
            detail: None,
            fix: None,
        }
    }

    /// A failing check — `detail`/`fix` are independently optional since
    /// not every failure has a known remedy to suggest.
    pub fn fail(title: impl Into<String>, detail: Option<&str>, fix: Option<&str>) -> CheckResult {
        CheckResult {
            title: title.into(),
            status: Status::Fail,
            detail: detail.map(str::to_owned),
            fix: fix.map(str::to_owned),
        }
    }

    /// Like [`Self::fail`] but for a check that could not reach a
    /// conclusion, not one that found a problem.
    pub fn skip(title: impl Into<String>, detail: Option<&str>, fix: Option<&str>) -> CheckResult {
        CheckResult {
            title: title.into(),
            status: Status::Skip,
            detail: detail.map(str::to_owned),
            fix: fix.map(str::to_owned),
        }
    }
}

/// Resolved copy of the env vars the bash script reads once at the top
/// (`DATADIR`, `FIRECRAB_API_USER`, ...). Explicit injection instead of
/// each check reading `std::env` directly keeps checks pure and lets tests
/// set values without mutating process-global env vars (which would race
/// under `cargo test`'s parallel test runner).
pub struct DoctorEnv {
    pub datadir: String,
    /// `FIRECRAB_API_USER`, falling back to the older `FIRECRAB_USER` name.
    pub api_user: Option<String>,
    pub helper_sock: String,
    pub dnsmasq_conf: String,
    pub dnsmasq_pid: String,
    pub libdir: String,
    /// Unset means checks that need it (e.g. image tooling) should `Skip`,
    /// not assume a default path.
    pub image_root: Option<String>,
    pub image_base_url: Option<String>,
    pub storage_roots: Option<String>,
    /// Unset means "resolve `firecracker` from `$PATH`" — not an error by
    /// itself.
    pub firecracker_bin: Option<String>,
    pub confdir: String,
}

impl DoctorEnv {
    /// Snapshots the process environment once, at startup — see the type's
    /// own doc comment for why checks don't read `std::env` directly.
    pub fn from_process_env() -> Self {
        Self {
            datadir: env_or("DATADIR", "/var/lib/firecrab"),
            api_user: std::env::var("FIRECRAB_API_USER")
                .ok()
                .or_else(|| std::env::var("FIRECRAB_USER").ok()),
            helper_sock: env_or("FIRECRAB_NET_HELPER_SOCK", "/run/firecrab/net-helper.sock"),
            dnsmasq_conf: env_or("FIRECRAB_DNSMASQ_CONF", "/run/firecrab/dnsmasq.conf"),
            dnsmasq_pid: env_or("FIRECRAB_DNSMASQ_PID", "/run/firecrab/dnsmasq.pid"),
            libdir: env_or("FIRECRAB_LIBDIR", "/usr/local/lib/firecrab"),
            image_root: std::env::var("FIRECRAB_IMAGE_ROOT").ok(),
            image_base_url: std::env::var("FIRECRAB_IMAGE_BASE_URL").ok(),
            storage_roots: std::env::var("FIRECRAB_STORAGE_ROOTS").ok(),
            firecracker_bin: std::env::var("FIRECRAB_FIRECRACKER_BIN").ok(),
            confdir: env_or("CONFDIR", "/etc/firecrab"),
        }
    }
}

impl Default for DoctorEnv {
    /// install.sh's defaults, with every optional var unset — used by tests
    /// that don't care about a specific path.
    fn default() -> Self {
        Self {
            datadir: "/var/lib/firecrab".to_owned(),
            api_user: None,
            helper_sock: "/run/firecrab/net-helper.sock".to_owned(),
            dnsmasq_conf: "/run/firecrab/dnsmasq.conf".to_owned(),
            dnsmasq_pid: "/run/firecrab/dnsmasq.pid".to_owned(),
            libdir: "/usr/local/lib/firecrab".to_owned(),
            image_root: None,
            image_base_url: None,
            storage_roots: None,
            firecracker_bin: None,
            confdir: "/etc/firecrab".to_owned(),
        }
    }
}

fn env_or(var: &str, default: &str) -> String {
    std::env::var(var).unwrap_or_else(|_| default.to_owned())
}

use crate::shell::CommandRunner;

/// Full output of a `doctor` run — every [`CheckResult`] from every check,
/// in the fixed order [`run_all`] runs them.
#[derive(Debug, Serialize)]
pub struct Report {
    pub results: Vec<CheckResult>,
}

impl Report {
    /// Count of [`Status::Pass`] results.
    pub fn ok_count(&self) -> usize {
        self.results
            .iter()
            .filter(|r| r.status == Status::Pass)
            .count()
    }
    /// Count of [`Status::Fail`] results.
    pub fn fail_count(&self) -> usize {
        self.results
            .iter()
            .filter(|r| r.status == Status::Fail)
            .count()
    }
    /// Count of [`Status::Skip`] results.
    pub fn skip_count(&self) -> usize {
        self.results
            .iter()
            .filter(|r| r.status == Status::Skip)
            .count()
    }
    /// Same contract as the bash script: non-zero if any check FAILed,
    /// zero if the rest is PASS/SKIP only. CI depends on this.
    pub fn exit_code(&self) -> i32 {
        if self.fail_count() > 0 { 1 } else { 0 }
    }
}

/// Runs one check and, if it panics, turns that into a synthesized FAIL
/// instead of aborting the whole report — mirrors bash's `on_unexpected_error`
/// ERR trap ("partial results even for a crash the checks never anticipated").
fn run_checked<F: FnOnce() -> Vec<CheckResult>>(name: &str, f: F) -> Vec<CheckResult> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(r) => r,
        Err(_) => vec![CheckResult::fail(
            format!(
                "{name}: internal error (this is a bug in firecrab doctor itself, not a host problem)"
            ),
            None,
            None,
        )],
    }
}

/// Runs all 13 checks in the same order as the bash script's `run` section
/// (kvm, firecracker, ip_forward, nft, dnsmasq, helper_socket, ufw,
/// data_root, images, image_install_tools, selinux_domain, registry_egress,
/// reflink).
pub fn run_all(env: &DoctorEnv, runner: &dyn CommandRunner, digest: bool) -> Report {
    let mut results = Vec::new();
    results.extend(run_checked("kvm", checks::check_kvm));
    results.extend(run_checked("firecracker", || {
        checks::check_firecracker(env, runner)
    }));
    results.extend(run_checked("ip_forward", checks::check_ip_forward));
    results.extend(run_checked("nft", || checks::check_nft(env, runner)));
    results.extend(run_checked("dnsmasq", || {
        checks::check_dnsmasq(env, runner)
    }));
    results.extend(run_checked("helper_socket", || {
        checks::check_helper_socket(env, runner)
    }));
    results.extend(run_checked("ufw", || checks::check_ufw(runner)));
    results.extend(run_checked("data_root", || checks::check_data_root(env)));
    results.extend(run_checked("images", || checks::check_images(env, digest)));
    results.extend(run_checked("image_install_tools", || {
        checks::check_image_install_tools(env, runner)
    }));
    results.extend(run_checked("selinux_domain", || {
        checks::check_selinux_domain(env, runner)
    }));
    results.extend(run_checked("registry_egress", || {
        checks::check_registry_egress(env, runner)
    }));
    results.extend(run_checked("reflink", || checks::check_reflink(env)));
    Report { results }
}

/// Builds the human-readable output as a `String` — split out from
/// [`print_human`] so tests can assert on the formatted content (the
/// all-pass / some-skip / has-fail summary branches, and the per-result
/// FAIL/SKIP formatting) without capturing real stdout. Matches the bash
/// script's style: one summary line, then `[FAIL]`/`[SKIP]` blocks with
/// indented detail/fix lines. PASS results are silent (bash never prints
/// them either).
fn format_human(report: &Report) -> String {
    use std::fmt::Write;
    let ok = report.ok_count();
    let fail = report.fail_count();
    let skip = report.skip_count();

    let mut out = String::new();
    if fail == 0 && skip == 0 {
        writeln!(out, "doctor: all checks passed ({ok} ok)").unwrap();
    } else if fail == 0 {
        writeln!(out, "doctor: {ok} ok, {skip} skipped (no failures)").unwrap();
    } else {
        writeln!(out, "doctor: {fail} failed, {skip} skipped, {ok} ok").unwrap();
    }

    for r in &report.results {
        let tag = match r.status {
            Status::Pass => continue,
            Status::Fail => "[FAIL]",
            Status::Skip => "[SKIP]",
        };
        writeln!(out, "{tag} {}", r.title).unwrap();
        if let Some(detail) = &r.detail {
            for line in detail.lines() {
                writeln!(out, "  {line}").unwrap();
            }
        }
        if let Some(fix) = &r.fix {
            writeln!(out, "  → {fix}").unwrap();
        }
    }
    out
}

/// Human-readable output matching the bash script's style — see
/// [`format_human`] for the formatting rules.
pub fn print_human(report: &Report) {
    print!("{}", format_human(report));
}

#[cfg(test)]
mod tests {
    use crate::shell::FakeCommandRunner;

    use super::*;

    #[test]
    fn exit_code_zero_when_no_failures() {
        let report = Report {
            results: vec![CheckResult::pass("x"), CheckResult::skip("y", None, None)],
        };
        assert_eq!(report.exit_code(), 0);
    }

    #[test]
    fn exit_code_nonzero_when_any_failure() {
        let report = Report {
            results: vec![CheckResult::pass("x"), CheckResult::fail("y", None, None)],
        };
        assert_eq!(report.exit_code(), 1);
    }

    #[test]
    fn run_all_produces_thirteen_or_more_results() {
        // 13 named checks; `ufw` alone can emit >1 CheckResult, so the
        // total is always >= 13, never fewer.
        let env = DoctorEnv::default();
        let fake = FakeCommandRunner::new();
        let report = run_all(&env, &fake, false);
        assert!(report.results.len() >= 13);
    }

    #[test]
    fn run_checked_synthesizes_fail_on_panic() {
        let results = run_checked("boom", || panic!("simulated check bug"));
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status, Status::Fail);
        assert!(results[0].title.contains("boom: internal error"));
    }

    #[test]
    fn run_checked_passes_through_normal_results() {
        let results = run_checked("ok", || vec![CheckResult::pass("ok")]);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status, Status::Pass);
    }

    #[test]
    fn format_human_all_pass() {
        let report = Report {
            results: vec![CheckResult::pass("a"), CheckResult::pass("b")],
        };
        let text = format_human(&report);
        assert_eq!(text, "doctor: all checks passed (2 ok)\n");
    }

    #[test]
    fn format_human_no_failures_but_some_skipped() {
        let report = Report {
            results: vec![
                CheckResult::pass("a"),
                CheckResult::skip("b", Some("detail line"), Some("do the fix")),
            ],
        };
        let text = format_human(&report);
        assert!(text.starts_with("doctor: 1 ok, 1 skipped (no failures)\n"));
        assert!(text.contains("[SKIP] b\n"));
        assert!(text.contains("  detail line\n"));
        assert!(text.contains("  → do the fix\n"));
        // Pass results never print their own line.
        assert!(!text.contains("[PASS]"));
    }

    #[test]
    fn format_human_has_failures() {
        let report = Report {
            results: vec![
                CheckResult::pass("a"),
                CheckResult::fail("b", Some("multi\nline detail"), None),
            ],
        };
        let text = format_human(&report);
        assert!(text.starts_with("doctor: 1 failed, 0 skipped, 1 ok\n"));
        assert!(text.contains("[FAIL] b\n"));
        assert!(text.contains("  multi\n"));
        assert!(text.contains("  line detail\n"));
    }

    #[test]
    fn print_human_does_not_panic() {
        let report = Report {
            results: vec![CheckResult::fail("x", None, None)],
        };
        print_human(&report);
    }
}
