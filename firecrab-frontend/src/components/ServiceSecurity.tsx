import { Fragment, useCallback, useEffect, useRef, useState } from "react";
import {
  Activity,
  Bell,
  Database,
  FileText,
  Package,
  RefreshCw,
  Server,
  Settings2,
  Shield,
  ShieldCheck,
  ShieldOff,
  Sparkles,
} from "lucide-react";
import { fetchApiResponse, fetchJson } from "../api/client";
import { useI18n } from "../i18n";
import { readingLines, severityRank } from "../lib/serviceSecurity";
import type {
  SecurityActivity,
  SecurityCoverage,
  SecurityExplanation,
  SecurityFinding,
  SecurityModel,
  SecurityScan,
  SecurityService,
} from "../lib/serviceSecurity";
import Screen from "./layout/Screen";
import "../styles/security.css";

type Translate = (english: string, korean: string) => string;
const modelValue = (model: SecurityModel) =>
  JSON.stringify({ connectionId: model.connectionId, model: model.model });

function Prose({ text }: { text: string }) {
  return readingLines(text).map((line, index) => (
    <Fragment key={index}>
      <span className="security-copy-line">{line}</span>
      {"\n"}
    </Fragment>
  ));
}
function Severity({ value }: { value?: string }) {
  const { t } = useI18n();
  const label =
    value === "critical"
      ? t("Critical", "치명적")
      : value === "high"
        ? t("High", "높음")
        : value === "medium"
          ? t("Medium", "보통")
          : value === "low"
            ? t("Low", "낮음")
            : t("Unknown", "확인 불가");
  return (
    <span
      className="security-badge"
      data-tone={
        value === "critical" || value === "high"
          ? "danger"
          : value === "medium"
            ? "warning"
            : "neutral"
      }
    >
      {label}
    </span>
  );
}
function Cves({ findings }: { findings: SecurityFinding[] }) {
  const { t } = useI18n();
  return (
    <>
      <ul className="security-findings">
        {findings.slice(0, 100).map((finding, index) => (
          <li key={`${finding.cve}-${finding.package}-${index}`}>
            <div className="security-finding-title">
              <Severity value={finding.severity} />
              <a
                href={`https://nvd.nist.gov/vuln/detail/${encodeURIComponent(finding.cve)}`}
                target="_blank"
                rel="noopener noreferrer"
              >
                {finding.cve}
              </a>
            </div>
            <div className="security-finding-body">
              <p className="security-package">
                <strong>
                  {finding.package} {finding.installedVersion}
                </strong>
              </p>
              <p className="security-fix">
                <Prose
                  text={
                    finding.fixedVersion
                      ? `${t("Update to", "다음 버전으로 업데이트:")} ${finding.fixedVersion}`
                      : t(
                          "No fixed version reported. Review the advisory.",
                          "수정 버전 정보가 없다. 취약점 안내를 확인한다.",
                        )
                  }
                />
              </p>
              <p className="security-meta">
                {t("Advisory source", "취약점 출처")}: {finding.advisorySource}{" "}
                · {finding.target}
              </p>
            </div>
          </li>
        ))}
      </ul>
      {findings.length > 100 && (
        <p className="security-meta">
          {t(
            "Only the first 100 findings are shown.",
            "처음 100개 결과만 표시한다.",
          )}
        </p>
      )}
    </>
  );
}
function ActivityFinding({ event }: { event: SecurityActivity }) {
  const { t } = useI18n();
  return (
    <li>
      <div className="security-finding-title">
        <Severity value={event.severity} />
        <strong>
          {event.source === "PostgreSQL JSON authentication logs"
            ? t("Authentication failures", "인증 실패")
            : t("401/403 records", "401/403 기록")}
          : {event.count ?? 0}
        </strong>
      </div>
      <div className="security-finding-body">
        <p>
          <Prose text={event.recommendation ?? ""} />
        </p>
        <p className="security-meta">
          {t("Source", "출처")}: {event.source}
        </p>
      </div>
    </li>
  );
}
function scanStatus(
  t: Translate,
  scan: SecurityScan | null | undefined,
  digest?: string | null,
): string {
  switch (scan?.status) {
    case "complete":
      return `${t("Reported CVEs", "확인된 CVE")}: ${scan.findings?.length ?? 0}`;
    case "stale":
    case "database_stale":
      return t("CVE data is stale", "CVE 정보가 오래됨");
    case "scanner_missing":
      return t("CVE scanner is not installed", "CVE 검사기가 설치되지 않음");
    case "registry_auth_unavailable":
      return t("Registry authentication unavailable", "레지스트리 인증 불가");
    case "unsupported_platform":
    case "invalid_image":
      return t(
        "CVE scan unsupported for this image",
        "이 이미지의 CVE 검사를 지원하지 않음",
      );
    case "report_invalid":
      return t("Invalid CVE scanner report", "CVE 검사 보고서가 올바르지 않음");
    case "scanner_error":
    case "scanner_timeout":
    case "report_too_large":
    case "database_unavailable":
      return t("CVE scan failed", "CVE 검사 실패");
    default:
      return digest
        ? t("CVE scan pending", "CVE 검사 대기")
        : t("Image digest unavailable", "이미지 식별값 없음");
  }
}
function limitation(t: Translate, value: string): string {
  if (value === "os_end_of_support")
    return t(
      "The image OS is no longer supported. CVE coverage may be incomplete.",
      "운영체제 지원이 종료됐다. CVE 검사가 불완전할 수 있다.",
    );
  if (value === "os_not_detected")
    return t(
      "Image OS detection failed. OS package coverage is unavailable.",
      "이미지 운영체제를 확인하지 못했다. OS 패키지 검사 범위를 알 수 없다.",
    );
  if (value === "non_cve_advisories_omitted")
    return t(
      "Some advisories have no CVE identifier. These advisories are not shown.",
      "CVE 번호가 없는 일부 권고는 표시되지 않는다.",
    );
  return t("CVE scan coverage is limited.", "CVE 검사 범위가 제한된다.");
}
function unsupportedReason(t: Translate, value: string): string {
  if (value === "cluster_managed")
    return t("Cluster-managed service", "클러스터 관리 서비스");
  if (value === "database_service_unavailable")
    return t("Linked service unavailable", "연결된 서비스를 사용할 수 없음");
  if (value === "database_connection_unlinked")
    return t(
      "Database connection: image and logs not linked",
      "데이터베이스 연결: 이미지와 로그가 연결되지 않음",
    );
  return t("Unsupported workload", "지원하지 않는 워크로드");
}
async function discoverModels(signal: AbortSignal): Promise<SecurityModel[]> {
  const accounts = await fetchJson<{ id: string; name: string }[]>(
    "/api/pipe/ai/connections",
    { signal },
  );
  const rows = await Promise.all(
    accounts.map(async (account) => {
      const catalog = await fetchJson<{
        models?: { id: string; name: string; verified: boolean }[];
      }>(
        `/api/ai-sandbox/connections/${encodeURIComponent(account.id)}/models`,
        { signal },
      ).catch(() => ({ models: [] }));
      return (catalog.models ?? [])
        .filter((model) => model.verified)
        .map((model) => ({
          connectionId: account.id,
          model: model.id,
          account: account.name,
          name: model.name,
        }));
    }),
  );
  return rows.flat();
}

