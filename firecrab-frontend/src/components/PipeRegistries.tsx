import { useEffect, useRef, useState, type FormEvent } from "react";
import { useI18n } from "../i18n";
import { deleteRegistry, getRegistries, getRegistryTransfer, saveRegistry, startRegistryTransfer, type RegistryConnection } from "../api/registries";
import type { ImageInstallResponse } from "../bindings";
import RegistryBrowser from "./RegistryBrowser";
import PipeServiceMenu from "./PipeServiceMenu";

type Provider = RegistryConnection["provider"];
type ActiveJob = { kind: "transfer"; id: string; reference: string };
interface Editor { original: string | null; provider: Provider; host: string; username: string; secret: string; accountName: string }

const JOB_KEY = "firecrab.registry-job";
const PROVIDERS: { id: Provider; name: string; short: string }[] = [
  { id: "dockerHub", name: "Docker Hub", short: "Docker Hub" },
  { id: "ghcr", name: "GitHub Container Registry", short: "GHCR" },
  { id: "ecr", name: "Amazon ECR", short: "Amazon ECR" },
  { id: "harbor", name: "Harbor", short: "Harbor" },
];

function restoredJob(): ActiveJob | null {
  try {
    const value = JSON.parse(localStorage.getItem(JOB_KEY) ?? "null");
    if (!value || value.kind !== "transfer" || typeof value.id !== "string") return null;
    // Jobs saved before the reference was stored still resume; they just show no image line.
    return { kind: "transfer", id: value.id, reference: typeof value.reference === "string" ? value.reference : "" };
  } catch { return null; }
}

