//! Live check of the key a running guest presents on port 22.
//!
//! The dashboard used to print `ssh-keyscan … | ssh-keygen -lf -` and ask the
//! operator to paste the result back. The Firecrab host can run that scan
//! itself, so the comparison happens where the expected key already lives —
//! under `{vms}/{id}/ssh/` — and the operator only reads the verdict.
//!
//! The fingerprint is computed here rather than by piping into `ssh-keygen`:
//! an OpenSSH `SHA256:…` fingerprint is base64 of the SHA-256 of the raw key
//! blob, so one pure function replaces a second process and stays testable
//! without a guest listening anywhere.

use std::process::Command;

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD};
use firecrab_api_types::{SshHostKeyCheckResponse, SshHostKeyCheckStatus};
use sha2::{Digest, Sha256};

/// Program that asks the guest which host key it serves.
const SSH_KEYSCAN: &str = "ssh-keyscan";
/// Seconds `ssh-keyscan` may spend on the connection.
///
/// A stopped guest must not hold the dashboard's request open: the tab runs
/// this check on open, so the unreachable verdict has to arrive quickly.
const KEYSCAN_TIMEOUT_SECS: &str = "5";
/// Key type Firecrab generates, and therefore the only one worth scanning.
const KEY_TYPE: &str = "ed25519";
/// Key-type field naming an ed25519 key in an `ssh-keyscan` line.
const KEY_PREFIX: &str = "ssh-ed25519";

/// `SHA256:…` of the ed25519 key in one `ssh-keyscan` body.
///
/// Returns `None` for an empty body, which is what an unreachable guest
/// produces: `ssh-keyscan` reports the failure on stderr and still exits 0.
pub fn fingerprint_from_keyscan(output: &str) -> Option<String> {
    output
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .find_map(|line| {
            let mut fields = line.split_whitespace().skip_while(|f| *f != KEY_PREFIX);
            fingerprint_of_blob(fields.nth(1)?)
        })
}

/// `SHA256:…` for one base64 OpenSSH key blob.
fn fingerprint_of_blob(blob: &str) -> Option<String> {
    let raw = STANDARD.decode(blob).ok()?;
    let digest = Sha256::digest(&raw);
    Some(format!("SHA256:{}", STANDARD_NO_PAD.encode(digest)))
}

