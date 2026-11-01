import { useEffect, useRef, useState, type FormEvent } from "react";
import {
  AlignLeft,
  Bell,
  CalendarClock,
  RefreshCw,
  Upload,
} from "lucide-react";
import { fetchJson } from "../api/client";
import { useI18n } from "../i18n";

interface Observation {
  timestamp: string;
  message: string;
  stream?: string;
  instanceId?: string;
  severity?: string;
  kind?: string;
  active?: boolean;
}
interface Exporter {
  managedBy?: "signoz" | null;
  endpoint: string | null;
  status: string;
  queuePersistent: boolean;
  queued: number;
  inFlight: number;
  delivered: number;
  dropped: number;
  rejected: number;
  failedBatches: number;
}
interface Snapshot {
  logsStatus: string;
  logs: Observation[];
  events: Observation[];
  alerts: Observation[];
  otlp: Exporter;
}
type View = "logs" | "events" | "alerts" | "otlp";

/** Observations are read only until the operator explicitly saves an exporter URL. */
export default function ServiceObservability({
  serviceId,
  active,
}: {
  serviceId: string;
  active: boolean;
}) {
  const { t } = useI18n();
  const [view, setView] = useState<View | null>(null),
    [snapshot, setSnapshot] = useState<Snapshot | null>(null);
  const [endpoint, setEndpoint] = useState(""),
    [error, setError] = useState<string | null>(null),
    [saveError, setSaveError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false),
    [refresh, setRefresh] = useState(0),
    [loading, setLoading] = useState(false);
  const edited = useRef(false),
    revision = useRef(0),
    mounted = useRef(true),
    acting = useRef(false);
  const base = `/api/micro-services/${encodeURIComponent(serviceId)}/observability`;
  const opened = view !== null;
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
      revision.current++;
    };
  }, []);
  useEffect(() => {
    if (!active || !opened) return;
    let alive = true,
      pending = false;
    const controllers = new Set<AbortController>();
    const read = async () => {
      if (pending || acting.current) return;
      pending = true;
      setLoading(true);
      const generation = revision.current,
        controller = new AbortController();
      controllers.add(controller);
      try {
        const data = await fetchJson<Snapshot>(base, {
          signal: controller.signal,
        });
        if (
          !data ||
          !Array.isArray(data.logs) ||
          !Array.isArray(data.events) ||
          !Array.isArray(data.alerts) ||
          typeof data.otlp?.status !== "string"
        )
          throw new Error("Invalid observations");
        if (!alive || generation !== revision.current) return;
        setSnapshot(data);
        setError(null);
        if (!edited.current) setEndpoint(data.otlp.endpoint ?? "");
      } catch {
        if (alive && generation === revision.current)
          setError(
            t(
              "Could not load observations.",
              "로그·이벤트·알림을 불러올 수 없다.",
            ),
          );
      } finally {
        pending = false;
        controllers.delete(controller);
        if (alive) setLoading(false);
      }
    };
    void read();
    const timer = setInterval(() => void read(), 3000);
    return () => {
      alive = false;
      clearInterval(timer);
      for (const controller of controllers) controller.abort();
    };
  }, [active, opened, base, refresh, t]);
  const save = async (event: FormEvent) => {
    event.preventDefault();
    if (acting.current) return;
    setSaveError(null);
    const value = endpoint.trim();
    if (value) {
      try {
        const url = new URL(value),
          authority = value.split("://")[1]?.split(/[/?#]/)[0];
        if (
          value.length > 2048 ||
          /\s/.test(value) ||
          !/^https?:$/.test(url.protocol) ||
          !url.hostname ||
          url.username ||
          url.password ||
          authority?.includes("@") ||
          url.search ||
          url.hash
        )
          throw new Error();
      } catch {
        setSaveError(
          t(
            "Use an HTTP(S) collector base URL without credentials, query or fragment.",
            "인증 정보·쿼리·프래그먼트가 없는 HTTP(S) 수집기 주소를 입력한다.",
          ),
        );
        return;
      }
    }
    acting.current = true;
    revision.current++;
    setSaving(true);
    try {
      const otlp = await fetchJson<Exporter>(`${base}/otlp`, {
        method: "PUT",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ endpoint: value || null }),
      });
      if (typeof otlp?.status !== "string")
        throw new Error("Invalid exporter response");
      if (!mounted.current) return;
      setSnapshot((current) => (current ? { ...current, otlp } : null));
      setEndpoint(otlp.endpoint ?? "");
      edited.current = false;
      setError(null);
    } catch {
      if (mounted.current)
        setSaveError(
          t(
            "Could not save the collector setting.",
            "수집기 설정을 저장할 수 없다.",
          ),
        );
    } finally {
      acting.current = false;
      if (mounted.current) setSaving(false);
    }
  };
  const controls = [
    { id: "logs" as const, label: t("Logs", "로그"), Icon: AlignLeft },
    {
      id: "events" as const,
      label: t("Events", "이벤트"),
      Icon: CalendarClock,
    },
    { id: "alerts" as const, label: t("Alerts", "알림"), Icon: Bell },
    { id: "otlp" as const, label: "OTLP", Icon: Upload },
  ];
  const statuses: Record<string, string> = {
    waiting: t("Waiting for logs.", "로그를 기다리는 중이다."),
    unavailable: t(
      "Log collection is unavailable. Start a new VM or redeploy.",
      "로그를 수집할 수 없다. 새 VM을 시작하거나 다시 배포한다.",
    ),
    truncated: t("Some logs were lost.", "일부 로그가 유실되었다."),
    error: t("Log collection failed.", "로그 수집에 실패했다."),
    active: t("Collecting logs.", "로그를 수집하고 있다."),
  };
  return (
    <section
      id="service-observability"
      className="detail-sec service-observability"
      aria-label={t("Logs, events and alerts", "로그·이벤트·알림")}
    >
      <div className="detail-sec-head">
        <h3>{t("Logs, events and alerts", "로그·이벤트·알림")}</h3>
        <div className="detail-actions">
          {controls.map(({ id, label, Icon }) => (
            <button
              type="button"
              className="icon-btn"
              key={id}
              title={label}
              aria-label={label}
              aria-pressed={view === id}
              onClick={() => setView((current) => (current === id ? null : id))}
            >
              <Icon aria-hidden="true" />
            </button>
          ))}
          {opened && (
            <button
              className="icon-btn"
              title={t("Refresh", "새로 고침")}
              aria-label={t("Refresh", "새로 고침")}
              disabled={loading || saving}
              onClick={() => setRefresh((value) => value + 1)}
            >
              <RefreshCw aria-hidden="true" />
            </button>
          )}
        </div>
      </div>
      {opened && (
        <div className="service-observation-content">
          {(error || saveError) && (
            <p role="alert" className="field-error">
              {saveError || error}
            </p>
          )}
          {!snapshot && !error && (
            <p role="status">{t("Loading…", "불러오는 중…")}</p>
          )}
          {snapshot && view !== "otlp" && !error && (
            <>
              {view === "logs" && (
                <p>
                  {statuses[snapshot.logsStatus] ??
                    t(
                      "Unknown log collection state.",
                      "로그 수집 상태를 확인할 수 없다.",
                    )}
                </p>
              )}
              {snapshot[view!].length === 0 ? (
                <p>
                  {view === "logs"
                    ? t("No logs.", "로그가 없다.")
                    : view === "events"
                      ? t("No events.", "이벤트가 없다.")
                      : t("No alerts.", "알림이 없다.")}
                </p>
              ) : (
                <ul className="service-observability-list">
                  {snapshot[view!].map((row, index) => (
                    <li key={`${row.timestamp}-${index}`}>
                      <div className="meta">
                        {[
                          row.timestamp,
                          row.stream,
                          row.instanceId,
                          row.severity,
                          row.kind,
                          row.active === undefined
                            ? null
                            : row.active
                              ? t("Active", "활성")
                              : t("Resolved", "해결"),
                        ]
                          .filter(Boolean)
                          .join(" · ")}
                      </div>
                      <pre>{row.message}</pre>
                    </li>
                  ))}
                </ul>
              )}
            </>
          )}
          {snapshot && view === "otlp" && (
            <form onSubmit={save}>
              <div className="field">
                <label htmlFor="service-collector">
                  {t("Collector base URL", "수집기 기본 주소")}
                </label>
                <input
                  id="service-collector"
                  type="text"
                  inputMode="url"
                  maxLength={2048}
                  disabled={saving || snapshot.otlp.managedBy === "signoz"}
                  value={endpoint}
                  onChange={(event) => {
                    edited.current = true;
                    setEndpoint(event.target.value);
                    setSaveError(null);
                  }}
                  placeholder="https://collector.example.com"
                />
              </div>
              {snapshot.otlp.managedBy === "signoz" && (
                <p>
                  <a href="#/observability">
                    {t(
                      "Managed by the SigNoz integration. Open Observability to change this connection.",
                      "SigNoz 연동에서 관리한다. 옵저버빌리티에서 연결 설정을 변경한다.",
                    )}
                  </a>
                </p>
              )}
              <p>
                {t(
                  "Saving sends platform logs and events to this collector. App logs can contain sensitive values.",
                  "저장하면 플랫폼 로그와 이벤트를 이 수집기로 전송한다. 앱 로그에는 민감한 값이 포함될 수 있다.",
                )}
              </p>
              {!snapshot.otlp.queuePersistent && (
                <p>
                  {t(
                    "Queued data is lost on API restart or when the endpoint is changed or disabled.",
                    "API를 재시작하거나 수집기 주소를 바꾸거나 해제하면 대기 중인 데이터가 사라진다.",
                  )}
                </p>
              )}
              <p role="status">{snapshot.otlp.status}</p>
              <dl className="props service-otlp-metrics">
                {(
                  [
                    ["queued", t("Queued", "대기")],
                    ["inFlight", t("In flight", "전송 중")],
                    ["delivered", t("Delivered", "전송 완료")],
                    ["dropped", t("Dropped", "유실")],
                    ["rejected", t("Rejected", "거부")],
                    ["failedBatches", t("Failed batches", "실패한 묶음")],
                  ] as const
                ).map(([key, label]) => (
                  <div key={key}>
                    <dt>{label}</dt>
                    <dd>{snapshot.otlp[key]}</dd>
                  </div>
                ))}
              </dl>
              <button
                className="btn primary"
                disabled={saving || snapshot.otlp.managedBy === "signoz"}
              >
                {saving ? t("Saving…", "저장 중…") : t("Save", "저장")}
              </button>
            </form>
          )}
        </div>
      )}
    </section>
  );
}
