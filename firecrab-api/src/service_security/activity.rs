//! Conservative signals from structured application logs with status codes.
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::collections::HashSet;

use crate::persistence::service_logs::ServiceLog;

/// A count-based signal; no request paths, addresses, or raw logs leave the host.
#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Signal {
    pub kind: &'static str,
    pub severity: &'static str,
    pub count: usize,
    pub source: &'static str,
    pub recommendation: &'static str,
}

/// Only JSON access records from current replicas in the last five minutes count.
pub(super) fn detect(
    logs: &[ServiceLog],
    current_instances: &HashSet<String>,
    now: DateTime<Utc>,
) -> (Vec<Signal>, usize) {
    let mut denied = 0;
    let mut records = 0;
    for log in logs {
        if !current_instances.contains(&log.instance_id) {
            continue;
        }
        let Ok(time) = DateTime::parse_from_rfc3339(&log.timestamp) else {
            continue;
        };
        let age = now.signed_duration_since(time.with_timezone(&Utc));
        if age.num_seconds() < 0 || age.num_seconds() >= 300 {
            continue;
        }
        for line in log.message.lines() {
            let Ok(record) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            if let Some(status) = record.get("status").and_then(|s| s.as_u64()) {
                records += 1;
                if matches!(status, 401 | 403) {
                    denied += 1;
                }
            }
        }
    }
    if denied < 10 {
        return (Vec::new(), records);
    }
    (
        vec![Signal {
            kind: "repeated_access_denials",
            severity: "medium",
            count: denied,
            source: "structured application logs with status codes",
            recommendation: "Review the matching application logs and authentication policy for repeated 401/403 status codes.",
        }],
        records,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn log(instance: &str, timestamp: &str, message: &str) -> ServiceLog {
        ServiceLog {
            id: 1,
            timestamp: timestamp.into(),
            stream: "stdout".into(),
            message: message.into(),
            instance_id: instance.into(),
        }
    }
    #[test]
    fn counts_only_current_recent_structured_denials_without_disclosing_logs() {
        let now = DateTime::parse_from_rfc3339("2026-10-30T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let instances = HashSet::from(["active".to_owned()]);
        let message = "{\"status\":401,\"path\":\"/secret\",\"token\":\"private\"}\n".repeat(10);
        let logs = vec![
            log("active", "2026-10-30T11:59:00Z", &message),
            log("old", "2026-10-30T11:59:00Z", &message),
            log("active", "2026-10-30T11:50:00Z", &message),
        ];
        let (signals, records) = detect(&logs, &instances, now);
        assert_eq!(records, 10);
        assert_eq!(signals.len(), 1);
        assert_eq!(signals[0].count, 10);
        let output = serde_json::to_string(&signals).unwrap();
        assert!(!output.contains("private"));
        assert!(!output.contains("/secret"));
    }
    #[test]
    fn ignores_non_json_or_single_denial() {
        let now = DateTime::parse_from_rfc3339("2026-10-30T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let logs = [log(
            "active",
            "2026-10-30T11:59:00Z",
            "401 denied\n{\"status\":403}",
        )];
        assert!(
            detect(&logs, &HashSet::from(["active".into()]), now)
                .0
                .is_empty()
        );
    }
}
