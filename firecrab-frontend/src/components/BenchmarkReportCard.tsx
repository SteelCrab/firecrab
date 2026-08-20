import type { BenchmarkResult } from "../benchmark";
import { useI18n } from "../i18n";

interface BenchmarkReportCardProps {
  run: BenchmarkResult;
  history: BenchmarkResult[];
  title: string;
  description: string;
}

/** Human-readable latest-run report for one benchmark type. */
export default function BenchmarkReportCard({ run, history, title, description }: BenchmarkReportCardProps) {
  const { t } = useI18n();
  const status = reportStatus(run.failure_rate, t);
  const primary = primaryResult(run, t);
  const latency = run.latency;

  return (
    <article className="benchmark-card">
      <header className="benchmark-card-header">
        <div>
          <h4>{title}</h4>
          <p>{description}</p>
        </div>
        <span className={`benchmark-report-status ${status.kind}`}>{status.label}</span>
      </header>

      <div className="benchmark-card-primary">
        <span>{primary.label}</span>
        <strong>{primary.value}</strong>
        <p>{primary.explanation}</p>
      </div>

      <dl className="benchmark-card-facts">
        <ReportFact label={t("Success", "성공")} value={`${run.successful_count}/${run.attempted_count}`} />
        <ReportFact label={t("Failure", "실패율")} value={`${formatNumber(run.failure_rate, 2)}%`} />
        <ReportFact label="Min" value={latency ? formatDuration(latency.minimum_ms) : "—"} />
        <ReportFact label="Avg" value={latency ? formatDuration(latency.average_ms) : "—"} />
        <ReportFact label="P95" value={latency ? formatDuration(latency.p95_ms) : "—"} />
        <ReportFact label="Max" value={latency ? formatDuration(latency.maximum_ms) : "—"} />
        <ReportFact label="Host CPU" value={run.host_resources.cpu_percent == null ? "—" : `${formatNumber(run.host_resources.cpu_percent, 1)}%`} />
      </dl>

      {(run.failures?.length ?? 0) > 0 && (
        <details className="benchmark-card-failure">
          <summary>{t(`Failure reason (${run.failures?.length})`, `실패 원인 (${run.failures?.length})`)}</summary>
          <code>{run.failures?.[0]}</code>
          {(run.failures?.length ?? 0) > 1 && (
            <small>{t("The first failure is shown. Open the benchmark job log for the complete list.", "첫 번째 실패만 표시합니다. 전체 목록은 Benchmark 작업 로그에서 확인할 수 있습니다.")}</small>
          )}
        </details>
      )}

      <section className="benchmark-card-history" aria-label={t("Recent history", "최근 이력")}>
        <span>{t("Recent history", "최근 이력")}</span>
        <ol>
          {history.map((item) => (
            <li key={item.run.run_id}>
              <time dateTime={item.run.timestamp}>{formatShortTimestamp(item.run.timestamp)}</time>
              <strong>{historyMetric(item, t)}</strong>
            </li>
          ))}
        </ol>
      </section>

      <footer className="benchmark-card-meta">
        <span>{sourceDescription(run, t)}</span>
        <time dateTime={run.run.timestamp}>{new Date(run.run.timestamp).toLocaleString()}</time>
      </footer>
    </article>
  );
}

function ReportFact({ label, value }: { label: string; value: string }) {
  return <div><dt>{label}</dt><dd>{value}</dd></div>;
}

type Translate = (english: string, korean: string) => string;

function reportStatus(failureRate: number, t: Translate) {
  if (failureRate >= 100) return { kind: "failed", label: t("Failed", "실패") };
  if (failureRate > 0) return { kind: "degraded", label: t("Partial failure", "일부 실패") };
  return { kind: "passed", label: t("Passed", "정상") };
}

