import { useEffect, useState, type FormEvent } from "react";
import { ChevronLeft, ChevronRight, Eye, EyeOff, Plus, RefreshCw, X } from "lucide-react";
import { createPipeDatabase, deletePipeDatabase, getDatabaseTables, listPipeDatabases, previewDatabase, testPipeDatabase, type DatabaseInput, type DatabasePreview, type DatabaseProvider, type DatabaseTable, type DatabaseTlsMode, type PipeDatabase } from "../api/pipeDatabases";
import { useI18n } from "../i18n";
import PipeServiceMenu from "./PipeServiceMenu";

interface Props { onUnsavedChange: (dirty: boolean) => void }
const providers: Record<DatabaseProvider, string> = { local: "Local DB URI", supabase: "Supabase", rds: "Amazon RDS" };

export default function PipeDatabases({ onUnsavedChange }: Props) {
  const { t } = useI18n();
  const [connections, setConnections] = useState<PipeDatabase[] | null>(null);
  const [loading, setLoading] = useState(true);
  const [loadRevision, setLoadRevision] = useState(0);
  const [editorOpen, setEditorOpen] = useState(false);
  const [name, setName] = useState("");
  const [provider, setProvider] = useState<DatabaseProvider>("local");
  const [uri, setUri] = useState("");
  const [tls, setTls] = useState<DatabaseTlsMode | "">("");
  const [ca, setCa] = useState("");
  const [showUri, setShowUri] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [browsing, setBrowsing] = useState<PipeDatabase | null>(null);
  const [tables, setTables] = useState<DatabaseTable[]>([]);
  const [tablesTruncated, setTablesTruncated] = useState(false);
  const [selectedTable, setSelectedTable] = useState<number | null>(null);
  const [preview, setPreview] = useState<DatabasePreview | null>(null);
  const [previousOffsets, setPreviousOffsets] = useState<number[]>([]);
  const dirty = editorOpen && (!!name || !!uri || !!ca || provider !== "local" || tls !== "");

  useEffect(() => {
    onUnsavedChange(dirty);
    return () => onUnsavedChange(false);
  }, [dirty, onUnsavedChange]);

  useEffect(() => {
    const controller = new AbortController();
    void listPipeDatabases(controller.signal).then(next => {
      if (controller.signal.aborted) return;
      setConnections(next); setError(null);
    }).catch((failure: Error) => {
      if (controller.signal.aborted) return;
      setConnections(null); setError(failure.message);
    }).finally(() => { if (!controller.signal.aborted) setLoading(false); });
    return () => controller.abort();
  }, [loadRevision]);

  function closeEditor() {
    setEditorOpen(false); setName(""); setProvider("local"); setUri(""); setTls(""); setCa(""); setShowUri(false);
  }

  function input(): DatabaseInput {
    return { name: name.trim(), provider, uri: uri.trim(), ...(tls ? { tlsMode: tls } : {}), ...(ca.trim() ? { caCertificate: ca.trim() } : {}) };
  }

  async function connect(event: FormEvent) {
    event.preventDefault();
    if (busy) return;
    setBusy(true); setError(null); setNotice(null);
    try {
      const saved = await createPipeDatabase(input());
      setConnections(current => [...(current ?? []), saved]); closeEditor();
      setNotice(t("Database connection saved.", "DB 연결을 저장했다."));
    } catch (failure) { setError((failure as Error).message); }
    finally { setBusy(false); }
  }

  async function testConnection() {
    if (busy) return;
    setBusy(true); setError(null); setNotice(null);
    try { await testPipeDatabase(input()); setNotice(t("Database connection succeeded.", "DB 연결을 확인했다.")); }
    catch (failure) { setError((failure as Error).message); }
    finally { setBusy(false); }
  }

  async function remove(connection: PipeDatabase) {
    if (busy) return;
    setBusy(true); setError(null); setNotice(null);
    try {
      await deletePipeDatabase(connection.id);
      setConnections(current => current?.filter(value => value.id !== connection.id) ?? []);
      if (browsing?.id === connection.id) { setBrowsing(null); setTables([]); setPreview(null); }
      setNotice(t("Connection removed. The source database is unchanged.", "연결을 제거했다. 원본 DB는 유지한다."));
    } catch (failure) { setError((failure as Error).message); }
    finally { setBusy(false); }
  }

  async function browse(connection: PipeDatabase) {
    if (busy) return;
    setBusy(true); setError(null); setNotice(null);
    setBrowsing(connection); setTables([]); setPreview(null); setSelectedTable(null); setPreviousOffsets([]); setTablesTruncated(false);
    try {
      const result = await getDatabaseTables(connection.id);
      setTables(result.tables); setTablesTruncated(result.truncated);
    } catch (failure) { setError((failure as Error).message); }
    finally { setBusy(false); }
  }

  async function readTable(index: number, offset = 0, history: number[] = []) {
    if (busy || !browsing || !tables[index]) return;
    setBusy(true); setError(null); setNotice(null); setPreview(null); setSelectedTable(index);
    try {
      const next = await previewDatabase(browsing.id, tables[index], offset);
      setPreview(next); setPreviousOffsets(history);
    } catch (failure) { setError((failure as Error).message); }
    finally { setBusy(false); }
  }

  const ready = !!name.trim() && !!uri.trim() && !busy && !loading && connections !== null;
  return <section className="panel pipe-databases" aria-label={t("Databases", "데이터베이스")}>
    <h2 className="panel-title"><span>{t("Databases", "데이터베이스")}</span>
      <button className="btn" type="button" disabled={busy || loading || connections === null || editorOpen} aria-expanded={editorOpen} aria-controls="pipe-database-editor" onClick={() => { setEditorOpen(true); setNotice(null); }}><Plus aria-hidden="true" />{t("Add database", "DB 가져오기")}</button>
    </h2>
    <p className="panel-intro">{t("MySQL and PostgreSQL connections. Browse tables with read-only previews.", "MySQL과 PostgreSQL을 연결한다. 테이블을 읽기 전용으로 조회한다.")}</p>
    {error && <div className="banner error" role="alert">{error}</div>}
    {notice && <div className="banner info" role="status">{notice}</div>}
    {!loading && connections === null && <button className="btn" type="button" onClick={() => { setLoading(true); setLoadRevision(value => value + 1); }}>{t("Retry database list", "DB 목록 다시 불러오기")}</button>}
    {editorOpen && <form id="pipe-database-editor" className="pipe-database-form" aria-labelledby="pipe-database-editor-title" onSubmit={connect}>
      <h3 id="pipe-database-editor-title" className="pipe-database-form-title">{t("Add database", "DB 가져오기")}</h3>
      <div className="pipe-database-fields">
        <div className="field"><label htmlFor="pipe-database-name">{t("Connection name", "연결 이름")}</label><input id="pipe-database-name" value={name} maxLength={100} required disabled={busy} autoFocus placeholder={t("Application DB", "앱 DB")} onChange={event => { setName(event.target.value); setNotice(null); }} /></div>
        <div className="field"><label htmlFor="pipe-database-source">{t("Database source", "DB 위치")}</label><select id="pipe-database-source" value={provider} disabled={busy} onChange={event => { setProvider(event.target.value as DatabaseProvider); setTls(""); setNotice(null); }}>
          <option value="local">{t("Local DB URI", "로컬 DB URI")}</option><option value="supabase">Supabase</option><option value="rds">Amazon RDS</option>
        </select></div>
      </div>
      <div className="field"><label htmlFor="pipe-database-uri">{t("Database URI", "DB URI")}</label>
        <div className="pipe-database-uri">
          <input id="pipe-database-uri" type={showUri ? "text" : "password"} value={uri} autoComplete="off" spellCheck={false} required maxLength={8192} disabled={busy} aria-describedby="pipe-database-uri-help" placeholder={provider === "supabase" ? "postgresql://postgres:password@db.project.supabase.co:5432/postgres" : "postgresql://user:password@host:5432/database"} onChange={event => { setUri(event.target.value); setNotice(null); }} />
          <button className="icon-btn" type="button" disabled={busy} aria-label={showUri ? t("Hide URI", "URI 숨기기") : t("Show URI", "URI 표시")} title={showUri ? t("Hide URI", "URI 숨기기") : t("Show URI", "URI 표시")} aria-pressed={showUri} onClick={() => setShowUri(value => !value)}>{showUri ? <EyeOff aria-hidden="true" /> : <Eye aria-hidden="true" />}</button>
        </div>
        <p id="pipe-database-uri-help" className="poll-note">{provider === "supabase"
        ? t("Copy the PostgreSQL URI from Supabase → Connect. Use the session pooler for IPv4. HTTPS URLs and API keys are not supported.", "Supabase → Connect에서 PostgreSQL URI를 복사한다. IPv4 환경에서는 Session pooler를 사용한다. HTTPS 주소와 API 키는 지원하지 않는다.")
        : provider === "rds"
          ? t("Use your RDS MySQL or PostgreSQL endpoint, database name and DB credentials. Allow the Firecrab API host in the RDS security group.", "RDS의 MySQL 또는 PostgreSQL 엔드포인트와 DB 계정을 사용한다. RDS 보안 그룹에서 Firecrab API 호스트의 접근을 허용한다.")
          : t("Use mysql://, postgres:// or postgresql://. Localhost is the API host, or the managed Linux VM on macOS and Windows.", "mysql://, postgres://, postgresql://를 사용한다. localhost는 API 호스트다. macOS와 Windows에서는 관리 Linux VM을 가리킨다.")}</p>
      </div>
      <details className="pipe-database-security">
        <summary>{t("TLS & certificates", "TLS와 인증서")}</summary>
        <div className="pipe-database-fields">
        <div className="field"><label htmlFor="pipe-database-tls">TLS</label><select id="pipe-database-tls" value={tls} disabled={busy} onChange={event => { setTls(event.target.value as DatabaseTlsMode | ""); setNotice(null); }}>
          <option value="">{t("Automatic (URI or source default)", "자동 (URI 또는 DB 위치 기본값)")}</option>
          <option value="verify_full">{t("Verify certificate and hostname", "인증서와 호스트 이름 검증")}</option>
          <option value="require">{t("Require encryption", "암호화 필수")}</option>
          {provider === "local" && <option value="disable">{t("Disabled", "사용 안 함")}</option>}
        </select></div>
        <div className="field"><label htmlFor="pipe-database-ca">{t("CA certificate (PEM, optional)", "CA 인증서 (PEM, 선택)")}</label><textarea id="pipe-database-ca" value={ca} rows={3} maxLength={32768} disabled={busy} spellCheck={false} placeholder="-----BEGIN CERTIFICATE-----" onChange={event => { setCa(event.target.value); setNotice(null); }} /></div>
        </div>
        <p className="poll-note">{t("Cloud connections verify certificates by default. Add the RDS CA bundle when needed.", "외부 서비스는 기본으로 인증서를 검증한다. 필요한 경우 RDS CA 인증서를 입력한다.")}</p>
      </details>
      <p className="poll-note">{t("Use a DB account with SELECT permission. Credentials are stored on the API host.", "SELECT 권한이 있는 DB 계정을 사용한다. 연결 정보는 API 호스트에 저장한다.")}</p>
      <div className="pipe-row-actions">
        <button className="btn primary" type="submit" disabled={!ready}>{busy ? t("Connecting…", "연결 중…") : t("Save connection", "연결 저장")}</button>
        <button className="btn" type="button" disabled={!ready} onClick={() => void testConnection()}>{t("Test connection", "연결 검사")}</button>
        <button className="btn ghost" type="button" disabled={busy} onClick={closeEditor}>{t("Cancel", "취소")}</button>
      </div>
    </form>}
    {loading ? <p className="empty">{t("Loading databases…", "DB 목록을 불러오는 중…")}</p> : connections?.length === 0
      ? !editorOpen && <p className="empty">{t("No database connections. Add a local DB URI, Supabase or Amazon RDS database.", "DB 연결이 없다. 로컬 DB URI, Supabase 또는 Amazon RDS DB를 추가한다.")}</p>
      : !!connections?.length && <div className="table-scroll"><table className="vm-table pipe-database-table" aria-label={t("Saved databases", "저장한 DB")}><thead><tr>
        <th scope="col">{t("Name", "이름")}</th><th scope="col">{t("Source", "위치")}</th><th scope="col">{t("Engine", "엔진")}</th><th scope="col">{t("Database address", "DB 주소")}</th><th scope="col">TLS</th><th scope="col" className="actions">{t("Actions", "작업")}</th>
      </tr></thead><tbody>{connections.map(connection => <tr key={connection.id} className={browsing?.id === connection.id ? "selected" : undefined}>
        <td className="name">{connection.name}</td><td data-label={t("Source", "위치")}>{connection.provider === "local" ? t("Local DB URI", "로컬 DB URI") : providers[connection.provider]}</td>
        <td data-label={t("Engine", "엔진")}>{connection.engine === "postgres" ? "PostgreSQL" : "MySQL"}</td>
        <td className="mono" data-label={t("Address", "주소")}>{connection.host.includes(":") ? `[${connection.host}]` : connection.host}:{connection.port}/{connection.database}</td>
        <td data-label="TLS">{connection.tlsMode === "verify_full" ? t("Verified", "검증") : connection.tlsMode === "require" ? t("Encrypted", "암호화") : t("Disabled", "사용 안 함")}</td>
        <td className="actions"><div className="pipe-row-actions"><button className="btn sm" type="button" disabled={busy} aria-label={t(`Browse ${connection.name}`, `${connection.name} 조회`)} onClick={() => void browse(connection)}>{t("Browse", "조회")}</button>
          <PipeServiceMenu label={t(`${connection.name} actions`, `${connection.name} 작업`)} disabled={busy} items={[
            { label: t(`Remove connection ${connection.name}`, `${connection.name} 연결 제거`), danger: true, onSelect: () => void remove(connection) },
          ]} /></div></td>
      </tr>)}</tbody></table></div>}
    {browsing && <div className="pipe-database-browser" aria-label={t("Database browser", "DB 조회")}>
      <div className="pipe-database-browser-heading"><h3>{browsing.name}</h3>
        <button className="icon-btn" type="button" disabled={busy} aria-label={t("Close browser", "조회 닫기")} title={t("Close browser", "조회 닫기")} onClick={() => { setBrowsing(null); setPreview(null); }}><X aria-hidden="true" /></button>
      </div>
      <div className="pipe-database-toolbar"><div className="field"><label htmlFor="pipe-database-table">{t("Table", "테이블")}</label><select id="pipe-database-table" value={selectedTable ?? ""} disabled={busy || !tables.length} onChange={event => { if (event.target.value !== "") void readTable(Number(event.target.value)); }}>
        <option value="" disabled>{t("Select a table", "테이블 선택")}</option>{tables.map((table, index) => <option key={JSON.stringify(table)} value={index}>{table.schema}.{table.name}</option>)}
      </select></div><button className="btn" type="button" disabled={busy} aria-label={t("Refresh tables", "테이블 새로고침")} title={t("Refresh tables", "테이블 새로고침")} onClick={() => void browse(browsing)}><RefreshCw aria-hidden="true" />{t("Tables", "테이블")}</button>
        <button className="btn" type="button" disabled={busy || selectedTable === null} aria-label={t("Refresh rows", "데이터 새로고침")} onClick={() => { if (selectedTable !== null) void readTable(selectedTable); }}><RefreshCw aria-hidden="true" />{t("Rows", "데이터")}</button></div>
      {busy && <p className="poll-note" role="status">{t("Reading database…", "DB를 읽는 중…")}</p>}
      {!busy && !tables.length && <p className="empty">{t("No accessible tables. Check SELECT permissions or refresh the table list.", "조회할 수 있는 테이블이 없다. SELECT 권한을 확인하거나 목록을 새로고침한다.")}</p>}
      {tablesTruncated && <p className="pipe-description">{t("Showing the first 500 tables.", "처음 500개 테이블을 표시한다.")}</p>}
      {preview && <>
        <p className="poll-note">{t("Read-only · up to 50 rows, 2048 characters per cell. Binary values use hex. Row order may change.", "읽기 전용 · 최대 50행, 셀당 2048자를 표시한다. 바이너리 값은 16진수로 표시한다. 행 순서가 달라질 수 있다.")}</p>
        {preview.rows.length ? <div className="table-scroll"><table className="vm-table pipe-database-preview" aria-label={t("Data preview", "데이터 미리보기")}><thead><tr>{preview.columns.map(column => <th scope="col" key={column.name} title={column.dataType}>{column.name}<small>{column.dataType}</small></th>)}</tr></thead>
          <tbody>{preview.rows.map((row, index) => <tr key={index}>{row.map((value, cell) => <td key={preview.columns[cell].name}><div className="pipe-database-cell" tabIndex={value !== null && value.length > 160 ? 0 : undefined}>{value === null ? <span className="pipe-database-null">NULL</span> : value}</div></td>)}</tr>)}</tbody></table></div>
          : <p className="empty">{t("This table has no rows on this page.", "이 페이지에 데이터가 없다.")}</p>}
        <div className="pipe-database-pagination">
          <span>{t(`${preview.rows.length} rows · offset ${preview.offset}`, `${preview.rows.length}행 · 시작 위치 ${preview.offset}`)}</span>
          <div className="pipe-row-actions">
            <button className="btn sm" type="button" disabled={busy || !previousOffsets.length} aria-label={t("Previous rows", "이전 행")} onClick={() => { if (selectedTable !== null) void readTable(selectedTable, previousOffsets.at(-1)!, previousOffsets.slice(0, -1)); }}><ChevronLeft aria-hidden="true" />{t("Previous", "이전")}</button>
            <button className="btn sm" type="button" disabled={busy || !preview.hasMore || !preview.rows.length || preview.offset + preview.rows.length > 100_000} aria-label={t("Next rows", "다음 행")} onClick={() => { if (selectedTable !== null) void readTable(selectedTable, preview.offset + preview.rows.length, [...previousOffsets, preview.offset]); }}>{t("Next", "다음")}<ChevronRight aria-hidden="true" /></button>
          </div>
        </div>
      </>}
    </div>}
  </section>;
}
