import { useEffect, useRef, useState } from "react";
import { deleteFirecrabAddress, saveFirecrabAddress, type FirecrabAddress, type FirecrabTarget, type PipeSnapshot } from "../api/pipe";
import { useI18n } from "../i18n";
import { canUsePipeApps, type PipeApp } from "../pipeApps";
import PipeAppPicker from "./PipeAppPicker";
import PipeCopyButton from "./PipeCopyButton";
import PipeServiceMenu from "./PipeServiceMenu";

interface Props {
  snapshot: PipeSnapshot | null;
  busy: boolean;
  apps: PipeApp[];
  vmsLoading: boolean;
  vmsUnavailable: boolean;
  onBusy: (value: boolean) => void;
  onConnected: (snapshot: PipeSnapshot) => void;
  onUnsavedChange: (value: boolean) => void;
}
interface Editor { original: string | null; token: string; kind: "apps" | "nginx"; source: string; addresses: string[] }

export default function FirecrabTunnel({ snapshot, busy, apps, vmsLoading, vmsUnavailable, onBusy, onConnected, onUnsavedChange }: Props) {
  const { t } = useI18n();
  const [editor, setEditor] = useState<Editor | null>(null);
  const [error, setError] = useState<string | null>(null);
  const active = useRef(false);
  const addresses = snapshot?.firecrabTunnels ?? [];
  const hosts = [...new Set(snapshot?.config?.routes.map(route => route.host) ?? [])];
  const original = addresses.find(address => address.subdomain === editor?.original);
  const originalTarget = original ? addressTarget(original) : null;
  const preserved = originalTarget?.type === "apps" ? originalTarget.upstreams : [];
  const draftTarget = editor ? (editor.kind === "apps" ? { type: "apps", upstreams: editor.addresses } : { type: "nginx", host: editor.source }) : null;
  const changed = !!editor && (editor.original
    ? JSON.stringify(draftTarget) !== JSON.stringify(originalTarget)
    : !!editor.token || !!editor.source || editor.addresses.length > 0);
  const sourceValid = !!editor && (editor.kind === "apps" ? !vmsLoading && !vmsUnavailable && canUsePipeApps(apps, editor.addresses, preserved) : hosts.includes(editor.source));
  const valid = !!editor && (!!editor.original || /^[a-fA-F0-9]{64}$/.test(editor.token.trim())) && sourceValid;
  useEffect(() => { onUnsavedChange(changed); return () => onUnsavedChange(false); }, [changed, onUnsavedChange]);

  async function mutate(request: () => Promise<PipeSnapshot>) {
    if (active.current || busy) return;
    active.current = true; onBusy(true); setError(null);
    try { onConnected(await request()); setEditor(null); }
    catch (failure) { setError((failure as Error).message); }
    finally { active.current = false; onBusy(false); }
  }
  function edit(address: FirecrabAddress) {
    const selected = addressTarget(address);
    setEditor({ original: address.subdomain, token: "", kind: selected?.type === "nginx" ? "nginx" : "apps", source: selected?.type === "nginx" ? selected.host : "", addresses: selected?.type === "apps" ? [...selected.upstreams] : [] });
    setError(null);
  }
  function addressTarget(address: FirecrabAddress): FirecrabTarget | null {
    if (address.target.type !== "quick_tunnel") return address.target;
    const id = address.target.id;
    const saved = snapshot?.quickTunnels?.find(tunnel => tunnel.id === id)?.target;
    if (!saved) return null;
    return saved.type === "apps" ? saved : { type: "nginx", host: saved.host };
  }
  function appLabel(address: string) {
    const app = apps.find(value => value.address === address);
    return app?.port ? app.name + " · TCP " + app.port : address;
  }
  function target(): FirecrabTarget {
    return editor!.kind === "apps" ? { type: "apps", upstreams: editor!.addresses } : { type: "nginx", host: editor!.source };
  }
  return <section className="panel" aria-label="Firecrab Tunnel">
    <h2 className="panel-title"><span className="panel-title-with-icon"><img className="panel-brand-icon" src="/firecrab.png" alt="" width={28} height={28} />Firecrab Tunnel</span><button className="btn" type="button" disabled={busy || !!editor || !snapshot || addresses.length >= 16} aria-expanded={!!editor} aria-controls="firecrab-address-editor" onClick={() => { setEditor({ original: null, token: "", kind: "apps", source: "", addresses: [] }); setError(null); }}>{t("Add fixed address", "고정 주소 추가")}</button></h2>
    <p className="pipe-description">{t("Use a dashboard token to connect VM apps or saved Nginx routes to your fixed address.", "대시보드에서 발급한 토큰으로 VM 앱 또는 저장된 Nginx 라우트를 고정 주소에 연결한다.")}</p>
    <p className="pipe-description"><a href="https://firecrab.dev/dashboard/" target="_blank" rel="noreferrer">{t("Get a token from the dashboard", "대시보드에서 토큰 발급")}</a></p>
    {error && <div className="banner error" role="alert">{error}</div>}
    {editor && <form id="firecrab-address-editor" className="create-grid quick-tunnel-form" onSubmit={event => { event.preventDefault(); if (valid) void mutate(() => saveFirecrabAddress(editor.original ? { subdomain: editor.original } : { token: editor.token.trim() }, target())); }}>
      {!editor.original ? <label className="field"><span>{t("Connection token", "연결 토큰")}</span><input type="password" aria-label={t("Connection token", "연결 토큰")} value={editor.token} maxLength={64} required autoComplete="off" spellCheck={false} disabled={busy} onChange={event => setEditor({ ...editor, token: event.target.value })} autoFocus /><span className="poll-note">{t("Paste the token issued by the Firecrab dashboard.", "Firecrab 대시보드에서 발급한 토큰을 붙여 넣는다.")}</span></label>
        : <p className="poll-note">{t("Uses the token already saved on this host.", "이 호스트에 저장한 토큰을 사용한다.")}</p>}
      <label className="field"><span>{t("Connection target", "연결 대상")}</span><select aria-label={t("Connection target", "연결 대상")} value={editor.kind} disabled={busy} onChange={event => setEditor({ ...editor, kind: event.target.value as Editor["kind"], source: "", addresses: [] })}>
        <option value="apps">{t("VM apps", "VM 앱")}</option>
        <option value="nginx">{t("Nginx routes", "Nginx 라우트")}</option>
      </select></label>
      {editor.kind === "apps" && <PipeAppPicker id="firecrab-apps" label={t("Public apps", "공개할 앱 선택")} apps={apps} selected={editor.addresses} preserved={preserved} loading={vmsLoading} unavailable={vmsUnavailable} disabled={busy} onChange={addresses => setEditor({ ...editor, addresses })} />}
      {editor.kind === "nginx" && <label className="field"><span>{t("Nginx route domain", "Nginx 라우트 도메인")}</span><select aria-label={t("Nginx route domain", "Nginx 라우트 도메인")} value={editor.source} disabled={busy || !hosts.length} required onChange={event => setEditor({ ...editor, source: event.target.value })}><option value="">{t("Select saved routes", "저장된 라우트 선택")}</option>{hosts.map(host => <option key={host} value={host}>{host}</option>)}</select><span className="poll-note">{t("Publishes every saved path. The private Nginx connection is managed automatically.", "저장된 모든 경로를 공개한다. 사설 Nginx의 연결 프로그램은 자동으로 관리한다.")}</span></label>}
      <div className="pipe-controls quick-tunnel-submit"><button className="btn primary" type="submit" disabled={busy || !valid}>{busy ? t("Connecting…", "연결 중…") : t("Connect address", "주소 연결")}</button><button className="btn" type="button" disabled={busy} onClick={() => setEditor(null)}>{t("Cancel", "취소")}</button></div>
    </form>}
    {!snapshot ? <p className="poll-note">{t("Loading addresses…", "주소를 불러오는 중이다.")}</p> : !addresses.length ? <div className="empty">{t("No fixed addresses. Add a connection token and choose its target.", "고정 주소가 없다. 연결 토큰을 입력하고 대상을 선택한다.")}</div> : <div className="table-scroll"><table className="vm-table quick-tunnel-table"><thead><tr><th>{t("Address", "주소")}</th><th>{t("Target", "연결 대상")}</th><th>{t("State", "상태")}</th><th className="actions">{t("Actions", "작업")}</th></tr></thead><tbody>{addresses.map(address => { const selected = addressTarget(address); return <tr key={address.subdomain}>
      <td><div className="pipe-url"><a href={address.publicUrl} target="_blank" rel="noreferrer">{address.publicUrl}</a><PipeCopyButton url={address.publicUrl} onError={setError} /></div></td>
      <td>{selected?.type === "nginx" ? <>{t("Nginx routes", "Nginx 라우트")}<div className="quick-tunnel-target">{selected.host}</div></> : selected?.type === "apps" ? <>{t("VM apps", "VM 앱")}{selected.upstreams.map(address => <div className="quick-tunnel-target" key={address}>{appLabel(address)}</div>)}</> : t("Unavailable", "확인 불가")}</td>
      <td><span className={"state-badge" + (address.connected ? " running" : "")}>{address.connected ? t("Connected", "연결됨") : t("Pending", "연결 대기")}</span>{address.error && <div className="field-error">{address.error}</div>}</td>
      <td className="actions"><PipeServiceMenu label={address.subdomain + " " + t("actions", "작업")} disabled={busy} items={[{ label: t("Edit", "수정"), disabled: !!editor, onSelect: () => edit(address) }, { label: t("Delete", "삭제"), danger: true, onSelect: () => void mutate(() => deleteFirecrabAddress(address.subdomain)) }]} /></td>
    </tr>; })}</tbody></table></div>}
  </section>;
}
