import { useEffect, useRef, useState, type FormEvent } from "react";
import { ArrowLeft, ArrowRight, ChevronRight, Cloud, File, Folder, FolderOpen, Plus, RefreshCw, Upload } from "lucide-react";
import { useI18n } from "../i18n";
import { viewHash } from "../navigation";
import { disconnectStorage, downloadObject, listObjects, listStorage, removeObject, saveStorage, uploadObject, verifyStorage, type ObjectPage, type StorageConnection, type StorageInput } from "../api/pipeStorage";
import PipeServiceMenu from "./PipeServiceMenu";
import StorageDialog from "./pipe-storage/StorageDialog";
import StorageConnectionForm, { type StorageEditor } from "./pipe-storage/StorageConnectionForm";
import "../styles/pipe-storage.css";

const newStorageEditor = (): StorageEditor => ({ name: "", bucket: "", region: "ap-northeast-2", prefix: "", credentialRef: "pipeAuthAws", replaceKeys: false });
const folderPrefix = (value: string) => value && !value.endsWith("/") ? `${value}/` : value;

function fileSize(bytes: number) {
  if (bytes < 1024) return `${bytes} B`;
  const unit = Math.min(Math.floor(Math.log(bytes) / Math.log(1024)), 3);
  return `${Number((bytes / 1024 ** unit).toFixed(1))} ${["B", "KiB", "MiB", "GiB"][unit]}`;
}

