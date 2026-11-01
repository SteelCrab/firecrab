import { useEffect, useRef, useState } from "react";
import {
  Activity,
  ArrowRight,
  Box,
  ChevronRight,
  Clock,
  ExternalLink,
  Info,
  List,
  Pause,
  Play,
  RefreshCw,
  Settings2,
  ShieldCheck,
} from "lucide-react";
import {
  disconnectSignoz,
  getObservability,
  saveSignoz,
  sendSignozTest,
  type DeliveryState,
  type ObservationSnapshot,
} from "../api/observability";
import { useI18n } from "../i18n";
import Screen from "./layout/Screen";
import SignozSettings from "./SignozSettings";
import "../styles/observability.css";

function failure(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

export default function Observability({
  onUnsavedChange,
}: {
  onUnsavedChange: (dirty: boolean) => void;
}) {
  const { t, locale } = useI18n();
  const [snapshot, setSnapshot] = useState<ObservationSnapshot | null>(null);
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");
  const [busy, setBusy] = useState(false);
  const [editing, setEditing] = useState(false);
  const [tab, setTab] = useState<"integrations" | "history">("integrations");
  const revision = useRef(0),
    acting = useRef(false),
    mounted = useRef(false);
  const confirmRef = useRef<HTMLDialogElement>(null);
  useEffect(() => {
    mounted.current = true;
    let pending = false;
    const controller = new AbortController();
    const read = async () => {
      if (pending || acting.current) return;
      pending = true;
      const generation = revision.current;
      try {
        const value = await getObservability(controller.signal);
        if (mounted.current && generation === revision.current) {
          setSnapshot(value);
          setError("");
        }
      } catch (error) {
        if (
          mounted.current &&
          !controller.signal.aborted &&
          generation === revision.current
        )
          setError(failure(error));
      } finally {
        pending = false;
      }
    };
    void read();
    const timer = setInterval(() => void read(), 3000);
    return () => {
      mounted.current = false;
      controller.abort();
      clearInterval(timer);
      onUnsavedChange(false);
    };
  }, [onUnsavedChange]);

  const labels: Record<DeliveryState | "unconfigured", string> = {
    unconfigured: t("Not connected", "연결 전"),
    pending: t("Awaiting data", "전송 대기"),
    delivered: t("Connected", "연결됨"),
    retrying: t("Retrying", "재시도 중"),
    partial: t("Partially accepted", "일부 수락"),
    failed: t("Delivery error", "전송 오류"),
    disabled: t("Disabled", "사용 안 함"),
    paused: t("Paused", "일시 중지"),
  };
  const config = snapshot?.config;
  const sources = snapshot?.sources.filter((source) => source.selected) ?? [];
  const statuses = sources.flatMap((source) =>
    source.status ? [source.status] : [],
  );
  const state: DeliveryState | "unconfigured" = !config
    ? "unconfigured"
    : config.paused
      ? "paused"
      : statuses.some((status) =>
            ["failed", "retrying", "partial"].includes(status.state),
          )
        ? "failed"
        : statuses.some((status) => status.state === "delivered")
          ? "delivered"
          : "pending";
  const total = (
    key:
      | "delivered"
      | "queued"
      | "failedBatches"
      | "dropped"
      | "rejected"
      | "inFlight",
  ) => statuses.reduce((sum, status) => sum + status[key], 0);
  const format = (value: number) =>
    value.toLocaleString(locale === "ko" ? "ko-KR" : "en-US");
  const date = (value: number | null) =>
    value
      ? new Date(value).toLocaleTimeString(
          locale === "ko" ? "ko-KR" : "en-GB",
          { hour: "2-digit", minute: "2-digit", second: "2-digit" },
        )
      : "—";
  const last = Math.max(
    0,
    ...statuses.map((status) => status.lastDeliveredAtMs ?? 0),
  );
  async function mutate(action: () => Promise<ObservationSnapshot>) {
    if (acting.current) return;
    acting.current = true;
    revision.current++;
    setBusy(true);
    setError("");
    setNotice("");
    try {
      const value = await action();
      if (mounted.current) setSnapshot(value);
    } catch (error) {
      if (mounted.current) setError(failure(error));
      throw error;
    } finally {
      acting.current = false;
      if (mounted.current) setBusy(false);
    }
  }
  async function sendTest() {
    if (acting.current || !sources.length || config?.paused) return;
    acting.current = true;
    revision.current++;
    setBusy(true);
    setNotice("");
    setError("");
    try {
      const result = await sendSignozTest(sources[0].id);
      if (!mounted.current) return;
      setNotice(
        t(
          `Test batch: ${result.delivered}/${result.records} records accepted. ${result.httpStatus ? `HTTP ${result.httpStatus}.` : "No HTTP response."}`,
          `테스트 배치: ${result.records}건 중 ${result.delivered}건 수락. ${result.httpStatus ? `HTTP ${result.httpStatus}.` : "HTTP 응답 없음."}`,
        ),
      );
      setSnapshot(await getObservability());
    } catch (error) {
      if (mounted.current) setError(failure(error));
    } finally {
      acting.current = false;
      if (mounted.current) setBusy(false);
    }
  }
  const minute = Math.floor((snapshot?.observedAtMs ?? 0) / 60000);
  const volumes = Array.from({ length: 60 }, (_, index) => ({
    minute: minute - 59 + index,
    count: 0,
  }));
  for (const status of statuses)
    for (const batch of status.history) {
      const index = Math.floor(batch.timestampMs / 60000) - minute + 59;
      if (index >= 0 && index < volumes.length)
        volumes[index].count += batch.delivered;
    }
  const peak = Math.max(1, ...volumes.map((bucket) => bucket.count));
  return (
    <Screen view="observability" live={false}>
      <div className="observability-page">
        <div className="obs-intro">
          <div>
            <span className="obs-eyebrow">OBSERVABILITY</span>
            <h2>
              {t(
                "A clearer view of your services.",
                "서비스의 흐름을 더 선명하게.",
              )}
            </h2>
            <p>
              {t(
                "Send service logs and events to your own platform and inspect delivery.",
                "서비스 로그와 이벤트를 외부 플랫폼으로 보내고, 전송 상태를 확인한다.",
              )}
            </p>
          </div>
          <a
            className="btn"
            href="https://signoz.io/docs/ingestion/self-hosted/overview/"
            target="_blank"
            rel="noreferrer"
          >
            {t("Integration guide", "연동 가이드")}
            <ExternalLink size={14} />
          </a>
        </div>
        {error && (
          <p className="obs-notice error" role="alert">
            {error}
          </p>
        )}
        {notice && (
          <p className="obs-notice" role="status">
            {notice}
          </p>
        )}
        <div
          className="obs-tabs"
          role="tablist"
          aria-label={t("Observability views", "옵저버빌리티 화면")}
        >
          {(["integrations", "history"] as const).map((id, index) => (
            <button
              type="button"
              key={id}
              role="tab"
              id={`obs-tab-${id}`}
              aria-controls={`obs-${id}`}
              aria-selected={tab === id}
              tabIndex={tab === id ? 0 : -1}
              onClick={() => setTab(id)}
              onKeyDown={(event) => {
                if (
                  ["ArrowLeft", "ArrowRight", "Home", "End"].includes(event.key)
                ) {
                  event.preventDefault();
                  const next =
                    event.key === "Home"
                      ? "integrations"
                      : event.key === "End"
                        ? "history"
                        : index === 0
                          ? "history"
                          : "integrations";
                  setTab(next);
                  document.getElementById(`obs-tab-${next}`)?.focus();
                }
              }}
            >
              {id === "integrations"
                ? t("Integrations", "외부 연동")
                : t("Delivery history", "전송 기록")}
              {id === "integrations" && <span>1</span>}
            </button>
          ))}
          <span className="obs-tab-note">
            {t("First integration, SigNoz", "첫 번째 연동, SigNoz")}
          </span>
        </div>
        {!snapshot && (
          <p role="status">
            {t("Loading observability…", "관측 정보를 불러오는 중…")}
          </p>
        )}
        {snapshot && (
          <>
            <section
              role="tabpanel"
              id="obs-integrations"
              aria-labelledby="obs-tab-integrations"
              hidden={tab !== "integrations"}
            >
              <div className="obs-provider">
                <div className="obs-provider-top">
                  <div className="obs-identity">
                    <span className="obs-signoz">
                      <Activity />
                    </span>
                    <div>
                      <div className="obs-provider-name">
                        <h3>SigNoz</h3>
                        <span className="obs-tag">Open source</span>
                        <span className="obs-tag">Self-hosted</span>
                      </div>
                      <p>
                        {t(
                          "Observability data, on your infrastructure.",
                          "내 인프라에 보관하는 관측 데이터",
                        )}
                      </p>
                    </div>
                  </div>
                  <span className={`obs-state ${state}`}>
                    <i />
                    {labels[state]}
                  </span>
                </div>
                <div className="obs-provider-details">
                  <div>
                    <span className="obs-caption">
                      {t("Collector endpoint", "수집기 주소")}
                    </span>
                    <code>
                      {config?.endpoint ??
                        t(
                          "No collector connected.",
                          "아직 연결된 수집기가 없다.",
                        )}
                    </code>
                    {config && (
                      <span className="obs-protocol">OTLP / HTTP</span>
                    )}
                  </div>
                  <div className="obs-actions">
                    {config && (
                      <>
                        <button
                          className="btn"
                          disabled={busy || config.paused || !sources.length}
                          onClick={() => void sendTest()}
                        >
                          <RefreshCw size={14} />
                          {busy
                            ? t("Working…", "처리 중…")
                            : t("Send test log", "테스트 로그 전송")}
                        </button>
                        <button
                          className="btn"
                          disabled={busy}
                          onClick={() =>
                            void mutate(() =>
                              saveSignoz({ ...config, paused: !config.paused }),
                            ).catch(() => {})
                          }
                        >
                          {config.paused ? (
                            <Play size={14} />
                          ) : (
                            <Pause size={14} />
                          )}{" "}
                          {config.paused
                            ? t("Resume", "재개")
                            : t("Pause", "일시 중지")}
                        </button>
                      </>
                    )}
                    <button
                      className="btn primary"
                      disabled={busy}
                      onClick={() => setEditing(true)}
                    >
                      <Settings2 size={14} />
                      {config
                        ? t("Connection settings", "연결 설정")
                        : t("Connect SigNoz", "SigNoz 연결")}
                    </button>
                  </div>
                </div>
                <div className="obs-provider-footer">
                  <span>
                    <List size={13} />
                    {t("Logs & events", "로그·이벤트 전송")}
                  </span>
                  <span className="obs-planned">
                    {t("Metrics", "메트릭")}{" "}
                    <small>{t("Planned", "예정")}</small>
                  </span>
                  <span className="obs-planned">
                    {t("Traces", "트레이스")}{" "}
                    <small>{t("Planned", "예정")}</small>
                  </span>
                  <a
                    href="https://signoz.io/docs/product-features/logs-explorer/"
                    target="_blank"
                    rel="noreferrer"
                  >
                    {t("SigNoz logs guide", "SigNoz 로그 탐색 가이드")}
                    <ExternalLink size={12} />
                  </a>
                </div>
              </div>
              {state === "failed" && (
                <p className="obs-notice error" role="alert">
                  <Info size={16} />
                  {t(
                    "Some records were not accepted. Check the collector and delivery history, then send a test log to retry.",
                    "수락되지 않은 기록이 있다. 수집기와 전송 기록을 확인한 뒤 테스트 로그로 연결을 다시 확인한다.",
                  )}
                </p>
              )}
              {config?.paused && (
                <p className="obs-notice">
                  <Pause size={15} />
                  {t(
                    "Paused. New records enter the bounded queue. Resume to send pending records; overflow records are dropped.",
                    "전송을 일시 중지했다. 새 기록은 제한된 대기열에 보관한다. 재개하면 전송한다. 한도를 넘는 기록은 버린다.",
                  )}
                </p>
              )}
              <div className="obs-pipeline">
                <div>
                  <span className="obs-node">
                    <Box />
                  </span>
                  <div>
                    <strong>MicroService</strong>
                    <small>
                      {t(
                        `${sources.length} selected services`,
                        `선택한 서비스 ${sources.length}개`,
                      )}
                    </small>
                  </div>
                </div>
                <ChevronRight />
                <div>
                  <span className="obs-node">
                    <img src="/firecrab.png" alt="" width={22} />
                  </span>
                  <div>
                    <strong>Firecrab</strong>
                    <small>
                      {t(
                        "Collection & delivery queue",
                        "로그 수집 · 전송 대기열",
                      )}
                    </small>
                  </div>
                </div>
                <ChevronRight />
                <div>
                  <span className="obs-node orange">
                    <Activity />
                  </span>
                  <div>
                    <strong>SigNoz</strong>
                    <small>
                      {t("Receive, search, analyze", "수신 · 검색 · 분석")}
                    </small>
                  </div>
                </div>
              </div>
              <dl className="obs-stats">
                {[
                  [
                    t("Accepted records", "수락한 기록"),
                    format(total("delivered")),
                    t("Since exporter start", "전송기 시작 이후"),
                  ],
                  [
                    t("Queued records", "전송 대기"),
                    format(total("queued")),
                    t(
                      `${total("inFlight")} in flight`,
                      `${total("inFlight")}건 전송 중`,
                    ),
                  ],
                  [
                    t("Failed batches", "실패한 배치"),
                    format(total("failedBatches")),
                    t(
                      `${total("dropped")} dropped · ${total("rejected")} rejected`,
                      `${total("dropped")}건 버림 · ${total("rejected")}건 거부`,
                    ),
                  ],
                  [
                    t("Last accepted delivery", "최근 수락한 전송"),
                    date(last),
                    t("Collector acknowledgement", "수집기 수락 응답"),
                  ],
                ].map(([label, value, note]) => (
                  <div key={label}>
                    <dt>{label}</dt>
                    <dd>{value}</dd>
                    <small>{note}</small>
                  </div>
                ))}
              </dl>
              <div className="obs-section-heading">
                <div>
                  <h3>
                    {t("Sources", "전송 소스")} <span>{sources.length}</span>
                  </h3>
                  <p>
                    {t(
                      "Only new logs and events from selected services are exported.",
                      "선택한 서비스에서 새로 발생한 로그와 이벤트를 전송한다.",
                    )}
                  </p>
                </div>
                <button
                  className="obs-text-button"
                  disabled={busy}
                  onClick={() => setEditing(true)}
                >
                  {t("Manage sources", "소스 관리")}
                  <ArrowRight size={14} />
                </button>
              </div>
              <div className="obs-table-wrap">
                <table>
                  <thead>
                    <tr>
                      <th>{t("Service", "서비스")}</th>
                      <th>{t("Data", "전송 데이터")}</th>
                      <th>{t("State", "상태")}</th>
                      <th>{t("Last delivery", "최근 전송")}</th>
                      <th className="obs-number">
                        {t("Accepted", "수락한 기록")}
                      </th>
                    </tr>
                  </thead>
                  <tbody>
                    {sources.map((source) => (
                      <tr key={source.id}>
                        <td>
                          <span className="obs-service">
                            <Box size={16} />
                            {source.name}
                          </span>
                        </td>
                        <td>{t("Logs & events", "로그·이벤트")}</td>
                        <td>
                          <span
                            className={`obs-state ${source.status?.state ?? "pending"}`}
                          >
                            <i />
                            {labels[source.status?.state ?? "pending"]}
                          </span>
                        </td>
                        <td>
                          {date(source.status?.lastDeliveredAtMs ?? null)}
                        </td>
                        <td className="obs-number">
                          {format(source.status?.delivered ?? 0)}
                        </td>
                      </tr>
                    ))}
                    {!sources.length && (
                      <tr>
                        <td colSpan={5} className="obs-empty">
                          {t(
                            "Connect SigNoz and select services to start exporting.",
                            "SigNoz를 연결하고 전송할 서비스를 선택한다.",
                          )}
                        </td>
                      </tr>
                    )}
                  </tbody>
                </table>
              </div>
              <div className="obs-bottom">
                <section className="obs-chart-card">
                  <div>
                    <h3>{t("Accepted log records", "수락한 로그 기록")}</h3>
                    <span>
                      {t(
                        "Last 60 minutes · retained batches",
                        "최근 60분 · 보관한 배치 기준",
                      )}
                    </span>
                  </div>
                  <div
                    className="obs-chart"
                    role="img"
                    aria-label={t(
                      "Accepted records per minute in retained delivery history",
                      "보관한 전송 기록의 분당 수락 건수",
                    )}
                  >
                    {volumes.map((bucket) => (
                      <span
                        key={bucket.minute}
                        style={{ height: `${(bucket.count / peak) * 100}%` }}
                        title={`${date(bucket.minute * 60000)} · ${format(bucket.count)}`}
                      />
                    ))}
                  </div>
                  <div className="obs-chart-axis">
                    <span>{date((minute - 59) * 60000)}</span>
                    <span>{date(minute * 60000)}</span>
                  </div>
                </section>
                <aside className="obs-information">
                  <h3>
                    <ShieldCheck size={16} />
                    {t(
                      "Your data, your infrastructure.",
                      "데이터는 내 인프라에.",
                    )}
                  </h3>
                  <p>
                    {t(
                      "Records go directly to the configured collector. Search the accepted data in your SigNoz instance.",
                      "설정한 수집기로 직접 전송한다. 수락된 데이터는 SigNoz에서 검색한다.",
                    )}
                  </p>
                  <span>
                    {t("First integration scope", "이번 연동 범위")}
                    <strong>
                      {t("Logs + service events", "로그 + 서비스 이벤트")}
                    </strong>
                  </span>
                </aside>
              </div>
            </section>
            <section
              role="tabpanel"
              id="obs-history"
              aria-labelledby="obs-tab-history"
              hidden={tab !== "history"}
            >
              <div className="obs-section-heading">
                <div>
                  <h3>{t("Recent delivery history", "최근 전송 기록")}</h3>
                  <p>
                    {t(
                      `Up to ${snapshot.displayHistoryLimit} recent batches across selected services.`,
                      `선택한 서비스의 최근 배치를 최대 ${snapshot.displayHistoryLimit}개 표시한다.`,
                    )}
                  </p>
                </div>
                <Clock size={17} />
              </div>
              <div className="obs-table-wrap">
                <table>
                  <thead>
                    <tr>
                      <th>{t("Time", "시간")}</th>
                      <th>{t("Service", "서비스")}</th>
                      <th>{t("Accepted / total", "수락 / 전체")}</th>
                      <th>{t("Result", "결과")}</th>
                      <th>HTTP</th>
                    </tr>
                  </thead>
                  <tbody>
                    {snapshot.history.map((batch, index) => (
                      <tr
                        key={`${batch.serviceId}-${batch.timestampMs}-${index}`}
                      >
                        <td>{date(batch.timestampMs)}</td>
                        <td>{batch.serviceName}</td>
                        <td>
                          {format(batch.delivered)} / {format(batch.records)}
                        </td>
                        <td>
                          <span className={`obs-state ${batch.status}`}>
                            <i />
                            {labels[batch.status]}
                          </span>
                        </td>
                        <td>
                          {batch.httpStatus ?? t("No response", "응답 없음")}
                        </td>
                      </tr>
                    ))}
                    {!snapshot.history.length && (
                      <tr>
                        <td colSpan={5} className="obs-empty">
                          {t(
                            "No completed deliveries yet.",
                            "완료한 전송이 없다.",
                          )}
                        </td>
                      </tr>
                    )}
                  </tbody>
                </table>
              </div>
            </section>
            <footer className="obs-footnote">
              <Info size={14} />
              <span>
                {t(
                  `Counters and history reset after an API restart or exporter replacement. Up to ${snapshot.historyLimitPerService} batches per service are retained in memory. The queue holds up to ${snapshot.queueLimitPerService} records per service, plus an active batch, and does not survive restarts.`,
                  `API 재시작 또는 전송기 교체 시 집계와 기록을 초기화한다. 서비스당 최근 ${snapshot.historyLimitPerService}개 배치를 메모리에 보관한다. 대기열은 서비스당 ${snapshot.queueLimitPerService}건과 처리 중인 배치로 제한한다. 재시작하면 대기 기록이 사라진다.`,
                )}
              </span>
            </footer>
          </>
        )}
        {editing && snapshot && (
          <SignozSettings
            snapshot={snapshot}
            busy={busy}
            onClose={() => setEditing(false)}
            onDirty={onUnsavedChange}
            onSave={async (draft) => {
              await mutate(() => saveSignoz(draft));
              if (mounted.current) setEditing(false);
            }}
            onDisconnect={() => confirmRef.current?.showModal()}
          />
        )}
        <dialog
          ref={confirmRef}
          className="obs-confirm"
          aria-labelledby="obs-disconnect-title"
        >
          <h2 id="obs-disconnect-title">
            {t("Disconnect SigNoz?", "SigNoz 연결을 해제할까?")}
          </h2>
          <p>
            {t(
              "Stop new exports. Data already accepted by SigNoz remains there. Pending records in Firecrab will be discarded.",
              "새 전송을 중단한다. SigNoz에서 이미 수락한 데이터는 유지한다. Firecrab의 대기 기록은 버린다.",
            )}
          </p>
          <div className="obs-actions">
            <button
              className="btn"
              disabled={busy}
              onClick={() => confirmRef.current?.close()}
            >
              {t("Cancel", "취소")}
            </button>
            <button
              className="btn danger"
              disabled={busy}
              onClick={() =>
                void mutate(disconnectSignoz)
                  .then(() => {
                    confirmRef.current?.close();
                    setEditing(false);
                  })
                  .catch(() => {})
              }
            >
              {t("Disconnect", "연결 해제")}
            </button>
          </div>
          {error && <p role="alert">{error}</p>}
        </dialog>
      </div>
    </Screen>
  );
}
