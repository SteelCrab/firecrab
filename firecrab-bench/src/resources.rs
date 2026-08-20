use std::fs;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;

/// Metadata identifying one benchmark execution and its environment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunMetadata {
    /// Unique identifier for this benchmark result.
    pub run_id: Uuid,
    /// Git commit supplied by CI, or `unknown` for a local run.
    pub commit_sha: String,
    /// Git branch supplied by CI, or `unknown` for a local run.
    pub branch: String,
    /// RFC 3339 UTC timestamp recorded when the result is assembled.
    pub timestamp: String,
    /// Benchmark host name.
    pub host: String,
    /// Version of the benchmark binary and Firecrab workspace.
    pub firecrab_version: String,
    /// Host kernel release.
    pub kernel_version: String,
}

impl RunMetadata {
    /// Captures metadata from the process and Linux host environment.
    pub fn capture() -> Self {
        Self {
            run_id: Uuid::new_v4(),
            commit_sha: environment_value("GITHUB_SHA"),
            branch: std::env::var("GITHUB_HEAD_REF")
                .ok()
                .filter(|value| !value.is_empty())
                .or_else(|| {
                    std::env::var("GITHUB_REF_NAME")
                        .ok()
                        .filter(|value| !value.is_empty())
                })
                .unwrap_or_else(|| "unknown".to_owned()),
            timestamp: OffsetDateTime::now_utc()
                .format(&Rfc3339)
                .unwrap_or_else(|_| "unknown".to_owned()),
            host: std::env::var("HOSTNAME")
                .ok()
                .filter(|value| !value.is_empty())
                .or_else(|| read_trimmed("/etc/hostname"))
                .unwrap_or_else(|| "unknown".to_owned()),
            firecrab_version: env!("CARGO_PKG_VERSION").to_owned(),
            kernel_version: read_trimmed("/proc/sys/kernel/osrelease")
                .unwrap_or_else(|| "unknown".to_owned()),
        }
    }
}

/// Host resource usage captured across one benchmark command.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct HostResourceUsage {
    /// Average host CPU busy percentage during the command.
    pub cpu_percent: Option<f64>,
    /// Used host memory in MiB at command completion.
    pub memory_used_mib: Option<u64>,
    /// Total host memory in MiB.
    pub memory_total_mib: Option<u64>,
    /// One-minute host load average at command completion.
    pub load_average_1m: Option<f64>,
    /// Firecracker processes present at command completion.
    pub firecracker_process_count: Option<u32>,
}

#[derive(Debug, Clone, Copy)]
struct CpuTimes {
    total: u64,
    idle: u64,
}

pub(crate) struct HostResourceSampler {
    initial_cpu: Option<CpuTimes>,
}

impl HostResourceSampler {
    pub(crate) fn start() -> Self {
        Self {
            initial_cpu: read_cpu_times(),
        }
    }

    pub(crate) fn finish(self) -> HostResourceUsage {
        let cpu_percent = self
            .initial_cpu
            .zip(read_cpu_times())
            .and_then(|(initial, final_times)| cpu_percent(initial, final_times));
        let (memory_used_mib, memory_total_mib) = read_memory_usage()
            .map(|(used, total)| (Some(used), Some(total)))
            .unwrap_or((None, None));
        HostResourceUsage {
            cpu_percent,
            memory_used_mib,
            memory_total_mib,
            load_average_1m: read_load_average(),
            firecracker_process_count: Some(count_firecracker_processes()),
        }
    }
}

fn environment_value(name: &str) -> String {
    std::env::var(name)
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "unknown".to_owned())
}

fn read_trimmed(path: &str) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn read_cpu_times() -> Option<CpuTimes> {
    parse_cpu_times(&fs::read_to_string("/proc/stat").ok()?)
}

fn parse_cpu_times(contents: &str) -> Option<CpuTimes> {
    let values = contents
        .lines()
        .next()?
        .strip_prefix("cpu ")?
        .split_whitespace()
        .map(str::parse::<u64>)
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    if values.len() < 4 {
        return None;
    }
    let idle = values[3] + values.get(4).copied().unwrap_or(0);
    Some(CpuTimes {
        total: values.iter().sum(),
        idle,
    })
}

fn cpu_percent(initial: CpuTimes, final_times: CpuTimes) -> Option<f64> {
    let total = final_times.total.checked_sub(initial.total)?;
    let idle = final_times.idle.checked_sub(initial.idle)?;
    if total == 0 {
        return None;
    }
    Some((total.saturating_sub(idle)) as f64 * 100.0 / total as f64)
}

fn read_memory_usage() -> Option<(u64, u64)> {
    parse_memory_usage(&fs::read_to_string("/proc/meminfo").ok()?)
}

fn parse_memory_usage(contents: &str) -> Option<(u64, u64)> {
    let mut total_kib = None;
    let mut available_kib = None;
    for line in contents.lines() {
        let mut fields = line.split_whitespace();
        match fields.next() {
            Some("MemTotal:") => total_kib = fields.next()?.parse::<u64>().ok(),
            Some("MemAvailable:") => available_kib = fields.next()?.parse::<u64>().ok(),
            _ => {}
        }
    }
    let total_kib = total_kib?;
    let available_kib = available_kib?;
    Some((
        total_kib.checked_sub(available_kib)? / 1024,
        total_kib / 1024,
    ))
}

fn read_load_average() -> Option<f64> {
    fs::read_to_string("/proc/loadavg")
        .ok()?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

pub(crate) fn count_firecracker_processes() -> u32 {
    let Ok(entries) = fs::read_dir("/proc") else {
        return 0;
    };
    entries
        .flatten()
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .bytes()
                .all(|byte| byte.is_ascii_digit())
        })
        .filter(|entry| {
            fs::read_to_string(entry.path().join("comm"))
                .is_ok_and(|name| name.trim() == "firecracker")
        })
        .count() as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cpu_times_and_busy_percentage() {
        let initial = parse_cpu_times("cpu  100 10 20 400 30 0 0 0\n").unwrap();
        let final_times = parse_cpu_times("cpu  140 10 30 450 30 0 0 0\n").unwrap();
        assert_eq!(cpu_percent(initial, final_times), Some(50.0));
    }

    #[test]
    fn parses_used_and_total_memory_in_mib() {
        let contents = "MemTotal:       32768 kB\nMemAvailable:   8192 kB\n";
        assert_eq!(parse_memory_usage(contents), Some((24, 32)));
    }

    #[test]
    fn rejects_incomplete_proc_values() {
        assert!(parse_cpu_times("cpu  1 2 3\n").is_none());
        assert!(parse_memory_usage("MemTotal: 1024 kB\n").is_none());
    }

    #[test]
    fn captures_common_run_metadata() {
        let metadata = RunMetadata::capture();
        assert_ne!(metadata.run_id, Uuid::nil());
        assert!(metadata.timestamp.contains('T'));
        assert!(!metadata.firecrab_version.is_empty());
    }

    #[test]
    fn samples_linux_host_resources() {
        let usage = HostResourceSampler::start().finish();
        assert!(usage.memory_total_mib.is_some());
        assert!(usage.load_average_1m.is_some());
        assert!(usage.firecracker_process_count.is_some());
    }
}
