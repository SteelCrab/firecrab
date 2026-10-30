//! Parse bounded image-scanner reports. The scanner is not connected yet.
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// One scanner-reported CVE affecting an installed image package.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Finding {
    pub cve: String,
    pub package: String,
    pub installed_version: String,
    pub fixed_version: Option<String>,
    pub severity: String,
    pub source: String,
    pub target: String,
}

/// Convert a scanner report into bounded, evidence-linked CVE records.
/// A report without a matching pinned target or package results is not a clean scan.
pub(super) fn parse_report(bytes: &[u8], expected: &str) -> Result<Vec<Finding>, &'static str> {
    if bytes.len() > 16 * 1024 * 1024 {
        return Err("Scanner report exceeds the output limit");
    }
    let doc: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|_| "Invalid scanner report")?;
    if doc["SchemaVersion"].as_u64() != Some(2) || doc["ArtifactName"].as_str() != Some(expected) {
        return Err("Scanner report does not match the pinned image");
    }
    let results = doc["Results"]
        .as_array()
        .ok_or("Scanner found no supported package inventory")?;
    if results.is_empty() {
        return Err("Scanner found no supported package inventory");
    }
    if !results.iter().any(|result| {
        result["Packages"]
            .as_array()
            .is_some_and(|packages| !packages.is_empty())
    }) {
        return Err("Scanner found no supported package inventory");
    }
    let mut findings = Vec::new();
    let mut seen = BTreeSet::new();
    for result in results {
        let target = result["Target"]
            .as_str()
            .ok_or("Scanner report has no package target")?;
        if target.len() > 512 {
            return Err("Scanner target exceeds the limit");
        }
        let Some(vulns) = result.get("Vulnerabilities") else {
            continue;
        };
        let vulns = vulns
            .as_array()
            .ok_or("Invalid scanner vulnerability list")?;
        for v in vulns {
            let cve = v["VulnerabilityID"].as_str().unwrap_or("");
            // The UI promises CVEs. Other vendor advisories may not have a CVE ID.
            if !valid_cve(cve) {
                continue;
            }
            let package = v["PkgName"]
                .as_str()
                .filter(|x| !x.is_empty())
                .ok_or("Scanner finding has no package")?;
            let version = v["InstalledVersion"]
                .as_str()
                .filter(|x| !x.is_empty())
                .ok_or("Scanner finding has no installed version")?;
            if package.len() > 256 || version.len() > 256 {
                return Err("Scanner finding exceeds the limit");
            }
            let key = (
                target.to_owned(),
                cve.to_owned(),
                package.to_owned(),
                version.to_owned(),
            );
            if !seen.insert(key) {
                continue;
            }
            if findings.len() >= 10_000 {
                return Err("Scanner returned too many findings");
            }
            let severity = match v["Severity"].as_str().unwrap_or("UNKNOWN") {
                "LOW" => "low",
                "MEDIUM" => "medium",
                "HIGH" => "high",
                "CRITICAL" => "critical",
                _ => "unknown",
            };
            let source = format!("https://nvd.nist.gov/vuln/detail/{cve}");
            let fixed = v["FixedVersion"]
                .as_str()
                .filter(|x| !x.is_empty() && x.len() <= 256)
                .map(str::to_owned);
            findings.push(Finding {
                cve: cve.into(),
                package: package.into(),
                installed_version: version.into(),
                fixed_version: fixed,
                severity: severity.into(),
                source,
                target: target.into(),
            });
        }
    }
    Ok(findings)
}

fn valid_cve(value: &str) -> bool {
    let Some(rest) = value.strip_prefix("CVE-") else {
        return false;
    };
    let Some((year, number)) = rest.split_once('-') else {
        return false;
    };
    year.len() == 4
        && year.bytes().all(|c| c.is_ascii_digit())
        && (4..=10).contains(&number.len())
        && number.bytes().all(|c| c.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn matches_pinned_image_and_preserves_cve_evidence() {
        let expected = "ghcr.io/team/app@sha256:abc";
        let report = json!({"SchemaVersion":2,"ArtifactName":expected,"Results":[{"Target":"alpine","Packages":[{"Name":"openssl","Version":"1.0"}],"Vulnerabilities":[{"VulnerabilityID":"CVE-2024-12345","PkgName":"openssl","InstalledVersion":"1.0","FixedVersion":"1.1","Severity":"HIGH"},{"VulnerabilityID":"GHSA-test","PkgName":"x","InstalledVersion":"1"}]}]});
        let bytes = serde_json::to_vec(&report).unwrap();
        let findings = parse_report(&bytes, expected).unwrap();
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, "high");
        assert_eq!(findings[0].fixed_version.as_deref(), Some("1.1"));
        assert_eq!(
            findings[0].source,
            "https://nvd.nist.gov/vuln/detail/CVE-2024-12345"
        );
        assert!(parse_report(&bytes, "another-image").is_err());
    }

    #[test]
    fn incomplete_or_invalid_reports_cannot_claim_a_clean_scan() {
        assert!(parse_report(br#"{"SchemaVersion":2,"ArtifactName":"x"}"#, "x").is_err());
        assert!(
            parse_report(
                br#"{"SchemaVersion":2,"ArtifactName":"x","Results":[]}"#,
                "x"
            )
            .is_err()
        );
        assert!(
            parse_report(
                br#"{"SchemaVersion":2,"ArtifactName":"x","Results":[{"Target":"unknown"}]}"#,
                "x"
            )
            .is_err()
        );
        assert!(parse_report(b"not json", "x").is_err());
        assert!(!valid_cve("CVE-2024-1234<script>"));
    }
}
