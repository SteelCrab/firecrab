//! Guest OS resource samples for running VMs (Firecrab Metrics Agent).
//!
//! Source: lines printed by `firecrab-guest-agent` (see [`crate::guest_agent`])
//! on the serial console (`FIRECRAB_USAGE …`). Values are **guest** CPU % and
//! memory used (CloudWatch Agent–style: MemTotal − MemAvailable), not host
//! Firecracker process RSS. Interactive terminals strip those lines via
//! [`crate::console::ConsoleBroker`]; the metrics tracker still reads the
//! raw console stream.

use std::collections::{HashMap, VecDeque};
use std::time::{SystemTime, UNIX_EPOCH};

use firecrab_api_types::VmUsageSample;
use uuid::Uuid;

/// How many samples to keep per VM (dashboard polls ~3s → ~3 minutes).
const HISTORY_CAP: usize = 60;

/// Prefix of every agent report line (must stay stable for parsers).
pub const USAGE_LINE_PREFIX: &str = "FIRECRAB_USAGE";

/// Latest guest sample for one VM, plus sparkline history.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct UsageSnapshot {
    /// Guest CPU busy percent (0–100 typical; multi-core averaged).
    pub cpu_usage_percent: Option<f32>,
    /// Guest used memory in MiB (`MemTotal − MemAvailable`).
    pub memory_used_mib: Option<u64>,
    /// Guest total memory in MiB (`MemTotal`).
    pub memory_total_mib: Option<u64>,
    /// Guest used memory percent of MemTotal.
    pub memory_used_percent: Option<f32>,
    /// Oldest-first recent samples.
    pub history: Vec<VmUsageSample>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct GuestSample {
    cpu_usage_percent: Option<f32>,
    memory_used_mib: Option<u64>,
    memory_total_mib: Option<u64>,
    memory_used_percent: Option<f32>,
}

/// Per-VM latest guest sample and ring buffer for graphs.
#[derive(Debug, Default)]
pub struct ProcessMetricsTracker {
    latest: HashMap<Uuid, GuestSample>,
    history: HashMap<Uuid, VecDeque<VmUsageSample>>,
    /// Incomplete UTF-8 / partial lines across console chunks.
    line_bufs: HashMap<Uuid, String>,
}

impl ProcessMetricsTracker {
    /// Returns the last guest sample and history without sampling the host.
    pub fn snapshot(&self, id: Uuid) -> UsageSnapshot {
        let Some(sample) = self.latest.get(&id).copied() else {
            return UsageSnapshot {
                history: self
                    .history
                    .get(&id)
                    .map(|ring| ring.iter().cloned().collect())
                    .unwrap_or_default(),
                ..UsageSnapshot::default()
            };
        };
        UsageSnapshot {
            cpu_usage_percent: sample.cpu_usage_percent,
            memory_used_mib: sample.memory_used_mib,
            memory_total_mib: sample.memory_total_mib,
            memory_used_percent: sample.memory_used_percent,
            history: self
                .history
                .get(&id)
                .map(|ring| ring.iter().cloned().collect())
                .unwrap_or_default(),
        }
    }

    /// Feeds raw console output; extracts complete `FIRECRAB_USAGE` lines.
    pub fn ingest_console(&mut self, id: Uuid, chunk: &[u8]) {
        let text = String::from_utf8_lossy(chunk);
        let buf = self.line_bufs.entry(id).or_default();
        buf.push_str(&text);
        // Cap partial buffer so a flood cannot grow unbounded.
        const MAX_PARTIAL: usize = 8 * 1024;
        if buf.len() > MAX_PARTIAL {
            let excess = buf.len() - MAX_PARTIAL;
            buf.drain(..excess);
        }
        let mut complete = Vec::new();
        while let Some(pos) = buf.find('\n') {
            let mut line = buf.drain(..=pos).collect::<String>();
            if line.ends_with('\n') {
                line.pop();
            }
            if line.ends_with('\r') {
                line.pop();
            }
            complete.push(line);
        }
        for line in complete {
            if let Some(sample) = parse_usage_line(&line) {
                self.record(id, sample);
            }
        }
    }

    /// Drops state when a process is no longer tracked.
    pub fn clear(&mut self, id: Uuid) {
        self.latest.remove(&id);
        self.history.remove(&id);
        self.line_bufs.remove(&id);
    }

    fn record(&mut self, id: Uuid, sample: GuestSample) {
        self.latest.insert(id, sample);
        let point = VmUsageSample {
            at_ms: unix_ms_now(),
            cpu_usage_percent: sample.cpu_usage_percent,
            memory_used_mib: sample.memory_used_mib,
            memory_total_mib: sample.memory_total_mib,
            memory_used_percent: sample.memory_used_percent,
        };
        let ring = self.history.entry(id).or_default();
        ring.push_back(point);
        while ring.len() > HISTORY_CAP {
            ring.pop_front();
        }
    }
}

