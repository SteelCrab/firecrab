//! Host Firecracker process CPU and RSS sampling for running VMs.
//!
//! Values come from Linux `/proc/<pid>` and describe the **host process**,
//! not guest-internal free memory or guest CPU accounting. CPU percent is
//! derived from utime+stime jiffy deltas between successive samples.
//! A bounded ring buffer keeps recent points for dashboard sparklines.

use std::collections::{HashMap, VecDeque};
use std::fs;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use firecrab_api_types::VmUsageSample;
use uuid::Uuid;

/// How many samples to keep per VM (dashboard polls ~3s → ~3 minutes).
const HISTORY_CAP: usize = 60;

/// Latest CPU and memory sample for one VM, plus sparkline history.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct UsageSnapshot {
    /// Percent of one host core since the previous sample, if known.
    pub cpu_usage_percent: Option<f32>,
    /// Process RSS in MiB, if `/proc` could be read.
    pub memory_used_mib: Option<u64>,
    /// Oldest-first recent samples (includes this observation when present).
    pub history: Vec<VmUsageSample>,
}

#[derive(Debug, Clone, Copy)]
struct LastSample {
    at: Instant,
    jiffies: u64,
}

/// Per-VM previous jiffy sample and ring buffer for graphs.
#[derive(Debug, Default)]
pub struct ProcessMetricsTracker {
    last: HashMap<Uuid, LastSample>,
    history: HashMap<Uuid, VecDeque<VmUsageSample>>,
}

impl ProcessMetricsTracker {
    /// Samples `pid` and updates the running CPU delta for `id`.
    pub fn observe(&mut self, id: Uuid, pid: u32) -> UsageSnapshot {
        let Some(sample) = read_process_sample(pid) else {
            self.last.remove(&id);
            self.history.remove(&id);
            return UsageSnapshot::default();
        };

        let memory_used_mib = Some(sample.rss_kib / 1024);
        let cpu_usage_percent = self
            .last
            .get(&id)
            .and_then(|prev| cpu_percent(prev, &sample));

        self.last.insert(
            id,
            LastSample {
                at: sample.at,
                jiffies: sample.jiffies,
            },
        );

        let point = VmUsageSample {
            at_ms: unix_ms_now(),
            cpu_usage_percent,
            memory_used_mib,
        };
        let ring = self.history.entry(id).or_default();
        ring.push_back(point);
        while ring.len() > HISTORY_CAP {
            ring.pop_front();
        }

        UsageSnapshot {
            cpu_usage_percent,
            memory_used_mib,
            history: ring.iter().cloned().collect(),
        }
    }

    /// Drops the previous sample and history when a process is no longer tracked.
    pub fn clear(&mut self, id: Uuid) {
        self.last.remove(&id);
        self.history.remove(&id);
    }
}

#[derive(Debug, Clone, Copy)]
struct RawSample {
    at: Instant,
    jiffies: u64,
    rss_kib: u64,
}

fn unix_ms_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn read_process_sample(pid: u32) -> Option<RawSample> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let status = fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    let jiffies = parse_stat_jiffies(&stat)?;
    let rss_kib = parse_status_rss_kib(&status)?;
    Some(RawSample {
        at: Instant::now(),
        jiffies,
        rss_kib,
    })
}

fn cpu_percent(prev: &LastSample, sample: &RawSample) -> Option<f32> {
    if sample.jiffies < prev.jiffies {
        return None;
    }
    let elapsed = sample.at.duration_since(prev.at).as_secs_f32();
    if elapsed <= 0.0 {
        return None;
    }
    let delta_jiffies = (sample.jiffies - prev.jiffies) as f32;
    let hz = clock_ticks_per_second() as f32;
    if hz <= 0.0 {
        return None;
    }
    // Percent of one host CPU core; multi-vCPU VMs may exceed 100.
    Some((delta_jiffies / hz) / elapsed * 100.0)
}

fn clock_ticks_per_second() -> i64 {
    // SAFETY: `_SC_CLK_TCK` is a valid sysconf name; a negative return is
    // treated as a hard-coded fallback below.
    let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    if ticks > 0 { ticks } else { 100 }
}

