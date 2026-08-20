import { useCallback, useEffect, useMemo, useState } from "react";
import type { VmResponse, VmState } from "../bindings";
import { listBenchmarks } from "../api/client";
import type { BenchmarkResult } from "../benchmark";
import { useI18n } from "../i18n";
import BenchmarkReportCard from "./BenchmarkReportCard";
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
  const trends = useMemo(() => buildTrends(runs, t), [runs, t]);
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
          <section className="benchmark-section" aria-labelledby="benchmark-latest-title">
            <h3 id="benchmark-latest-title">{t("Latest test reports", "테스트별 최신 보고서")}</h3>
            <p className="benchmark-section-intro">
              {t(
                "One card per test type. Compare runs only when the image, VM resources, and host conditions are similar.",
                "테스트 종류별 최신 결과입니다. 이미지·VM 사양·Host 조건이 비슷한 실행끼리 비교해야 합니다.",
              )}
            </p>
            <div className="benchmark-summary">
              {latest.slice(0, 6).map((run) => (
                <BenchmarkReportCard
                  key={run.test}
                  run={run}
                  history={runs.filter((item) => item.test === run.test).slice(0, 3)}
                  title={displayTestName(run.test, t)}
                  description={describeTest(run.test, t)}
                />
              ))}
            </div>
          </section>

          <section className="benchmark-section" aria-labelledby="benchmark-trends-title">
            <h3 id="benchmark-trends-title">{t("Performance trends", "성능 추세")}</h3>
            <div className="benchmark-chart-grid">
              <BenchmarkTrendChart
                title={t("Boot P95 latency", "부팅 P95 지연 시간")}
                points={trends.bootP95}
                unit=" ms"
                color="var(--ember)"
                fill="rgba(196, 62, 18, 0.12)"
                latestLabel={t("Latest", "최신")}
                recentLabel={t("Recent runs", "최근 실행")}
                description={t(
                  "Time within which 95% of successful boot operations reached running state. Lower is better.",
                  "성공한 부팅 작업의 95%가 실행 상태에 도달한 시간입니다. 낮을수록 좋습니다.",
                )}
                emptyLabel={t("No boot latency samples", "부팅 지연 시간 샘플 없음")}
              />
              <BenchmarkTrendChart
                title={t("Failure rate", "실패율")}
                points={trends.failureRate}
                unit="%"
                color="var(--error)"
                fill="rgba(179, 38, 30, 0.10)"
                yMaxHint={5}
                latestLabel={t("Latest", "최신")}
                recentLabel={t("Recent runs", "최근 실행")}
                description={t(
                  "Failure percentage for each run across all test types. The point label identifies the test. Lower is better.",
                  "모든 테스트 실행별 실패 비율입니다. 점 라벨에서 테스트 종류를 확인할 수 있으며 낮을수록 좋습니다.",
                )}
                emptyLabel={t("No failure samples", "실패율 샘플 없음")}
              />
              <BenchmarkTrendChart
                title={t("Host CPU", "Host CPU")}
                points={trends.hostCpu}
                unit="%"
                color="var(--ready)"
                fill="rgba(21, 127, 99, 0.10)"
                yMaxHint={100}
                latestLabel={t("Latest", "최신")}
                recentLabel={t("Recent runs", "최근 실행")}
                description={t(
                  "Host CPU measured during each run. Use it to compare results under similar host load.",
                  "각 실행 중 측정한 Host CPU입니다. 비슷한 Host 부하에서 결과를 비교하는 참고 지표입니다.",
                )}
                emptyLabel={t("No host CPU samples", "Host CPU 샘플 없음")}
              />
            </div>
          </section>

          <section className="benchmark-section" aria-labelledby="benchmark-history-title">
            <h3 id="benchmark-history-title">{t("Run history", "실행 이력")}</h3>
            <p className="benchmark-section-intro">
              {t(
                "Each row is one benchmark command. Latency values summarize successful operations only.",
                "한 행은 Benchmark 명령 한 번을 의미하며, 지연 시간은 성공한 작업만 집계합니다.",
              )}
            </p>
            <div className="table-scroll">
              <table className="vm-table benchmark-table">
                <thead>
                  <tr>
                    <th>{t("Source", "출처")}</th>
                    <th>Test</th>
                    <th>Min</th>
                    <th>Avg</th>
                    <th>P50</th>
                    <th>P95</th>
                    <th>P99</th>
                    <th>Max</th>
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
                      <td className="mono">{sourceLabel(run, t)}</td>
                      <td>{displayTestName(run.test, t)}</td>
                      <td>{formatLatency(run, "minimum_ms")}</td>
                      <td>{formatLatency(run, "average_ms")}</td>
                      <td>{formatLatency(run, "p50_ms")}</td>
                      <td>{formatLatency(run, "p95_ms")}</td>
                      <td>{formatLatency(run, "p99_ms")}</td>
                      <td>{formatLatency(run, "maximum_ms")}</td>
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
              <p className="benchmark-section-intro">
                {t("Additional values produced only by each command type.", "각 명령 종류에서만 생성되는 추가 지표입니다.")}
              </p>
              <div className="table-scroll">
                <table className="vm-table benchmark-table">
                  <thead>
                    <tr><th>Test</th><th>{t("Metric", "지표")}</th><th>{t("Value", "값")}</th><th>{t("Source", "출처")}</th></tr>
                  </thead>
                  <tbody>
                    {metricRows.map(({ run, name, value }) => (
                      <tr key={`${run.run.run_id}-${name}`}>
                        <td>{displayTestName(run.test, t)}</td>
                        <td>{displayMetricName(name, t)}</td>
                        <td className="mono">{formatMetric(name, value)}</td>
                        <td className="mono">{sourceLabel(run, t)}</td>
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

type Translate = (english: string, korean: string) => string;

function buildTrends(runs: BenchmarkResult[], t: Translate) {
  const chronological = [...runs].reverse();
  const points = (
    items: BenchmarkResult[],
    value: (run: BenchmarkResult) => number | null,
    includeTest: boolean,
  ): BenchmarkTrendPoint[] =>
    items.flatMap((run) => {
      const metric = value(run);
      if (metric == null || !Number.isFinite(metric)) return [];
      const source = run.run.commit_sha === "unknown" ? formatShortTimestamp(run.run.timestamp) : `${run.run.commit_sha.slice(0, 7)}${run.run.dirty ? "*" : ""}`;
      const test = displayTestName(run.test, t);
      return [{ label: includeTest ? `${test} · ${source}` : source, value: metric, detail: `${test} · ${formatTimestamp(run.run.timestamp)}` }];
    }).slice(-HISTORY_POINTS);
  return {
    bootP95: points(chronological.filter((run) => run.test === "vm_boot"), (run) => run.latency?.p95_ms ?? null, false),
    failureRate: points(chronological, (run) => run.failure_rate, true),
    hostCpu: points(chronological, (run) => run.host_resources.cpu_percent, true),
  };
}

function displayTestName(test: string, t: Translate): string {
  if (test === "vm_boot") return t("MicroVM boot", "MicroVM 부팅");
  if (test === "concurrent_creation") return t("Concurrent creation", "동시 생성");
  if (test === "vm_lifecycle") return t("Lifecycle stress", "Lifecycle 반복");
  if (test === "vm_density") return t("Maximum density", "최대 밀도");
  return test.replaceAll("_", " ");
}

function describeTest(test: string, t: Translate): string {
  if (test === "vm_boot") return t("Sequential create request to running state", "순차 생성 요청부터 실행 상태까지 측정");
  if (test === "concurrent_creation") return t("Parallel create requests to running state", "동시 생성 요청부터 실행 상태까지 측정");
  if (test === "vm_lifecycle") return t("Create, start, stop, restart, and delete cycle", "생성·시작·정지·재시작·삭제 전체 주기 측정");
  if (test === "vm_density") return t("Stable running MicroVM limit on this host", "현재 Host에서 안정적으로 실행되는 MicroVM 한계 측정");
  return t("Normalized benchmark result", "공통 형식 Benchmark 결과");
}

function sourceLabel(run: BenchmarkResult, t: Translate): string {
  if (run.run.commit_sha === "unknown") return t("Local", "로컬");
  return `${run.run.commit_sha.slice(0, 7)}${run.run.dirty ? "*" : ""}`;
}
function formatOptional(value: number | null | undefined, suffix: string): string { return value == null ? "—" : `${value.toFixed(1)}${suffix}`; }
function formatLatency(run: BenchmarkResult, field: keyof NonNullable<BenchmarkResult["latency"]>): string {
  return run.latency ? formatDuration(run.latency[field]) : "—";
}
function formatTimestamp(timestamp: string): string { return new Date(timestamp).toLocaleString(); }
function formatShortTimestamp(timestamp: string): string {
  return new Date(timestamp).toLocaleString(undefined, { month: "numeric", day: "numeric", hour: "2-digit", minute: "2-digit" });
}
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
  if (name.endsWith("_ms")) return formatDuration(value);
  if (name === "vm_per_second") return `${value.toLocaleString(undefined, { maximumFractionDigits: 2 })} VM/s`;
  if (name === "iterations_per_second") return `${value.toLocaleString(undefined, { maximumFractionDigits: 2 })} cycles/s`;
  if (name === "max_stable_microvms") return `${value.toLocaleString(undefined, { maximumFractionDigits: 0 })} VMs`;
  if (name === "requests_per_second") return `${value.toLocaleString(undefined, { maximumFractionDigits: 0 })} req/s`;
  if (name === "throughput_mbps") return `${value.toLocaleString(undefined, { maximumFractionDigits: 2 })} Mbps`;
  if (name === "iops") return `${value.toLocaleString(undefined, { maximumFractionDigits: 0 })} IOPS`;
  return value.toLocaleString(undefined, { maximumFractionDigits: 2 });
}

function displayMetricName(name: string, t: Translate): string {
  if (name === "total_creation_time_ms") return t("Total creation time", "전체 생성 시간");
  if (name === "vm_per_second") return t("VM creation rate", "VM 생성률");
  if (name === "iterations_per_second") return t("Lifecycle rate", "Lifecycle 처리율");
  if (name === "max_stable_microvms") return t("Maximum stable MicroVMs", "최대 안정 MicroVM");
  if (name === "requests_per_second") return t("API request rate", "API 요청 처리율");
  if (name === "throughput_mbps") return t("Network throughput", "Network 처리량");
  if (name === "iops") return "Storage IOPS";
  return name.replaceAll("_", " ");
}

function formatDuration(milliseconds: number): string {
  if (milliseconds >= 1000) return `${(milliseconds / 1000).toLocaleString(undefined, { maximumFractionDigits: 2 })} s`;
  return `${milliseconds.toLocaleString(undefined, { maximumFractionDigits: milliseconds < 10 ? 2 : 0 })} ms`;
}