fn unix_ms_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Parses `FIRECRAB_USAGE cpu_pct=1.2 mem_used_mib=3 mem_total_mib=4 mem_used_pct=5.6`.
fn parse_usage_line(line: &str) -> Option<GuestSample> {
    let line = line.trim();
    // Allow ANSI / getty noise before the marker.
    let start = line.find(USAGE_LINE_PREFIX)?;
    let rest = line[start + USAGE_LINE_PREFIX.len()..].trim();
    if rest.is_empty() {
        return None;
    }

    let mut cpu_usage_percent = None;
    let mut memory_used_mib = None;
    let mut memory_total_mib = None;
    let mut memory_used_percent = None;

    for token in rest.split_whitespace() {
        let Some((key, value)) = token.split_once('=') else {
            continue;
        };
        match key {
            "cpu_pct" => cpu_usage_percent = value.parse().ok(),
            "mem_used_mib" => memory_used_mib = value.parse().ok(),
            "mem_total_mib" => memory_total_mib = value.parse().ok(),
            "mem_used_pct" => memory_used_percent = value.parse().ok(),
            _ => {}
        }
    }

    if cpu_usage_percent.is_none()
        && memory_used_mib.is_none()
        && memory_total_mib.is_none()
        && memory_used_percent.is_none()
    {
        return None;
    }

    Some(GuestSample {
        cpu_usage_percent,
        memory_used_mib,
        memory_total_mib,
        memory_used_percent,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_usage_line_full() {
        let s = parse_usage_line(
            "FIRECRAB_USAGE cpu_pct=12.5 mem_used_mib=900 mem_total_mib=2048 mem_used_pct=44.0",
        )
        .unwrap();
        assert_eq!(s.cpu_usage_percent, Some(12.5));
        assert_eq!(s.memory_used_mib, Some(900));
        assert_eq!(s.memory_total_mib, Some(2048));
        assert_eq!(s.memory_used_percent, Some(44.0));
    }

    #[test]
    fn parse_usage_line_tolerates_prefix_noise() {
        let s = parse_usage_line(
            "\x1b[0mFIRECRAB_USAGE cpu_pct=1.0 mem_used_mib=10 mem_total_mib=100 mem_used_pct=10.0\n",
        )
        .unwrap();
        assert_eq!(s.cpu_usage_percent, Some(1.0));
    }

    #[test]
    fn ingest_console_splits_across_chunks() {
        let mut tracker = ProcessMetricsTracker::default();
        let id = Uuid::from_u128(1);
        tracker.ingest_console(id, b"FIRECRAB_USAGE cpu_pct=2.0 mem_used_");
        assert!(tracker.snapshot(id).cpu_usage_percent.is_none());
        tracker.ingest_console(id, b"mib=50 mem_total_mib=200 mem_used_pct=25.0\ntrailing");
        let snap = tracker.snapshot(id);
        assert_eq!(snap.cpu_usage_percent, Some(2.0));
        assert_eq!(snap.memory_used_mib, Some(50));
        assert_eq!(snap.memory_total_mib, Some(200));
        assert_eq!(snap.memory_used_percent, Some(25.0));
        assert_eq!(snap.history.len(), 1);
    }

    #[test]
    fn history_is_capped() {
        let mut tracker = ProcessMetricsTracker::default();
        let id = Uuid::from_u128(9);
        for i in 0..(HISTORY_CAP + 5) {
            tracker.ingest_console(
                id,
                format!("FIRECRAB_USAGE cpu_pct={i}.0 mem_used_mib=1 mem_total_mib=2 mem_used_pct=50.0\n")
                    .as_bytes(),
            );
        }
        assert_eq!(tracker.snapshot(id).history.len(), HISTORY_CAP);
    }

    #[test]
    fn clear_drops_state() {
        let mut tracker = ProcessMetricsTracker::default();
        let id = Uuid::from_u128(2);
        tracker.ingest_console(
            id,
            b"FIRECRAB_USAGE cpu_pct=9.0 mem_used_mib=1 mem_total_mib=2 mem_used_pct=50.0\n",
        );
        tracker.clear(id);
        assert_eq!(tracker.snapshot(id), UsageSnapshot::default());
    }

    #[test]
    fn ignores_unrelated_lines() {
        let mut tracker = ProcessMetricsTracker::default();
        let id = Uuid::from_u128(3);
        tracker.ingest_console(id, b"FIRECRAB_NETWORK_READY 1.2.3.4\n");
        assert_eq!(tracker.snapshot(id), UsageSnapshot::default());
    }
}
