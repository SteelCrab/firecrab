import { useEffect, useRef, useState, type FormEvent } from "react";
import { useI18n } from "../i18n";
import { deleteRegistry, getRegistries, getRegistryTransfer, saveRegistry, startRegistryTransfer, type RegistryConnection } from "../api/registries";
import type { ImageInstallResponse } from "../bindings";
import RegistryBrowser from "./RegistryBrowser";

type ActiveJob = { kind: "transfer"; id: string };
const JOB_KEY = "firecrab.registry-job";
function restoredJob(): ActiveJob | null {
  try {
    const value = JSON.parse(localStorage.getItem(JOB_KEY) ?? "null");
    return value && value.kind === "transfer" && typeof value.id === "string" ? value : null;
  } catch { return null; }
}

export default function PipeRegistries({ onUnsavedChange }: { onUnsavedChange: (dirty: boolean) => void }) {
  const { t } = useI18n();
  const [connections, setConnections] = useState<RegistryConnection[]>([]);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [editor, setEditor] = useState(false);
  const [provider, setProvider] = useState("ghcr");
  const [host, setHost] = useState("");
  const [username, setUsername] = useState("");
  const [secret, setSecret] = useState("");
  const [selectedRegistry, setRegistry] = useState("");
  const [browserVersion, setBrowserVersion] = useState(0);
  const [activeJob, setActiveJob] = useState<ActiveJob | null>(restoredJob);
  const [job, setJob] = useState<ImageInstallResponse | null>(null);
  const inFlight = useRef(false);
  const disabled = loading || busy || activeJob !== null;
  const [initial, setInitial] = useState({ host: "", username: "" });
  const dirty = editor && (!!secret || host !== initial.host || username !== initial.username);

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
        setJob(next);
        if (next.status === "running") { timer = setTimeout(poll, 1_000); return; }
        if (next.status === "succeeded") {
          setNotice(t("Image pulled.", "가져오기 완료"));
        } else {
          setError(next.status === "idle" ? t("Job expired. Retry.", "작업 만료. 다시 시도한다.") : next.log.split("\n").filter(Boolean).at(-1) ?? t("Image pull failed.", "이미지 가져오기에 실패했다."));
        }
        if (!cancelled) setActiveJob(null);
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
  function resetEditor() { setEditor(false); setHost(""); setUsername(""); setSecret(""); }
  function edit(connection?: RegistryConnection) {
    const host = connection?.registry ?? "";
    const username = connection?.username ?? "";
    setProvider(connection?.provider ?? "ghcr"); setHost(host); setUsername(username); setInitial({ host, username }); setSecret(""); setEditor(true);
  }
  async function login(event: FormEvent) {
    event.preventDefault();
    await perform(async () => {
      const loginRegistry = ["ecr", "harbor"].includes(provider) ? host.trim() : provider === "dockerHub" ? "docker.io" : "ghcr.io";
      const saved = await saveRegistry(loginRegistry, provider === "ecr" ? "AWS" : username.trim(), secret, provider === "harbor" ? "harbor" : undefined);
      setConnections(current => [...current.filter(value => value.registry !== saved.registry), saved].sort((a,b) => a.registry.localeCompare(b.registry)));
      setRegistry(saved.registry); setBrowserVersion(value => value + 1); resetEditor(); setNotice(t("Login saved.", "인증 저장 완료"));
    });
  }
  async function pull(reference: string) {
    await perform(async () => {
      const next = await startRegistryTransfer({ action: "pull", reference });
      setJob(next); setActiveJob({ kind: "transfer", id: next.alias });
    });
  }
  const selectedConnection = connections.find(value => value.registry === selectedRegistry) ?? connections[0];
  const registry = selectedConnection?.registry ?? "";
  return <section className="panel pipe-registries" aria-label="Pipe Registry">
    <h2 className="panel-title"><span>Pipe Registry</span><button className="btn" disabled={disabled || editor} onClick={() => edit()}>{t("Add registry", "레지스트리 추가")}</button></h2>
    {error && <div className="banner error" role="alert">{error}</div>}
    {notice && <div className="banner info" role="status">{notice}</div>}
    {loading && <p role="status">{t("Loading registries…", "레지스트리 조회 중…")}</p>}
    {editor && <form className="registry-form" onSubmit={login} aria-label={t("Registry login", "레지스트리 인증")}>
      <label className="field"><span>{t("Provider", "서비스")}</span><select value={provider} disabled={disabled} onChange={event => { setProvider(event.target.value); setHost(""); setUsername(""); setSecret(""); }}><option value="dockerHub">Docker Hub</option><option value="ghcr">GitHub Container Registry</option><option value="ecr">Amazon ECR</option><option value="harbor">Harbor</option></select></label>
      {["ecr", "harbor"].includes(provider) && <label className="field"><span>{t("Registry hostname", "레지스트리 호스트")}</span><input value={host} onChange={event => setHost(event.target.value)} placeholder={provider === "harbor" ? "harbor.example.com or 127.0.0.1:18080" : "123456789012.dkr.ecr.ap-northeast-2.amazonaws.com"} disabled={disabled} required /></label>}
      {provider !== "ecr" && <label className="field"><span>{provider === "harbor" ? t("Harbor username", "Harbor 사용자명") : provider === "dockerHub" ? t("Docker Hub username", "Docker Hub 사용자명") : t("GitHub username", "GitHub 사용자명")}</span><input autoComplete="username" value={username} onChange={event => setUsername(event.target.value)} disabled={disabled} required /></label>}
      <label className="field"><span>{provider === "harbor" ? t("Harbor password or robot token", "Harbor 비밀번호 또는 로봇 토큰") : provider === "ghcr" ? t("Personal access token (classic)", "개인 액세스 토큰 (classic)") : provider === "dockerHub" ? t("Personal access token (PAT)", "개인 액세스 토큰 (PAT)") : t("ECR login password", "ECR 로그인 비밀번호")}</span><input type="password" autoComplete="new-password" value={secret} onChange={event => setSecret(event.target.value)} disabled={disabled} required /></label>
      {provider === "dockerHub" && <p className="registry-help registry-wide">{t("Token permission: Read", "토큰 권한: Read")}</p>}
      {provider === "ghcr" && <p className="registry-help registry-wide">{t("Token permission: read:packages", "토큰 권한: read:packages")}</p>}
      {provider === "ecr" && <p className="registry-help registry-wide"><code>aws ecr get-login-password --region REGION</code></p>}
      <div className="pipe-row-actions registry-wide"><button className="btn primary" disabled={disabled} type="submit">{t("Save login", "인증 저장")}</button><button className="btn" type="button" disabled={busy} onClick={resetEditor}>{t("Cancel", "취소")}</button></div>
    </form>}
    {!loading && connections.length > 0 && <div className="table-scroll"><table className="vm-table registry-table registry-connections"><thead><tr><th>{t("Registry", "레지스트리")}</th><th>{t("Account", "계정")}</th><th className="actions">{t("Actions", "작업")}</th></tr></thead><tbody>{connections.map(connection => <tr key={connection.registry} aria-selected={connection.registry === registry}><td className="registry-reference">{connection.registry}</td><td className="registry-account" data-label={t("Account", "계정")}>{connection.username}</td><td className="actions"><div className="pipe-row-actions"><button className="btn" disabled={disabled} onClick={() => { setRegistry(connection.registry); setBrowserVersion(value => value + 1); }}>{t("View images", "이미지 보기")}</button><button className="btn" disabled={disabled || editor} onClick={() => edit(connection)}>{t("Update token", "토큰 갱신")}</button><button className="btn danger" disabled={disabled} onClick={() => void perform(async () => { await deleteRegistry(connection.registry); setConnections(current => current.filter(value => value.registry !== connection.registry)); if (registry === connection.registry) setRegistry(""); setNotice(t("Login removed.", "인증 삭제 완료")); })}>{t("Remove", "삭제")}</button></div></td></tr>)}</tbody></table></div>}
    {!loading && connections.length === 0 && <p className="registry-help">{t("No registries.", "등록한 레지스트리가 없다.")}</p>}
    {selectedConnection && <>
      <h3>{t("Registry images", "레지스트리 이미지")}</h3>
      <div className="registry-form">
        <label className="field"><span>{t("Source registry", "원본 레지스트리")}</span><select value={registry} disabled={disabled} onChange={event => setRegistry(event.target.value)}>{connections.map(connection => <option key={connection.registry} value={connection.registry}>{connection.registry === "docker.io" ? "Docker Hub (docker.io)" : connection.registry} · {connection.username}</option>)}</select></label>
      </div>
      <RegistryBrowser key={`${registry}/${selectedConnection.username}/${browserVersion}`} registry={registry} disabled={disabled} onPull={pull} />
    </>}
    {job && <div aria-live="polite"><p className="registry-help">{job.status === "running" ? t("Pulling…", "가져오는 중…") : t("Last pull", "최근 가져오기")}</p><pre className="registry-log">{job.log}</pre></div>}
  </section>;
}
