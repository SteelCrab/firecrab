//! Allocate persistent backend listeners outside the host's outgoing TCP range.

use std::collections::HashSet;
use std::io;
use std::net::{Ipv4Addr, TcpListener};
use std::ops::RangeInclusive;
#[cfg(any(target_os = "macos", target_os = "windows"))]
use std::process::Command;

/// Keep new listeners below the usual dynamic ranges without migrating saved ports.
const FIRST: u16 = 20_000;
/// Scan every candidate exactly once from a random starting point.
const COUNT: u16 = 12_768;

/// Hold a free listener until the caller persists its reservation.
pub(super) fn available(reserved: &HashSet<u16>) -> io::Result<TcpListener> {
    let ephemeral = ephemeral_range().map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("could not read the host ephemeral TCP port range: {error}"),
        )
    })?;
    let bytes = uuid::Uuid::new_v4().into_bytes();
    let start = u16::from_le_bytes([bytes[0], bytes[1]]) % COUNT;
    bind_available(reserved, &ephemeral, start)
}

/// Skip saved reservations, outgoing ports, and listeners already owned by another process.
fn bind_available(
    reserved: &HashSet<u16>,
    ephemeral: &RangeInclusive<u16>,
    start: u16,
) -> io::Result<TcpListener> {
    for offset in 0..COUNT {
        let port = FIRST + (start + offset) % COUNT;
        if reserved.contains(&port) || ephemeral.contains(&port) {
            continue;
        }
        match TcpListener::bind((Ipv4Addr::LOCALHOST, port)) {
            Ok(listener) => return Ok(listener),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::AddrInUse | io::ErrorKind::PermissionDenied
                ) => {}
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AddrInUse,
        "no free development API/resource port in 20000..32767 outside the host ephemeral range",
    ))
}

/// Reject ambiguous OS output rather than silently allocating outgoing ports.
fn invalid_range() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "invalid ephemeral TCP port range",
    )
}

/// Validate inclusive endpoints before using them to exclude candidates.
fn range(first: u32, last: u32) -> io::Result<RangeInclusive<u16>> {
    if first == 0 || first > last || last > u32::from(u16::MAX) {
        return Err(invalid_range());
    }
    Ok(first as u16..=last as u16)
}

/// Read two endpoints; Darwin also permits descending automatic port selection.
#[cfg(any(target_os = "linux", target_os = "macos", test))]
fn parse_range(text: &str, allow_descending: bool) -> io::Result<RangeInclusive<u16>> {
    let values = text
        .split_whitespace()
        .map(str::parse::<u32>)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| invalid_range())?;
    let [first, last] = values.as_slice() else {
        return Err(invalid_range());
    };
    if allow_descending {
        range(*first.min(last), *first.max(last))
    } else {
        range(*first, *last)
    }
}

/// Read the active namespace's range without modifying sysctl or reserved ports.
#[cfg(target_os = "linux")]
fn ephemeral_range() -> io::Result<RangeInclusive<u16>> {
    parse_range(
        &std::fs::read_to_string("/proc/sys/net/ipv4/ip_local_port_range")?,
        false,
    )
}

/// macOS uses these TCP endpoints for automatic local port selection.
#[cfg(target_os = "macos")]
fn ephemeral_range() -> io::Result<RangeInclusive<u16>> {
    let output = Command::new("/usr/sbin/sysctl")
        .args([
            "-n",
            "net.inet.ip.portrange.first",
            "net.inet.ip.portrange.last",
        ])
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other(
            "sysctl could not report the TCP port range",
        ));
    }
    parse_range(&String::from_utf8_lossy(&output.stdout), true)
}

