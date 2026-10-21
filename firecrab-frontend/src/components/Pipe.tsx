import { Fragment, useEffect, useRef, useState, type FormEvent } from "react";
import { applyPipe, controlPipeService, getPipe, type PipeConfig, type PipeRoute, type PipeService, type PipeServiceAction, type PipeSnapshot } from "../api/pipe";
import { useI18n } from "../i18n";
import CloudflareConnect from "./CloudflareConnect";
import NamedCloudflareTunnel from "./NamedCloudflareTunnel";
import FirecrabTunnel from "./FirecrabTunnel";
import type { VmResponse } from "../bindings";
import { canUsePipeApps, pipeApps } from "../pipeApps";
import PipeAppPicker from "./PipeAppPicker";
import PipeServiceMenu from "./PipeServiceMenu";

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
  const [listenInput, setListenInput] = useState<string | null>(null);
  const [host, setHost] = useState("");
  const [path, setPath] = useState("/");
  const [upstreams, setUpstreams] = useState<string[]>([]);
  const [editing, setEditing] = useState<number | "new" | null>(null);
  const [quickDirty, setQuickDirty] = useState(false);
  const [namedDirty, setNamedDirty] = useState(false);
  const [firecrabDirty, setFirecrabDirty] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [dirty, setDirty] = useState(false);
  const [busy, setBusy] = useState(false);
  const initialized = useRef(false);
  const revision = useRef(0);
  const mutating = useRef(false);
  const apps = pipeApps(vms);
  const editorOpen = editing !== null;
  const preserved = typeof editing === "number" ? draft.routes[editing]?.upstreams ?? [] : [];
  const appsReady = !vmsLoading && !vmsUnavailable && canUsePipeApps(apps, upstreams, preserved);
  const editorPending = typeof editing === "number" || !!host.trim() || path !== "/" || upstreams.length > 0;
  const hasUnsavedChanges = dirty || editorPending || quickDirty || namedDirty || firecrabDirty || (listenInput !== null && listenInput !== draft.listen);
  const saveDisabled = busy || !snapshot?.nginxAvailable || (listenInput !== null && !listenInput.trim()) || (editorOpen && (!host.trim() || !path.startsWith("/") || !appsReady)) || ((draft.tunnel !== null || !!draft.quickTunnel || !!snapshot?.config?.quickTunnels?.length) && !snapshot.cloudflaredAvailable);

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
    let config = { ...draft, tunnel: snapshot?.config?.tunnel ?? null, quickTunnel: snapshot?.config?.quickTunnel ?? null, quickTunnels: snapshot?.config?.quickTunnels ?? [] };
    if (listenInput !== null) config = { ...config, listen: listenInput.trim() };
    if (editorOpen) {
      if (!host.trim() || !path.startsWith("/") || !appsReady) { setError(t("Enter a domain and path, then select at least one app.", "도메인과 경로를 입력하고 앱을 하나 이상 선택한다.")); return; }
      const route = { host: host.trim().toLowerCase(), pathPrefix: path.trim() || "/", upstreams };
      const duplicate = draft.routes.some((existing, index) => index !== editing && existing.host === route.host && existing.pathPrefix.replace(/\/+$/, "") === route.pathPrefix.replace(/\/+$/, ""));
      if (duplicate) { setError(t("This domain and path already exist.", "이 도메인과 경로가 이미 있다.")); return; }
      config = { ...config, routes: editing === "new" ? [...draft.routes, route] : draft.routes.map((existing, index) => index === editing ? route : existing) };
    }
    mutating.current = true; setBusy(true); revision.current++; setError(null); setNotice(null);
    try {
      const next = await applyPipe(config, null);
      setSnapshot(next); setDraft(next.config ?? config); setDirty(false); setListenInput(null); clearEditor();
      setNotice(t("Routes saved.", "라우트를 저장했다."));
    } catch (failure) { setError((failure as Error).message); }
    finally { mutating.current = false; setBusy(false); revision.current++; }
  }
  async function control(service: PipeService, action: PipeServiceAction) {
    if (mutating.current) return;
    mutating.current = true; setBusy(true); revision.current++; setError(null); setNotice(null);
    const name = service === "ingress" ? "Nginx" : "Cloudflare Tunnel";
    try {
      setSnapshot(await controlPipeService(service, action));
      const messages = {
        start: t(`${name} start requested.`, `${name} 시작을 요청했다.`),
        stop: t(`${name} stop requested.`, `${name} 중지를 요청했다.`),
        restart: t(`${name} restart requested.`, `${name} 재시작을 요청했다.`),
      };
      setNotice(messages[action]);
    } catch (failure) { setError((failure as Error).message); }
    finally { mutating.current = false; setBusy(false); revision.current++; }
  }

  return <div className="stack">
    <section className="panel" aria-label="Pipe">
      <h2 className="panel-title"><span>Pipe</span><span className="poll-note">{t("Nginx routing and load balancing", "Nginx 요청 전달과 요청 분산")}</span></h2>
      <p className="pipe-description">{t("Public domain or Quick Tunnel URL → Nginx → VM apps. Nginx routes requests by domain and path and distributes them between selected apps.", "공개 도메인 또는 Quick Tunnel 주소 → Nginx → VM 앱. Nginx가 도메인과 경로에 따라 요청을 전달하고 선택한 앱으로 분산한다.")}</p>
      {error && <div className="banner error" role="alert">{error}</div>}
      {notice && <div className="banner info" role="status">{notice}</div>}
      <div className="table-scroll pipe-status-scroll"><table className="vm-table pipe-status-table"><thead><tr><th>{t("Service", "서비스")}</th><th>{t("State", "상태")}</th><th>PID</th><th className="actions">{t("Actions", "작업")}</th></tr></thead><tbody>
        {["firecrab-ingress.service", ...(snapshot?.config?.tunnel ? ["firecrab-tunnel.service"] : [])].map(unit => {
          const status = snapshot?.units.find(value => value.unit === unit);
          const state = status?.activeState ?? "unknown";
          const isNginx = unit === "firecrab-ingress.service";
          const service = isNginx ? "ingress" : "tunnel";
          const name = isNginx ? "Nginx" : "Cloudflare Tunnel";
          const changing = ["activating", "deactivating", "reloading"].includes(state);
          const configured = isNginx ? !!snapshot?.config : !!(snapshot?.config?.tunnel || snapshot?.config?.quickTunnel);
          const available = isNginx ? snapshot?.nginxAvailable : snapshot?.cloudflaredAvailable;
          const ready = configured && available && status?.loadState === "loaded";
          return <Fragment key={unit}>
            <tr>
              <td>{isNginx ? "Nginx" : "Cloudflare Tunnel"}</td>
              <td><span className={`state-badge${state === "active" ? " running" : state === "failed" ? " error" : ""}`}>{status ? state : t("Unavailable", "확인 불가")}</span></td>
              <td>{status?.mainPid || "—"}</td>
              <td className="actions"><PipeServiceMenu label={t(`${name} actions`, `${name} 작업`)} disabled={busy || !snapshot || changing} items={[
                ...(isNginx ? [{ label: t("Edit", "수정"), disabled: listenInput !== null, onSelect: () => setListenInput(draft.listen) }] : []),
                { label: t("Start", "시작"), disabled: !ready || state === "active", onSelect: () => void control(service, "start") },
                { label: t("Stop", "중지"), disabled: status?.loadState !== "loaded" || !["active", "failed"].includes(state), danger: true, onSelect: () => void control(service, "stop") },
                { label: t("Restart", "재시작"), disabled: !ready, onSelect: () => void control(service, "restart") },
              ]} /></td>
            </tr>
            {isNginx && listenInput !== null && <tr className="pipe-nginx-editor-row"><td colSpan={4}>
              <form id="pipe-listen-editor" className="pipe-nginx-editor" aria-labelledby="pipe-nginx-title" onSubmit={save}>
                <h3 id="pipe-nginx-title">{t("Nginx settings", "Nginx 설정")}</h3>
                <p className="pipe-description">{t("Set the address where Nginx receives requests inside the management VM.", "관리 VM 안에서 Nginx가 요청을 받을 주소를 설정한다.")}</p>
                <div className="pipe-controls">
                  <label className="field"><span>{t("Listen address", "수신 주소")}</span><input aria-label={t("Listen address", "수신 주소")} value={listenInput} disabled={busy} onChange={event => setListenInput(event.target.value)} autoFocus /></label>
                  <button className="btn primary" type="submit" disabled={saveDisabled}>{busy ? t("Saving…", "저장 중…") : t("Save", "저장")}</button>
                  <button className="btn" type="button" disabled={busy} onClick={() => setListenInput(null)}>{t("Cancel", "취소")}</button>
                </div>
              </form>
            </td></tr>}
          </Fragment>;
        })}
      </tbody></table></div>
      {snapshot && !snapshot.nginxAvailable && <p className="field-error">{t("Install Nginx on this host before applying routes.", "라우트를 적용하기 전에 이 호스트에 Nginx를 설치한다.")}</p>}
    </section>

    <FirecrabTunnel snapshot={snapshot} busy={busy} apps={apps} vmsLoading={vmsLoading} vmsUnavailable={vmsUnavailable}
      onBusy={value => { mutating.current = value; setBusy(value); revision.current++; }} onUnsavedChange={setFirecrabDirty}
      onConnected={next => {
        setSnapshot(next);
        if (next.config) {
          const { tunnel, quickTunnel, quickTunnels } = next.config;
          setDraft(current => ({ ...current, tunnel, quickTunnel, quickTunnels }));
        }
        setError(null);
      }} />

    <CloudflareConnect config={snapshot?.config ?? EMPTY} snapshot={snapshot} busy={busy} apps={apps} vmsLoading={vmsLoading} vmsUnavailable={vmsUnavailable}
      onBusy={value => { mutating.current = value; setBusy(value); revision.current++; }} onUnsavedChange={setQuickDirty}
      onConnected={next => {
        setSnapshot(next);
        if (next.config) {
          const { tunnel, quickTunnel, quickTunnels } = next.config;
          setDraft(current => ({ ...current, tunnel, quickTunnel, quickTunnels }));
        }
        setError(null);
      }} />

    <NamedCloudflareTunnel config={snapshot?.config ?? EMPTY} snapshot={snapshot} busy={busy}
      onBusy={value => { mutating.current = value; setBusy(value); revision.current++; }} onUnsavedChange={setNamedDirty}
      onConnected={next => {
        setSnapshot(next);
        if (next.config) {
          const { tunnel, quickTunnel, quickTunnels } = next.config;
          setDraft(current => ({ ...current, tunnel, quickTunnel, quickTunnels }));
        }
        setError(null);
      }} />

    <section className="panel" aria-label={t("Routes", "라우트")}>
      <h2 className="panel-title"><span>{t("Routes", "라우트")}</span><span className="pipe-row-actions">
        {!editorOpen && dirty && <button className="btn primary" type="button" disabled={saveDisabled} onClick={() => void save()}>{busy ? t("Saving…", "저장 중…") : t("Save", "저장")}</button>}
        <button className="btn" type="button" disabled={busy || editorOpen} aria-expanded={editorOpen} aria-controls="pipe-route-editor" onClick={() => setEditing("new")}>{t("Add route", "라우트 추가")}</button>
      </span></h2>
      <p className="pipe-description">{t("Routes send requests to apps based on the domain and URL path. Select multiple apps to distribute requests between them.", "도메인과 URL 경로에 따라 요청을 앱으로 전달한다. 앱을 여러 개 선택하면 요청을 나누어 처리한다.")}</p>
      <p className="pipe-domain-note">{t("For a public domain, enter the full hostname, such as my-app.firecrab.dev. Direct access requires DNS and HTTPS to reach Nginx. Use Cloudflare Tunnel above to connect a private Nginx listener.", "공개 도메인은 my-app.firecrab.dev처럼 전체 이름을 입력한다. 직접 접속에는 Nginx로 이어지는 DNS와 HTTPS 설정이 필요하다. 내부 Nginx는 위 Cloudflare Tunnel에서 공개할 수 있다.")}</p>
      {editorOpen && <form id="pipe-route-editor" className="create-grid pipe-route-form" onSubmit={save}>
        <div className="field"><label htmlFor="pipe-domain">{t("Domain", "도메인")}</label><input id="pipe-domain" placeholder="app.example.com" value={host} onChange={event => setHost(event.target.value)} required disabled={busy} autoFocus /></div>
        <div className="field"><label htmlFor="pipe-path">{t("Path prefix", "경로 시작")}</label><input id="pipe-path" placeholder="/api" value={path} onChange={event => setPath(event.target.value)} required pattern="/.*" disabled={busy} /></div>
        <PipeAppPicker id="pipe-apps" label={t("Apps", "앱 선택")} apps={apps} selected={upstreams} preserved={preserved} loading={vmsLoading} unavailable={vmsUnavailable} disabled={busy} onChange={setUpstreams} />
        <div className="field field-submit"><label>&nbsp;</label><button className="btn primary" type="submit" disabled={saveDisabled || !hasUnsavedChanges}>{busy ? t("Saving…", "저장 중…") : t("Save", "저장")}</button><button type="button" className="btn" disabled={busy} onClick={clearEditor}>{t("Cancel", "취소")}</button></div>
      </form>}
      {draft.routes.length > 0 ? <div className="table-scroll"><table className="vm-table pipe-route-table"><thead><tr><th>{t("Domain", "도메인")}</th><th>{t("Path", "경로")}</th><th>{t("App addresses", "앱 주소")}</th><th>{t("Actions", "작업")}</th></tr></thead><tbody>
        {draft.routes.map((route, index) => <tr key={`${index}:${route.host}:${route.pathPrefix}`}><td>{route.host}</td><td>{route.pathPrefix}</td><td className="pipe-addresses">{route.upstreams.map(address => <div key={address}>{address}</div>)}</td><td><div className="pipe-row-actions"><button className="btn" type="button" disabled={busy} onClick={() => edit(route, index)}>{t("Edit", "수정")}</button><button className="btn" type="button" disabled={busy} onClick={() => { change({ ...draft, routes: draft.routes.filter((_, row) => row !== index) }); clearEditor(); }}>{t("Remove", "제거")}</button></div></td></tr>)}
      </tbody></table></div> : <div className="empty">{t("No routes. Add a domain, path and app addresses.", "라우트가 없다. 도메인, 경로, 앱 주소를 추가한다.")}</div>}
    </section>


  </div>;
}