function primaryResult(run: BenchmarkResult, t: Translate) {
  const metrics = run.metrics ?? {};
  if (run.failure_rate >= 100) {
    return {
      label: t("Result", "결과"),
      value: t("All failed", "전체 실패"),
      explanation: t(
        `All ${run.attempted_count} attempted operations failed. Review the job log before comparing performance.`,
        `시도한 ${run.attempted_count}개 작업이 모두 실패했습니다. 성능 비교 전에 작업 로그 확인이 필요합니다.`,
      ),
    };
  }
  if (run.test === "concurrent_creation" && metrics.vm_per_second !== undefined) {
    return {
      label: t("Creation throughput", "생성 처리량"),
      value: `${formatNumber(metrics.vm_per_second, 2)} VM/s`,
      explanation: t(
        "Average number of MicroVMs reaching running state per second. Higher is better.",
        "초당 실행 상태에 도달한 MicroVM 수입니다. 높을수록 좋습니다.",
      ),
    };
  }
  if (run.test === "vm_density" && metrics.max_stable_microvms !== undefined) {
    return {
      label: t("Maximum stable MicroVMs", "최대 안정 MicroVM"),
      value: `${formatNumber(metrics.max_stable_microvms, 0)} VMs`,
      explanation: t(
        "MicroVMs that remained running during the stability check. Higher is better.",
        "안정성 확인 동안 실행 상태를 유지한 MicroVM 수입니다. 높을수록 좋습니다.",
      ),
    };
  }
  if (run.test === "vm_lifecycle" && metrics.iterations_per_second !== undefined) {
    return {
      label: t("Lifecycle throughput", "Lifecycle 처리량"),
      value: `${formatNumber(metrics.iterations_per_second, 2)} cycles/s`,
      explanation: t(
        "Completed create/start/stop/start/stop/delete cycles per second. Higher is better.",
        "초당 완료한 생성·시작·정지·재시작·삭제 주기 수입니다. 높을수록 좋습니다.",
      ),
    };
  }
  if (run.latency) {
    return {
      label: t("P95 latency", "P95 지연 시간"),
      value: formatDuration(run.latency.p95_ms),
      explanation: t(
        `95% of ${run.successful_count} successful operations completed within this time. Lower is better.`,
        `성공한 ${run.successful_count}개 작업 중 95%가 이 시간 안에 완료되었습니다. 낮을수록 좋습니다.`,
      ),
    };
  }
  return {
    label: t("Failure rate", "실패율"),
    value: `${formatNumber(run.failure_rate, 2)}%`,
    explanation: t("Share of attempted operations that failed. Lower is better.", "시도한 작업 중 실패한 비율입니다. 낮을수록 좋습니다."),
  };
}

function formatDuration(milliseconds: number): string {
  if (milliseconds >= 1000) return `${formatNumber(milliseconds / 1000, 2)} s`;
  return `${formatNumber(milliseconds, milliseconds < 10 ? 2 : 0)} ms`;
}

function historyMetric(run: BenchmarkResult, t: Translate): string {
  const metrics = run.metrics ?? {};
  if (run.failure_rate >= 100) return t("100% failed", "100% 실패");
  if (run.test === "concurrent_creation" && metrics.vm_per_second !== undefined) return `${formatNumber(metrics.vm_per_second, 2)} VM/s`;
  if (run.test === "vm_density" && metrics.max_stable_microvms !== undefined) return `${formatNumber(metrics.max_stable_microvms, 0)} VMs`;
  if (run.test === "vm_lifecycle" && metrics.iterations_per_second !== undefined) return `${formatNumber(metrics.iterations_per_second, 2)} cycles/s`;
  if (run.latency) return `P95 ${formatDuration(run.latency.p95_ms)}`;
  return `${formatNumber(run.failure_rate, 2)}% ${t("failed", "실패")}`;
}

function formatShortTimestamp(timestamp: string): string {
  return new Date(timestamp).toLocaleString(undefined, { month: "numeric", day: "numeric", hour: "2-digit", minute: "2-digit" });
}

function sourceDescription(run: BenchmarkResult, t: Translate): string {
  const commit = run.run.commit_sha === "unknown" ? t("Local run", "로컬 실행") : run.run.commit_sha.slice(0, 7);
  const branch = run.run.branch === "unknown" ? null : run.run.branch;
  const dirty = run.run.dirty ? t("modified build", "수정 빌드") : null;
  return [branch, commit, dirty].filter(Boolean).join(" · ");
}

function formatNumber(value: number, maximumFractionDigits: number): string {
  return value.toLocaleString(undefined, { maximumFractionDigits });
}
