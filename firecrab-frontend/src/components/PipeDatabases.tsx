import { useEffect, useState, type FormEvent } from "react";
import { createPipeDatabase, deletePipeDatabase, getDatabaseTables, listPipeDatabases, previewDatabase, testPipeDatabase, type DatabaseInput, type DatabasePreview, type DatabaseProvider, type DatabaseTable, type DatabaseTlsMode, type PipeDatabase } from "../api/pipeDatabases";
import { useI18n } from "../i18n";

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
  return <section className="panel" aria-label={t("Databases", "데이터베이스")}>
    <h2 className="panel-title"><span>{t("Databases", "데이터베이스")}</span>
      <button className="btn" type="button" disabled={busy || loading || connections === null || editorOpen} aria-expanded={editorOpen} aria-controls="pipe-database-editor" onClick={() => { setEditorOpen(true); setNotice(null); }}>{t("Add database", "DB 가져오기")}</button>
    </h2>
    <p className="pipe-description">{t("Connect an existing MySQL or PostgreSQL database. Browse its tables and preview data with read-only queries.", "기존 MySQL 또는 PostgreSQL DB를 연결한다. 테이블과 데이터를 읽기 전용으로 조회한다.")}</p>
    {error && <div className="banner error" role="alert">{error}</div>}
    {notice && <div className="banner info" role="status">{notice}</div>}
    {!loading && connections === null && <button className="btn" type="button" onClick={() => { setLoading(true); setLoadRevision(value => value + 1); }}>{t("Retry database list", "DB 목록 다시 불러오기")}</button>}
    {editorOpen && <form id="pipe-database-editor" className="pipe-database-form" onSubmit={connect}>
      <div className="pipe-tunnel-fields">
        <label className="field"><span>{t("Connection name", "연결 이름")}</span><input value={name} maxLength={100} required disabled={busy} autoFocus onChange={event => { setName(event.target.value); setNotice(null); }} /></label>
        <div className="field"><label htmlFor="pipe-database-source">{t("Database source", "DB 위치")}</label><select id="pipe-database-source" value={provider} disabled={busy} onChange={event => { setProvider(event.target.value as DatabaseProvider); setTls(""); setNotice(null); }}>
          <option value="local">{t("Local DB URI", "로컬 DB URI")}</option><option value="supabase">Supabase</option><option value="rds">Amazon RDS</option>
        </select></div>
      </div>
      <label className="field"><span>{t("Database URI", "DB URI")}</span><input type={showUri ? "text" : "password"} value={uri} autoComplete="off" spellCheck={false} required maxLength={8192} disabled={busy} placeholder={provider === "supabase" ? "postgresql://postgres:password@db.project.supabase.co:5432/postgres" : "postgresql://user:password@host:5432/database"} onChange={event => { setUri(event.target.value); setNotice(null); }} /></label>
      <button className="btn" type="button" disabled={busy} aria-pressed={showUri} onClick={() => setShowUri(value => !value)}>{showUri ? t("Hide URI", "URI 숨기기") : t("Show URI", "URI 표시")}</button>
      <p className="pipe-description">{provider === "supabase"
        ? t("Copy the PostgreSQL URI from Supabase → Connect. Use the session pooler on IPv4-only networks. A project HTTPS URL or API key cannot be used here.", "Supabase → Connect에서 PostgreSQL URI를 복사한다. IPv4 전용 환경에서는 Session pooler를 사용한다. 프로젝트 HTTPS 주소와 API 키는 사용할 수 없다.")
        : provider === "rds"
          ? t("Use your RDS MySQL or PostgreSQL endpoint, database name and DB credentials. Allow the Firecrab API host in the RDS security group.", "RDS의 MySQL 또는 PostgreSQL 엔드포인트와 DB 계정을 사용한다. RDS 보안 그룹에서 Firecrab API 호스트의 접근을 허용한다.")
          : t("Accepts mysql://, postgres:// and postgresql:// URIs. Localhost refers to the Firecrab API host. On macOS and Windows, this is the managed Linux VM.", "mysql://, postgres://, postgresql:// URI를 지원한다. localhost는 Firecrab API 호스트를 뜻한다. macOS와 Windows에서는 관리 Linux VM을 가리킨다.")}</p>
      <div className="pipe-tunnel-fields">
        <div className="field"><label htmlFor="pipe-database-tls">TLS</label><select id="pipe-database-tls" value={tls} disabled={busy} onChange={event => { setTls(event.target.value as DatabaseTlsMode | ""); setNotice(null); }}>
          <option value="">{t("Automatic (URI or source default)", "자동 (URI 또는 DB 위치 기본값)")}</option>
          <option value="verify_full">{t("Verify certificate and hostname", "인증서와 호스트 이름 검증")}</option>
          <option value="require">{t("Require encryption", "암호화 필수")}</option>
          {provider === "local" && <option value="disable">{t("Disabled", "사용 안 함")}</option>}
        </select></div>
        <label className="field"><span>{t("CA certificate (PEM, optional)", "CA 인증서 (PEM, 선택)")}</span><textarea value={ca} rows={3} maxLength={32768} disabled={busy} spellCheck={false} placeholder="-----BEGIN CERTIFICATE-----" onChange={event => { setCa(event.target.value); setNotice(null); }} /></label>
      </div>
      <p className="pipe-description">{t("Cloud connections default to certificate verification. Paste the RDS CA bundle above when needed. Use a DB account with SELECT permission. Credentials are stored on the API host.", "외부 서비스는 기본으로 인증서를 검증한다. 필요한 경우 RDS CA 인증서를 입력한다. SELECT 권한이 있는 DB 계정을 사용한다. 연결 정보는 API 호스트에 저장한다.")}</p>
      <div className="pipe-row-actions">
        <button className="btn" type="button" disabled={!ready} onClick={() => void testConnection()}>{t("Test connection", "연결 검사")}</button>
        <button className="btn primary" type="submit" disabled={!ready}>{busy ? t("Connecting…", "연결 중…") : t("Save connection", "연결 저장")}</button>
        <button className="btn" type="button" disabled={busy} onClick={closeEditor}>{t("Cancel", "취소")}</button>
      </div>
    </form>}
    {loading ? <p className="empty">{t("Loading databases…", "DB 목록을 불러오는 중…")}</p> : connections?.length === 0
      ? <p className="empty">{t("No database connections. Add a local DB URI, Supabase or Amazon RDS database.", "DB 연결이 없다. 로컬 DB URI, Supabase 또는 Amazon RDS DB를 추가한다.")}</p>
      : !!connections?.length && <div className="table-scroll"><table className="vm-table pipe-database-table"><thead><tr>
        <th>{t("Name", "이름")}</th><th>{t("Source", "위치")}</th><th>{t("Engine", "엔진")}</th><th>{t("Database address", "DB 주소")}</th><th>TLS</th><th>{t("Actions", "작업")}</th>
      </tr></thead><tbody>{connections.map(connection => <tr key={connection.id}>
        <td>{connection.name}</td><td>{connection.provider === "local" ? t("Local DB URI", "로컬 DB URI") : providers[connection.provider]}</td>
        <td>{connection.engine === "postgres" ? "PostgreSQL" : "MySQL"}</td>
        <td className="pipe-addresses">{connection.host.includes(":") ? `[${connection.host}]` : connection.host}:{connection.port}/{connection.database}</td>
        <td>{connection.tlsMode === "verify_full" ? t("Verified", "검증") : connection.tlsMode === "require" ? t("Encrypted", "암호화") : t("Disabled", "사용 안 함")}</td>
        <td><div className="pipe-row-actions"><button className="btn" type="button" disabled={busy} aria-label={t(`Browse ${connection.name}`, `${connection.name} 조회`)} onClick={() => void browse(connection)}>{t("Browse", "조회")}</button>
          <button className="btn" type="button" disabled={busy} aria-label={t(`Remove connection ${connection.name}`, `${connection.name} 연결 제거`)} onClick={() => void remove(connection)}>{t("Remove", "제거")}</button></div></td>
      </tr>)}</tbody></table></div>}
    {browsing && <div className="pipe-database-browser" aria-label={t("Database browser", "DB 조회")}>
      <h3>{browsing.name}</h3>
      <div className="pipe-controls"><div className="field"><label htmlFor="pipe-database-table">{t("Table", "테이블")}</label><select id="pipe-database-table" value={selectedTable ?? ""} disabled={busy || !tables.length} onChange={event => { if (event.target.value !== "") void readTable(Number(event.target.value)); }}>
        <option value="" disabled>{t("Select a table", "테이블 선택")}</option>{tables.map((table, index) => <option key={JSON.stringify(table)} value={index}>{table.schema}.{table.name}</option>)}
      </select></div><button className="btn" type="button" disabled={busy} onClick={() => void browse(browsing)}>{t("Refresh tables", "테이블 새로고침")}</button>
        <button className="btn" type="button" disabled={busy || selectedTable === null} onClick={() => { if (selectedTable !== null) void readTable(selectedTable); }}>{t("Refresh rows", "데이터 새로고침")}</button>
        <button className="btn" type="button" disabled={busy} onClick={() => { setBrowsing(null); setPreview(null); }}>{t("Close browser", "조회 닫기")}</button></div>
      {busy && <p role="status">{t("Reading database…", "DB를 읽는 중…")}</p>}
      {!busy && !tables.length && <p className="empty">{t("No accessible tables. Check SELECT permissions or refresh the table list.", "조회할 수 있는 테이블이 없다. SELECT 권한을 확인하거나 목록을 새로고침한다.")}</p>}
      {tablesTruncated && <p className="pipe-description">{t("Showing the first 500 tables.", "처음 500개 테이블을 표시한다.")}</p>}
      {preview && <>
        <p className="pipe-description">{t("Read-only preview: up to 50 rows, 2048 characters per cell. Binary values use hex. Row order can change between requests.", "읽기 전용 미리보기: 최대 50행, 셀당 2048자를 표시한다. 바이너리 값은 16진수로 표시한다. 조회할 때 행 순서가 달라질 수 있다.")}</p>
        {preview.rows.length ? <div className="table-scroll"><table className="vm-table pipe-database-preview"><thead><tr>{preview.columns.map(column => <th key={column.name} title={column.dataType}>{column.name}<small>{column.dataType}</small></th>)}</tr></thead>
          <tbody>{preview.rows.map((row, index) => <tr key={index}>{row.map((value, cell) => <td key={preview.columns[cell].name}><div className="pipe-database-cell" tabIndex={value !== null && value.length > 160 ? 0 : undefined}>{value === null ? <span className="pipe-database-null">NULL</span> : value}</div></td>)}</tr>)}</tbody></table></div>
          : <p className="empty">{t("This table has no rows on this page.", "이 페이지에 데이터가 없다.")}</p>}
        <div className="pipe-controls">
          <button className="btn" type="button" disabled={busy || !previousOffsets.length} onClick={() => { if (selectedTable !== null) void readTable(selectedTable, previousOffsets.at(-1)!, previousOffsets.slice(0, -1)); }}>{t("Previous rows", "이전 행")}</button>
          <span>{t(`${preview.rows.length} rows · offset ${preview.offset}`, `${preview.rows.length}행 · 시작 위치 ${preview.offset}`)}</span>
          <button className="btn" type="button" disabled={busy || !preview.hasMore || !preview.rows.length || preview.offset + preview.rows.length > 100_000} onClick={() => { if (selectedTable !== null) void readTable(selectedTable, preview.offset + preview.rows.length, [...previousOffsets, preview.offset]); }}>{t("Next rows", "다음 행")}</button>
        </div>
      </>}
    </div>}
  </section>;
}