/** Saved registry logins; the selected row's repositories open below the table. */
export default function PipeRegistries({ onUnsavedChange }: { onUnsavedChange: (dirty: boolean) => void }) {
  const { t } = useI18n();
  const [connections, setConnections] = useState<RegistryConnection[]>([]);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [editor, setEditor] = useState<Editor | null>(null);
  const [selectedRegistry, setRegistry] = useState("");
  const [browserVersion, setBrowserVersion] = useState(0);
  const [imageVersion, setImageVersion] = useState(0);
  const [activeJob, setActiveJob] = useState<ActiveJob | null>(restoredJob);
  const [job, setJob] = useState<{ reference: string; snapshot: ImageInstallResponse } | null>(null);
  const inFlight = useRef(false);
  const original = connections.find(value => value.registry === editor?.original);
  const dirty = !!editor && (!!editor.secret || editor.accountName !== (original?.accountName ?? "") || (original ? editor.username !== original.username : !!editor.host || !!editor.username));

  useEffect(() => { onUnsavedChange(dirty); return () => onUnsavedChange(false); }, [dirty, onUnsavedChange]);
  useEffect(() => {
    let cancelled = false;
    void getRegistries().then(connections => {
      if (!cancelled) { setConnections(connections); if (connections.length) setRegistry(connections[0].registry); }
    }).catch(failure => { if (!cancelled) setError((failure as Error).message); })
      .finally(() => { if (!cancelled) setLoading(false); });
    return () => { cancelled = true; };
  }, []);

  useEffect(() => {
    try {
      if (activeJob) localStorage.setItem(JOB_KEY, JSON.stringify(activeJob));
      else localStorage.removeItem(JOB_KEY);
    } catch { /* Job recovery is optional when browser storage is unavailable. */ }
    if (!activeJob) return;
    let cancelled = false;
    let timer: ReturnType<typeof setTimeout>;
    async function poll() {
      try {
        const next = await getRegistryTransfer(activeJob!.id);
        if (cancelled) return;
        setJob({ reference: activeJob!.reference, snapshot: next });
        if (next.status === "running") { timer = setTimeout(poll, 1_000); return; }
        if (next.status === "succeeded") setImageVersion(value => value + 1);
        else setError(next.status === "idle" ? t("Job expired. Retry.", "작업이 만료됐다. 다시 시도한다.") : next.log.split("\n").filter(Boolean).at(-1) ?? t("Image pull failed.", "이미지 가져오기에 실패했다."));
        setActiveJob(null);
      } catch (failure) {
        if (!cancelled) { setError((failure as Error).message); timer = setTimeout(poll, 5_000); }
      }
    }
    void poll();
    return () => { cancelled = true; clearTimeout(timer); };
  }, [activeJob, t]);

  async function perform(action: () => Promise<void>) {
    if (inFlight.current) return;
    inFlight.current = true; setBusy(true); setError(null); setNotice(null);
    try { await action(); } catch (failure) { setError((failure as Error).message); }
    finally { inFlight.current = false; setBusy(false); }
  }
  function open(connection?: RegistryConnection) {
    setEditor(connection
      ? { original: connection.registry, provider: connection.provider, host: connection.registry, username: connection.username, secret: "", accountName: connection.accountName ?? "" }
      : { original: null, provider: "ghcr", host: "", username: "", secret: "", accountName: "" });
    setError(null); setNotice(null);
  }
  function select(registry: string) {
    if (registry === selectedRegistry) return;
    setRegistry(registry); setBrowserVersion(value => value + 1);
  }
  async function login(event: FormEvent) {
    event.preventDefault();
    if (!editor) return;
    const { provider, host, username, secret } = editor;
    await perform(async () => {
      const registry = ["ecr", "harbor"].includes(provider) ? host.trim() : provider === "dockerHub" ? "docker.io" : "ghcr.io";
      const saved = await saveRegistry(registry, provider === "ecr" ? "AWS" : username.trim(), secret, provider === "harbor" ? "harbor" : undefined, provider === "ecr" ? editor.accountName.trim() : undefined);
      setConnections(current => [...current.filter(value => value.registry !== saved.registry), saved].sort((a, b) => a.registry.localeCompare(b.registry)));
      setRegistry(saved.registry); setBrowserVersion(value => value + 1); setEditor(null);
      setNotice(t("Login saved.", "인증을 저장했다."));
    });
  }
  function remove(registry: string) {
    if (busy || !window.confirm(t(`Delete the ${registry} login? Pulled images are kept.`, `${registry} 인증을 삭제할까요? 가져온 이미지는 유지된다.`))) return;
    void perform(async () => {
      await deleteRegistry(registry);
      setConnections(current => current.filter(value => value.registry !== registry));
      setEditor(current => current?.original === registry ? null : current);
      setNotice(t("Login deleted.", "인증을 삭제했다."));
    });
  }
  async function pull(reference: string) {
    await perform(async () => {
      const next = await startRegistryTransfer({ action: "pull", reference });
      setJob({ reference, snapshot: next }); setActiveJob({ kind: "transfer", id: next.alias, reference });
    });
  }

  const selectedConnection = connections.find(value => value.registry === selectedRegistry) ?? connections[0];
  const registry = selectedConnection?.registry ?? "";
  const providerName = (id: Provider) => PROVIDERS.find(value => value.id === id)?.short ?? id;
  const jobLabel = { idle: t("Expired", "만료"), running: t("Pulling…", "가져오는 중…"), succeeded: t("Pulled", "가져옴"), failed: t("Failed", "실패") };
  const jobTone = { idle: "", running: " starting", succeeded: " running", failed: " error" };
  return <section className="panel pipe-registries" aria-label="Pipe Registry">
    <h2 className="panel-title"><span>Pipe Registry</span><button className="btn" type="button" disabled={loading || busy || !!editor} aria-expanded={!!editor && !editor.original} aria-controls="registry-login-editor" onClick={() => open()}>{t("Add registry", "레지스트리 추가")}</button></h2>
    {error && <div className="banner error" role="alert">{error}</div>}
    {notice && <div className="banner info" role="status">{notice}</div>}
    {editor && <RegistryForm editor={editor} busy={busy} onChange={setEditor} onSubmit={login} onCancel={() => setEditor(null)} />}
    {loading ? <p className="poll-note" role="status">{t("Loading registries…", "레지스트리 조회 중…")}</p>
      : !connections.length ? <div className="empty">{t("No registries.", "등록한 레지스트리가 없다.")}</div>
      : <div className="table-scroll"><table className="vm-table registry-connections" aria-label={t("Saved registries", "등록한 레지스트리")}>
        <thead><tr><th scope="col">{t("Registry", "레지스트리")}</th><th scope="col">{t("Provider", "서비스")}</th><th scope="col">{t("Account", "계정")}</th><th scope="col" className="actions">{t("Actions", "작업")}</th></tr></thead>
        <tbody>{connections.map(connection => {
          const selected = connection.registry === registry;
          const accountId = connection.registry.match(/^\d{12}(?=\.dkr\.ecr\.)/)?.[0];
          // The row takes the click; its button gives keyboard users the same selection.
          return <tr key={connection.registry} className={selected ? "selected" : undefined} onClick={() => select(connection.registry)}>
            <td className="registry-name"><button className="registry-select" type="button" aria-pressed={selected}>{connection.registry}</button></td>
            <td data-label={t("Provider", "서비스")}>{providerName(connection.provider)}</td>
            <td data-label={t("Account", "계정")} title={connection.provider === "ecr" ? accountId : undefined}>{connection.provider === "ecr" ? connection.accountName || accountId || "—" : connection.username}</td>
            <td className="actions" onClick={event => event.stopPropagation()}><PipeServiceMenu label={t(`${connection.registry} actions`, `${connection.registry} 작업`)} disabled={busy} items={[
              { label: t("Update token", "토큰 갱신"), disabled: !!editor, onSelect: () => open(connection) },
              { label: t("Delete", "삭제"), danger: true, onSelect: () => remove(connection.registry) },
            ]} /></td>
          </tr>;
        })}</tbody>
      </table></div>}
    {selectedConnection && <RegistryBrowser key={`${registry}/${selectedConnection.username}/${browserVersion}`} registry={registry} imageVersion={imageVersion} disabled={busy} pulling={activeJob !== null} onPull={pull} />}
    {job && <div className="subpanel registry-job" role="group" aria-label={t("Last pull", "최근 가져오기")} aria-live="polite">
      <dl className="detail-fields mono">
        <dt>{t("Pull status", "가져오기 상태")}</dt><dd><span className={`state-badge${jobTone[job.snapshot.status]}`}>{jobLabel[job.snapshot.status]}</span></dd>
        {job.reference && <><dt>{t("Image", "이미지")}</dt><dd>{job.reference}</dd></>}
      </dl>
      <pre className="detail-log registry-log">{job.snapshot.log}</pre>
    </div>}
  </section>;
}