export default function ServiceSecurity() {
  const { t } = useI18n();
  const [coverage, setCoverage] = useState<SecurityCoverage | null>(null);
  const [models, setModels] = useState<SecurityModel[]>([]);
  const [selectedModel, setSelectedModel] = useState("");
  const [explanations, setExplanations] = useState<
    Record<string, SecurityExplanation>
  >({});
  const [error, setError] = useState("");
  const [loadError, setLoadError] = useState("");
  const [busy, setBusy] = useState(false);
  const lifecycle = useRef<AbortController | null>(null);
  const reload = useCallback(async (signal: AbortSignal) => {
    const data = await fetchJson<SecurityCoverage>("/api/service-security", {
      signal,
    });
    if (!signal.aborted) {
      setCoverage(data);
      setLoadError("");
    }
  }, []);
  useEffect(() => {
    const controller = new AbortController();
    lifecycle.current = controller;
    const refresh = () => {
      void reload(controller.signal).catch((failure: Error) => {
        if (!controller.signal.aborted) setLoadError(failure.message);
      });
    };
    refresh();
    void discoverModels(controller.signal)
      .then((rows) => {
        if (!controller.signal.aborted) setModels(rows);
      })
      .catch(() => {});
    const timer = window.setInterval(refresh, 5000);
    return () => {
      controller.abort();
      window.clearInterval(timer);
    };
  }, [reload]);
  const selection = models.some((model) => modelValue(model) === selectedModel)
    ? selectedModel
    : models[0]
      ? modelValue(models[0])
      : "";
  const perform = async (
    action: "scan" | "analysis" | "link" | "unlink",
    id: string,
    serviceId?: string,
  ) => {
    if (
      busy ||
      (action === "analysis" && !selection) ||
      (action === "link" && !serviceId)
    )
      return;
    const signal = lifecycle.current?.signal;
    if (!signal || signal.aborted) return;
    setBusy(true);
    setError("");
    try {
      const path =
        action === "link" || action === "unlink"
          ? `/api/service-security/databases/${encodeURIComponent(id)}/link`
          : `/api/service-security/${encodeURIComponent(id)}/${action}`;
      const method =
        action === "link" ? "PUT" : action === "unlink" ? "DELETE" : "POST";
      const init = {
        signal,
        method,
        headers: { "Content-Type": "application/json" },
        body:
          action === "analysis"
            ? selection
            : action === "link"
              ? JSON.stringify({ serviceId })
              : undefined,
      };
      if (action === "analysis") {
        const result = await fetchJson<SecurityExplanation>(path, init);
        if (!signal.aborted)
          setExplanations((previous) => ({ ...previous, [id]: result }));
      } else {
        await fetchApiResponse(path, init);
        if (!signal.aborted) await reload(signal);
      }
    } catch (failure) {
      if (!signal.aborted) setError((failure as Error).message);
    } finally {
      if (!signal.aborted) setBusy(false);
    }
  };
  const services = coverage?.services ?? [];
  const unsupported = coverage?.unsupportedWorkloads ?? [];
  const needsReview = services.filter(
    (service) =>
      (service.scan?.findings?.length ?? 0) > 0 ||
      (service.signals?.length ?? 0) > 0 ||
      service.activityHistory?.some(
        (event) => event.deploymentId === service.deploymentId,
      ),
  ).length;
  return (
    <Screen view="security">
      <section className="security-page">
        <header className="security-heading">
          <h2>
            <Shield aria-hidden="true" />
            {t("Serverless security", "서버리스 보안")}
          </h2>
          <p>
            {t(
              "Review vulnerabilities and recommended actions.",
              "취약점과 조치 방향을 확인한다.",
            )}
          </p>
        </header>
        {(error || loadError) && (
          <p role="alert" className="security-warning">
            {error || loadError}
          </p>
        )}
        {!coverage && !error && !loadError && (
          <p role="status">
            {t("Loading security findings…", "보안 결과를 불러오는 중…")}
          </p>
        )}
        <dl
          className="security-overview"
          aria-label={t("Security overview", "보안 요약")}
        >
          <div>
            <dt>
              <Bell aria-hidden="true" />
              {t("Services to review", "확인할 서비스")}
            </dt>
            <dd>{needsReview}</dd>
          </div>
          <div>
            <dt>
              <ShieldCheck aria-hidden="true" />
              {t("Images scanned", "이미지 검사 완료")}
            </dt>
            <dd>
              {
                services.filter(
                  (service) => service.scan?.status === "complete",
                ).length
              }
              <span> / {services.length}</span>
            </dd>
          </div>
          <div>
            <dt>
              <ShieldOff aria-hidden="true" />
              {t("Outside coverage", "검사 범위 밖")}
            </dt>
            <dd>{unsupported.length}</dd>
          </div>
        </dl>
        {coverage?.ingressLogs === "degraded" && (
          <p role="status" className="security-warning">
            {t(
              "Ingress event collection needs attention.",
              "인그레스 이벤트 수집 상태를 확인해야 한다.",
            )}
          </p>
        )}
        <div className="security-toolbar">
          <h3>
            <Server aria-hidden="true" />
            {t("Services", "서비스")}
          </h3>
          <details id="security-ai-settings" className="security-ai-settings">
            <summary>
              <Settings2 aria-hidden="true" />
              {t("AI guidance settings", "AI 조치 안내 설정")}
            </summary>
            <div>
              {models.length ? (
                <>
                  <label htmlFor="security-model">
                    {t("AI model", "AI 모델")}
                  </label>
                  <select
                    id="security-model"
                    value={selection}
                    onChange={(event) => setSelectedModel(event.target.value)}
                  >
                    {models.map((model) => (
                      <option value={modelValue(model)} key={modelValue(model)}>
                        {model.account} · {model.name}
                      </option>
                    ))}
                  </select>
                </>
              ) : (
                <p>
                  {t(
                    "Verify an AI model to explain security findings.",
                    "보안 탐지 결과를 분석하려면 AI 모델을 검증한다.",
                  )}{" "}
                  <a href="#/ai-sandbox">{t("AI Sandbox", "AI 샌드박스")}</a>
                </p>
              )}
              <p className="security-meta">
                <Prose
                  text={t(
                    "AI explains findings and suggests actions. It does not change your services.",
                    "AI는 결과를 설명하고 조치를 제안한다. 서비스를 직접 변경하지 않는다.",
                  )}
                />
              </p>
            </div>
          </details>
        </div>
        <div className="security-services">
          {coverage && !services.length && (
            <div className="security-empty">
              {t("No service deployment records.", "서비스 배포 기록이 없다.")}
              <p>
                {t(
                  "Deploy a service to view security findings.",
                  "마이크로서비스를 배포하면 보안 결과를 확인할 수 있다.",
                )}
              </p>
              <a href="#/services">{t("View services", "서비스 보기")}</a>
            </div>
          )}
          {services.map((service) => (
            <ServiceCard
              key={service.id}
              service={service}
              explanation={explanations[service.id]}
              busy={busy}
              canExplain={!!selection}
              perform={perform}
            />
          ))}
        </div>
        {!!coverage?.databases?.length && (
          <details
            className="security-disclosure security-settings"
            id="security-databases"
          >
            <summary>
              <Database aria-hidden="true" />
              {t("Database connections", "데이터베이스 연결")}{" "}
              <span>({coverage.databases.length})</span>
            </summary>
            <p>
              <Prose
                text={t(
                  "Select the service running this database. Verify the connection target yourself. Firecrab does not verify this link. Findings appear under the selected service.",
                  "데이터베이스를 실행하는 서비스를 선택한다. 연결 대상을 직접 확인한다. Firecrab은 연결 대상을 검증하지 않는다. 결과는 선택한 서비스에 표시된다.",
                )}
              />
            </p>
            {coverage.databases.map((database) => (
              <DatabaseLink
                key={database.id}
                database={database}
                services={services}
                busy={busy}
                perform={perform}
              />
            ))}
          </details>
        )}
        <details
          className="security-disclosure security-settings"
          id="security-coverage"
        >
          <summary>
            <Shield aria-hidden="true" />
            {t("Coverage and limitations", "검사 범위와 제한")}
          </summary>
          <div className="security-coverage-grid">
            <section>
              <h5>
                <Package aria-hidden="true" />
                {t("Image checks", "이미지 검사")}
              </h5>
              <p>
                <Prose
                  text={t(
                    "CVE scans check supported image packages.",
                    "CVE 검사는 지원되는 이미지 패키지를 확인한다.",
                  )}
                />
              </p>
            </section>
            <section>
              <h5>
                <Activity aria-hidden="true" />
                {t("Activity checks", "활동 검사")}
              </h5>
              <p>
                <Prose
                  text={
                    coverage?.ingressLogs === "limited"
                      ? t(
                          "Uses structured logs and ingress 401/403 counts. Some events may be missed. Counts can overlap across sources.",
                          "구조화된 로그와 인그레스 401/403 횟수를 확인한다. 일부 이벤트가 누락될 수 있다. 출처별 횟수는 중복될 수 있다.",
                        )
                      : coverage?.ingressLogs === "degraded"
                        ? t(
                            "Some ingress security events were lost. Structured application logs remain available.",
                            "일부 인그레스 보안 이벤트가 누락됐다. 구조화된 애플리케이션 로그도 확인한다.",
                          )
                        : t(
                            "Ingress logs are unavailable. Reapply Pipe settings to restore collection. Structured logs remain the only activity source.",
                            "인그레스 로그는 사용할 수 없다. 수집 복구를 위해 Pipe 설정을 다시 적용한다. 구조화된 로그만 활동 검사에 사용한다.",
                          )
                  }
                />
              </p>
            </section>
            <section>
              <h5>
                <Sparkles aria-hidden="true" />
                {t("AI privacy", "AI 데이터 보호")}
              </h5>
              <p>
                <Prose
                  text={t(
                    "AI receives CVE IDs and aggregate counts. Severity and fix versions are included. Raw logs stay on your host.",
                    "AI에는 CVE 번호와 활동 집계만 전달한다. 심각도와 수정 버전도 포함한다. 원본 로그는 호스트에 유지한다.",
                  )}
                />
              </p>
            </section>
            <section>
              <h5>
                <ShieldOff aria-hidden="true" />
                {t("Excluded workloads", "검사 제외 대상")}
              </h5>
              <p>
                <Prose
                  text={t(
                    "Standalone MicroVMs are not checked. Cluster-managed services are not checked. Database connections need linked images and logs.",
                    "단독 마이크로VM은 검사하지 않는다. 클러스터 관리 서비스는 검사하지 않는다. 데이터베이스 연결에는 이미지와 로그 연결이 필요하다.",
                  )}
                />
              </p>
            </section>
          </div>
          {!!unsupported.length && (
            <>
              <h4>{t("Items not checked", "검사하지 않는 항목")}</h4>
              <ul className="security-unsupported">
                {unsupported.map((workload) => (
                  <li key={workload.id}>
                    {workload.name} · {unsupportedReason(t, workload.reason)}
                  </li>
                ))}
              </ul>
            </>
          )}
        </details>
      </section>
    </Screen>
  );
}