/// netsh localizes labels and may use an OEM code page; only numeric field values matter.
#[cfg(any(target_os = "windows", test))]
fn parse_netsh_range(text: &str) -> io::Result<RangeInclusive<u16>> {
    let values = text
        .lines()
        .filter_map(|line| line.rsplit_once(':')?.1.trim().parse::<u32>().ok())
        .collect::<Vec<_>>();
    let [first, count] = values.as_slice() else {
        return Err(invalid_range());
    };
    let last = count
        .checked_sub(1)
        .and_then(|offset| first.checked_add(offset))
        .ok_or_else(invalid_range)?;
    range(*first, last)
}

/// Query active IPv4 TCP configuration through the native Windows network command.
#[cfg(target_os = "windows")]
fn ephemeral_range() -> io::Result<RangeInclusive<u16>> {
    let output = Command::new("netsh.exe")
        .args([
            "interface",
            "ipv4",
            "show",
            "dynamicportrange",
            "protocol=tcp",
        ])
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other(
            "netsh could not report the TCP port range",
        ));
    }
    parse_netsh_range(&String::from_utf8_lossy(&output.stdout))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_output_requires_two_valid_inclusive_endpoints() {
        assert_eq!(parse_range("32768\t60999\n", false).unwrap(), 32768..=60999);
        assert_eq!(parse_range("49152\n65535\n", false).unwrap(), 49152..=65535);
        assert_eq!(parse_range("65535\n49152\n", true).unwrap(), 49152..=65535);
        for text in ["", "49152", "0 65535", "65535 49152", "1 65536", "1 2 3"] {
            assert!(parse_range(text, false).is_err(), "{text:?}");
        }
        assert!(parse_range("65535 0", true).is_err());
    }

    #[test]
    fn windows_ranges_ignore_translated_labels_and_handle_counts_safely() {
        for text in [
            "Protocol tcp Dynamic Port Range\nStart Port : 49152\nNumber of Ports : 16384\n",
            "프로토콜 tcp 동적 포트 범위\n시작 포트 : 20000\n포트 수 : 1000\n",
        ] {
            let expected = if text.contains("49152") {
                49152..=65535
            } else {
                20000..=20999
            };
            assert_eq!(parse_netsh_range(text).unwrap(), expected);
        }
        let oem = String::from_utf8_lossy(b"\xff : 49152\n\xfe : 16384\n");
        assert_eq!(parse_netsh_range(&oem).unwrap(), 49152..=65535);
        for text in [
            "Start : 0\nCount : 1",
            "Start : 49152\nCount : 0",
            "Start : 49152\nCount : 16385",
            "Start : 4294967295\nCount : 2",
            "Start : 49152\nCount : invalid",
            "Start : 49152\nCount : 16384\nExtra : 1",
        ] {
            assert!(parse_netsh_range(text).is_err(), "{text:?}");
        }
    }

    #[test]
    fn allocator_skips_outgoing_ports_and_saved_reservations() {
        let ephemeral = 20_000..=26_000;
        let reserved = HashSet::from([26_001, 26_002]);
        let listener = bind_available(&reserved, &ephemeral, 0).unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!((FIRST..FIRST + COUNT).contains(&port));
        assert!(!ephemeral.contains(&port));
        assert!(!reserved.contains(&port));
    }

    #[test]
    fn allocator_preserves_an_occupied_listener_and_finds_another_port() {
        let held = bind_available(&HashSet::new(), &(49152..=65535), 0).unwrap();
        let port = held.local_addr().unwrap().port();
        let another = bind_available(&HashSet::new(), &(49152..=65535), port - FIRST).unwrap();
        assert_ne!(another.local_addr().unwrap().port(), port);
        assert!(held.local_addr().is_ok());
    }

    #[test]
    fn exhausted_candidate_ranges_fail_without_falling_back_to_ephemeral_ports() {
        let all = (FIRST..FIRST + COUNT).collect();
        for (reserved, ephemeral) in [(HashSet::new(), 1..=65535), (all, 49152..=65535)] {
            assert_eq!(
                bind_available(&reserved, &ephemeral, 0).unwrap_err().kind(),
                io::ErrorKind::AddrInUse
            );
        }
    }
}