function RegistryForm({ editor, busy, onChange, onSubmit, onCancel }: {
  editor: Editor;
  busy: boolean;
  onChange: (editor: Editor) => void;
  onSubmit: (event: FormEvent) => void;
  onCancel: () => void;
}) {
  const { t } = useI18n();
  const { original, provider } = editor;
  const update = (patch: Partial<Editor>) => onChange({ ...editor, ...patch });
  const usernameLabel = provider === "harbor" ? t("Harbor username", "Harbor 사용자명") : provider === "dockerHub" ? t("Docker Hub username", "Docker Hub 사용자명") : t("GitHub username", "GitHub 사용자명");
  const secretLabel = provider === "harbor" ? t("Harbor password or robot token", "Harbor 비밀번호 또는 로봇 토큰")
    : provider === "ghcr" ? t("Personal access token (classic)", "개인 액세스 토큰 (classic)")
    : provider === "dockerHub" ? t("Personal access token (PAT)", "개인 액세스 토큰 (PAT)") : t("ECR login password", "ECR 로그인 비밀번호");
  const secretHelp = provider === "harbor" ? t("Needs list and pull access.", "목록 조회와 가져오기 권한이 필요하다.")
    : provider === "ghcr" ? t("Permission: read:packages", "권한: read:packages")
    : provider === "dockerHub" ? t("Permission: Read", "권한: Read") : "aws ecr get-login-password --region REGION";
  return <form id="registry-login-editor" className="registry-form" onSubmit={onSubmit} aria-labelledby="registry-login-title">
    <h3 id="registry-login-title" className="registry-form-title">{original ? <>{t("Update token", "토큰 갱신")} <span className="registry-source">{original}</span></> : t("Add registry", "레지스트리 추가")}</h3>
    {!original && <div className="field"><label htmlFor="registry-provider">{t("Provider", "서비스")}</label>
      <select id="registry-provider" value={provider} disabled={busy} autoFocus onChange={event => onChange({ ...editor, provider: event.target.value as Provider, host: "", username: "", secret: "", accountName: "" })}>
        {PROVIDERS.map(value => <option key={value.id} value={value.id}>{value.name}</option>)}
      </select>
    </div>}
    {!original && ["ecr", "harbor"].includes(provider) && <div className="field"><label htmlFor="registry-host">{provider === "ecr" ? t("Registry URI", "레지스트리 URI") : t("Registry hostname", "레지스트리 호스트")}</label>
      <input id="registry-host" value={editor.host} onChange={event => update({ host: event.target.value })} placeholder={provider === "harbor" ? "harbor.example.com" : "123456789012.dkr.ecr.ap-northeast-2.amazonaws.com"} autoComplete="off" spellCheck={false} disabled={busy} required aria-describedby={provider === "ecr" ? "ecr-registry-uri-help" : undefined} />
      {provider === "ecr" && <span className="poll-note" id="ecr-registry-uri-help">{t("Enter the login registry URI without https://, a repository path, or a tag.", "로그인용 레지스트리 URI를 입력한다. https://, 저장소 경로와 태그는 제외한다.")}</span>}
    </div>}
    {provider === "ecr" && <div className="field"><label htmlFor="registry-account-name">{t("AWS account name (optional)", "AWS 계정 이름 (선택)")}</label>
      <input id="registry-account-name" value={editor.accountName} onChange={event => update({ accountName: event.target.value })} maxLength={255} autoComplete="off" disabled={busy} />
    </div>}
    {provider !== "ecr" && <div className="field"><label htmlFor="registry-username">{usernameLabel}</label>
      <input id="registry-username" autoComplete="username" spellCheck={false} value={editor.username} onChange={event => update({ username: event.target.value })} disabled={busy} required />
    </div>}
    <div className="field"><label htmlFor="registry-secret">{secretLabel}</label>
      <input id="registry-secret" type="password" autoComplete="new-password" aria-describedby="registry-secret-help" value={editor.secret} onChange={event => update({ secret: event.target.value })} disabled={busy} required autoFocus={!!original} />
      <span className="poll-note" id="registry-secret-help">{secretHelp}</span>
    </div>
    <div className="pipe-controls registry-wide"><button className="btn primary" type="submit" disabled={busy}>{busy ? t("Saving…", "저장 중…") : t("Save login", "인증 저장")}</button><button className="btn" type="button" disabled={busy} onClick={onCancel}>{t("Cancel", "취소")}</button></div>
  </form>;
}
