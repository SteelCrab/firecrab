import { useCallback, useEffect, useState } from "react";
import * as db from "../api/pipeDatabases";
import { useI18n } from "../i18n";

export default function DatabaseMaintenance({
  connection,
  onDirty,
}: {
  connection: db.PipeDatabase;
  onDirty: (dirty: boolean) => void;
}) {
  const { t } = useI18n();
  const [open, setOpen] = useState(false),
    [jobs, setJobs] = useState<db.DatabaseJob[]>([]),
    [tools, setTools] = useState<db.DatabaseTools | null>(null);
  const [busy, setBusy] = useState(false),
    [error, setError] = useState<string | null>(null),
    [notice, setNotice] = useState<string | null>(null);
  const [version, setVersion] = useState(""),
    [sql, setSql] = useState(""),
    [confirmation, setConfirmation] = useState(""),
    [stopped, setStopped] = useState(false),
    [restore, setRestore] = useState<string | null>(null);
  const [requestId, setRequestId] = useState(() => crypto.randomUUID());
  const dirty =
    !!sql || !!version || !!confirmation || stopped || restore !== null;
  useEffect(() => {
    onDirty(dirty);
    return () => onDirty(false);
  }, [dirty, onDirty]);
  const refresh = useCallback(async () => {
    const result = await db.databaseJobs(connection.id);
    setJobs(result);
  }, [connection.id]);
  useEffect(() => {
    if (!open) return;
    let active = true;
    const initial = setTimeout(() => {
      void db
        .databaseTools()
        .then((result) => {
          if (active) setTools(result);
        })
        .catch((e: Error) => {
          if (active) setError(e.message);
        });
      void refresh().catch((e: Error) => {
        if (active) setError(e.message);
      });
    }, 0);
    const timer = setInterval(() => {
      void refresh().catch((e: Error) => {
        if (active) setError(e.message);
      });
    }, 3000);
    return () => {
      active = false;
      clearTimeout(initial);
      clearInterval(timer);
    };
  }, [open, refresh]);
  const pending = jobs.some(
    (j) => j.status === "queued" || j.status === "running",
  );
  const recovery = jobs.some((j) => j.status === "needs_recovery");
  const available =
    !!tools?.[connection.engine === "postgres" ? "postgres" : "mysql"];
  function clear() {
    setVersion("");
    setSql("");
    setConfirmation("");
    setStopped(false);
    setRestore(null);
    setRequestId(crypto.randomUUID());
  }
  async function run(action: () => Promise<unknown>, message: string) {
    if (busy) return;
    setBusy(true);
    setError(null);
    setNotice(null);
    try {
      await action();
      await refresh();
      setNotice(message);
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setBusy(false);
    }
  }
  function status(value: string) {
    return (
      {
        queued: t("Queued", "대기"),
        running: t("Running", "실행 중"),
        succeeded: t("Completed", "완료"),
        failed: t("Failed", "실패"),
        needs_recovery: t("Recovery required", "복구 필요"),
        recovered: t("Recovered", "복구 완료"),
        expired: t("Expired", "만료"),
      }[value] ?? value
    );
  }
  return (
    <details
      className="pipe-database-security"
      onToggle={(e) => setOpen(e.currentTarget.open)}
    >
      <summary>{t("Backups & schema changes", "백업과 스키마 변경")}</summary>
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
      <p className="poll-note">
        {connection.writeEnabled
          ? t(
              "Daily logical backup · latest 7 snapshots. Before schema changes or restore, Firecrab creates a safety backup. Files stay private on the API host.",
              "매일 논리 백업을 실행하고 최근 7개를 보관한다. 스키마 변경과 복구 전에는 안전 백업을 만든다. 파일은 API 호스트에 비공개로 보관한다.",
            )
          : t(
              "Create a logical snapshot with the database's native tools. Writes remain disabled.",
              "DB 네이티브 도구로 논리 백업을 만든다. 쓰기는 비활성 상태를 유지한다.",
            )}
      </p>
      {tools && !available && (
        <p className="banner info">
          {t(
            "Backup client tools are missing on the API host. Run firecrab service install with dependency installation, then refresh. The PostgreSQL client must support the server version.",
            "API 호스트에 백업 클라이언트가 없다. 의존성 설치를 포함해 firecrab service install을 실행한 후 새로고침한다. PostgreSQL 클라이언트는 서버 버전을 지원해야 한다.",
          )}
        </p>
      )}
      {recovery && (
        <p className="banner error" role="alert">
          {t(
            "A previous maintenance job needs recovery. Keep services stopped and restore its safety backup.",
            "이전 유지보수 작업의 복구가 필요하다. 서비스를 중지한 상태로 안전 백업을 복원한다.",
          )}
        </p>
      )}
      <div className="pipe-row-actions">
        <button
          className="btn"
          type="button"
          disabled={busy || pending || recovery || !available}
          onClick={() =>
            void run(
              async () => {
                await db.createDatabaseBackup(connection.id, requestId);
                setRequestId(crypto.randomUUID());
              },
              t("Backup queued.", "백업을 예약했다."),
            )
          }
        >
          {t("Back up now", "지금 백업")}
        </button>
        <button
          className="btn"
          type="button"
          disabled={busy}
          onClick={() =>
            void run(
              async () => {
                setTools(await db.databaseTools());
                await refresh();
              },
              t("Job list refreshed.", "작업 목록을 새로고침했다."),
            )
          }
        >
          {t("Refresh jobs", "작업 새로고침")}
        </button>
      </div>
      {!!jobs.length && (
        <div className="table-scroll">
          <table
            className="vm-table"
            aria-label={t("Database jobs", "DB 작업")}
          >
            <thead>
              <tr>
                <th scope="col">{t("Operation", "작업")}</th>
                <th scope="col">{t("Created", "생성 시각")}</th>
                <th scope="col">{t("Status", "상태")}</th>
                <th scope="col">{t("Actions", "작업")}</th>
              </tr>
            </thead>
            <tbody>
              {jobs.map((job) => (
                <tr key={job.id}>
                  <td>
                    {job.kind === "backup"
                      ? t("Backup", "백업")
                      : job.kind === "restore"
                        ? t("Restore", "복구")
                        : t("Schema change", "스키마 변경")}
                    {job.version && ` · ${job.version}`}
                    {job.bytes !== null && (
                      <small>{` · ${(job.bytes / 1024 / 1024).toFixed(1)} MiB`}</small>
                    )}
                  </td>
                  <td>{new Date(job.createdAt * 1000).toLocaleString()}</td>
                  <td>
                    {status(job.status)}
                    {job.error && (
                      <small className="database-job-error">{job.error}</small>
                    )}
                    {job.safetyBackupId && (
                      <small>
                        {t("Safety backup: ", "안전 백업: ")}
                        {job.safetyBackupId}
                      </small>
                    )}
                  </td>
                  <td>
                    <div className="pipe-row-actions">
                      {job.kind === "backup" && job.status === "succeeded" && (
                        <>
                          <a
                            className="btn sm"
                            href={`/api/pipe/database-jobs/${encodeURIComponent(job.id)}/backup`}
                            download
                          >
                            {t("Download", "다운로드")}
                          </a>
                          {connection.writeEnabled && (
                            <button
                              className="btn sm"
                              type="button"
                              disabled={busy || pending || dirty}
                              onClick={() => {
                                setRestore(job.id);
                                setSql("");
                                setVersion("");
                                setConfirmation("");
                                setStopped(false);
                                setRequestId(crypto.randomUUID());
                              }}
                            >
                              {t("Restore…", "복구…")}
                            </button>
                          )}
                        </>
                      )}
                      {job.kind === "backup" &&
                        (job.status === "succeeded" ||
                          job.status === "failed") && (
                          <button
                            className="btn sm danger"
                            type="button"
                            disabled={busy || pending || recovery || dirty}
                            onClick={() =>
                              void run(
                                () => db.removeDatabaseBackup(job.id),
                                t(
                                  "Local backup removed.",
                                  "로컬 백업을 제거했다.",
                                ),
                              )
                            }
                          >
                            {t("Remove backup", "백업 제거")}
                          </button>
                        )}
                    </div>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
      {connection.writeEnabled && (
        <details open={restore !== null || undefined}>
          <summary>
            {restore
              ? t("Restore selected snapshot", "선택한 백업 복구")
              : t("Apply a schema version", "스키마 버전 적용")}
          </summary>
          <form
            onSubmit={(e) => {
              e.preventDefault();
              void run(
                async () => {
                  if (restore)
                    await db.restoreDatabase(connection.id, {
                      requestId,
                      backupId: restore,
                      confirmDatabase: confirmation,
                      externalWritersStopped: stopped,
                    });
                  else
                    await db.migrateDatabase(connection.id, {
                      requestId,
                      version,
                      sql,
                      confirmDatabase: confirmation,
                      externalWritersStopped: stopped,
                    });
                  clear();
                },
                t(
                  "Maintenance queued. Keep services stopped until it completes.",
                  "유지보수를 예약했다. 완료할 때까지 서비스를 중지한다.",
                ),
              );
            }}
            className="pipe-database-form"
          >
            {!restore && (
              <>
                <div className="field">
                  <label htmlFor="database-schema-version">
                    {t("Version", "버전")}
                  </label>
                  <input
                    id="database-schema-version"
                    value={version}
                    maxLength={100}
                    pattern="[A-Za-z0-9._-]+"
                    placeholder="001-add-orders"
                    required
                    disabled={busy || pending || recovery}
                    onChange={(e) => {
                      setVersion(e.target.value);
                      setRequestId(crypto.randomUUID());
                    }}
                  />
                </div>
                <div className="field">
                  <label htmlFor="database-schema-sql">SQL</label>
                  <textarea
                    id="database-schema-sql"
                    rows={6}
                    value={sql}
                    maxLength={65536}
                    placeholder="ALTER TABLE orders ADD COLUMN note TEXT;"
                    spellCheck={false}
                    required
                    disabled={busy || pending || recovery}
                    onChange={(e) => {
                      setSql(e.target.value);
                      setRequestId(crypto.randomUUID());
                    }}
                  />
                </div>
              </>
            )}
            <p className="poll-note">
              {restore
                ? t(
                    "Restore replaces this database's snapshot state. Firecrab takes a fresh safety backup first. PostgreSQL restores in one transaction. Interrupted MySQL restores require recovery.",
                    "선택한 백업의 상태로 DB를 복구한다. 먼저 현재 상태의 안전 백업을 만든다. PostgreSQL은 하나의 트랜잭션으로 복구한다. 중단된 MySQL 복구는 후속 복구가 필요하다.",
                  )
                : t(
                    "Stop bound services and pause triggers. Versions are immutable. MySQL DDL can commit partly; a failed job blocks writes until recovery.",
                    "연결된 서비스를 중지하고 트리거를 일시 중지한다. 버전의 SQL은 변경할 수 없다. MySQL DDL은 일부만 적용될 수 있다. 실패하면 복구 전까지 쓰기를 차단한다.",
                  )}
            </p>
            <div className="field">
              <label htmlFor="database-maintenance-confirm">
                {t(
                  `Type database name: ${connection.database}`,
                  `DB 이름 입력: ${connection.database}`,
                )}
              </label>
              <input
                id="database-maintenance-confirm"
                autoComplete="off"
                value={confirmation}
                required
                disabled={busy || pending}
                onChange={(e) => setConfirmation(e.target.value)}
              />
            </div>
            <label className="pipe-database-write-option">
              <input
                type="checkbox"
                checked={stopped}
                disabled={busy || pending}
                required
                onChange={(e) => setStopped(e.target.checked)}
              />
              {t(
                "I stopped all external database writers.",
                "외부 DB 쓰기 작업을 모두 중지했다.",
              )}
            </label>
            <div className="pipe-row-actions">
              <button
                className={`btn ${restore ? "danger" : "primary"}`}
                type="submit"
                disabled={
                  busy ||
                  pending ||
                  !available ||
                  confirmation !== connection.database ||
                  !stopped ||
                  (!restore && (!version || !sql || recovery))
                }
              >
                {restore
                  ? t("Restore database", "DB 복구")
                  : t("Apply version", "버전 적용")}
              </button>
              <button
                className="btn ghost"
                type="button"
                disabled={busy}
                onClick={clear}
              >
                {t("Cancel", "취소")}
              </button>
            </div>
          </form>
        </details>
      )}
    </details>
  );
}
