import { useEffect, useRef, useState } from "react";
import { controlQuickTunnel, deleteQuickTunnel, saveQuickTunnel, type PipeConfig, type PipeSnapshot, type QuickTunnelSnapshot, type QuickTunnelTarget } from "../api/pipe";
import { useI18n } from "../i18n";
import { canUsePipeApps, type PipeApp } from "../pipeApps";
import PipeAppPicker from "./PipeAppPicker";
import PipeServiceMenu from "./PipeServiceMenu";
import PipeCopyButton from "./PipeCopyButton";

interface Props {
  config: PipeConfig;
  snapshot: PipeSnapshot | null;
  busy: boolean;
  apps: PipeApp[];
  vmsLoading: boolean;
  vmsUnavailable: boolean;
  onBusy: (busy: boolean) => void;
  onConnected: (snapshot: PipeSnapshot) => void;
  onUnsavedChange: (unsaved: boolean) => void;
}

interface Editor {
  id: string | null;
  name: string;
  kind: "apps" | "routes";
  addresses: string[];
  host: string;
}

function target(editor: Editor): QuickTunnelTarget {
  return editor.kind === "apps" ? { type: "apps", upstreams: editor.addresses } : { type: "routes", host: editor.host };
}

export default function CloudflareConnect({ config, snapshot, busy, apps, vmsLoading, vmsUnavailable, onBusy, onConnected, onUnsavedChange }: Props) {
  const { t } = useI18n();
  const [editor, setEditor] = useState<Editor | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const running = useRef(false);
  const tunnels = snapshot?.quickTunnels ?? [];
  const hosts = [...new Set(config.routes.map(route => route.host))];
  const original = tunnels.find(tunnel => tunnel.id === editor?.id);
  const preserved = original?.target.type === "apps" ? original.target.upstreams : [];
  const changed = !!editor && (editor.id === null
    ? !!editor.name.trim() || editor.addresses.length > 0 || !!editor.host
    : !original || editor.name !== original.name || JSON.stringify(target(editor)) !== JSON.stringify(original.target));
  const ready = !!editor && !!editor.name.trim() && (editor.kind === "routes"
    ? hosts.includes(editor.host)
    : !vmsLoading && !vmsUnavailable && canUsePipeApps(apps, editor.addresses, preserved));
  const migrating = editor?.id === "00000000-0000-0000-0000-000000000001" && !!config.quickTunnel;
  const saveDisabled = busy || !snapshot?.nginxAvailable || !ready || (!!editor?.id && !changed && !migrating);

  useEffect(() => {
    onUnsavedChange(changed);
    return () => onUnsavedChange(false);
  }, [changed, onUnsavedChange]);

  function appLabel(address: string) {
    const app = apps.find(value => value.address === address);
    return app?.port ? app.name + " · TCP " + app.port : address;
  }

  function edit(tunnel: QuickTunnelSnapshot) {
    setError(null); setNotice(null);
    setEditor({ id: tunnel.id, name: tunnel.name, kind: tunnel.target.type,
      addresses: tunnel.target.type === "apps" ? [...tunnel.target.upstreams] : [],
      host: tunnel.target.type === "routes" ? tunnel.target.host : "" });
  }

  async function mutate(request: () => Promise<PipeSnapshot>, done?: () => void) {
    if (busy || running.current) return;
    running.current = true; onBusy(true); setError(null); setNotice(null);
    try { onConnected(await request()); done?.(); }
    catch (failure) { setError((failure as Error).message); }
    finally { running.current = false; onBusy(false); }
  }

  function save() {
    if (!editor || saveDisabled) return;
    void mutate(() => saveQuickTunnel({ id: editor.id, name: editor.name.trim(), target: target(editor) }), () => {
      setEditor(null); setNotice(t("Tunnel saved. Its URL appears when issued.", "터널을 저장했다. 주소가 발급되면 목록에 표시한다."));
    });
  }


  return <section className="panel cloudflare-connect" aria-label="Quick Tunnel">
    <h2 className="panel-title"><span className="panel-title-with-icon">
      <img className="panel-brand-icon" src="/cloudflare.svg" alt="" width={28} height={28} />Cloudflare Quick Tunnel
    </span><button className="btn" type="button" disabled={busy || !!editor || !snapshot || tunnels.length >= 16} aria-expanded={!!editor} aria-controls="quick-tunnel-editor"
      onClick={() => { setEditor({ id: null, name: "", kind: "apps", addresses: [], host: "" }); setError(null); setNotice(null); }}>{t("Add tunnel", "터널 추가")}</button></h2>
    <p className="pipe-description">{t("Publish VM apps or saved Nginx routes through temporary public URLs. Each tunnel has its own URL and service controls. Restarting a tunnel changes its URL.", "VM 앱이나 저장된 Nginx 라우트를 임시 주소로 공개한다. 터널마다 주소와 실행 상태를 따로 관리한다. 터널을 재시작하면 주소가 바뀐다.")}</p>
    {error && <div className="banner error" role="alert">{error}</div>}
    {notice && <div className="banner info" role="status">{notice}</div>}
    {editor && <form id="quick-tunnel-editor" className="create-grid quick-tunnel-form" onSubmit={event => { event.preventDefault(); save(); }}>
      <label className="field"><span>{t("Name", "이름")}</span><input aria-label={t("Name", "이름")} value={editor.name} maxLength={64} required disabled={busy} autoFocus onChange={event => setEditor({ ...editor, name: event.target.value })} /></label>
      <label className="field"><span>{t("Target", "공개 대상")}</span><select aria-label={t("Target", "공개 대상")} value={editor.kind} disabled={busy} onChange={event => setEditor({ ...editor, kind: event.target.value as Editor["kind"] })}>
        <option value="apps">{t("VM apps", "VM 앱")}</option><option value="routes">{t("Nginx routes", "Nginx 라우트")}</option>
      </select></label>
      {editor.kind === "apps" ? <PipeAppPicker id="quick-tunnel-apps" label={t("Public apps", "공개할 앱 선택")} apps={apps} selected={editor.addresses} preserved={preserved} loading={vmsLoading} unavailable={vmsUnavailable} disabled={busy} onChange={addresses => setEditor({ ...editor, addresses })} />
        : <div className="field quick-tunnel-route-target"><label htmlFor="quick-tunnel-host">{t("Route domain", "라우트 도메인")}</label>
          <select id="quick-tunnel-host" value={editor.host} required disabled={busy || !hosts.length} onChange={event => setEditor({ ...editor, host: event.target.value })}>
            <option value="">{t("Select a saved route domain", "저장된 라우트 도메인 선택")}</option>
            {hosts.map(host => <option key={host} value={host}>{host}</option>)}
            {editor.host && !hosts.includes(editor.host) && <option value={editor.host}>{editor.host}</option>}
          </select>
          <span className="poll-note">{t("Publishes all saved paths for this domain. Route changes update the tunnel's destinations without changing its URL.", "이 도메인에 저장된 모든 경로를 공개한다. 라우트를 수정하면 같은 임시 주소에서 변경된 앱으로 요청을 전달한다.")}</span>
          {!!editor.host && hosts.includes(editor.host) && <span className="quick-tunnel-paths">{config.routes.filter(route => route.host === editor.host).map(route => route.pathPrefix).join(", ")}</span>}
          {!hosts.length && <span className="field-error">{t("Save a route before selecting Nginx routes.", "Nginx 라우트를 선택하려면 먼저 라우트를 저장한다.")}</span>}
        </div>}
      {migrating && <p className="poll-note quick-tunnel-submit">{t("Saving moves this existing tunnel into the list management mode. Its temporary URL changes once.", "저장하면 기존 터널을 목록 관리 방식으로 전환한다. 임시 주소가 한 번 바뀐다.")}</p>}
      <div className="pipe-controls quick-tunnel-submit"><button className="btn primary" type="submit" disabled={saveDisabled}>
        {busy ? t("Saving…", "저장 중…") : editor.id ? t("Save", "저장") : t("Create temporary URL", "임시 주소 만들기")}
      </button><button className="btn" type="button" disabled={busy} onClick={() => setEditor(null)}>{t("Cancel", "취소")}</button></div>
    </form>}
    {tunnels.length >= 16 && <p className="poll-note">{t("You can keep up to 16 Quick Tunnels.", "Quick Tunnel은 최대 16개까지 저장한다.")}</p>}
    {!snapshot ? <p className="poll-note">{t("Loading tunnels…", "터널 목록을 불러오는 중이다.")}</p>
      : tunnels.length === 0 ? <div className="empty">{t("No Quick Tunnels. Add a tunnel to publish apps or Nginx routes.", "Quick Tunnel이 없다. 터널을 추가해 앱이나 Nginx 라우트를 공개한다.")}</div>
        : <div className="table-scroll"><table className="vm-table quick-tunnel-table"><thead><tr>
          <th>{t("Name", "이름")}</th><th>{t("Target", "공개 대상")}</th><th>{t("Public URL", "공개 주소")}</th><th>{t("State", "상태")}</th><th className="actions">{t("Actions", "작업")}</th>
        </tr></thead><tbody>{tunnels.map(tunnel => {
          const state = tunnel.unit.activeState;
          const url = state === "active" ? tunnel.publicUrl : null;
          const changing = ["activating", "deactivating", "reloading"].includes(state) && tunnel.unit.subState !== "auto-restart";
          const available = tunnel.unit.loadState === "loaded" && snapshot.cloudflaredAvailable;
          return <tr key={tunnel.id}>
            <td>{tunnel.name}</td>
            <td>{tunnel.target.type === "routes" ? <><div>{t("Nginx routes", "Nginx 라우트")}</div><div className="quick-tunnel-target">{tunnel.target.host}</div><div className="poll-note">{config.routes.filter(route => route.host === (tunnel.target.type === "routes" ? tunnel.target.host : "")).map(route => route.pathPrefix).join(", ")}</div></>
              : <><div>{t("VM apps", "VM 앱")}</div>{tunnel.target.upstreams.map(address => <div className="quick-tunnel-target" key={address}>{appLabel(address)}</div>)}</>}</td>
            <td>{url ? <div className="pipe-url"><a className="quick-tunnel-link" href={url} target="_blank" rel="noreferrer">{url}</a><PipeCopyButton url={url} onError={setError} /></div>
              : tunnel.error ? <span className="field-error">{tunnel.error}</span> : <span className="poll-note">{state === "active" || state === "activating" ? t("Preparing URL…", "주소 발급 중…") : "—"}</span>}</td>
            <td><span className={"state-badge" + (tunnel.error || state === "failed" ? " error" : state === "active" ? " running" : "")}>{state}</span></td>
            <td className="actions"><PipeServiceMenu label={t(tunnel.name + " actions", tunnel.name + " 작업")} disabled={busy || changing} items={[
              { label: t("Edit", "수정"), disabled: !!editor, onSelect: () => edit(tunnel) },
              { label: t("Start", "시작"), disabled: !available || state === "active", onSelect: () => void mutate(() => controlQuickTunnel(tunnel.id, "start")) },
              { label: t("Stop", "중지"), disabled: tunnel.unit.loadState !== "loaded" || !(["active", "failed"].includes(state) || tunnel.unit.subState === "auto-restart"), danger: true, onSelect: () => void mutate(() => controlQuickTunnel(tunnel.id, "stop")) },
              { label: t("Restart", "재시작"), disabled: !available, onSelect: () => void mutate(() => controlQuickTunnel(tunnel.id, "restart")) },
              { label: t("Delete", "삭제"), danger: true, onSelect: () => void mutate(() => deleteQuickTunnel(tunnel.id), () => { if (editor?.id === tunnel.id) setEditor(null); }) },
            ]} /></td>
          </tr>;
        })}</tbody></table></div>}
    {snapshot && !snapshot.nginxAvailable && <p className="field-error">{t("Install Nginx on the managed host before connecting.", "연결 전에 관리 호스트에 Nginx를 설치한다.")}</p>}
  </section>;
}
