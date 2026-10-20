import { useEffect, useRef, useState, type FormEvent } from "react";
import { applyPipe, controlPipeService, EMPTY_PIPE, type PipeConfig, type PipeRoute, type PipeServiceAction, type PipeSnapshot } from "../api/pipe";
import { useI18n } from "../i18n";
import { canUsePipeApps, type PipeApp } from "../pipeApps";
import PipeAppPicker from "./PipeAppPicker";
import PipeServiceMenu from "./PipeServiceMenu";

interface Props {
  snapshot: PipeSnapshot | null;
  busy: boolean;
  apps: PipeApp[];
  vmsLoading: boolean;
  vmsUnavailable: boolean;
  onBusy: (busy: boolean) => void;
  onSnapshot: (snapshot: PipeSnapshot) => void;
  onUnsavedChange: (unsaved: boolean) => void;
}

/** Nginx service status and the domain/path routes it serves, saved as one Pipe config. */
export default function PipeRouting({ snapshot, busy, apps, vmsLoading, vmsUnavailable, onBusy, onSnapshot, onUnsavedChange }: Props) {
  const { t } = useI18n();
  const [draft, setDraft] = useState<PipeConfig>(snapshot?.config ?? EMPTY_PIPE);
  const [listenInput, setListenInput] = useState<string | null>(null);
  const [host, setHost] = useState("");
  const [path, setPath] = useState("/");
  const [upstreams, setUpstreams] = useState<string[]>([]);
  const [editing, setEditing] = useState<number | "new" | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [dirty, setDirty] = useState(false);
  const initialized = useRef(snapshot !== null);
  const active = useRef(false);
  const saved = snapshot?.config;
  const editorOpen = editing !== null;
  const preserved = typeof editing === "number" ? draft.routes[editing]?.upstreams ?? [] : [];
  const appsReady = !vmsLoading && !vmsUnavailable && canUsePipeApps(apps, upstreams, preserved);
  const editorPending = typeof editing === "number" || !!host.trim() || path !== "/" || upstreams.length > 0;
  const hasUnsavedChanges = dirty || editorPending || (listenInput !== null && listenInput !== draft.listen);
  const saveDisabled = busy || !snapshot?.nginxAvailable || (listenInput !== null && !listenInput.trim()) || (editorOpen && (!host.trim() || !path.startsWith("/") || !appsReady)) || ((!!saved?.tunnel || !!saved?.quickTunnel || !!saved?.quickTunnels?.length) && !snapshot.cloudflaredAvailable);

  useEffect(() => {
    onUnsavedChange(hasUnsavedChanges);
    return () => onUnsavedChange(false);
  }, [hasUnsavedChanges, onUnsavedChange]);

  // A card opened before the first poll adopts the first saved config it sees.
  useEffect(() => {
    if (!snapshot || initialized.current) return;
    initialized.current = true;
    setDraft(snapshot.config ?? EMPTY_PIPE);
  }, [snapshot]);

  function change(next: PipeConfig) { initialized.current = true; setDraft(next); setDirty(true); setNotice(null); }
  function clearEditor() { setHost(""); setPath("/"); setUpstreams([]); setEditing(null); }
  function edit(route: PipeRoute, index: number) { setHost(route.host); setPath(route.pathPrefix); setUpstreams([...route.upstreams]); setEditing(index); }
  async function save(event?: FormEvent) {
    event?.preventDefault();
    if (active.current) return;
    let config = { ...draft, tunnel: saved?.tunnel ?? null, quickTunnel: saved?.quickTunnel ?? null, quickTunnels: saved?.quickTunnels ?? [] };
    if (listenInput !== null) config = { ...config, listen: listenInput.trim() };
    if (editorOpen) {
      if (!host.trim() || !path.startsWith("/") || !appsReady) { setError(t("Enter a domain and path, then select at least one app.", "도메인과 경로를 입력하고 앱을 하나 이상 선택한다.")); return; }
      const route = { host: host.trim().toLowerCase(), pathPrefix: path.trim() || "/", upstreams };
      const duplicate = draft.routes.some((existing, index) => index !== editing && existing.host === route.host && existing.pathPrefix.replace(/\/+$/, "") === route.pathPrefix.replace(/\/+$/, ""));
      if (duplicate) { setError(t("This domain and path already exist.", "이 도메인과 경로가 이미 있다.")); return; }
      config = { ...config, routes: editing === "new" ? [...draft.routes, route] : draft.routes.map((existing, index) => index === editing ? route : existing) };
    }
    active.current = true; onBusy(true); setError(null); setNotice(null);
    try {
      const next = await applyPipe(config, null);
      onSnapshot(next); setDraft(next.config ?? config); setDirty(false); setListenInput(null); clearEditor();
      setNotice(t("Routes saved.", "라우트를 저장했다."));
    } catch (failure) { setError((failure as Error).message); }
    finally { active.current = false; onBusy(false); }
  }
  async function control(action: PipeServiceAction) {
    if (active.current) return;
    active.current = true; onBusy(true); setError(null); setNotice(null);
    try {
      onSnapshot(await controlPipeService("ingress", action));
      const messages = {
        start: t("Nginx start requested.", "Nginx 시작을 요청했다."),
        stop: t("Nginx stop requested.", "Nginx 중지를 요청했다."),
        restart: t("Nginx restart requested.", "Nginx 재시작을 요청했다."),
      };
      setNotice(messages[action]);
    } catch (failure) { setError((failure as Error).message); }
    finally { active.current = false; onBusy(false); }
  }

  const status = snapshot?.units.find(value => value.unit === "firecrab-ingress.service");
  const state = status?.activeState ?? "unknown";
  const changing = ["activating", "deactivating", "reloading"].includes(state);
  const ready = !!saved && snapshot.nginxAvailable && status?.loadState === "loaded";
  return <section className="panel" aria-label="Route">
    <h2 className="panel-title"><span>Route</span></h2>
    {error && <div className="banner error" role="alert">{error}</div>}
    {notice && <div className="banner info" role="status">{notice}</div>}
    <div className="table-scroll pipe-status-scroll"><table className="vm-table pipe-status-table"><thead><tr><th>{t("Service", "서비스")}</th><th>{t("State", "상태")}</th><th>PID</th><th className="actions">{t("Actions", "작업")}</th></tr></thead><tbody>
      <tr>
        <td>Nginx</td>
        <td><span className={`state-badge${state === "active" ? " running" : state === "failed" ? " error" : ""}`}>{status ? state : t("Unavailable", "확인 불가")}</span></td>
        <td>{status?.mainPid || "—"}</td>
        <td className="actions"><PipeServiceMenu label={t("Nginx actions", "Nginx 작업")} disabled={busy || !snapshot || changing} items={[
          { label: t("Edit", "수정"), disabled: listenInput !== null, onSelect: () => setListenInput(draft.listen) },
          { label: t("Start", "시작"), disabled: !ready || state === "active", onSelect: () => void control("start") },
          { label: t("Stop", "중지"), disabled: status?.loadState !== "loaded" || !["active", "failed"].includes(state), danger: true, onSelect: () => void control("stop") },
          { label: t("Restart", "재시작"), disabled: !ready, onSelect: () => void control("restart") },
        ]} /></td>
      </tr>
      {listenInput !== null && <tr className="pipe-nginx-editor-row"><td colSpan={4}>
        <form id="pipe-listen-editor" className="pipe-nginx-editor" aria-labelledby="pipe-nginx-title" onSubmit={save}>
          <h3 id="pipe-nginx-title">{t("Nginx settings", "Nginx 설정")}</h3>
          <div className="pipe-controls">
            <label className="field"><span>{t("Listen address", "수신 주소")}</span><input aria-label={t("Listen address", "수신 주소")} value={listenInput} disabled={busy} onChange={event => setListenInput(event.target.value)} autoFocus /></label>
            <button className="btn primary" type="submit" disabled={saveDisabled}>{busy ? t("Saving…", "저장 중…") : t("Save", "저장")}</button>
            <button className="btn" type="button" disabled={busy} onClick={() => setListenInput(null)}>{t("Cancel", "취소")}</button>
          </div>
        </form>
      </td></tr>}
    </tbody></table></div>
    {snapshot && !snapshot.nginxAvailable && <p className="field-error">{t("Install Nginx on this host before applying routes.", "라우트를 적용하기 전에 이 호스트에 Nginx를 설치한다.")}</p>}

    <section className="pipe-subsection" aria-label={t("Routes", "라우트")}>
      <h3 className="panel-title"><span>{t("Routes", "라우트")}</span><span className="pipe-row-actions">
        {!editorOpen && dirty && <button className="btn primary" type="button" disabled={saveDisabled} onClick={() => void save()}>{busy ? t("Saving…", "저장 중…") : t("Save", "저장")}</button>}
        <button className="btn" type="button" disabled={busy || editorOpen} aria-expanded={editorOpen} aria-controls="pipe-route-editor" onClick={() => setEditing("new")}>{t("Add route", "라우트 추가")}</button>
      </span></h3>
      {editorOpen && <form id="pipe-route-editor" className="create-grid pipe-route-form" onSubmit={save}>
        <div className="field"><label htmlFor="pipe-domain">{t("Domain", "도메인")}</label><input id="pipe-domain" placeholder="app.example.com" value={host} onChange={event => setHost(event.target.value)} required disabled={busy} autoFocus /></div>
        <div className="field"><label htmlFor="pipe-path">{t("Path prefix", "경로 시작")}</label><input id="pipe-path" placeholder="/api" value={path} onChange={event => setPath(event.target.value)} required pattern="/.*" disabled={busy} /></div>
        <PipeAppPicker id="pipe-apps" label={t("Apps", "앱 선택")} apps={apps} selected={upstreams} preserved={preserved} loading={vmsLoading} unavailable={vmsUnavailable} disabled={busy} onChange={setUpstreams} />
        <div className="field field-submit"><label>&nbsp;</label><button className="btn primary" type="submit" disabled={saveDisabled || !hasUnsavedChanges}>{busy ? t("Saving…", "저장 중…") : t("Save", "저장")}</button><button type="button" className="btn" disabled={busy} onClick={clearEditor}>{t("Cancel", "취소")}</button></div>
      </form>}
      {draft.routes.length > 0 ? <div className="table-scroll"><table className="vm-table pipe-route-table"><thead><tr><th>{t("Domain", "도메인")}</th><th>{t("Path", "경로")}</th><th>{t("App addresses", "앱 주소")}</th><th>{t("Actions", "작업")}</th></tr></thead><tbody>
        {draft.routes.map((route, index) => <tr key={`${index}:${route.host}:${route.pathPrefix}`}><td>{route.host}</td><td>{route.pathPrefix}</td><td className="pipe-addresses">{route.upstreams.map(address => <div key={address}>{address}</div>)}</td><td><div className="pipe-row-actions"><button className="btn" type="button" disabled={busy} onClick={() => edit(route, index)}>{t("Edit", "수정")}</button><button className="btn" type="button" disabled={busy} onClick={() => { change({ ...draft, routes: draft.routes.filter((_, row) => row !== index) }); clearEditor(); }}>{t("Remove", "제거")}</button></div></td></tr>)}
      </tbody></table></div> : <div className="empty">{t("No routes.", "등록한 라우트가 없다.")}</div>}
    </section>
  </section>;
}
