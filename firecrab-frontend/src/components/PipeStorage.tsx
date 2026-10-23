import { useEffect, useRef, useState, type FormEvent } from "react";
import { useI18n } from "../i18n";
import { disconnectStorage, downloadObject, listObjects, listStorage, removeObject, saveStorage, uploadObject, verifyStorage, type ObjectPage, type StorageConnection, type StorageInput } from "../api/pipeStorage";

interface Editor extends StorageInput { id?: string; replaceKeys: boolean }
const newEditor = (): Editor => ({ name: "", bucket: "", region: "ap-northeast-2", prefix: "", credentialRef: "pipeAuthAws", replaceKeys: false });

export default function PipeStorage({ onUnsavedChange }: { onUnsavedChange: (dirty: boolean) => void }) {
  const { t } = useI18n();
  const [connections, setConnections] = useState<StorageConnection[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const inFlight = useRef(false);
  const [editor, setEditor] = useState<Editor | null>(null);
  const [browsing, setBrowsing] = useState<StorageConnection | null>(null);
  const [page, setPage] = useState<ObjectPage | null>(null);
  const [prefix, setPrefix] = useState("");
  const [file, setFile] = useState<File | null>(null);
  const [key, setKey] = useState("");
  const [overwrite, setOverwrite] = useState(false);
  const fileInput = useRef<HTMLInputElement>(null);

  useEffect(() => { onUnsavedChange(!!editor || !!file || busy); return () => onUnsavedChange(false); }, [editor, file, busy, onUnsavedChange]);
  useEffect(() => {
    const controller = new AbortController();
    void listStorage(controller.signal).then(rows => { if (!controller.signal.aborted) setConnections(rows); })
      .catch(failure => { if (!controller.signal.aborted) setError((failure as Error).message); })
      .finally(() => { if (!controller.signal.aborted) setLoading(false); });
    return () => controller.abort();
  }, []);

  async function perform(action: () => Promise<void>) {
    if (inFlight.current) return;
    inFlight.current = true; setBusy(true); setError(null); setNotice(null);
    try { await action(); } catch (failure) { setError((failure as Error).message); }
    finally { inFlight.current = false; setBusy(false); }
  }
  function update(patch: Partial<Editor>) { setEditor(current => current && ({ ...current, ...patch })); }
  function open(connection?: StorageConnection) {
    setEditor(connection ? { ...connection, replaceKeys: false } : newEditor());
    setBrowsing(null); setPage(null); setError(null); setNotice(null);
  }
  async function submit(event: FormEvent) {
    event.preventDefault();
    if (!editor) return;
    const { name, bucket, region, prefix: savedPrefix, credentialRef, credentials } = editor;
    const input: StorageInput = { name, bucket, region, prefix: savedPrefix, credentialRef };
    if (credentialRef === "connection" && (!editor.id || editor.replaceKeys)) input.credentials = credentials;
    await perform(async () => {
      const saved = await saveStorage(input, editor.id);
      setConnections(rows => [...rows.filter(row => row.id !== saved.id), saved]);
      setEditor(null);
      setNotice(t("Connection verified and saved.", "연결을 확인하고 저장했다."));
    });
  }
  async function browse(connection: StorageConnection, nextPrefix = connection.prefix, cursor?: string) {
    await perform(async () => {
      const next = await listObjects(connection.id, nextPrefix, cursor);
      setBrowsing(connection); setPage(next); setPrefix(nextPrefix); setKey(nextPrefix);
    });
  }
  function changeKeys(field: "accessKeyId" | "secretAccessKey" | "sessionToken", value: string) {
    update({ credentials: { accessKeyId: "", secretAccessKey: "", sessionToken: "", ...editor?.credentials, [field]: value } });
  }
  function disconnect(connection: StorageConnection) {
    if (!window.confirm(t(`Disconnect ${connection.name}? The S3 bucket and objects will be retained.`, `${connection.name} 연결을 해제할까요? S3 버킷과 파일은 보존한다.`))) return;
    void perform(async () => {
      await disconnectStorage(connection.id);
      setConnections(rows => rows.filter(row => row.id !== connection.id));
      if (browsing?.id === connection.id) { setBrowsing(null); setPage(null); }
    });
  }
  async function upload(event: FormEvent) {
    event.preventDefault();
    if (!browsing || !file) return;
    if (file.size > 512 * 1024 * 1024) { setError(t("Files must be at most 512 MiB.", "파일 크기는 512 MiB 이하여야 한다.")); return; }
    if (overwrite && !window.confirm(t(`Overwrite ${key} if it exists?`, `${key} 파일이 있으면 덮어쓸까요?`))) return;
    await perform(async () => {
      await uploadObject(browsing.id, key, file, overwrite);
      setFile(null); setOverwrite(false);
      if (fileInput.current) fileInput.current.value = "";
      setPage(await listObjects(browsing.id, prefix));
      setNotice(t("Upload complete.", "파일을 업로드했다."));
    });
  }

  return <section className="panel pipe-storage" aria-label="PipeStorage">
    <h2 className="panel-title">PipeStorage · Amazon S3</h2>
    <p className="pipe-description">{t("Connect an existing S3 bucket. Files stay in AWS. Connection checks only list the configured prefix; read, write and delete permissions are checked when you use them.", "기존 S3 버킷을 연결한다. 파일은 AWS에 저장한다. 연결 검사에서는 지정 경로의 목록만 확인한다. 읽기·쓰기·삭제 권한은 해당 작업을 실행할 때 확인한다.")}</p>
    {error && <div className="banner error" role="alert">{error}</div>}
    {notice && <div className="banner info" role="status">{notice}</div>}
    <div className="pipe-controls">
      <button type="button" className="btn primary" disabled={busy || !!editor || !!file} onClick={() => open()}>{t("Add S3 connection", "S3 연결 추가")}</button>
      <button type="button" className="btn" disabled={busy} onClick={() => void perform(async () => { setConnections(await listStorage()); })}>{t("Refresh connections", "연결 새로고침")}</button>
    </div>
    {editor && <form onSubmit={submit} className="pipe-storage-form" data-editing>
      <h3>{editor.id ? t("Edit S3 connection", "S3 연결 수정") : t("New S3 connection", "S3 연결 추가")}</h3>
      <div className="pipe-tunnel-fields">
        <label className="field"><span>{t("Name", "이름")}</span><input required maxLength={100} value={editor.name} disabled={busy} onChange={e => update({ name: e.target.value })} /></label>
        <label className="field"><span>{t("Bucket", "버킷")}</span><input required maxLength={63} value={editor.bucket} disabled={busy} autoComplete="off" spellCheck={false} onChange={e => update({ bucket: e.target.value })} /></label>
        <label className="field"><span>{t("Region", "리전")}</span><input required value={editor.region} disabled={busy} autoComplete="off" spellCheck={false} onChange={e => update({ region: e.target.value })} /></label>
        <label className="field"><span>{t("Prefix (optional)", "경로 (선택)")}</span><input maxLength={1024} placeholder="team/" value={editor.prefix} disabled={busy} onChange={e => update({ prefix: e.target.value })} /></label>
        <div className="field"><label htmlFor="s3-credential-source">{t("Credentials", "자격 증명")}</label><select id="s3-credential-source" value={editor.credentialRef} disabled={busy} onChange={e => update({ credentialRef: e.target.value as Editor["credentialRef"], replaceKeys: e.target.value === "connection", credentials: undefined })}>
          <option value="pipeAuthAws">Auth · AWS</option><option value="connection">{t("Connection-specific AWS keys", "이 연결의 AWS 키")}</option>
        </select></div>
      </div>
      {editor.credentialRef === "connection" && <>
        {!!editor.id && <label className="pipe-storage-check"><input type="checkbox" checked={editor.replaceKeys} disabled={busy} onChange={e => update({ replaceKeys: e.target.checked, credentials: undefined })} />{t("Replace saved keys", "저장된 키 교체")}</label>}
        {(!editor.id || editor.replaceKeys) && <div className="pipe-tunnel-fields">
          <label className="field"><span>AWS access key ID</span><input type="password" required maxLength={128} autoComplete="new-password" value={editor.credentials?.accessKeyId ?? ""} disabled={busy} onChange={e => changeKeys("accessKeyId", e.target.value)} /></label>
          <label className="field"><span>AWS secret access key</span><input type="password" required maxLength={128} autoComplete="new-password" value={editor.credentials?.secretAccessKey ?? ""} disabled={busy} onChange={e => changeKeys("secretAccessKey", e.target.value)} /></label>
          <label className="field"><span>{t("Session token (optional)", "세션 토큰 (선택)")}</span><input type="password" maxLength={16384} autoComplete="new-password" value={editor.credentials?.sessionToken ?? ""} disabled={busy} onChange={e => changeKeys("sessionToken", e.target.value)} /></label>
        </div>}
      </>}
      <p className="pipe-description">{t("Use keys limited to this bucket/prefix. S3-only keys do not need ECR permissions. Stop and unbind services before editing a connection.", "이 버킷과 경로로 권한을 제한한 키를 사용한다. S3 전용 키에는 ECR 권한이 필요하지 않다. 연결을 수정하려면 먼저 서비스를 중지하고 연결 참조를 해제한다.")}</p>
      <div className="pipe-row-actions"><button className="btn primary" disabled={busy} type="submit">{busy ? t("Verifying…", "연결 확인 중…") : t("Verify and save", "연결 확인 후 저장")}</button><button className="btn" disabled={busy} type="button" onClick={() => setEditor(null)}>{t("Cancel", "취소")}</button></div>
    </form>}
    {loading ? <p role="status">{t("Loading S3 connections…", "S3 연결을 불러오는 중…")}</p> : !!connections.length && <div className="table-scroll"><table className="vm-table"><thead><tr><th>{t("Name", "이름")}</th><th>{t("Bucket / prefix", "버킷 / 경로")}</th><th>{t("Region", "리전")}</th><th>{t("Services", "서비스")}</th><th>{t("Actions", "작업")}</th></tr></thead><tbody>
      {connections.map(connection => <tr key={connection.id}><td>{connection.name}</td><td className="pipe-storage-key">s3://{connection.bucket}/{connection.prefix}</td><td>{connection.region}</td><td>{connection.services.map(service => service.name).join(", ") || "—"}</td><td><div className="pipe-row-actions">
        <button className="btn" disabled={busy || !!editor || !!file} onClick={() => void browse(connection)}>{t("Browse", "조회")}</button>
        <button className="btn" disabled={busy} onClick={() => void perform(async () => { await verifyStorage(connection.id); setNotice(t("Listing is allowed. Read, write and delete permissions are verified by their respective operations.", "목록 조회 권한을 확인했다. 읽기·쓰기·삭제 권한은 각 작업을 실행할 때 확인한다.")); })}>{t("Verify", "연결 검사")}</button>
        <button className="btn" disabled={busy || !!editor || !!file || !!connection.services.length} onClick={() => open(connection)}>{t("Edit", "수정")}</button>
        <button className="btn" disabled={busy || !!connection.services.length} onClick={() => disconnect(connection)}>{t("Disconnect", "연결 해제")}</button>
      </div></td></tr>)}
    </tbody></table></div>}
    {!loading && !connections.length && !error && <p className="empty">{t("No S3 connections.", "S3 연결이 없다.")}</p>}
    {browsing && page && <section className="pipe-storage-browser" aria-label={t("S3 object browser", "S3 파일 조회")}>
      <h3>{browsing.name}</h3>
      <form className="pipe-controls" onSubmit={event => { event.preventDefault(); void browse(browsing, prefix); }}>
        <label className="field"><span>{t("Directory prefix", "조회 경로")}</span><input value={prefix} disabled={busy || !!file} onChange={event => setPrefix(event.target.value)} /></label>
        <button className="btn" disabled={busy || !!file}>{t("Open prefix", "경로 열기")}</button>
        <button className="btn" type="button" disabled={busy || !!file || page.prefix === browsing.prefix} onClick={() => void browse(browsing, page.prefix.slice(0, -1).replace(/[^/]*$/, ""))}>{t("Parent directory", "상위 경로")}</button>
        <button className="btn" type="button" disabled={busy || !!file} onClick={() => { setBrowsing(null); setPage(null); }}>{t("Close browser", "조회 닫기")}</button>
      </form>
      <p className="pipe-description pipe-storage-key">s3://{browsing.bucket}/{page.prefix}</p>
      {page.prefixes.map(directory => <button key={directory} className="btn" disabled={busy || !!file} onClick={() => void browse(browsing, directory)}>{directory.slice(page.prefix.length)}</button>)}
      <div className="table-scroll"><table className="vm-table"><thead><tr><th>{t("Key", "파일 경로")}</th><th>{t("Bytes", "바이트")}</th><th>{t("Modified", "수정 시각")}</th><th>{t("Actions", "작업")}</th></tr></thead><tbody>
        {page.objects.map(object => <tr key={object.key}><td className="pipe-storage-key">{object.key}</td><td>{object.size.toLocaleString()}</td><td>{object.lastModified}</td><td><div className="pipe-row-actions">
          <button className="btn" disabled={busy} onClick={() => void perform(() => downloadObject(browsing.id, object.key))}>{t("Download", "다운로드")}</button>
          <button className="btn" disabled={busy} onClick={() => { if (window.confirm(t(`Delete S3 object ${object.key}?`, `S3 파일 ${object.key}을 삭제할까요?`))) void perform(async () => { await removeObject(browsing.id, object.key); setPage(await listObjects(browsing.id, page.prefix)); }); }}>{t("Delete object", "파일 삭제")}</button>
        </div></td></tr>)}
      </tbody></table></div>
      {!page.objects.length && !page.prefixes.length && <p className="empty">{t("No objects in this directory.", "이 경로에 파일이 없다.")}</p>}
      <button className="btn" disabled={busy || !!file || !page.nextCursor} onClick={() => { if (page.nextCursor) void browse(browsing, page.prefix, page.nextCursor); }}>{t("Next page", "다음 페이지")}</button>
      <form onSubmit={upload} className="pipe-storage-form">
        <h3>{t("Upload file · up to 512 MiB", "파일 업로드 · 최대 512 MiB")}</h3>
        <label className="field"><span>{t("File", "파일")}</span><input type="file" ref={fileInput} disabled={busy} required onChange={e => { const next = e.target.files?.[0] ?? null; setFile(next); if (next) setKey(page.prefix + next.name); }} /></label>
        <label className="field"><span>{t("Full object key", "전체 파일 경로")}</span><input value={key} required maxLength={1024} disabled={busy} onChange={e => setKey(e.target.value)} /></label>
        <label className="pipe-storage-check"><input type="checkbox" checked={overwrite} disabled={busy} onChange={e => setOverwrite(e.target.checked)} />{t("Allow overwrite", "덮어쓰기 허용")}</label>
        <div className="pipe-row-actions"><button className="btn primary" disabled={busy || !file}>{busy ? t("Working…", "작업 중…") : t("Upload", "업로드")}</button>
          {!!file && <button type="button" className="btn" disabled={busy} onClick={() => { setFile(null); if (fileInput.current) fileInput.current.value = ""; }}>{t("Clear file", "파일 선택 해제")}</button>}</div>
      </form>
    </section>}
  </section>;
}