export default function PipeStorage({ onUnsavedChange }: { onUnsavedChange: (dirty: boolean) => void }) {
  const { t, locale } = useI18n();
  const [connections, setConnections] = useState<StorageConnection[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const inFlight = useRef(false);
  const [editor, setEditor] = useState<StorageEditor | null>(null);
  const [originalEditor, setOriginalEditor] = useState("");
  const [browsing, setBrowsing] = useState<StorageConnection | null>(null);
  const [page, setPage] = useState<ObjectPage | null>(null);
  const [pathEditor, setPathEditor] = useState(false);
  const [prefix, setPrefix] = useState("");
  const [file, setFile] = useState<File | null>(null);
  const [name, setName] = useState("");
  const [overwrite, setOverwrite] = useState(false);
  const fileInput = useRef<HTMLInputElement>(null);
  const editorDirty = !!editor && JSON.stringify(editor) !== originalEditor;

  useEffect(() => { onUnsavedChange(editorDirty || !!file || busy); return () => onUnsavedChange(false); }, [editorDirty, file, busy, onUnsavedChange]);
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
  function open(connection?: StorageConnection) {
    const next = connection ? { ...connection, replaceKeys: false } : newStorageEditor();
    setOriginalEditor(JSON.stringify(next));
    setEditor(next); setError(null); setNotice(null);
  }
  function cancelEditor() { setEditor(null); setError(null); }
  function closeEditor() {
    if (!editorDirty || window.confirm(t("Discard these connection changes?", "연결 변경 사항을 취소할까요?"))) cancelEditor();
  }
  async function submit(event: FormEvent) {
    event.preventDefault();
    if (!editor) return;
    const { bucket, region, prefix: savedPrefix, credentialRef, credentials } = editor;
    const input: StorageInput = { name: editor.name.trim() || bucket, bucket, region, prefix: folderPrefix(savedPrefix), credentialRef };
    if (credentialRef === "connection" && (!editor.id || editor.replaceKeys)) input.credentials = credentials;
    await perform(async () => {
      const saved = await saveStorage(input, editor.id);
      setConnections(rows => [...rows.filter(row => row.id !== saved.id), saved]);
      setEditor(null);
      if (browsing?.id === saved.id) { setBrowsing(saved); setPage(null); setPage(await listObjects(saved.id, saved.prefix)); }
      setNotice(t("Connection verified and saved.", "연결을 확인하고 저장했다."));
    });
  }
  async function browse(connection: StorageConnection, nextPrefix = connection.prefix, cursor?: string) {
    await perform(async () => {
      const next = await listObjects(connection.id, nextPrefix, cursor);
      setBrowsing(connection);
      setPage(current => cursor && current && current.prefix === next.prefix ? {
        ...next, objects: [...current.objects, ...next.objects], prefixes: [...new Set([...current.prefixes, ...next.prefixes])],
      } : next);
      setPrefix(next.prefix); setPathEditor(false);
    });
  }
  function back() { setBrowsing(null); setPage(null); setPathEditor(false); setError(null); setNotice(null); }
  function disconnect(connection: StorageConnection) {
    if (!window.confirm(t(`Disconnect ${connection.name}? Your S3 files will stay in AWS.`, `${connection.name} 연결을 해제할까요? S3 파일은 AWS에 보존한다.`))) return;
    void perform(async () => {
      await disconnectStorage(connection.id);
      setConnections(rows => rows.filter(row => row.id !== connection.id));
      if (browsing?.id === connection.id) back();
    });
  }
  function connectionMenu(connection: StorageConnection) {
    return <PipeServiceMenu label={t(`${connection.name} actions`, `${connection.name} 작업`)} disabled={busy} items={[
      { label: t("Edit connection", "연결 수정"), disabled: !!connection.services.length, onSelect: () => open(connection) },
      { label: t("Check connection", "연결 검사"), onSelect: () => void perform(async () => {
        await verifyStorage(connection.id); setNotice(t("Listing is allowed. File permissions are checked when you use them.", "목록 조회 권한을 확인했다. 파일 권한은 해당 작업을 할 때 확인한다."));
      }) },
      { label: t("Disconnect", "연결 해제"), danger: true, disabled: !!connection.services.length, onSelect: () => disconnect(connection) },
    ]} />;
  }
  function clearFile() {
    setFile(null); setName(""); setOverwrite(false); setError(null);
    if (fileInput.current) fileInput.current.value = "";
  }
  function chooseFile(next: File | null) {
    if (!next) return;
    if (next.size > 512 * 1024 * 1024) {
      setError(t("Files must be at most 512 MiB.", "파일 크기는 512 MiB 이하여야 한다."));
      if (fileInput.current) fileInput.current.value = "";
      return;
    }
    setFile(next); setName(next.name); setOverwrite(false); setError(null); setNotice(null);
  }
  async function upload(event: FormEvent) {
    event.preventDefault();
    if (!browsing || !page || !file) return;
    const key = page.prefix + name;
    if (overwrite && !window.confirm(t(`Overwrite ${key} if it exists?`, `${key} 파일이 있으면 덮어쓸까요?`))) return;
    await perform(async () => {
      await uploadObject(browsing.id, key, file, overwrite);
      clearFile(); setNotice(t("Upload complete.", "파일을 업로드했다."));
      setPage(await listObjects(browsing.id, page.prefix));
    });
  }
  function deleteFile(key: string) {
    if (!browsing || !page || !window.confirm(t(`Delete ${key}? This cannot be undone.`, `${key} 파일을 삭제할까요? 이 작업은 되돌릴 수 없다.`))) return;
    void perform(async () => {
      await removeObject(browsing.id, key); setPage(await listObjects(browsing.id, page.prefix)); setNotice(t("File deleted.", "파일을 삭제했다."));
    });
  }
  function modified(value: string) {
    const date = new Date(value);
    return Number.isNaN(date.valueOf()) ? value : date.toLocaleString(locale === "ko" ? "ko-KR" : "en-US", { year: "numeric", month: "short", day: "numeric", hour: "numeric", minute: "2-digit" });
  }

  const relativeParts = browsing && page ? page.prefix.slice(browsing.prefix.length).split("/") : [];
  const messages = <>{error && <div className="banner error" role="alert">{error}</div>}{notice && <div className="s3-notice" role="status">{notice}</div>}</>;
  return <section className="s3-storage" aria-label="PipeStorage" aria-busy={busy}>
    <nav className="s3-back" aria-label={browsing ? t("Storage navigation", "스토리지 이동") : t("Pipe navigation", "Pipe 이동")}>
      {browsing ? <button type="button" disabled={busy} onClick={back}><ArrowLeft aria-hidden="true" />{t("All connections", "모든 연결")}</button> : <a href={viewHash("pipe")}><ArrowLeft aria-hidden="true" />Pipe</a>}
    </nav>
    <header className="s3-heading">
      <div className="s3-heading-copy"><h2>{browsing ? browsing.name : t("S3 Storage", "S3 스토리지")}</h2><p>{browsing ? `s3://${browsing.bucket}/${browsing.prefix}` : t("Access your files in AWS.", "AWS에 저장한 파일을 관리한다.")}</p></div>
      <div className="s3-toolbar">
        <button type="button" className="icon-btn" disabled={busy || loading} aria-label={t("Refresh", "새로고침")} title={t("Refresh", "새로고침")} onClick={() => browsing ? void browse(browsing, page?.prefix) : void perform(async () => { setConnections(await listStorage()); })}><RefreshCw aria-hidden="true" className={busy ? "s3-spinning" : undefined} /></button>
        {browsing ? <><button type="button" className="btn primary" disabled={busy || !page} onClick={() => fileInput.current?.click()}><Upload aria-hidden="true" />{t("Upload file", "파일 업로드")}</button>{connectionMenu(browsing)}</> : !!connections.length && <button type="button" className="btn primary" disabled={busy || loading} onClick={() => open()}><Plus aria-hidden="true" />{t("Connect bucket", "버킷 연결")}</button>}
      </div>
    </header>
    {!editor && !file && messages}
    {loading ? <div className="s3-empty" role="status">{t("Loading S3 connections…", "S3 연결을 불러오는 중…")}</div> : !browsing && <>
      {!connections.length ? <div className="s3-empty"><Cloud aria-hidden="true" /><h3>{t("Connect your first bucket", "첫 버킷 연결")}</h3><p>{t("Browse and upload files without leaving Firecrab.", "Firecrab에서 파일을 조회하고 업로드한다.")}</p><button type="button" className="btn primary" disabled={busy} onClick={() => open()}><Plus aria-hidden="true" />{t("Connect bucket", "버킷 연결")}</button></div> : <ul className="s3-connections" aria-label={t("S3 connections", "S3 연결")}>
        {connections.map(connection => <li key={connection.id}>
          <button type="button" className="s3-connection-open" disabled={busy} aria-label={t(`Open files in ${connection.name}`, `${connection.name} 파일 열기`)} onClick={() => void browse(connection)}>
            <span className="s3-bucket-icon"><FolderOpen aria-hidden="true" /></span><span className="s3-connection-copy"><strong>{connection.name}</strong><span>{connection.bucket}{connection.prefix && ` / ${connection.prefix}`}</span>{!!connection.services.length && <span className="s3-bound">{t("Used by", "사용 중:")} {connection.services.map(service => service.name).join(", ")}</span>}</span><span className="s3-region">{connection.region}</span><ChevronRight className="s3-open-arrow" aria-hidden="true" />
          </button>{connectionMenu(connection)}
        </li>)}
      </ul>}
      {connections.some(connection => connection.services.length) && <p className="s3-hint s3-connection-note">{t("Unbind these services before editing or disconnecting.", "연결을 수정하거나 해제하려면 서비스에서 연결을 먼저 해제한다.")}</p>}
    </>}
    {browsing && <section className="s3-browser" aria-label={t("S3 object browser", "S3 파일 조회")}>
      <input type="file" aria-label={t("File", "파일")} ref={fileInput} hidden disabled={busy} onChange={event => chooseFile(event.target.files?.[0] ?? null)} />
      {page && <>
        <div className="s3-pathbar"><nav className="s3-breadcrumbs" aria-label={t("Folder navigation", "폴더 이동")}>
          <button type="button" disabled={busy} aria-current={page.prefix === browsing.prefix ? "location" : undefined} onClick={() => void browse(browsing)}><FolderOpen aria-hidden="true" />{browsing.prefix || browsing.bucket}</button>
          {relativeParts.map((part, index) => part || index < relativeParts.length - 1 ? <span key={index}><ChevronRight aria-hidden="true" /><button type="button" disabled={busy} aria-current={index === relativeParts.length - (page.prefix.endsWith("/") ? 2 : 1) ? "location" : undefined} onClick={() => void browse(browsing, browsing.prefix + relativeParts.slice(0, index + 1).join("/") + (index < relativeParts.length - 1 ? "/" : ""))}>{part || "/"}</button></span> : null)}
        </nav><button className="s3-text-button" type="button" disabled={busy} aria-expanded={pathEditor} onClick={() => { setPrefix(page.prefix); setPathEditor(value => !value); }}>{t("Enter folder path", "경로 입력")}</button></div>
        {pathEditor && <form className="s3-path-form" onSubmit={event => { event.preventDefault(); void browse(browsing, folderPrefix(prefix || browsing.prefix)); }}><input aria-label={t("Directory prefix", "조회 경로")} autoFocus value={prefix} disabled={busy} onChange={event => setPrefix(event.target.value)} /><button type="submit" className="btn" disabled={busy}><ArrowRight aria-hidden="true" />{t("Open prefix", "경로 열기")}</button></form>}
        <table className="s3-files"><thead><tr><th>{t("Name", "이름")}</th><th className="s3-file-size">{t("Size", "크기")}</th><th className="s3-file-modified">{t("Modified", "수정일")}</th><th><span className="sr-only">{t("Actions", "작업")}</span></th></tr></thead><tbody>
          {page.prefixes.map(directory => <tr key={directory}><td><button type="button" className="s3-file-name" disabled={busy} onClick={() => void browse(browsing, directory)}><Folder className="s3-folder-icon" aria-hidden="true" /><span>{directory.slice(page.prefix.length).replace(/\/$/, "") || "/"}</span></button></td><td className="s3-file-size">—</td><td className="s3-file-modified">{t("Folder", "폴더")}</td><td><ChevronRight className="s3-folder-arrow" aria-hidden="true" /></td></tr>)}
          {page.objects.map(object => { const filename = object.key.slice(page.prefix.length) || object.key; return <tr key={object.key}>
            <td><button type="button" className="s3-file-name" title={object.key} disabled={busy} onClick={() => void perform(() => downloadObject(browsing.id, object.key))}><File aria-hidden="true" /><span>{filename}</span></button><span className="s3-file-mobile-meta">{fileSize(object.size)} · {modified(object.lastModified)}</span></td><td className="s3-file-size">{fileSize(object.size)}</td><td className="s3-file-modified"><time dateTime={object.lastModified}>{modified(object.lastModified)}</time></td><td><PipeServiceMenu label={t(`${filename} actions`, `${filename} 작업`)} disabled={busy} items={[
              { label: t("Download", "다운로드"), onSelect: () => void perform(() => downloadObject(browsing.id, object.key)) },
              { label: t("Delete file", "파일 삭제"), danger: true, onSelect: () => deleteFile(object.key) },
            ]} /></td>
          </tr>; })}
        </tbody></table>
        {!page.objects.length && !page.prefixes.length && <div className="s3-empty s3-empty-folder"><FolderOpen aria-hidden="true" /><h3>{t("This folder is empty", "폴더가 비어 있다")}</h3><p>{t("Upload a file to get started.", "파일을 업로드해 시작한다.")}</p></div>}
        <footer className="s3-files-footer"><span>{t(`${page.objects.length + page.prefixes.length} items`, `${page.objects.length + page.prefixes.length}개 항목`)}</span>{page.nextCursor && <button type="button" className="btn" disabled={busy} onClick={() => void browse(browsing, page.prefix, page.nextCursor!)}>{t("Load more", "더 보기")}</button>}</footer>
      </>}
      {!!browsing.services.length && <p className="s3-hint s3-bound-note">{t("Used by", "사용 중:")} {browsing.services.map(service => service.name).join(", ")}. {t("Unbind these services before editing or disconnecting.", "연결을 수정하거나 해제하려면 서비스에서 연결을 먼저 해제한다.")}</p>}
    </section>}
    {editor && <StorageDialog title={editor.id ? t("Edit S3 connection", "S3 연결 수정") : t("Connect S3 bucket", "S3 버킷 연결")} busy={busy} onClose={closeEditor}>
      {messages}<StorageConnectionForm editor={editor} busy={busy} onChange={setEditor} onSubmit={event => void submit(event)} onCancel={cancelEditor} />
    </StorageDialog>}
    {file && browsing && page && <StorageDialog title={t("Upload file", "파일 업로드")} busy={busy} onClose={clearFile}>
      {messages}<form className="s3-form" onSubmit={event => void upload(event)}>
        <div className="s3-upload-file"><File aria-hidden="true" /><div><strong>{file.name}</strong><span>{fileSize(file.size)}</span></div></div>
        <label className="field"><span>{t("File name", "파일 이름")}</span><input required maxLength={1024 - page.prefix.length} value={name} disabled={busy} onChange={event => setName(event.target.value)} /></label>
        <p className="s3-hint s3-destination">{t("Upload to", "저장 위치:")} <span>s3://{browsing.bucket}/{page.prefix}{name}</span></p>
        <label className="s3-check"><input type="checkbox" checked={overwrite} disabled={busy} onChange={event => setOverwrite(event.target.checked)} />{t("Allow overwrite", "덮어쓰기 허용")}</label>
        <footer className="s3-dialog-actions"><button type="button" className="btn" disabled={busy} onClick={clearFile}>{t("Cancel", "취소")}</button><button type="submit" className="btn primary" disabled={busy}><Upload aria-hidden="true" />{busy ? t("Uploading…", "업로드 중…") : t("Upload", "업로드")}</button></footer>
      </form>
    </StorageDialog>}
  </section>;
}
