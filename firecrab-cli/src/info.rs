use serde::Serialize;

/// Matches install.sh's PREFIX=/usr/local, LIBDIR/SHAREDIR derive from it,
/// DATADIR=/var/lib/firecrab, CONFDIR=/etc/firecrab, UNITDIR=/etc/systemd/system.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InfoReport {
    pub version: String,
    pub prefix: String,
    pub datadir: String,
    pub confdir: String,
    pub unitdir: String,
    pub api_base: String,
}

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

pub fn print_human(report: &InfoReport) {
    println!("firecrab {}", report.version);
    println!("  prefix:  {}", report.prefix);
    println!("  datadir: {}", report.datadir);
    println!("  confdir: {}", report.confdir);
    println!("  unitdir: {}", report.unitdir);
    println!("  api:     {}", report.api_base);
}

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
}
