use serde::Serialize;

/// Matches install.sh's PREFIX=/usr/local, LIBDIR/SHAREDIR derive from it,
/// DATADIR=/var/lib/firecrab, CONFDIR=/etc/firecrab, UNITDIR=/etc/systemd/system.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InfoReport {
    /// `CARGO_PKG_VERSION` of this binary, not a running service's version.
    pub version: String,
    /// From `$PREFIX`, or install.sh's default.
    pub prefix: String,
    /// From `$DATADIR`, or install.sh's default.
    pub datadir: String,
    /// From `$CONFDIR`, or install.sh's default.
    pub confdir: String,
    /// From `$UNITDIR`, or install.sh's default.
    pub unitdir: String,
    /// Resolved by [`crate::api_client::resolve_api_base`]; not re-derived here.
    pub api_base: String,
}

/// Reads `PREFIX`/`DATADIR`/`CONFDIR`/`UNITDIR` from the environment,
/// falling back to install.sh's own defaults when unset.
pub fn collect(api_base: &str) -> InfoReport {
    InfoReport {
        version: env!("CARGO_PKG_VERSION").to_owned(),
        prefix: std::env::var("PREFIX").unwrap_or_else(|_| "/usr/local".to_owned()),
        datadir: std::env::var("DATADIR").unwrap_or_else(|_| "/var/lib/firecrab".to_owned()),
        confdir: std::env::var("CONFDIR").unwrap_or_else(|_| "/etc/firecrab".to_owned()),
        unitdir: std::env::var("UNITDIR").unwrap_or_else(|_| "/etc/systemd/system".to_owned()),
        api_base: api_base.to_owned(),
    }
}

/// Builds the plain-text rendering as a `String` — split out from
/// [`print_human`] so tests can assert on the formatted content without
/// capturing real stdout.
fn format_human(report: &InfoReport) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    writeln!(out, "firecrab {}", report.version).unwrap();
    writeln!(out, "  prefix:  {}", report.prefix).unwrap();
    writeln!(out, "  datadir: {}", report.datadir).unwrap();
    writeln!(out, "  confdir: {}", report.confdir).unwrap();
    writeln!(out, "  unitdir: {}", report.unitdir).unwrap();
    writeln!(out, "  api:     {}", report.api_base).unwrap();
    out
}

/// Plain-text rendering for a terminal (the default output mode).
pub fn print_human(report: &InfoReport) {
    print!("{}", format_human(report));
}

/// `--json` output mode, for scripting.
pub fn print_json(report: &InfoReport) {
    println!("{}", serde_json::to_string_pretty(report).unwrap());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collect_uses_cargo_pkg_version() {
        let report = collect("http://127.0.0.1:5523");
        assert_eq!(report.version, env!("CARGO_PKG_VERSION"));
        assert_eq!(report.api_base, "http://127.0.0.1:5523");
    }

    #[test]
    fn collect_falls_back_to_install_sh_defaults() {
        // No PREFIX/DATADIR/CONFDIR/UNITDIR set in this test process —
        // must match install.sh's own defaults exactly.
        let report = collect("http://127.0.0.1:5523");
        assert_eq!(report.prefix, "/usr/local");
        assert_eq!(report.datadir, "/var/lib/firecrab");
        assert_eq!(report.confdir, "/etc/firecrab");
        assert_eq!(report.unitdir, "/etc/systemd/system");
    }

    #[test]
    fn format_human_includes_all_fields() {
        let report = collect("http://127.0.0.1:5523");
        let text = format_human(&report);
        assert!(text.starts_with(&format!("firecrab {}\n", report.version)));
        assert!(text.contains(&format!("prefix:  {}", report.prefix)));
        assert!(text.contains(&format!("datadir: {}", report.datadir)));
        assert!(text.contains(&format!("confdir: {}", report.confdir)));
        assert!(text.contains(&format!("unitdir: {}", report.unitdir)));
        assert!(text.contains("api:     http://127.0.0.1:5523"));
    }

    #[test]
    fn print_human_and_print_json_do_not_panic() {
        let report = collect("http://127.0.0.1:5523");
        print_human(&report);
        print_json(&report);
    }

    #[test]
    fn print_json_output_parses_back_to_report_fields() {
        let report = collect("http://127.0.0.1:5523");
        let json = serde_json::to_string_pretty(&report).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["version"], report.version);
        assert_eq!(value["apiBase"], "http://127.0.0.1:5523");
    }
}
