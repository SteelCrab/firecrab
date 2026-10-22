import { useCallback, useEffect, useState } from "react";
import { listMicroServices } from "../api/client";
import type { MicroServiceResponse } from "../bindings";
import * as db from "../api/pipeDatabases";
import { useI18n } from "../i18n";

export default function DatabaseServices({
  connection,
  tables,
  onDirty,
}: {
  connection: db.PipeDatabase;
  tables: db.DatabaseTable[];
  onDirty: (dirty: boolean) => void;
}) {
  const { t } = useI18n();
  const [open, setOpen] = useState(false),
    [busy, setBusy] = useState(false),
    [error, setError] = useState<string | null>(null),
    [notice, setNotice] = useState<string | null>(null);
  const [services, setServices] = useState<MicroServiceResponse[]>([]),
    [bindings, setBindings] = useState<db.DatabaseBinding[]>([]),
    [triggers, setTriggers] = useState<db.DatabaseTrigger[]>([]);
  const [kafka, setKafka] = useState<db.KafkaStatus>({ configured: false }),
    [managed, setManaged] = useState<db.ManagedKafka | null>(null),
    [development, setDevelopment] = useState(false);
  const [service, setService] = useState(""),
    [access, setAccess] = useState<"read" | "write">("read"),
    [bindingId, setBindingId] = useState(() => crypto.randomUUID());
  const [handler, setHandler] = useState(""),
    [table, setTable] = useState(""),
    [path, setPath] = useState("/events/database"),
    [triggerId, setTriggerId] = useState(() => crypto.randomUUID());
  const [brokers, setBrokers] = useState(""),
    [ca, setCa] = useState(""),
    [username, setUsername] = useState(""),
    [password, setPassword] = useState(""),
    [tls, setTls] = useState(true);
  const [clientCertificate, setClientCertificate] = useState(""),
    [clientKey, setClientKey] = useState("");
  const dirty =
    !!service ||
    !!handler ||
    table !== "" ||
    !!brokers ||
    !!ca ||
    !!username ||
    !!password ||
    !!clientCertificate ||
    !!clientKey;
  useEffect(() => {
    onDirty(dirty);
    return () => onDirty(false);
  }, [dirty, onDirty]);
  const [failures, setFailures] = useState<{
    trigger: string;
    rows: { eventId: string; attempts: number }[];
  } | null>(null);
  const load = useCallback(async () => {
    const [s, b, tr, k, m] = await Promise.all([
      listMicroServices(),
      db.databaseBindings(connection.id),
      db.databaseTriggers(connection.id),
      db.kafkaStatus(),
      db.managedKafkaStatus(),
    ]);
    setServices(s);
    setBindings(b);
    setTriggers(tr);
    setKafka(k);
    setManaged(m);
  }, [connection.id]);
  useEffect(() => {
    if (!open) return;
    let active = true;
    const refresh = () => {
      void load().catch((e: Error) => {
        if (active) setError(e.message);
      });
    };
    refresh();
    const timer = setInterval(refresh, 5000);
    return () => {
      active = false;
      clearInterval(timer);
    };
  }, [open, load]);
  async function run(action: () => Promise<unknown>, success: string) {
    if (busy) return;
    setBusy(true);
    setError(null);
    setNotice(null);
    try {
      await action();
      await load();
      setNotice(success);
    } catch (e) {
      setError((e as Error).message);
      try {
        await load();
      } catch {
        /* The original operation failure is the actionable error. */
      }
    } finally {
      setBusy(false);
    }
  }
  const serviceName = (id: string) =>
    services.find((s) => s.id === id)?.name ?? id;
  return (
    <details
      className="pipe-database-security"
      onToggle={(event) => setOpen(event.currentTarget.open)}
    >
      <summary>
        {t("Services & reliable events", "서비스 연결과 안전한 이벤트")}
      </summary>
      {error && (
        <div className="banner error" role="alert">
          {error}
        </div>
      )}
      {notice && (
        <div className="banner info" role="status">
          {notice}
        </div>
      )}
      <section aria-label={t("Database service bindings", "DB 서비스 연결")}>
        <h4>{t("Service access", "서비스 접근")}</h4>
        <p className="poll-note">
          {t(
            "Select a stopped service. Firecrab creates its account and injects DATABASE_URL and CA files on deployment.",
            "중지한 서비스를 선택한다. Firecrab이 전용 계정을 만들고 배포 시 DATABASE_URL과 CA 파일을 주입한다.",
          )}
        </p>
        {bindings.map((binding) => (
          <div className="pipe-database-binding" key={binding.id}>
            <span>
              {serviceName(binding.serviceId)} ·{" "}
              {binding.access === "read"
                ? t("Read only", "읽기 전용")
                : t("Read & write", "읽기·쓰기")}{" "}
              ·{" "}
              {binding.ready
                ? t(
                    `${binding.tableCount} tables`,
                    `${binding.tableCount}개 테이블`,
                  )
                : t("Setup incomplete", "설정 미완료")}
            </span>
            <div className="pipe-row-actions">
              {!binding.ready && (
                <button
                  className="btn sm"
                  type="button"
                  disabled={busy}
                  onClick={() =>
                    void run(
                      () => db.bindDatabase(connection.id, binding),
                      t("Binding ready.", "서비스 연결을 준비했다."),
                    )
                  }
                >
                  {t("Retry setup", "설정 재시도")}
                </button>
              )}
              <button
                className="btn sm danger"
                type="button"
                disabled={busy}
                onClick={() =>
                  void run(
                    () => db.removeBinding(binding.id),
                    t("Account access revoked.", "계정 접근을 해제했다."),
                  )
                }
              >
                {t("Remove binding", "서비스 연결 해제")}
              </button>
            </div>
          </div>
        ))}
        <form
          onSubmit={(e) => {
            e.preventDefault();
            void run(
              async () => {
                await db.bindDatabase(connection.id, {
                  requestId: bindingId,
                  serviceId: service,
                  access,
                });
                setBindingId(crypto.randomUUID());
                setService("");
              },
              t(
                "Binding ready. Deploy the service to use it.",
                "서비스 연결을 준비했다. 서비스를 배포하면 적용한다.",
              ),
            );
          }}
        >
          <div className="pipe-database-fields">
            <div className="field">
              <label htmlFor="database-binding-service">
                {t("Service", "서비스")}
              </label>
              <select
                id="database-binding-service"
                value={service}
                disabled={busy}
                required
                onChange={(e) => {
                  setService(e.target.value);
                  setBindingId(crypto.randomUUID());
                }}
              >
                <option value="">{t("Select service", "서비스 선택")}</option>
                {services.map((s) => (
                  <option value={s.id} key={s.id}>
                    {s.name}
                  </option>
                ))}
              </select>
            </div>
            <div className="field">
              <label htmlFor="database-binding-access">
                {t("Access", "접근 권한")}
              </label>
              <select
                id="database-binding-access"
                value={access}
                disabled={busy}
                onChange={(e) => {
                  setAccess(e.target.value as "read" | "write");
                  setBindingId(crypto.randomUUID());
                }}
              >
                <option value="read">{t("Read only", "읽기 전용")}</option>
                {connection.writeEnabled && (
                  <option value="write">
                    {t("Read & write", "읽기·쓰기")}
                  </option>
                )}
              </select>
            </div>
          </div>
          <button className="btn" type="submit" disabled={busy || !service}>
            {t("Connect service", "서비스 연결")}
          </button>
        </form>
      </section>
      <section aria-label={t("Reliable database events", "안전한 DB 이벤트")}>
        <h4>{t("Database events", "DB 이벤트")}</h4>
        <p className="poll-note">
          {t(
            "Changes commit with an outbox entry. Kafka and HTTP delivery retry automatically. Handlers must deduplicate by event ID in their own transaction.",
            "변경과 이벤트를 같은 트랜잭션에 저장한다. Kafka와 HTTP 전달은 자동으로 재시도한다. 처리 서비스는 자체 트랜잭션에서 이벤트 ID의 중복을 검사해야 한다.",
          )}
        </p>
        {!kafka.configured && (
          <div className="pipe-database-form">
            <h4>{t("Prepare Kafka once", "Kafka 최초 준비")}</h4>
            <div className="field">
              <label htmlFor="database-kafka-profile">
                {t("Kafka profile", "Kafka 구성")}
              </label>
              <select
                id="database-kafka-profile"
                value={development ? "development" : "durable"}
                disabled={busy}
                onChange={(e) =>
                  setDevelopment(e.target.value === "development")
                }
              >
                <option value="durable">
                  {t(
                    "Durable · 3 brokers, 30 GiB",
                    "내구성 구성 · 브로커 3개, 30 GiB",
                  )}
                </option>
                <option value="development">
                  {t(
                    "Development · 1 broker, 10 GiB",
                    "개발용 · 브로커 1개, 10 GiB",
                  )}
                </option>
              </select>
            </div>
            <p className="poll-note">
              {development
                ? t(
                    "Single-broker development cannot tolerate broker data loss.",
                    "개발용 단일 브로커는 브로커 데이터 손실을 견디지 못한다.",
                  )
                : t(
                    "Each broker uses 1536 MiB RAM. All local brokers share one host failure domain. Use an external cluster for host failover.",
                    "브로커당 메모리 1536 MiB를 사용한다. 로컬 브로커는 같은 호스트에 위치한다. 호스트 장애 전환에는 외부 클러스터를 사용한다.",
                  )}
            </p>
            <button
              className="btn primary"
              type="button"
              disabled={busy || (!!managed && managed.phase !== "failed")}
              onClick={() =>
                void run(
                  () => db.createManagedKafka(development),
                  t("Kafka preparation started.", "Kafka 준비를 시작했다."),
                )
              }
            >
              {managed?.phase === "failed"
                ? t("Retry Kafka setup", "Kafka 준비 재시도")
                : t("Prepare Kafka automatically", "Kafka 자동 준비")}
            </button>
          </div>
        )}
        {managed && (
          <p className="poll-note" role="status">
            Kafka · {managed.phase}
            {managed.error ? ` · ${managed.error}` : ""}
          </p>
        )}
        {kafka.configured && (
          <p className="poll-note">
            Kafka ·{" "}
            {kafka.development
              ? t("Development", "개발용")
              : t("TLS enabled", "TLS 사용")}{" "}
            · {kafka.brokers?.join(", ")}
          </p>
        )}
        <details>
          <summary>{t("Use existing Kafka", "기존 Kafka 연결")}</summary>
          <form
            onSubmit={(e) => {
              e.preventDefault();
              void run(
                async () => {
                  await db.saveKafka({
                    brokers: brokers
                      .split(",")
                      .map((b) => b.trim())
                      .filter(Boolean),
                    tls,
                    development,
                    ...(ca.trim() ? { caCertificate: ca } : {}),
                    ...(username ? { username, password } : {}),
                    ...(clientCertificate
                      ? { clientCertificate, clientKey }
                      : {}),
                  });
                  setPassword("");
                  setCa("");
                  setUsername("");
                  setBrokers("");
                  setClientCertificate("");
                  setClientKey("");
                },
                t("Kafka connection saved.", "Kafka 연결을 저장했다."),
              );
            }}
          >
            <div className="field">
              <label htmlFor="database-kafka-brokers">
                {t("Broker addresses", "브로커 주소")}
              </label>
              <input
                id="database-kafka-brokers"
                placeholder="broker-1:9093, broker-2:9093, broker-3:9093"
                value={brokers}
                required
                disabled={busy}
                onChange={(e) => setBrokers(e.target.value)}
              />
            </div>
            <div className="pipe-database-fields">
              <div className="field">
                <label htmlFor="database-kafka-user">
                  {t(
                    "SASL/PLAIN username (optional)",
                    "SASL/PLAIN 계정 (선택)",
                  )}
                </label>
                <input
                  id="database-kafka-user"
                  autoComplete="off"
                  value={username}
                  disabled={busy}
                  onChange={(e) => setUsername(e.target.value)}
                />
              </div>
              <div className="field">
                <label htmlFor="database-kafka-password">
                  {t("SASL password", "SASL 비밀번호")}
                </label>
                <input
                  id="database-kafka-password"
                  type="password"
                  autoComplete="new-password"
                  value={password}
                  disabled={busy}
                  onChange={(e) => setPassword(e.target.value)}
                />
              </div>
            </div>
            <div className="field">
              <label htmlFor="database-kafka-ca">
                {t(
                  "Kafka CA certificate (PEM, optional)",
                  "Kafka CA 인증서 (PEM, 선택)",
                )}
              </label>
              <textarea
                id="database-kafka-ca"
                value={ca}
                rows={3}
                maxLength={32768}
                disabled={busy}
                onChange={(e) => setCa(e.target.value)}
              />
            </div>
            <details>
              <summary>
                {t(
                  "Client certificates (optional)",
                  "클라이언트 인증서 (선택)",
                )}
              </summary>
              <div className="field">
                <label htmlFor="kafka-client-certificate">
                  {t("Client certificate PEM", "클라이언트 인증서 PEM")}
                </label>
                <textarea
                  id="kafka-client-certificate"
                  value={clientCertificate}
                  disabled={busy}
                  maxLength={32768}
                  onChange={(e) => setClientCertificate(e.target.value)}
                />
              </div>
              <div className="field">
                <label htmlFor="kafka-client-key">
                  {t("Client private key PEM", "클라이언트 개인 키 PEM")}
                </label>
                <textarea
                  id="kafka-client-key"
                  value={clientKey}
                  disabled={busy}
                  maxLength={32768}
                  autoComplete="off"
                  onChange={(e) => setClientKey(e.target.value)}
                />
              </div>
            </details>
            <label>
              <input
                type="checkbox"
                checked={development}
                disabled={busy}
                onChange={(e) => setDevelopment(e.target.checked)}
              />
              {t("Single-broker development", "단일 브로커 개발용")}
            </label>
            <label>
              <input
                type="checkbox"
                checked={tls}
                disabled={busy || !development}
                onChange={(e) => setTls(e.target.checked)}
              />{" "}
              TLS
            </label>
            <button className="btn" type="submit" disabled={busy}>
              {t("Save Kafka connection", "Kafka 연결 저장")}
            </button>
          </form>
        </details>
        {triggers.map((trigger) => (
          <div className="pipe-database-binding" key={trigger.id}>
            <span>
              {trigger.schema}.{trigger.table} →{" "}
              {serviceName(trigger.serviceId)}
              <small>
                {t(
                  `Pending ${trigger.pending} · Failed ${trigger.failed} · Delivered ${trigger.delivered}`,
                  `대기 ${trigger.pending} · 실패 ${trigger.failed} · 전달 ${trigger.delivered}`,
                )}
                {trigger.detached
                  ? ` · ${t("Capture detached", "변경 감지 해제")}`
                  : trigger.paused
                    ? ` · ${t("Paused", "일시 중지")}`
                    : ""}
                {trigger.lastError ? ` · ${trigger.lastError}` : ""}
              </small>
            </span>
            <div className="pipe-row-actions">
              {!trigger.installed && !trigger.detached && (
                <button
                  className="btn sm"
                  type="button"
                  disabled={busy}
                  onClick={() =>
                    void run(
                      () => db.createDatabaseTrigger(connection.id, trigger),
                      t("Trigger ready.", "트리거를 준비했다."),
                    )
                  }
                >
                  {t("Retry setup", "설정 재시도")}
                </button>
              )}
              {!trigger.detached && (
                <>
                  {trigger.installed && (
                    <button
                      className="btn sm"
                      type="button"
                      disabled={busy}
                      onClick={() =>
                        void run(
                          () =>
                            db.pauseDatabaseTrigger(
                              trigger.id,
                              !trigger.paused,
                            ),
                          t("Delivery setting saved.", "전달 설정을 저장했다."),
                        )
                      }
                    >
                      {trigger.paused
                        ? t("Resume", "다시 시작")
                        : t("Pause", "일시 중지")}
                    </button>
                  )}
                  <button
                    className="btn sm"
                    type="button"
                    disabled={busy}
                    onClick={() =>
                      void run(
                        () => db.detachDatabaseTrigger(trigger.id),
                        t(
                          "Capture detached. Retained events continue to drain.",
                          "변경 감지를 해제했다. 남은 이벤트는 계속 전달한다.",
                        ),
                      )
                    }
                  >
                    {t("Detach capture", "변경 감지 해제")}
                  </button>
                </>
              )}
              {trigger.failed > 0 && (
                <button
                  className="btn sm"
                  type="button"
                  disabled={busy}
                  onClick={() =>
                    void run(
                      async () =>
                        setFailures({
                          trigger: trigger.id,
                          rows: await db.failedDatabaseEvents(trigger.id),
                        }),
                      t("Failed events loaded.", "실패 이벤트를 불러왔다."),
                    )
                  }
                >
                  {t("Failed events", "실패 이벤트")}
                </button>
              )}
              {trigger.detached &&
                trigger.pending === 0 &&
                trigger.failed === 0 && (
                  <button
                    className="btn sm danger"
                    type="button"
                    disabled={busy}
                    onClick={() =>
                      void run(
                        () => db.removeDatabaseTrigger(trigger.id),
                        t(
                          "Trigger removed. Receipts retained in the source DB.",
                          "트리거를 제거했다. 처리 기록은 원본 DB에 보관한다.",
                        ),
                      )
                    }
                  >
                    {t("Remove trigger", "트리거 제거")}
                  </button>
                )}
            </div>
          </div>
        ))}
        {failures && (
          <div aria-label={t("Failed event replay", "실패 이벤트 재처리")}>
            {failures.rows.map((event) => (
              <div className="pipe-database-binding" key={event.eventId}>
                <code>{event.eventId}</code>
                <button
                  className="btn sm"
                  type="button"
                  disabled={busy}
                  onClick={() =>
                    void run(
                      async () => {
                        await db.replayDatabaseEvent(
                          failures.trigger,
                          event.eventId,
                        );
                        setFailures((current) =>
                          current
                            ? {
                                ...current,
                                rows: current.rows.filter(
                                  (e) => e.eventId !== event.eventId,
                                ),
                              }
                            : null,
                        );
                      },
                      t(
                        "Replay queued with the original event ID.",
                        "기존 이벤트 ID로 재처리를 예약했다.",
                      ),
                    )
                  }
                >
                  {t("Replay", "재처리")}
                </button>
              </div>
            ))}
          </div>
        )}
        {connection.writeEnabled && (
          <form
            onSubmit={(e) => {
              e.preventDefault();
              const selected = tables[Number(table)];
              if (!selected) return;
              void run(
                async () => {
                  await db.createDatabaseTrigger(connection.id, {
                    requestId: triggerId,
                    schema: selected.schema,
                    table: selected.name,
                    serviceId: handler,
                    path,
                  });
                  setTriggerId(crypto.randomUUID());
                  setHandler("");
                  setTable("");
                },
                t(
                  "Capture enabled. Deploy the handler service.",
                  "변경 감지를 켰다. 처리 서비스를 배포한다.",
                ),
              );
            }}
          >
            <div className="pipe-database-fields">
              <div className="field">
                <label htmlFor="database-event-table">
                  {t("Source table", "감지할 테이블")}
                </label>
                <select
                  id="database-event-table"
                  value={table}
                  required
                  disabled={busy || !kafka.configured}
                  onChange={(e) => {
                    setTable(e.target.value);
                    setTriggerId(crypto.randomUUID());
                  }}
                >
                  <option value="">{t("Select a table", "테이블 선택")}</option>
                  {tables.map((table, index) => (
                    <option key={JSON.stringify(table)} value={index}>
                      {table.schema}.{table.name}
                    </option>
                  ))}
                </select>
              </div>
              <div className="field">
                <label htmlFor="database-event-handler">
                  {t("Handler service", "처리 서비스")}
                </label>
                <select
                  id="database-event-handler"
                  value={handler}
                  required
                  disabled={busy || !kafka.configured}
                  onChange={(e) => {
                    setHandler(e.target.value);
                    setTriggerId(crypto.randomUUID());
                  }}
                >
                  <option value="">{t("Select service", "서비스 선택")}</option>
                  {services.map((service) => (
                    <option value={service.id} key={service.id}>
                      {service.name}
                    </option>
                  ))}
                </select>
              </div>
            </div>
            <details>
              <summary>{t("Endpoint path", "처리 경로")}</summary>
              <div className="field">
                <label htmlFor="database-event-path">
                  {t("HTTP path", "HTTP 경로")}
                </label>
                <input
                  id="database-event-path"
                  value={path}
                  maxLength={256}
                  disabled={busy}
                  required
                  onChange={(e) => {
                    setPath(e.target.value);
                    setTriggerId(crypto.randomUUID());
                  }}
                />
              </div>
            </details>
            <button
              className="btn"
              type="submit"
              disabled={busy || !kafka.configured || !handler || table === ""}
            >
              {t("Enable events", "이벤트 사용")}
            </button>
          </form>
        )}
      </section>
      {dirty && (
        <button
          className="btn ghost"
          type="button"
          disabled={busy}
          onClick={() => {
            setService("");
            setAccess("read");
            setHandler("");
            setTable("");
            setPath("/events/database");
            setBrokers("");
            setCa("");
            setUsername("");
            setPassword("");
            setClientCertificate("");
            setClientKey("");
            setBindingId(crypto.randomUUID());
            setTriggerId(crypto.randomUUID());
          }}
        >
          {t("Clear service settings draft", "서비스 설정 초안 취소")}
        </button>
      )}
    </details>
  );
}
