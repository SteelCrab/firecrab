import { useEffect, useState, type FormEvent } from "react";
import { Eye, EyeOff, Plus } from "lucide-react";
import { createPipeDatabase, deletePipeDatabase, listPipeDatabases, setDatabaseWrites, testPipeDatabase, updatePipeDatabase, type DatabaseInput, type DatabaseProvider, type DatabaseTlsMode, type PipeDatabase } from "../api/pipeDatabases";
import { useI18n } from "../i18n";
import PipeServiceMenu from "./PipeServiceMenu";
import DatabaseRows from "./DatabaseRows";

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
  const [writeEnabled,setWriteEnabled]=useState(false);
  const [editingId,setEditingId]=useState<string|null>(null);
  const [rowDirty,setRowDirty]=useState(false);
  const dirty = rowDirty || editorOpen && (!!name || !!uri || !!ca || provider !== "local" || tls !== "" || writeEnabled);

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
    setEditorOpen(false);setEditingId(null); setName(""); setProvider("local"); setUri(""); setTls(""); setCa(""); setShowUri(false);setWriteEnabled(false);
  }

  function input(): DatabaseInput {
    return { name: name.trim(), provider, uri: uri.trim(), ...(writeEnabled?{writeEnabled:true}:{}), ...(tls ? { tlsMode: tls } : {}), ...(ca.trim() ? { caCertificate: ca.trim() } : {}) };
  }

  async function connect(event: FormEvent) {
    event.preventDefault();
    if (busy) return;
    setBusy(true); setError(null); setNotice(null);
    try {
      const saved = editingId?await updatePipeDatabase(editingId,input()):await createPipeDatabase(input());
      setConnections(current => editingId?(current??[]).map(c=>c.id===saved.id?saved:c):[...(current ?? []), saved]);if(browsing?.id===saved.id)setBrowsing(saved); closeEditor();
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
      if (browsing?.id === connection.id) { setBrowsing(null); }
      setNotice(t("Connection removed. The source database is unchanged.", "연결을 제거했다. 원본 DB는 유지한다."));
    } catch (failure) { setError((failure as Error).message); }
    finally { setBusy(false); }
  }

  async function changeAccess(connection:PipeDatabase){
    if(busy||rowDirty)return;setBusy(true);setError(null);setNotice(null);
    try{const updated=await setDatabaseWrites(connection.id,!connection.writeEnabled);setConnections(current=>current?.map(c=>c.id===updated.id?updated:c)??[]);if(browsing?.id===updated.id)setBrowsing(updated)}
    catch(failure){setError((failure as Error).message)}finally{setBusy(false)}
  }

  const ready = !!name.trim() && !!uri.trim() && !busy && !loading && connections !== null;
  return <section className="panel pipe-databases" aria-label={t("Databases", "데이터베이스")}>
    <h2 className="panel-title"><span>{t("Databases", "데이터베이스")}</span>
      <button className="btn" type="button" disabled={busy || rowDirty || loading || connections === null || editorOpen} aria-expanded={editorOpen} aria-controls="pipe-database-editor" onClick={() => {closeEditor(); setEditorOpen(true); setNotice(null); }}><Plus aria-hidden="true" />{t("Add database", "DB 가져오기")}</button>
    </h2>
    <p className="panel-intro">{t("MySQL and PostgreSQL connections. Browse tables and connect services.", "MySQL과 PostgreSQL을 연결한다. 테이블을 조회하고 서비스에 연결한다.")}</p>
    {error && <div className="banner error" role="alert">{error}</div>}
    {notice && <div className="banner info" role="status">{notice}</div>}
    {!loading && connections === null && <button className="btn" type="button" onClick={() => { setLoading(true); setLoadRevision(value => value + 1); }}>{t("Retry database list", "DB 목록 다시 불러오기")}</button>}
    {editorOpen && <form id="pipe-database-editor" className="pipe-database-form" aria-labelledby="pipe-database-editor-title" onSubmit={connect}>
      <h3 id="pipe-database-editor-title" className="pipe-database-form-title">{editingId?t("Update connection credentials","연결 정보 변경"):t("Add database", "DB 가져오기")}</h3>
      <div className="pipe-database-fields">
        <div className="field"><label htmlFor="pipe-database-name">{t("Connection name", "연결 이름")}</label><input id="pipe-database-name" value={name} maxLength={100} required disabled={busy || rowDirty} autoFocus placeholder={t("Application DB", "앱 DB")} onChange={event => { setName(event.target.value); setNotice(null); }} /></div>
        <div className="field"><label htmlFor="pipe-database-source">{t("Database source", "DB 위치")}</label><select id="pipe-database-source" value={provider} disabled={busy || rowDirty} onChange={event => { setProvider(event.target.value as DatabaseProvider); setTls(""); setNotice(null); }}>
          <option value="local">{t("Local DB URI", "로컬 DB URI")}</option><option value="supabase">Supabase</option><option value="rds">Amazon RDS</option>
        </select></div>
      </div>
      <div className="field"><label htmlFor="pipe-database-uri">{t("Database URI", "DB URI")}</label>
        <div className="pipe-database-uri">
          <input id="pipe-database-uri" type={showUri ? "text" : "password"} value={uri} autoComplete="off" spellCheck={false} required maxLength={8192} disabled={busy || rowDirty} aria-describedby="pipe-database-uri-help" placeholder={provider === "supabase" ? "postgresql://postgres:password@db.project.supabase.co:5432/postgres" : "postgresql://user:password@host:5432/database"} onChange={event => { setUri(event.target.value); setNotice(null); }} />
          <button className="icon-btn" type="button" disabled={busy || rowDirty} aria-label={showUri ? t("Hide URI", "URI 숨기기") : t("Show URI", "URI 표시")} title={showUri ? t("Hide URI", "URI 숨기기") : t("Show URI", "URI 표시")} aria-pressed={showUri} onClick={() => setShowUri(value => !value)}>{showUri ? <EyeOff aria-hidden="true" /> : <Eye aria-hidden="true" />}</button>
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
        <div className="field"><label htmlFor="pipe-database-tls">TLS</label><select id="pipe-database-tls" value={tls} disabled={busy || rowDirty} onChange={event => { setTls(event.target.value as DatabaseTlsMode | ""); setNotice(null); }}>
          <option value="">{t("Automatic (URI or source default)", "자동 (URI 또는 DB 위치 기본값)")}</option>
          <option value="verify_full">{t("Verify certificate and hostname", "인증서와 호스트 이름 검증")}</option>
          <option value="require">{t("Require encryption", "암호화 필수")}</option>
          {provider === "local" && <option value="disable">{t("Disabled", "사용 안 함")}</option>}
        </select></div>
        <div className="field"><label htmlFor="pipe-database-ca">{t("CA certificate (PEM, optional)", "CA 인증서 (PEM, 선택)")}</label><textarea id="pipe-database-ca" value={ca} rows={3} maxLength={32768} disabled={busy || rowDirty} spellCheck={false} placeholder="-----BEGIN CERTIFICATE-----" onChange={event => { setCa(event.target.value); setNotice(null); }} /></div>
        </div>
        <p className="poll-note">{t("Cloud connections verify certificates by default. Add the RDS CA bundle when needed.", "외부 서비스는 기본으로 인증서를 검증한다. 필요한 경우 RDS CA 인증서를 입력한다.")}</p>
      </details>
      <label className="pipe-database-write-option"><input type="checkbox" checked={writeEnabled} disabled={busy || rowDirty} onChange={event=>setWriteEnabled(event.target.checked)}/>{t("Enable database writes", "DB 쓰기 사용")}</label>
      {editingId&&<p className="poll-note">{t("Leave CA blank to keep the saved certificate.","CA 인증서를 비워 두면 저장된 인증서를 유지한다.")}</p>}
      <p className="poll-note">{t("Reading requires SELECT. Write and service setup requires table, trigger and account privileges. Credentials stay on the API host.", "조회에는 SELECT 권한을 사용한다. 쓰기·서비스 설정에는 테이블·트리거·계정 생성 권한이 필요하다. 연결 정보는 API 호스트에 저장한다.")}</p>
      <div className="pipe-row-actions">
        <button className="btn primary" type="submit" disabled={!ready}>{busy ? t("Connecting…", "연결 중…") : t("Save connection", "연결 저장")}</button>
        <button className="btn" type="button" disabled={!ready} onClick={() => void testConnection()}>{t("Test connection", "연결 검사")}</button>
        <button className="btn ghost" type="button" disabled={busy || rowDirty} onClick={closeEditor}>{t("Cancel", "취소")}</button>
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
        <td className="actions"><div className="pipe-row-actions"><button className="btn sm" type="button" disabled={busy || rowDirty} aria-label={t(`Browse ${connection.name}`, `${connection.name} 조회`)} onClick={() => setBrowsing(connection)}>{t("Browse", "조회")}</button>
          <PipeServiceMenu label={t(`${connection.name} actions`, `${connection.name} 작업`)} disabled={busy || rowDirty} items={[
            {label:t("Update connection credentials","연결 정보 변경"),onSelect:()=>{closeEditor();setEditingId(connection.id);setName(connection.name);setProvider(connection.provider);setTls(connection.tlsMode);setWriteEnabled(!!connection.writeEnabled);setEditorOpen(true)}},
            {label:connection.writeEnabled?t("Disable writes","쓰기 끄기"):t("Enable writes","쓰기 사용"),onSelect:()=>void changeAccess(connection)},
            { label: t(`Remove connection ${connection.name}`, `${connection.name} 연결 제거`), danger: true, onSelect: () => void remove(connection) },
          ]} /></div></td>
      </tr>)}</tbody></table></div>}
    {browsing && <DatabaseRows key={`${browsing.id}:${!!browsing.writeEnabled}`} connection={browsing} onClose={()=>setBrowsing(null)} onDirty={setRowDirty}/>}
  </section>;
}