/// Asks `address` for its ed25519 host key.
///
/// The address comes from Firecrab's own IPAM, and `Command` never involves a
/// shell, so it is passed through as one argument.
pub fn keyscan(address: &str) -> Result<String, String> {
    let mut command = Command::new(SSH_KEYSCAN);
    if address.contains(':') {
        command.arg("-6");
    }
    let output = command
        .args(["-T", KEYSCAN_TIMEOUT_SECS, "-t", KEY_TYPE, address])
        .output()
        .map_err(|source| format!("{SSH_KEYSCAN}: {source}"))?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr);
        let detail = detail.trim();
        return Err(if detail.is_empty() {
            format!("{SSH_KEYSCAN} exited {}", output.status)
        } else {
            detail.to_owned()
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Turns one scan outcome into the payload the dashboard renders.
///
/// Kept apart from [`keyscan`] so every verdict is reachable in tests without
/// a listening guest.
pub fn decide(
    expected: Option<&str>,
    address: Option<&str>,
    scan: Result<String, String>,
) -> SshHostKeyCheckResponse {
    let mut check = SshHostKeyCheckResponse {
        status: SshHostKeyCheckStatus::NoHostKey,
        address: address.map(ToOwned::to_owned),
        expected: expected.map(ToOwned::to_owned),
        observed: None,
        detail: None,
    };

    let Some(expected) = expected else {
        // Nothing was injected yet, so there is nothing a scan could confirm.
        check.address = None;
        return check;
    };
    if address.is_none() {
        check.status = SshHostKeyCheckStatus::NoAddress;
        return check;
    }

    match scan {
        Err(detail) => {
            check.status = SshHostKeyCheckStatus::Unreachable;
            check.detail = Some(detail);
        }
        Ok(body) => match fingerprint_from_keyscan(&body) {
            Some(observed) => {
                check.status = if observed == expected {
                    SshHostKeyCheckStatus::Match
                } else {
                    SshHostKeyCheckStatus::Mismatch
                };
                check.observed = Some(observed);
            }
            None => {
                check.status = SshHostKeyCheckStatus::Unreachable;
                check.detail = Some(format!("no {KEY_TYPE} host key answered"));
            }
        },
    }
    check
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real `ssh-keygen -t ed25519` output, so the vector is not hand-rolled.
    const PUBLIC_KEY: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIIW+DXHqUG6EROzHfmTTaxXUzHc8hNJy7onrB9SctU4h";
    /// `ssh-keygen -lf` of that same key.
    const FINGERPRINT: &str = "SHA256:ulsrox/ylwdub8WsAxNsVcw+zuxeoiZo8nYo5KEuO+4";

    fn scan_body() -> String {
        format!("172.30.0.3 {PUBLIC_KEY}\n")
    }

    #[test]
    fn keyscan_line_yields_the_openssh_fingerprint() {
        assert_eq!(
            fingerprint_from_keyscan(&scan_body()).as_deref(),
            Some(FINGERPRINT)
        );
    }

    #[test]
    fn comment_and_blank_lines_are_skipped() {
        let body = format!("# 172.30.0.3:22 SSH-2.0-OpenSSH_9.6\n\n172.30.0.3 {PUBLIC_KEY}\n");
        assert_eq!(
            fingerprint_from_keyscan(&body).as_deref(),
            Some(FINGERPRINT)
        );
    }

    /// A bracketed non-standard port keeps the key fields in the same order.
    #[test]
    fn a_bracketed_address_still_yields_the_fingerprint() {
        let body = format!("[172.30.0.3]:2222 {PUBLIC_KEY}\n");
        assert_eq!(
            fingerprint_from_keyscan(&body).as_deref(),
            Some(FINGERPRINT)
        );
    }

    #[test]
    fn a_body_without_an_ed25519_key_has_no_fingerprint() {
        assert_eq!(fingerprint_from_keyscan(""), None);
        assert_eq!(
            fingerprint_from_keyscan("172.30.0.3 ssh-rsa AAAAB3Nza"),
            None
        );
        assert_eq!(fingerprint_from_keyscan("not a key at all"), None);
    }

    /// A truncated blob is not valid base64, so it yields nothing.
    #[test]
    fn an_undecodable_blob_has_no_fingerprint() {
        assert_eq!(
            fingerprint_from_keyscan("172.30.0.3 ssh-ed25519 not!base64"),
            None
        );
    }

    #[test]
    fn the_injected_key_coming_back_is_a_match() {
        let check = decide(Some(FINGERPRINT), Some("172.30.0.3"), Ok(scan_body()));
        assert_eq!(check.status, SshHostKeyCheckStatus::Match);
        assert_eq!(check.observed.as_deref(), Some(FINGERPRINT));
        assert_eq!(check.address.as_deref(), Some("172.30.0.3"));
    }

    #[test]
    fn another_guests_key_is_a_mismatch() {
        let check = decide(
            Some("SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"),
            Some("172.30.0.3"),
            Ok(scan_body()),
        );
        assert_eq!(check.status, SshHostKeyCheckStatus::Mismatch);
        assert_eq!(check.observed.as_deref(), Some(FINGERPRINT));
    }

    #[test]
    fn a_failed_scan_is_unreachable_and_keeps_the_reason() {
        let check = decide(
            Some(FINGERPRINT),
            Some("172.30.0.3"),
            Err("connection timed out".to_owned()),
        );
        assert_eq!(check.status, SshHostKeyCheckStatus::Unreachable);
        assert_eq!(check.detail.as_deref(), Some("connection timed out"));
        assert_eq!(check.observed, None);
    }

    #[test]
    fn a_silent_scan_is_unreachable_rather_than_a_mismatch() {
        let check = decide(Some(FINGERPRINT), Some("172.30.0.3"), Ok(String::new()));
        assert_eq!(check.status, SshHostKeyCheckStatus::Unreachable);
        assert_eq!(check.observed, None);
        assert!(check.detail.is_some(), "the empty body must be explained");
    }

    #[test]
    fn a_vm_without_an_address_is_never_scanned() {
        let check = decide(Some(FINGERPRINT), None, Ok(scan_body()));
        assert_eq!(check.status, SshHostKeyCheckStatus::NoAddress);
        assert_eq!(check.observed, None);
        assert_eq!(check.expected.as_deref(), Some(FINGERPRINT));
    }

    #[test]
    fn a_vm_that_never_started_has_no_host_key_to_compare() {
        let check = decide(None, Some("172.30.0.3"), Ok(scan_body()));
        assert_eq!(check.status, SshHostKeyCheckStatus::NoHostKey);
        assert_eq!(check.expected, None);
    }

    /// The scan runs a real program, so a bad address must return an error
    /// rather than hanging the request or panicking.
    #[test]
    fn scanning_an_unroutable_address_reports_a_reason() {
        match keyscan("0.0.0.1") {
            Err(detail) => assert!(!detail.is_empty(), "a failure must say why"),
            Ok(body) => assert_eq!(fingerprint_from_keyscan(&body), None),
        }
    }
}