type Perform = (
  action: "scan" | "analysis" | "link" | "unlink",
  id: string,
  serviceId?: string,
) => Promise<void>;
function ServiceCard({
  service,
  explanation,
  busy,
  canExplain,
  perform,
}: {
  service: SecurityService;
  explanation?: SecurityExplanation;
  busy: boolean;
  canExplain: boolean;
  perform: Perform;
}) {
  const { t } = useI18n();
  const scan = service.scan;
  const findings = [...(scan?.findings ?? [])].sort(
    (a, b) => severityRank(a.severity) - severityRank(b.severity),
  );
  const signals = service.signals ?? [];
  const history = service.activityHistory ?? [];
  const current = history.filter(
    (event) => event.deploymentId === service.deploymentId,
  );
  const activity =
    signals.length || current.length
      ? t("Activity detected", "활동 신호 확인")
      : !service.sampledRecords
        ? t("No recent structured access logs", "최근 구조화된 접근 로그 없음")
        : t(
            "No activity signal in sampled logs",
            "확인한 로그에 활동 신호 없음",
          );
  return (
    <article
      className="security-service"
      aria-labelledby={`security-name-${service.id}`}
    >
      <header className="security-service-heading">
        <div>
          <h4 id={`security-name-${service.id}`}>
            <Server aria-hidden="true" />
            <span>{service.name}</span>
          </h4>
          <p className="security-service-status">
            <span
              className="security-scan-status"
              data-complete={scan?.status === "complete"}
            >
              {scanStatus(t, scan, service.digest)}
            </span>
            <span>{activity}</span>
          </p>
        </div>
        <div className="security-actions">
          {service.digest && (
            <button
              type="button"
              className="btn"
              data-action="security-scan"
              disabled={busy}
              onClick={() => void perform("scan", service.id)}
            >
              <RefreshCw aria-hidden="true" />
              {t("Scan now", "지금 검사")}
            </button>
          )}
          {canExplain &&
            (!!signals.length ||
              !!history.length ||
              (scan?.status === "complete" && !!findings.length)) && (
              <button
                type="button"
                className="btn security-guidance-button"
                disabled={busy}
                onClick={() => void perform("analysis", service.id)}
              >
                <Sparkles aria-hidden="true" />
                {t("Explain with AI", "AI 조치 안내")}
              </button>
            )}
        </div>
      </header>
      {service.logGap && (
        <p className="security-warning">
          {t(
            "Some application logs may be missing.",
            "일부 애플리케이션 로그가 누락되었을 수 있다.",
          )}
        </p>
      )}
      {scan?.limitations?.map((value, index) => (
        <p className="security-warning" key={`${value}-${index}`}>
          <Prose text={limitation(t, value)} />
        </p>
      ))}
      {!!findings.length && (
        <>
          <h5 className="security-group-title">
            <Package aria-hidden="true" />
            <span>{t("Vulnerabilities", "취약점")}</span>
            <span className="security-group-count">{findings.length}</span>
          </h5>
          <Cves findings={findings.slice(0, 3)} />
        </>
      )}
      {findings.length > 3 && (
        <details className="security-disclosure">
          <summary>
            {t("More CVE findings", "나머지 CVE 결과")} ({findings.length - 3})
          </summary>
          <Cves findings={findings.slice(3)} />
        </details>
      )}
      {(!!signals.length || !!current.length) && (
        <>
          <h5 className="security-group-title">
            <Activity aria-hidden="true" />
            <span>{t("Activity alerts", "활동 알림")}</span>
            <span className="security-group-count">
              {signals.length + current.length}
            </span>
          </h5>
          <ul className="security-findings">
            {[...signals, ...current.slice(0, 20)].map((event, index) => (
              <ActivityFinding key={index} event={event} />
            ))}
          </ul>
        </>
      )}
      {explanation && explanation.deploymentId === service.deploymentId && (
        <aside
          className="security-guidance"
          aria-label={t("AI guidance", "AI 조치 안내")}
        >
          <h5>
            <Sparkles aria-hidden="true" />
            {t("AI guidance", "AI 조치 안내")}
          </h5>
          <p>
            <Prose text={explanation.text} />
          </p>
        </aside>
      )}
      <details className="security-disclosure security-evidence">
        <summary>
          <FileText aria-hidden="true" />
          {t("Evidence and history", "근거와 이전 기록")}
        </summary>
        <div className="security-evidence-body">
          <dl className="security-metadata">
            <dt>{t("Deployment", "배포")}</dt>
            <dd>{service.deploymentId}</dd>
            <dt>{t("Image digest", "이미지 식별값")}</dt>
            <dd className="mono">{service.digest || "—"}</dd>
            <dt>{t("Current replicas", "현재 복제본")}</dt>
            <dd>{service.replicaIds?.join(", ")}</dd>
            {scan?.scannedAt !== undefined && (
              <>
                <dt>{t("Scan time (Unix)", "검사 시각 (Unix)")}</dt>
                <dd>{scan.scannedAt}</dd>
                <dt>{t("Database updated", "DB 갱신")}</dt>
                <dd>{scan.databaseUpdatedAt}</dd>
              </>
            )}
          </dl>
          {history.slice(0, 20).map((event, index) => (
            <div className="security-history" key={index}>
              <p>
                <strong>
                  {t("Release", "릴리스")}: {event.deploymentId}
                </strong>{" "}
                · {t("Window (Unix)", "시작 시각 (Unix)")}:{" "}
                {event.bucketStart ?? 0}
              </p>
              <p className="security-meta">
                {t("Replicas", "복제본")}:{" "}
                {event.replicaIds?.join(", ") || t("Unavailable", "확인 불가")}{" "}
                · {t("Severity", "심각도")}: {event.severity}
              </p>
              {event.deploymentId !== service.deploymentId && (
                <ul className="security-findings">
                  <ActivityFinding event={event} />
                </ul>
              )}
            </div>
          ))}
          {service.previousScans?.slice(0, 20).map((previous, index) => (
            <div className="security-history" key={index}>
              <h5>
                {t("Previous release", "이전 릴리스")}: {previous.deploymentId}
              </h5>
              <p className="security-meta">
                {t("Image digest", "이미지 식별값")}: {previous.digest} ·{" "}
                {t("Scan status", "검사 상태")}: {previous.status} ·{" "}
                {t("Scan time (Unix)", "검사 시각 (Unix)")}:{" "}
                {previous.scannedAt ?? 0}
              </p>
              <p className="security-meta">
                {t("Scanned replicas", "검사 시 복제본")}:{" "}
                {previous.replicaIds?.join(", ")}
              </p>
              <Cves findings={previous.findings ?? []} />
            </div>
          ))}
        </div>
      </details>
    </article>
  );
}
function DatabaseLink({
  database,
  services,
  busy,
  perform,
}: {
  database: NonNullable<SecurityCoverage["databases"]>[number];
  services: SecurityService[];
  busy: boolean;
  perform: Perform;
}) {
  const { t } = useI18n();
  const [selected, setSelected] = useState(database.serviceId ?? "");
  const selectedId = services.some((service) => service.id === selected)
    ? selected
    : "";
  return (
    <div className="security-database-row">
      <label htmlFor={`security-db-${database.id}`}>
        {database.name}
        <span className="security-meta">
          {!database.serviceId
            ? t("Not linked", "연결되지 않음")
            : !database.serviceName
              ? t(
                  "Linked service unavailable",
                  "연결된 서비스를 사용할 수 없음",
                )
              : t("Linked", "연결됨")}
        </span>
      </label>
      <select
        id={`security-db-${database.id}`}
        value={selectedId}
        onChange={(event) => setSelected(event.target.value)}
      >
        <option value="">{t("Select a service", "서비스 선택")}</option>
        {services.map((service) => (
          <option value={service.id} key={service.id}>
            {service.name}
          </option>
        ))}
      </select>
      <div className="security-actions">
        <button
          type="button"
          className="btn"
          disabled={busy || !services.length || !selectedId}
          onClick={() => void perform("link", database.id, selectedId)}
        >
          {t("Link", "연결")}
        </button>
        {database.serviceId && (
          <button
            type="button"
            className="btn"
            disabled={busy}
            onClick={() => void perform("unlink", database.id)}
          >
            {t("Unlink", "연결 해제")}
          </button>
        )}
      </div>
    </div>
  );
}
