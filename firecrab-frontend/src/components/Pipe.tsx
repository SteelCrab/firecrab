import { useEffect, useRef, useState, type FormEvent } from "react";
import { applyPipe, disablePipe, getPipe, type PipeConfig, type PipeRoute, type PipeSnapshot } from "../api/pipe";
import { useI18n } from "../i18n";
import CloudflareConnect from "./CloudflareConnect";
import type { VmResponse } from "../bindings";
import { canUsePipeApps, pipeApps } from "../pipeApps";
import PipeAppPicker from "./PipeAppPicker";

const EMPTY: PipeConfig = { listen: "127.0.0.1:8080", routes: [], tunnel: null };

interface Props {
  vms: VmResponse[];
  vmsLoading: boolean;
  vmsUnavailable: boolean;
  onUnsavedChange: (unsaved: boolean) => void;
}

export default function Pipe({ vms, vmsLoading, vmsUnavailable, onUnsavedChange }: Props) {
  const { t } = useI18n();
  const [snapshot, setSnapshot] = useState<PipeSnapshot | null>(null);
  const [draft, setDraft] = useState<PipeConfig>(EMPTY);
  const [host, setHost] = useState("");
  const [path, setPath] = useState("/");
  const [upstreams, setUpstreams] = useState<string[]>([]);
  const [editing, setEditing] = useState<number | null>(null);
  const [credentials, setCredentials] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [dirty, setDirty] = useState(false);
  const [busy, setBusy] = useState(false);
  const initialized = useRef(false);
  const revision = useRef(0);
  const mutating = useRef(false);
  const apps = pipeApps(vms);
  const preserved = editing === null ? [] : draft.routes[editing]?.upstreams ?? [];
  const appsReady = !vmsLoading && !vmsUnavailable && canUsePipeApps(apps, upstreams, preserved);
  const editorPending = editing !== null || !!host.trim() || path !== "/" || upstreams.length > 0;
  const hasUnsavedChanges = dirty || editorPending || !!credentials.trim();
  const saveDisabled = busy || !snapshot?.nginxAvailable || (editorPending && (!host.trim() || !path.startsWith("/") || !appsReady)) || (draft.tunnel !== null && !snapshot.cloudflaredAvailable);

  useEffect(() => {
    onUnsavedChange(hasUnsavedChanges);
    return () => onUnsavedChange(false);
  }, [hasUnsavedChanges, onUnsavedChange]);

  useEffect(() => {
    if (!hasUnsavedChanges) return;
    const beforeUnload = (event: BeforeUnloadEvent) => {
      event.preventDefault();
      event.returnValue = "";
    };
    window.addEventListener("beforeunload", beforeUnload);
    return () => window.removeEventListener("beforeunload", beforeUnload);
  }, [hasUnsavedChanges]);

  useEffect(() => {
    let cancelled = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    let controller: AbortController | undefined;
    const refresh = async () => {
      if (mutating.current) { timer = setTimeout(refresh, 8_000); return; }
      const current = revision.current;
      controller = new AbortController();
      const deadline = setTimeout(() => controller?.abort(), 10_000);
      try {
        const next = await getPipe(controller.signal);
        if (cancelled || current !== revision.current) return;
        setSnapshot(next);
        setError(null);
        if (!initialized.current) { setDraft(next.config ?? EMPTY); initialized.current = true; }
      } catch (failure) {
        if (cancelled || current !== revision.current) return;
        setSnapshot(null);
        setError((failure as Error).message);
      } finally {
        clearTimeout(deadline);
        if (!cancelled) timer = setTimeout(refresh, 8_000);
      }
    };
    void refresh();
    return () => { cancelled = true; clearTimeout(timer); controller?.abort(); };
  }, []);

  function change(next: PipeConfig) { initialized.current = true; setDraft(next); setDirty(true); setNotice(null); }
  function clearEditor() { setHost(""); setPath("/"); setUpstreams([]); setEditing(null); }
  function edit(route: PipeRoute, index: number) { setHost(route.host); setPath(route.pathPrefix); setUpstreams([...route.upstreams]); setEditing(index); }
  async function save(event?: FormEvent) {
    event?.preventDefault();
    if (mutating.current) return;
    let config = draft;
    if (editorPending) {
      if (!host.trim() || !path.startsWith("/") || !appsReady) { setError(t("Enter a domain and path, then select at least one app.", "도메인과 경로를 입력하고 앱을 하나 이상 선택한다.")); return; }
      const route = { host: host.trim().toLowerCase(), pathPrefix: path.trim() || "/", upstreams };
      const duplicate = draft.routes.some((existing, index) => index !== editing && existing.host === route.host && existing.pathPrefix.replace(/\/+$/, "") === route.pathPrefix.replace(/\/+$/, ""));
      if (duplicate) { setError(t("This domain and path already exist.", "이 도메인과 경로가 이미 있다.")); return; }
      config = { ...draft, routes: editing === null ? [...draft.routes, route] : draft.routes.map((existing, index) => index === editing ? route : existing) };
    }
    if (draft.tunnel && !/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(draft.tunnel.id.trim())) { setError(t("Enter a valid tunnel UUID.", "올바른 터널 UUID를 입력한다.")); return; }
    let secret: unknown = null;
    try { if (config.tunnel && credentials.trim()) secret = JSON.parse(credentials); }
    catch { setError(t("Tunnel credentials must be valid JSON.", "터널 인증 정보는 올바른 JSON이어야 한다.")); return; }
    mutating.current = true; setBusy(true); revision.current++; setError(null); setNotice(null);
    try {
      const next = await applyPipe(config, secret);
      setSnapshot(next); setDraft(next.config ?? config); setDirty(false); setCredentials(""); clearEditor();
      setNotice(t("Routes saved.", "라우트를 저장했다."));
    } catch (failure) { setError((failure as Error).message); }
    finally { mutating.current = false; setBusy(false); revision.current++; }
  }
  async function stop() {
    if (busy || !window.confirm(t("Stop Pipe? Requests through these routes will stop.", "Pipe를 중지할까요? 이 경로의 요청 전달이 중지된다."))) return;
    mutating.current = true; setBusy(true); revision.current++; setError(null); setNotice(null);
    try { setSnapshot(await disablePipe()); setNotice(t("Pipe stopped. Saved routes are retained.", "Pipe를 중지했다. 저장한 라우트는 유지한다.")); }
    catch (failure) { setError((failure as Error).message); }
    finally { mutating.current = false; setBusy(false); revision.current++; }
  }

  return <div className="stack">
    <section className="panel" aria-label="Pipe">
      <h2 className="panel-title"><span>Pipe</span><span className="poll-note">{t("Nginx routing and load balancing", "Nginx 요청 전달과 요청 분산")}</span></h2>
      {error && <div className="banner error" role="alert">{error}</div>}
      {notice && <div className="banner info" role="status">{notice}</div>}
      <div className="table-scroll"><table className="vm-table pipe-status-table"><thead><tr><th>{t("Service", "서비스")}</th><th>{t("State", "상태")}</th><th>PID</th></tr></thead><tbody>
        {["firecrab-ingress.service", "firecrab-tunnel.service"].map(unit => {
          const status = snapshot?.units.find(value => value.unit === unit);
          const state = status?.activeState ?? "unknown";
          return <tr key={unit}><td>{unit.startsWith("firecrab-ingress") ? "Nginx" : "Cloudflare Tunnel"}</td><td><span className={`state-badge${state === "active" ? " running" : state === "failed" ? " error" : ""}`}>{status ? state : t("Unavailable", "확인 불가")}</span></td><td>{status?.mainPid || "—"}</td></tr>;
        })}
      </tbody></table></div>
      {snapshot && !snapshot.nginxAvailable && <p className="field-error">{t("Install Nginx on this host before applying routes.", "라우트를 적용하기 전에 이 호스트에 Nginx를 설치한다.")}</p>}
      <div className="pipe-controls">
        <label className="field"><span>{t("Listen address", "수신 주소")}</span><input aria-label={t("Listen address", "수신 주소")} value={draft.listen} disabled={busy} onChange={event => change({ ...draft, listen: event.target.value })} /></label>
        <button className="btn primary" type="button" disabled={saveDisabled} onClick={() => void save()}>{busy ? t("Saving…", "저장 중…") : t("Save", "저장")}</button>
        <button className="btn" type="button" disabled={busy || !snapshot || !snapshot.units.some(unit => unit.activeState === "active")} onClick={stop}>{t("Stop", "중지")}</button>
        <span className="poll-note">{hasUnsavedChanges ? t("Unsaved changes", "저장하지 않은 변경") : t("Saved configuration", "저장된 설정")}</span>
      </div>
    </section>

    <CloudflareConnect config={draft} snapshot={snapshot} busy={busy} apps={apps} vmsLoading={vmsLoading} vmsUnavailable={vmsUnavailable}
      onBusy={value => { mutating.current = value; setBusy(value); revision.current++; }}
      onConnected={next => { setSnapshot(next); setDraft(next.config ?? draft); setDirty(false); setCredentials(""); setError(null); }} />

    <section className="panel" aria-label={t("Routes", "라우트")}>
      <h2 className="panel-title">{t("Routes", "라우트")}</h2>
      <p className="pipe-description">{t("Routes send requests to apps based on the domain and URL path. Select multiple apps to distribute requests between them.", "도메인과 URL 경로에 따라 요청을 앱으로 전달한다. 앱을 여러 개 선택하면 요청을 나누어 처리한다.")}</p>
      {hasUnsavedChanges && <div className="pipe-unsaved" role="status">{t("You have unsaved changes.", "저장하지 않은 변경이 있다.")}</div>}
      <form className="create-grid pipe-route-form" onSubmit={save}>
        <div className="field"><label htmlFor="pipe-domain">{t("Domain", "도메인")}</label><input id="pipe-domain" placeholder="app.example.com" value={host} onChange={event => setHost(event.target.value)} required={editorPending} disabled={busy} /></div>
        <div className="field"><label htmlFor="pipe-path">{t("Path prefix", "경로 시작")}</label><input id="pipe-path" placeholder="/api" value={path} onChange={event => setPath(event.target.value)} required pattern="/.*" disabled={busy} /></div>
        <PipeAppPicker id="pipe-apps" label={t("Apps", "앱 선택")} apps={apps} selected={upstreams} preserved={preserved} loading={vmsLoading} unavailable={vmsUnavailable} disabled={busy} onChange={setUpstreams} />
        <div className="field field-submit"><label>&nbsp;</label><button className="btn primary" type="submit" disabled={saveDisabled || !hasUnsavedChanges}>{busy ? t("Saving…", "저장 중…") : t("Save", "저장")}</button>{editorPending && <button type="button" className="btn" disabled={busy} onClick={clearEditor}>{t("Cancel", "취소")}</button>}</div>
      </form>
      {draft.routes.length > 0 ? <div className="table-scroll"><table className="vm-table pipe-route-table"><thead><tr><th>{t("Domain", "도메인")}</th><th>{t("Path", "경로")}</th><th>{t("App addresses", "앱 주소")}</th><th>{t("Actions", "작업")}</th></tr></thead><tbody>
        {draft.routes.map((route, index) => <tr key={`${index}:${route.host}:${route.pathPrefix}`}><td>{route.host}</td><td>{route.pathPrefix}</td><td className="pipe-addresses">{route.upstreams.map(address => <div key={address}>{address}</div>)}</td><td><div className="pipe-row-actions"><button className="btn" type="button" disabled={busy} onClick={() => edit(route, index)}>{t("Edit", "수정")}</button><button className="btn" type="button" disabled={busy} onClick={() => { change({ ...draft, routes: draft.routes.filter((_, row) => row !== index) }); clearEditor(); }}>{t("Remove", "제거")}</button></div></td></tr>)}
      </tbody></table></div> : <div className="empty">{t("No routes. Add a domain, path and app addresses.", "라우트가 없다. 도메인, 경로, 앱 주소를 추가한다.")}</div>}
    </section>

    <section className="panel" aria-label="Cloudflare Tunnel">
      <h2 className="panel-title">{t("Advanced tunnel configuration", "고급 터널 설정")}</h2>
      <details className="cloudflare-help"><summary>{t("Open existing tunnel settings", "기존 터널 설정 열기")}</summary>
      <p className="poll-note">{t("Use this only for an existing locally-managed tunnel. New connections can use the guided setup above.", "기존 로컬 관리 터널을 연결할 때 사용한다. 새 연결은 위의 간편 연결을 사용한다.")}</p>
      <label className="checkbox-line"><input type="checkbox" checked={draft.tunnel !== null} disabled={busy} onChange={event => { setCredentials(""); change({ ...draft, tunnel: event.target.checked ? { id: "", credentialsFile: "credentials.json" } : null }); }} />{t("Enable public access through Cloudflare", "Cloudflare로 외부 접속 사용")}</label>
      {draft.tunnel && <div className="pipe-tunnel-fields">
        <div className="field"><label htmlFor="pipe-tunnel-id">Tunnel UUID</label><input id="pipe-tunnel-id" value={draft.tunnel.id} disabled={busy} onChange={event => change({ ...draft, tunnel: { ...draft.tunnel!, id: event.target.value.trim() } })} /></div>
        <div className="field"><label htmlFor="pipe-credentials">{t("Credentials JSON", "인증 정보 JSON")}</label><textarea id="pipe-credentials" className="pipe-credentials" autoComplete="off" spellCheck={false} value={credentials} disabled={busy} onChange={event => { setCredentials(event.target.value); setDirty(true); }} placeholder={t("Paste the tunnel credentials. Leave blank to keep saved credentials.", "터널 인증 정보를 입력한다. 비워 두면 저장한 인증 정보를 유지한다.")} /><span className="poll-note">{t("Credentials are cleared after saving and are never returned to the dashboard.", "저장 후 입력을 지운다. 저장한 인증 정보는 화면으로 반환하지 않는다.")}</span></div>
        {!snapshot?.cloudflaredAvailable && <p className="field-error">{t("Install the connector with firecrab pipe install.", "firecrab pipe install로 연결 프로그램을 설치한다.")}</p>}
      </div>}
      </details>
    </section>
  </div>;
}
