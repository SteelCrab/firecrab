import { useCallback, useEffect, useMemo, useState } from "react";
import type { VmResponse, VmState } from "../bindings";
import { listBenchmarks } from "../api/client";
import type { BenchmarkResult } from "../benchmark";
import { useI18n } from "../i18n";
import BenchmarkTrendChart, { type BenchmarkTrendPoint } from "./BenchmarkTrendChart";
import BenchmarkControls from "./BenchmarkControls";

const POLL_MILLIS = 15_000;
const HISTORY_POINTS = 20;
const VM_STATES: VmState[] = ["running", "starting", "stopping", "created", "stopped", "error"];

interface BenchmarksProps {
  vms: VmResponse[];
  vmsLoaded: boolean;
}

/** Benchmark trends, normalized result tables, and the current MicroVM state. */
export default function Benchmarks({ vms, vmsLoaded }: BenchmarksProps) {
  const { t } = useI18n();
  const [runs, setRuns] = useState<BenchmarkResult[]>([]);
  const [loaded, setLoaded] = useState(false);
  const [resultRefresh, setResultRefresh] = useState(0);
  const refreshResults = useCallback(() => setResultRefresh((value) => value + 1), []);

  useEffect(() => {
    let cancelled = false;
    const refresh = async () => {
      try {
        const next = await listBenchmarks();
        if (!cancelled) setRuns(next);
      } catch {
        // Keep the last successful history and retry on the next poll.
      } finally {
        if (!cancelled) setLoaded(true);
      }
    };
    refresh();
    const interval = setInterval(refresh, POLL_MILLIS);
    return () => {
      cancelled = true;
      clearInterval(interval);
    };
  }, [resultRefresh]);

  const latest = useMemo(() => {
    const values = new Map<string, BenchmarkResult>();
    for (const run of runs) if (!values.has(run.test)) values.set(run.test, run);
    return [...values.values()];
  }, [runs]);
  const trends = useMemo(() => buildTrends(runs), [runs]);
  const metricRows = useMemo(
    () => latest.flatMap((run) => Object.entries(run.metrics ?? {}).map(([name, value]) => ({ run, name, value }))),
    [latest],
  );
  const stateCounts = useMemo(() => {
    const counts = new Map<VmState, number>(VM_STATES.map((state) => [state, 0]));
    for (const vm of vms) counts.set(vm.state, (counts.get(vm.state) ?? 0) + 1);
    return counts;
  }, [vms]);

  return (
    <section className="panel benchmark-dashboard">
      <h2 className="panel-title">
        <span>{t("Benchmark Overview", "Benchmark 개요")}</span>
        <span className="poll-note">{t("Results every 15s · VMs every 3s", "결과 15초 · VM 3초 간격 갱신")}</span>
      </h2>

      <BenchmarkControls onResultPublished={refreshResults} />

      {!loaded ? (
        <div className="empty">{t("Loading benchmark results…", "Benchmark 결과 불러오는 중…")}</div>
      ) : runs.length === 0 ? (
        <div className="empty">{t("No benchmark results published", "게시된 Benchmark 결과 없음")}</div>
      ) : (
        <>
          <div className="benchmark-summary">
            {latest.slice(0, 6).map((run) => (
              <article className="benchmark-card" key={run.test}>
                <span>{displayTestName(run.test)}</span>
                <strong>{primaryMetric(run)}</strong>
                <small>{shortCommit(run.run.commit_sha)}</small>
              </article>
            ))}
          </div>

          <section className="benchmark-section" aria-labelledby="benchmark-trends-title">
            <h3 id="benchmark-trends-title">{t("Performance trends", "성능 추세")}</h3>
            <div className="benchmark-chart-grid">
              <BenchmarkTrendChart
                title={t("Boot P95 latency", "부팅 P95 지연 시간")}
                points={trends.bootP95}
                unit=" ms"
                color="var(--ember)"
                fill="rgba(196, 62, 18, 0.12)"
                emptyLabel={t("No boot latency samples", "부팅 지연 시간 샘플 없음")}
              />
              <BenchmarkTrendChart
                title={t("Failure rate", "실패율")}
                points={trends.failureRate}
                unit="%"
                color="var(--error)"
                fill="rgba(179, 38, 30, 0.10)"
                yMaxHint={5}
                emptyLabel={t("No failure samples", "실패율 샘플 없음")}
              />
              <BenchmarkTrendChart
                title={t("Host CPU", "Host CPU")}
                points={trends.hostCpu}
                unit="%"
                color="var(--ready)"
                fill="rgba(21, 127, 99, 0.10)"
                yMaxHint={100}
                emptyLabel={t("No host CPU samples", "Host CPU 샘플 없음")}
              />
            </div>
          </section>

          <section className="benchmark-section" aria-labelledby="benchmark-history-title">
            <h3 id="benchmark-history-title">{t("Run history", "실행 이력")}</h3>
            <div className="table-scroll">
              <table className="vm-table benchmark-table">
                <thead>
                  <tr>
                    <th>Commit</th>
                    <th>Test</th>
                    <th>P50</th>
                    <th>P95</th>
                    <th>P99</th>
                    <th>{t("Failure", "실패율")}</th>
                    <th>Host CPU</th>
                    <th>{t("Host memory", "Host 메모리")}</th>
                    <th>{t("Success", "성공")}</th>
                    <th>{t("Timestamp", "시각")}</th>
                  </tr>
                </thead>
                <tbody>
                  {runs.map((run) => (
                    <tr key={run.run.run_id}>
                      <td className="mono">{shortCommit(run.run.commit_sha)}</td>
                      <td>{displayTestName(run.test)}</td>
                      <td>{formatLatency(run, "p50_ms")}</td>
                      <td>{formatLatency(run, "p95_ms")}</td>
                      <td>{formatLatency(run, "p99_ms")}</td>
                      <td>{run.failure_rate.toFixed(2)}%</td>
                      <td>{formatOptional(run.host_resources.cpu_percent, "%")}</td>
                      <td>{formatHostMemory(run)}</td>
                      <td className="mono">{run.successful_count}/{run.attempted_count}</td>
                      <td className="mono">{formatTimestamp(run.run.timestamp)}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          </section>

          {metricRows.length > 0 && (
            <section className="benchmark-section" aria-labelledby="benchmark-metrics-title">
              <h3 id="benchmark-metrics-title">{t("Latest command metrics", "최신 명령별 지표")}</h3>
              <div className="table-scroll">
                <table className="vm-table benchmark-table">
                  <thead>
                    <tr><th>Test</th><th>Metric</th><th>{t("Value", "값")}</th><th>Commit</th></tr>
                  </thead>
                  <tbody>
                    {metricRows.map(({ run, name, value }) => (
                      <tr key={`${run.run.run_id}-${name}`}>
                        <td>{displayTestName(run.test)}</td>
                        <td className="mono">{name}</td>
                        <td className="mono">{formatMetric(name, value)}</td>
                        <td className="mono">{shortCommit(run.run.commit_sha)}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            </section>
          )}
        </>
      )}

      <section className="benchmark-section" aria-labelledby="benchmark-vms-title">
        <h3 id="benchmark-vms-title">{t("Current MicroVM state", "현재 MicroVM 상태")}</h3>
        {!vmsLoaded ? (
          <div className="empty">{t("Loading VMs…", "VM 불러오는 중…")}</div>
        ) : (
          <>
            <div className="benchmark-state-summary">
              <article className="benchmark-state-card total"><span>Total</span><strong>{vms.length}</strong></article>
              {VM_STATES.map((state) => (
                <article className={`benchmark-state-card ${state}`} key={state}>
                  <span>{state}</span><strong>{stateCounts.get(state) ?? 0}</strong>
                </article>
              ))}
            </div>
            {vms.length === 0 ? (
              <div className="empty">{t("No MicroVMs", "MicroVM 없음")}</div>
            ) : (
              <div className="table-scroll">
                <table className="vm-table benchmark-vm-table">
                  <thead>
                    <tr>
                      <th>{t("Name", "이름")}</th><th>{t("State", "상태")}</th><th>{t("Image", "이미지")}</th>
                      <th>vCPU</th><th>RAM</th><th>{t("Guest CPU", "Guest CPU")}</th>
                      <th>{t("Guest memory", "Guest 메모리")}</th><th>IPv4</th>
                    </tr>
                  </thead>
                  <tbody>
                    {vms.map((vm) => (
                      <tr key={vm.id}>
                        <td className="name">{vm.name}</td>
                        <td><span className={`state-badge ${vm.state}`}>{vm.state}</span></td>
                        <td className="mono">{vm.template}</td><td className="mono">{vm.cpu}</td>
                        <td className="mono">{vm.ram} MiB</td>
                        <td className="mono">{vm.state === "running" ? formatOptional(vm.cpuUsagePercent, "%") : "—"}</td>
                        <td className="mono">{formatVmMemory(vm)}</td>
                        <td className="mono">{vm.ipv4 ?? "—"}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            )}
          </>
        )}
      </section>
    </section>
  );
}

function buildTrends(runs: BenchmarkResult[]) {
  const chronological = [...runs].reverse();
  const points = (items: BenchmarkResult[], value: (run: BenchmarkResult) => number | null): BenchmarkTrendPoint[] =>
    items.flatMap((run) => {
      const metric = value(run);
      return metric == null || !Number.isFinite(metric) ? [] : [{ label: shortCommit(run.run.commit_sha), value: metric, detail: `${displayTestName(run.test)} · ${formatTimestamp(run.run.timestamp)}` }];
    }).slice(-HISTORY_POINTS);
  return {
    bootP95: points(chronological.filter((run) => run.test === "vm_boot"), (run) => run.latency?.p95_ms ?? null),
    failureRate: points(chronological, (run) => run.failure_rate),
    hostCpu: points(chronological, (run) => run.host_resources.cpu_percent),
  };
}

function primaryMetric(run: BenchmarkResult): string {
  if (run.latency) return `P95 ${run.latency.p95_ms} ms`;
  const metrics = run.metrics ?? {};
  if (metrics.vm_per_second !== undefined) return `${metrics.vm_per_second.toFixed(1)} VM/s`;
  if (metrics.requests_per_second !== undefined) return `${metrics.requests_per_second.toFixed(0)} req/s`;
  if (metrics.throughput_mbps !== undefined) return `${metrics.throughput_mbps.toFixed(1)} Mbps`;
  if (metrics.iops !== undefined) return `${metrics.iops.toFixed(0)} IOPS`;
  if (metrics.max_stable_microvms !== undefined) return `${metrics.max_stable_microvms} VMs`;
  return `${run.failure_rate.toFixed(2)}% failure`;
}

function displayTestName(test: string): string { return test.replaceAll("_", " "); }
function shortCommit(commit: string): string { return commit === "unknown" ? commit : commit.slice(0, 7); }
function formatOptional(value: number | null | undefined, suffix: string): string { return value == null ? "—" : `${value.toFixed(1)}${suffix}`; }
function formatLatency(run: BenchmarkResult, field: "p50_ms" | "p95_ms" | "p99_ms"): string { return run.latency ? `${run.latency[field]} ms` : "—"; }
function formatTimestamp(timestamp: string): string { return new Date(timestamp).toLocaleString(); }
function formatHostMemory(run: BenchmarkResult): string {
  const used = run.host_resources.memory_used_mib;
  const total = run.host_resources.memory_total_mib;
  return used == null ? "—" : `${used.toLocaleString()} / ${total?.toLocaleString() ?? "—"} MiB`;
}
function formatVmMemory(vm: VmResponse): string {
  if (vm.state !== "running" || vm.memoryUsedMib == null) return "—";
  return `${vm.memoryUsedMib} / ${vm.memoryTotalMib ?? vm.ram} MiB${vm.memoryUsedPercent == null ? "" : ` (${vm.memoryUsedPercent.toFixed(1)}%)`}`;
}
function formatMetric(name: string, value: number): string {
  if (name.endsWith("_percent") || name === "change_percent" || name === "regression_percent") return `${value.toFixed(2)}%`;
  if (name.endsWith("_ms")) return `${value.toFixed(2)} ms`;
  return value.toLocaleString(undefined, { maximumFractionDigits: 2 });
}
