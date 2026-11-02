import { useEffect, useRef, useState, type FormEvent } from "react";
import { fetchJson } from "../api/client";
import { nativeMutation, NativeMutationPending } from "../api/nativeMutations";
import type { PipeDatabase, DatabaseTable } from "../api/pipeDatabases";
import { useI18n } from "../i18n";

type Service = { id: string; name: string };
type Binding = {
  id: string;
  serviceId: string;
  access: string;
  ready: boolean;
  envName?: string;
};
type Trigger = {
  id: string;
  schema: string;
  table: string;
  serviceId: string;
  pending: number;
  failed: number;
  delivered: number;
  skipped: number;
  paused: boolean;
  detached: boolean;
  lastError: string | null;
};
type Failure = { eventId: string; error: string };
type Job = {
  id: string;
  kind: string;
  status: string;
  error: string | null;
  createdAt: string;
};
type Rabbit = { configured: boolean; brokers: string[]; tls: boolean };
type Tools = { postgres: boolean; mysql: boolean };
type Managed = { phase: string; error: string | null };
const enc = encodeURIComponent;
const write = (method: string, body?: object): RequestInit => ({
  method,
  ...(body
    ? {
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(body),
      }
    : {}),
});
export default function DatabaseOperations({
  database,
  tables,
}: {
  database: PipeDatabase;
  tables: DatabaseTable[];
}) {
  const { t } = useI18n();
  const mounted = useRef(false);
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);
  const [notice, setNotice] = useState("");
  const [pendingWork, setPendingWork] = useState(false);
  const base = `/api/pipe/databases/${enc(database.id)}`;
  const [open, setOpen] = useState(false),
    [maintenance, setMaintenance] = useState(false),
    [services, setServices] = useState<Service[]>([]),
    [bindings, setBindings] = useState<Binding[]>([]),
    [triggers, setTriggers] = useState<Trigger[]>([]),
    [rabbit, setRabbit] = useState<Rabbit | null>(null),
    [managed, setManaged] = useState<Managed | null>(null),
    [jobs, setJobs] = useState<Job[]>([]),
    [tools, setTools] = useState<Tools | null>(null),
    [failures, setFailures] = useState<Record<string, Failure[]>>({});
  const [pollError, setPollError] = useState("");
  const [external, setExternal] = useState(false),
    [brokers, setBrokers] = useState(""),
    [user, setUser] = useState(""),
    [password, setPassword] = useState(""),
    [vhost, setVhost] = useState("firecrab"),
    [development, setDevelopment] = useState(false),
    [tls, setTls] = useState(true),
    [ca, setCa] = useState(""),
    [cert, setCert] = useState(""),
    [clientKey, setClientKey] = useState("");
  const [error, setError] = useState(""),
    [busy, setBusy] = useState(false),
    [service, setService] = useState(""),
    [access, setAccess] = useState<"read" | "write">("read"),
    [table, setTable] = useState(""),
    [path, setPath] = useState("/events/database"),
    [version, setVersion] = useState(""),
    [sql, setSql] = useState(""),
    [backupId, setBackupId] = useState(""),
    [confirmName, setConfirmName] = useState(""),
    [stopped, setStopped] = useState(false);
  const pending = useRef<{ key: string; id: string } | null>(null);
  const load = async () => {
    const results = await Promise.allSettled([
      fetchJson<Service[]>("/api/micro-services"),
      fetchJson<Binding[]>(`${base}/bindings`),
      fetchJson<Trigger[]>(`${base}/triggers`),
      fetchJson<Rabbit>("/api/pipe/database-rabbitmq"),
      fetchJson<Managed>("/api/pipe/database-rabbitmq/managed"),
      fetchJson<Job[]>(`${base}/jobs`),
      fetchJson<Tools>("/api/pipe/database-maintenance/tools"),
    ] as const);
    const [s, b, tr, r, m, j, tool] = results;
    setTools(tool.status === "fulfilled" ? tool.value : null);
    if (s.status === "fulfilled") setServices(s.value);
    if (b.status === "fulfilled") setBindings(b.value);
    if (tr.status === "fulfilled") setTriggers(tr.value);
    if (r.status === "fulfilled") setRabbit(r.value);
    if (m.status === "fulfilled") setManaged(m.value);
    if (j.status === "fulfilled") setJobs(j.value);
    const failed = results
      .slice(0, 6)
      .filter((result) => result.status === "rejected");
    if (failed.length)
      throw new Error(failed.map((result) => String(result.reason)).join("; "));
  };
  useEffect(() => {
    if (!open && !maintenance) return;
    let live = true;
    const poll = async () => {
      try {
        await load();
        if (live) setPollError("");
      } catch (e) {
        if (live) setPollError(e instanceof Error ? e.message : String(e));
      }
    };
    void poll();
    const timer = window.setInterval(() => void poll(), 5000);
    return () => {
      live = false;
      window.clearInterval(timer);
    };
  }, [open, maintenance, database.id]);
  const act = async (
    method: string,
    url: string,
    body?: object,
    identity = false,
    fixedResourceId?: string,
  ) => {
    if (busy) return;
    const bindingCreate = method === "POST" && url === `${base}/bindings`;
    const bindingDelete =
      method === "DELETE" && url.startsWith("/api/pipe/database-bindings/");
    const nativeBinding = bindingCreate || bindingDelete;
    const key = JSON.stringify({ method, url, body });
    if (
      identity &&
      !nativeBinding &&
      (!pending.current || pending.current.key !== key)
    )
      pending.current = { key, id: crypto.randomUUID() };
    const payload =
      identity && !nativeBinding
        ? { ...body, requestId: pending.current!.id }
        : body;
    setBusy(true);
    setError("");
    setNotice("");
    try {
      if (nativeBinding) {
        const id = bindingDelete
          ? decodeURIComponent(url.slice("/api/pipe/database-bindings/".length))
          : undefined;
        const result = await nativeMutation<Binding | { deleted: boolean }>(
          method,
          url,
          bindingCreate
            ? (requestId: string) => ({
                ...body,
                requestId: fixedResourceId ?? requestId,
              })
            : undefined,
          bindingCreate ? "create_database_binding" : "delete_database_binding",
          "database_binding",
          async (resource) =>
            bindingDelete
              ? { deleted: true }
              : ((await fetchJson<Binding[]>(`${base}/bindings`)).find(
                  (item) => item.id === resource,
                ) ?? null),
          fixedResourceId ?? id,
        );
        if (!mounted.current) return;
        if (bindingCreate && result.value && "id" in result.value) {
          const saved = result.value;
          setBindings((current) => [
            ...current.filter((item) => item.id !== saved.id),
            saved,
          ]);
        }
        if (bindingDelete)
          setBindings((current) => current.filter((item) => item.id !== id));
        if (result.recovered)
          setNotice(
            t(
              "The original request completed. Review service access before making further changes.",
              "원래 요청을 완료했다. 다음 변경 전에 서비스 접근을 확인한다.",
            ),
          );
        setPendingWork(false);
        result.acknowledge();
      } else await fetchJson(url, write(method, payload));
      pending.current = null;
      try {
        await load();
      } catch (e) {
        setPollError(e instanceof Error ? e.message : String(e));
      }
    } catch (e) {
      setPendingWork(e instanceof NativeMutationPending);
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };
  const externalRabbit = async (event: FormEvent) => {
    event.preventDefault();
    if (busy) return;
    setBusy(true);
    setError("");
    try {
      await fetchJson(
        "/api/pipe/database-rabbitmq",
        write("PUT", {
          brokers: brokers
            .split(/[,\n]/)
            .map((v) => v.trim())
            .filter(Boolean),
          username: user,
          password,
          virtualHost: vhost,
          tls: !development || tls,
          development,
          ...(ca.trim() ? { caCertificate: ca.trim() } : {}),
          ...(cert.trim() ? { clientCertificate: cert.trim() } : {}),
          ...(clientKey.trim() ? { clientKey: clientKey.trim() } : {}),
        }),
      );
      setPassword("");
      setClientKey("");
      setExternal(false);
      try {
        await load();
      } catch (e) {
        setPollError(String(e));
      }
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };
  const bind = (event: FormEvent) => {
    event.preventDefault();
    if (service)
      void act(
        "POST",
        `${base}/bindings`,
        { serviceId: service, access, envName: "DATABASE_URL" },
        true,
      );
  };
  const trigger = (event: FormEvent) => {
    event.preventDefault();
    try {
      const pair = JSON.parse(table);
      if (!Array.isArray(pair) || pair.length !== 2 || !service)
        throw Error(
          t("Select a table and service.", "테이블과 서비스를 선택한다."),
        );
      void act(
        "POST",
        `${base}/triggers`,
        { schema: pair[0], table: pair[1], serviceId: service, path },
        true,
      );
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  };
  const job = (kind: "backup" | "migration" | "restore") => {
    if (kind !== "backup" && (confirmName !== database.database || !stopped)) {
      setError(
        t(
          "Type the database name and confirm external writers are stopped.",
          "DB 이름을 입력하고 외부 쓰기 중지를 확인한다.",
        ),
      );
      return;
    }
    const body =
      kind === "backup"
        ? {}
        : kind === "migration"
          ? {
              version,
              sql,
              confirmDatabase: confirmName,
              externalWritersStopped: stopped,
            }
          : {
              backupId,
              confirmDatabase: confirmName,
              externalWritersStopped: stopped,
            };
    void act(
      "POST",
      `${base}/${kind === "backup" ? "backups" : kind === "migration" ? "migrations" : "restore"}`,
      body,
      true,
    );
  };
  return (
    <section className="panel">
      <h3>{t("Database operations", "DB 작업")}</h3>
      {error && (
        <p role="alert">
          {error}
          {pendingWork && (
            <>
              {" "}
              <a href="#/stacks">
                {t("Review original operation", "원래 작업 확인")}
              </a>
            </>
          )}
        </p>
      )}
      {notice && <p role="status">{notice}</p>}
      {pollError && <p role="alert">{pollError}</p>}
      <button type="button" className="btn" onClick={() => setOpen((v) => !v)}>
        {t("Service access and events", "서비스 접근과 이벤트")}
      </button>
      <button
        type="button"
        className="btn"
        onClick={() => setMaintenance((v) => !v)}
      >
        {t("Backup and schema", "백업과 스키마")}
      </button>
      {open && (
        <div>
          <h4>{t("Service access", "서비스 접근")}</h4>
          <p>
            {t(
              "Generated service credentials use DATABASE_URL.",
              "생성한 서비스 인증 정보는 DATABASE_URL로 전달한다.",
            )}
          </p>
          {bindings.map((item) => (
            <p key={item.id}>
              {services.find((s) => s.id === item.serviceId)?.name ||
                item.serviceId}{" "}
              · {item.access} ·{" "}
              {item.ready ? t("Ready", "준비됨") : t("Pending", "대기")}{" "}
              {!item.ready && (
                <button
                  type="button"
                  className="btn"
                  disabled={busy}
                  onClick={() =>
                    void act(
                      "POST",
                      `${base}/bindings`,
                      {
                        serviceId: item.serviceId,
                        access: item.access,
                        envName: item.envName ?? "DATABASE_URL",
                      },
                      true,
                      item.id,
                    )
                  }
                >
                  {t("Retry setup", "설정 재시도")}
                </button>
              )}{" "}
              <button
                className="btn danger"
                disabled={busy}
                onClick={() => {
                  if (
                    window.confirm(
                      t(
                        "Revoke this service account? Stop its service first.",
                        "서비스 계정을 회수할까? 먼저 서비스를 중지한다.",
                      ),
                    )
                  )
                    void act(
                      "DELETE",
                      `/api/pipe/database-bindings/${enc(item.id)}`,
                    );
                }}
              >
                {t("Revoke", "회수")}
              </button>
            </p>
          ))}
          <form onSubmit={bind}>
            <label>
              {t("Service", "서비스")}{" "}
              <select
                required
                value={service}
                onChange={(e) => setService(e.target.value)}
              >
                <option value="">{t("Select service", "서비스 선택")}</option>
                {services.map((s) => (
                  <option key={s.id} value={s.id}>
                    {s.name}
                  </option>
                ))}
              </select>
            </label>
            <label>
              {t("Access", "접근 권한")}{" "}
              <select
                value={access}
                onChange={(e) => setAccess(e.target.value as "read" | "write")}
              >
                <option value="read">{t("Read only", "읽기 전용")}</option>
                <option value="write">{t("Read/write", "읽기/쓰기")}</option>
              </select>
            </label>
            <button className="btn" disabled={busy}>
              {t("Grant access", "접근 허용")}
            </button>
          </form>
          <h4>{t("Database events", "DB 이벤트")}</h4>
          <p>
            RabbitMQ ·{" "}
            {managed?.phase ||
              (rabbit?.configured ? "ready" : "not configured")}
          </p>
          {managed?.error && <p role="alert">{managed.error}</p>}
          <button
            className="btn"
            disabled={busy}
            onClick={() =>
              void act("POST", "/api/pipe/database-rabbitmq/managed", {
                development: false,
              })
            }
          >
            {t("Prepare RabbitMQ automatically", "RabbitMQ 자동 준비")}
          </button>
          <details
            open={external}
            onToggle={(e) => setExternal(e.currentTarget.open)}
          >
            <summary>
              {t("Use existing RabbitMQ", "기존 RabbitMQ 사용")}
            </summary>
            <form onSubmit={(event) => void externalRabbit(event)}>
              <label>
                {t("RabbitMQ endpoints", "RabbitMQ 주소")}{" "}
                <textarea
                  required
                  value={brokers}
                  onChange={(e) => setBrokers(e.target.value)}
                />
              </label>
              <label>
                {t("Username", "사용자 이름")}{" "}
                <input
                  required
                  value={user}
                  onChange={(e) => setUser(e.target.value)}
                />
              </label>
              <label>
                {t("Password", "비밀번호")}{" "}
                <input
                  type="password"
                  autoComplete="new-password"
                  value={password}
                  onChange={(e) => setPassword(e.target.value)}
                />
              </label>
              <label>
                {t("Virtual host", "가상 호스트")}{" "}
                <input
                  value={vhost}
                  onChange={(e) => setVhost(e.target.value)}
                />
              </label>
              <label>
                <input
                  type="checkbox"
                  checked={development}
                  onChange={(e) => setDevelopment(e.target.checked)}
                />
                {t("Single-node development only", "단일 노드 개발 환경")}
              </label>
              <label>
                <input
                  type="checkbox"
                  disabled={!development}
                  checked={!development || tls}
                  onChange={(e) => setTls(e.target.checked)}
                />
                TLS
              </label>
              <label>
                CA{" "}
                <textarea value={ca} onChange={(e) => setCa(e.target.value)} />
              </label>
              <label>
                {t("Client certificate", "클라이언트 인증서")}{" "}
                <textarea
                  value={cert}
                  onChange={(e) => setCert(e.target.value)}
                />
              </label>
              <label>
                {t("Client key", "클라이언트 키")}{" "}
                <textarea
                  value={clientKey}
                  onChange={(e) => setClientKey(e.target.value)}
                />
              </label>
              <button className="btn" disabled={busy}>
                {t("Save RabbitMQ", "RabbitMQ 저장")}
              </button>
            </form>
          </details>
          {triggers.map((item) => (
            <article key={item.id}>
              <p>
                {item.schema}.{item.table} → {item.serviceId} ·{" "}
                {t("Pending", "대기")} {item.pending} · {t("Failed", "실패")}{" "}
                {item.failed} · {t("Delivered", "전달")} {item.delivered} ·{" "}
                {t("Skipped", "건너뜀")} {item.skipped}
              </p>
              {item.lastError && <p role="alert">{item.lastError}</p>}
              <button
                className="btn"
                disabled={busy}
                onClick={() =>
                  void act(
                    "PUT",
                    `/api/pipe/database-triggers/${enc(item.id)}`,
                    { paused: !item.paused },
                  )
                }
              >
                {item.paused ? t("Resume", "재개") : t("Pause", "일시 중지")}
              </button>
              <button
                className="btn"
                disabled={
                  busy ||
                  (item.detached && (item.pending > 0 || item.failed > 0))
                }
                onClick={() => {
                  if (
                    window.confirm(
                      item.detached
                        ? t(
                            "Remove this drained trigger?",
                            "전달을 마친 트리거를 제거할까?",
                          )
                        : t(
                            "Stop capturing new changes?",
                            "새 변경 캡처를 중지할까?",
                          ),
                    )
                  )
                    void act(
                      item.detached ? "DELETE" : "POST",
                      `/api/pipe/database-triggers/${enc(item.id)}${item.detached ? "" : "/detach"}`,
                    );
                }}
              >
                {item.detached
                  ? t("Remove trigger", "트리거 제거")
                  : t("Detach capture", "캡처 중단")}
              </button>
              <button
                className="btn"
                disabled={busy}
                onClick={() =>
                  void fetchJson<Failure[]>(
                    `/api/pipe/database-triggers/${enc(item.id)}/failures`,
                  )
                    .then((rows) =>
                      setFailures((v) => ({ ...v, [item.id]: rows })),
                    )
                    .catch((e) => setError(String(e)))
                }
              >
                {t("Failed events", "실패 이벤트")}
              </button>
              {failures[item.id]?.map((f) => (
                <p key={f.eventId}>
                  {f.eventId} · {f.error}{" "}
                  <button
                    className="btn"
                    disabled={busy}
                    onClick={() =>
                      void act(
                        "POST",
                        `/api/pipe/database-triggers/${enc(item.id)}/replay/${enc(f.eventId.split(":").at(-1) || f.eventId)}`,
                      )
                    }
                  >
                    {t("Replay", "재처리")}
                  </button>
                  <button
                    className="btn"
                    disabled={busy}
                    onClick={() => {
                      if (
                        window.confirm(
                          t(
                            "Skip this failed event? Its business effect will not run.",
                            "실패 이벤트를 건너뛸까? 해당 업무 처리를 실행하지 않는다.",
                          ),
                        )
                      )
                        void act(
                          "POST",
                          `/api/pipe/database-triggers/${enc(item.id)}/acknowledge/${enc(f.eventId.split(":").at(-1) || f.eventId)}`,
                          { confirmEventId: f.eventId },
                        );
                    }}
                  >
                    {t("Skip", "건너뛰기")}
                  </button>
                </p>
              ))}
            </article>
          ))}
          <form onSubmit={trigger}>
            <label>
              {t("Source table", "원본 테이블")}{" "}
              <select
                required
                value={table}
                onChange={(e) => setTable(e.target.value)}
              >
                <option value="">{t("Select table", "테이블 선택")}</option>
                {tables.map((item) => (
                  <option
                    key={JSON.stringify(item)}
                    value={JSON.stringify([item.schema, item.name])}
                  >
                    {item.schema}.{item.name}
                  </option>
                ))}
              </select>
            </label>
            <label>
              {t("Handler service", "처리 서비스")}{" "}
              <select
                required
                value={service}
                onChange={(e) => setService(e.target.value)}
              >
                <option value="">{t("Select service", "서비스 선택")}</option>
                {services.map((s) => (
                  <option key={s.id} value={s.id}>
                    {s.name}
                  </option>
                ))}
              </select>
            </label>
            <label>
              {t("Endpoint path", "엔드포인트 경로")}{" "}
              <input
                required
                value={path}
                onChange={(e) => setPath(e.target.value)}
              />
            </label>
            <button className="btn" disabled={busy || !rabbit?.configured}>
              {t("Enable events", "이벤트 사용")}
            </button>
          </form>
        </div>
      )}
      {maintenance && (
        <div>
          <h4>{t("Backup and schema", "백업과 스키마")}</h4>
          <p>
            {tools
              ? tools[database.engine]
                ? t("Backup tools are available.", "백업 도구가 준비되었다.")
                : t(
                    "Backup tools are missing on the API host.",
                    "API 호스트에 백업 도구가 없다.",
                  )
              : t(
                  "Backup tool status is unavailable.",
                  "백업 도구 상태를 확인하지 못했다.",
                )}
          </p>
          <p>
            {t(
              "Schema changes and restores use a safety backup. Review the result before resuming writers.",
              "스키마 변경과 복구 전에 안전 백업을 만든다. 쓰기를 재개하기 전에 결과를 확인한다.",
            )}
          </p>
          <button
            className="btn"
            disabled={busy || tools?.[database.engine] === false}
            onClick={() => job("backup")}
          >
            {t("Create backup", "백업 만들기")}
          </button>
          {jobs.map((item) => (
            <p key={item.id}>
              {item.kind} · {item.status} · {item.error}
              {item.kind === "backup" && item.status === "succeeded" && (
                <a
                  href={`/api/pipe/database-jobs/${enc(item.id)}/backup`}
                  download
                >
                  {t("Download", "다운로드")}
                </a>
              )}
              {item.kind === "backup" &&
                ["succeeded", "failed"].includes(item.status) && (
                  <button
                    className="btn"
                    disabled={busy}
                    onClick={() => {
                      if (
                        window.confirm(
                          t("Remove this backup?", "백업을 제거할까?"),
                        )
                      )
                        void act(
                          "DELETE",
                          `/api/pipe/database-jobs/${enc(item.id)}/backup`,
                        );
                    }}
                  >
                    {t("Remove", "제거")}
                  </button>
                )}
            </p>
          ))}
          <label>
            {t("Version", "버전")}{" "}
            <input
              value={version}
              onChange={(e) => setVersion(e.target.value)}
            />
          </label>
          <label>
            SQL{" "}
            <textarea value={sql} onChange={(e) => setSql(e.target.value)} />
          </label>
          <label>
            {t("Backup", "백업")}{" "}
            <select
              value={backupId}
              onChange={(e) => setBackupId(e.target.value)}
            >
              <option value="">{t("Select backup", "백업 선택")}</option>
              {jobs
                .filter((j) => j.kind === "backup" && j.status === "succeeded")
                .map((j) => (
                  <option value={j.id} key={j.id}>
                    {j.id}
                  </option>
                ))}
            </select>
          </label>
          <label>
            {t("Confirm database name", "DB 이름 확인")}{" "}
            <input
              value={confirmName}
              onChange={(e) => setConfirmName(e.target.value)}
            />
          </label>
          <label>
            <input
              type="checkbox"
              checked={stopped}
              onChange={(e) => setStopped(e.target.checked)}
            />
            {t(
              "All external writers are stopped.",
              "외부 DB 쓰기를 모두 중지했다.",
            )}
          </label>
          <button
            className="btn"
            disabled={busy || !version || !sql}
            onClick={() => job("migration")}
          >
            {t("Apply schema change", "스키마 변경")}
          </button>
          <button
            className="btn"
            disabled={busy || !backupId}
            onClick={() => job("restore")}
          >
            {t("Restore snapshot", "백업 복구")}
          </button>
        </div>
      )}
    </section>
  );
}