/// Parses utime+stime jiffies from a `/proc/<pid>/stat` body.
///
/// The `comm` field is parenthesized and may contain spaces, so we split
/// after the last `)` and use field offsets from the man page (utime=14,
/// stime=15 overall → indices 11 and 12 after the state field).
pub(crate) fn parse_stat_jiffies(stat: &str) -> Option<u64> {
    let end = stat.rfind(')')?;
    let rest = stat.get(end + 1..)?.trim_start();
    let mut fields = rest.split_whitespace();
    // fields: state ppid pgrp session tty_nr tpgid flags minflt cminflt
    // majflt cmajflt utime stime ...
    let utime: u64 = fields.nth(11)?.parse().ok()?;
    let stime: u64 = fields.next()?.parse().ok()?;
    Some(utime.saturating_add(stime))
}

/// Parses `VmRSS` (kB) from a `/proc/<pid>/status` body.
pub(crate) fn parse_status_rss_kib(status: &str) -> Option<u64> {
    for line in status.lines() {
        let Some(rest) = line.strip_prefix("VmRSS:") else {
            continue;
        };
        return rest.split_whitespace().next()?.parse().ok();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn parses_stat_jiffies_with_spaces_in_comm() {
        // Synthetic line: utime=100 stime=50 after the (comm) section.
        let stat = "1 (fire cracker) S 0 0 0 0 -1 0 0 0 0 0 100 50 0 0 20 0 1 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0\n";
        assert_eq!(parse_stat_jiffies(stat), Some(150));
    }

    #[test]
    fn parses_vmrss_kib() {
        let status = "Name:\tfirecracker\nVmSize:\t204800 kB\nVmRSS:\t184320 kB\n";
        assert_eq!(parse_status_rss_kib(status), Some(184_320));
    }

    #[test]
    fn observe_first_sample_has_memory_only() {
        let mut tracker = ProcessMetricsTracker::default();
        let id = Uuid::from_u128(1);
        // Sample "self" — this test process always has a /proc entry.
        let pid = std::process::id();
        let first = tracker.observe(id, pid);
        assert!(first.memory_used_mib.is_some());
        assert!(first.cpu_usage_percent.is_none());
        assert_eq!(first.history.len(), 1);

        std::thread::sleep(Duration::from_millis(50));
        // Burn a little CPU so the second sample is non-zero more often.
        let mut n = 0u64;
        for i in 0..200_000 {
            n = n.wrapping_add(i);
        }
        std::hint::black_box(n);

        let second = tracker.observe(id, pid);
        assert!(second.memory_used_mib.is_some());
        assert!(second.cpu_usage_percent.is_some());
        assert!(second.cpu_usage_percent.unwrap() >= 0.0);
        assert_eq!(second.history.len(), 2);
    }

    #[test]
    fn history_is_capped() {
        let mut tracker = ProcessMetricsTracker::default();
        let id = Uuid::from_u128(9);
        let pid = std::process::id();
        for _ in 0..(HISTORY_CAP + 5) {
            let _ = tracker.observe(id, pid);
        }
        let snap = tracker.observe(id, pid);
        assert_eq!(snap.history.len(), HISTORY_CAP);
    }

    #[test]
    fn clear_drops_previous_sample() {
        let mut tracker = ProcessMetricsTracker::default();
        let id = Uuid::from_u128(2);
        let pid = std::process::id();
        let _ = tracker.observe(id, pid);
        tracker.clear(id);
        let again = tracker.observe(id, pid);
        assert!(again.cpu_usage_percent.is_none());
        assert!(again.memory_used_mib.is_some());
        assert_eq!(again.history.len(), 1);
    }

    #[test]
    fn missing_pid_returns_none() {
        let mut tracker = ProcessMetricsTracker::default();
        let snap = tracker.observe(Uuid::from_u128(3), u32::MAX);
        assert_eq!(snap, UsageSnapshot::default());
    }

    #[test]
    fn cpu_percent_from_jiffy_delta() {
        let prev = LastSample {
            at: Instant::now() - Duration::from_secs(1),
            jiffies: 100,
        };
        let sample = RawSample {
            at: Instant::now(),
            jiffies: 100 + clock_ticks_per_second() as u64, // ~1 second of one core
            rss_kib: 1024,
        };
        let pct = cpu_percent(&prev, &sample).unwrap();
        // About 100% of one core; allow clock skew on CI.
        assert!(pct > 50.0 && pct < 150.0, "pct={pct}");
    }
}
