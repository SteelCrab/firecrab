import { useEffect, useRef, useState } from "react";
import { connectCloudflare, controlPipeService, disconnectCloudflare, getCloudflareZones, type CloudflareZone, type PipeConfig, type PipeServiceAction, type PipeSnapshot } from "../api/pipe";
import { useI18n } from "../i18n";
import PipeCopyButton from "./PipeCopyButton";
import PipeServiceMenu from "./PipeServiceMenu";

interface Props {
  config: PipeConfig;
  snapshot: PipeSnapshot | null;
  busy: boolean;
  onBusy: (busy: boolean) => void;
  onConnected: (snapshot: PipeSnapshot) => void;
  onUnsavedChange: (dirty: boolean) => void;
}

export default function NamedCloudflareTunnel({ config, snapshot, busy, onBusy, onConnected, onUnsavedChange }: Props) {
  const { t } = useI18n();
  const [token, setToken] = useState("");
  const [zones, setZones] = useState<CloudflareZone[]>([]);
  const [zoneId, setZoneId] = useState("");
  const [host, setHost] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const active = useRef(false);
  const zone = zones.find(value => value.id === zoneId);
  const hosts = [...new Set(config.routes.map(route => route.host))].filter(host => zone && (host === zone.name || host.endsWith("." + zone.name)));
  const configured = config.tunnel;
  const urls = configured?.hostname ? [configured.hostname] : configured ? [...new Set(config.routes.map(route => route.host))] : [];
  const unit = snapshot?.units.find(unit => unit.unit === "firecrab-tunnel.service");
  const state = unit?.activeState;
  const changing = ["activating", "deactivating", "reloading"].includes(state ?? "");
  const ready = !!snapshot?.cloudflaredAvailable && unit?.loadState === "loaded";
  const changed = !configured && !!token;
  useEffect(() => { onUnsavedChange(changed); return () => onUnsavedChange(false); }, [changed, onUnsavedChange]);

  function clear() { setToken(""); setZones([]); setZoneId(""); setHost(""); setError(null); setNotice(null); }
  async function load() {
    if (active.current || busy || !token.trim()) return;
    active.current = true; onBusy(true); setLoading(true); setError(null);
    try { const zones = await getCloudflareZones(token.trim()); setZones(zones); setZoneId(zones[0]?.id ?? ""); setHost(""); }
    catch (failure) { setError((failure as Error).message); setZones([]); setZoneId(""); }
    finally { active.current = false; onBusy(false); setLoading(false); }
  }
  async function connect() {
    if (active.current || busy || config.quickTunnel || !snapshot?.nginxAvailable || !zone || !hosts.includes(host)) return;
    active.current = true; onBusy(true); setError(null);
    try { onConnected(await connectCloudflare(token.trim(), zone.id, host, config)); clear(); }
    catch (failure) { setError((failure as Error).message); }
    finally { active.current = false; onBusy(false); }
  }
  async function control(action: PipeServiceAction) {
    if (active.current || busy) return;
    active.current = true; onBusy(true); setError(null); setNotice(null);
    try {
      onConnected(await controlPipeService("tunnel", action));
      const messages = {
        start: t("Cloudflare Tunnel start requested.", "Cloudflare Tunnel 시작을 요청했다."),
        stop: t("Cloudflare Tunnel stop requested.", "Cloudflare Tunnel 중지를 요청했다."),
        restart: t("Cloudflare Tunnel restart requested.", "Cloudflare Tunnel 재시작을 요청했다."),
      };
      setNotice(messages[action]);
    } catch (failure) { setError((failure as Error).message); }
    finally { active.current = false; onBusy(false); }
  }
  async function disconnect() {
    if (active.current || busy) return;
    active.current = true; onBusy(true); setError(null);
    try { onConnected(await disconnectCloudflare()); clear(); }
    catch (failure) { setError((failure as Error).message); }
    finally { active.current = false; onBusy(false); }
  }
  return <section className="panel cloudflare-connect" aria-label="Cloudflare Named Tunnel">
    <h2 className="panel-title"><span className="panel-title-with-icon"><img className="panel-brand-icon" src="/cloudflare.svg" alt="" width={28} height={28} />Cloudflare Named Tunnel</span></h2>
    <p className="pipe-description">{t("Connect an existing domain to Nginx through Cloudflare. Select a saved route domain to publish its paths. The public address stays the same when this tunnel restarts.", "기존 도메인을 Cloudflare를 통해 Nginx에 연결한다. 저장된 라우트 도메인을 선택하면 해당 경로를 공개한다. 재시작해도 공개 주소를 유지한다.")}</p>
    {error && <div className="banner error" role="alert">{error}</div>}
    {notice && <div className="banner info" role="status">{notice}</div>}
    {configured ? <>
      <div className="table-scroll pipe-status-scroll"><table className="vm-table pipe-status-table"><thead><tr><th>{t("Service", "서비스")}</th><th>{t("State", "상태")}</th><th>PID</th><th className="actions">{t("Actions", "작업")}</th></tr></thead><tbody><tr>
        <td>Cloudflare Tunnel</td>
        <td><span className={`state-badge${state === "active" ? " running" : state === "failed" ? " error" : ""}`}>{unit ? state : t("Unavailable", "확인 불가")}</span></td>
        <td>{unit?.mainPid || "—"}</td>
        <td className="actions"><PipeServiceMenu label={t("Cloudflare Tunnel actions", "Cloudflare Tunnel 작업")} disabled={busy || !snapshot || changing} items={[
          { label: t("Start", "시작"), disabled: !ready || state === "active", onSelect: () => void control("start") },
          { label: t("Stop", "중지"), disabled: unit?.loadState !== "loaded" || !["active", "failed"].includes(state ?? ""), danger: true, onSelect: () => void control("stop") },
          { label: t("Restart", "재시작"), disabled: !ready, onSelect: () => void control("restart") },
        ]} /></td>
      </tr></tbody></table></div>
      <div className="named-tunnel-addresses">{urls.map(host => <div className="pipe-url" key={host}><a href={"https://" + host} target="_blank" rel="noreferrer">{"https://" + host}</a><PipeCopyButton url={"https://" + host} onError={setError} /></div>)}</div>
      <p className="poll-note">{state === "active" ? t("Connector running. Check the public address after DNS becomes available.", "연결 프로그램이 실행 중이다. DNS가 반영되면 공개 주소로 확인한다.") : t("The connector is stopped or unavailable. Use its service actions above to start it.", "연결 프로그램이 중지되었거나 확인되지 않는다. 위 서비스 표의 작업 메뉴에서 시작한다.")}</p>
      <div className="pipe-controls"><button className="btn danger" type="button" disabled={busy} onClick={() => void disconnect()}>{t("Disconnect", "연결 해제")}</button></div>
      <p className="poll-note">{t("Disconnect removes this host's connector settings. Cloudflare DNS records and the remote tunnel remain in your account.", "연결 해제는 이 호스트의 연결 설정을 제거한다. Cloudflare 계정의 DNS 기록과 터널은 유지한다.")}</p>
    </> : <>
      <details className="cloudflare-help"><summary>{t("How to create an API token", "API 토큰 발급 방법")}</summary>
        <ol><li>{t("Register your domain in Cloudflare and complete nameserver setup.", "Cloudflare에 소유 도메인을 등록하고 네임서버 설정을 완료한다.")}</li>
          <li><a href="https://dash.cloudflare.com/profile/api-tokens" target="_blank" rel="noreferrer">Cloudflare API Tokens</a>{t(" → Create Token → Create Custom Token.", "에서 Create Token → Create Custom Token을 선택한다.")}</li>
          <li>{t("Grant Account / Cloudflare Tunnel / Edit, Zone / DNS / Edit, and Zone / Zone / Read. Limit resources to your account and domain.", "Account / Cloudflare Tunnel / Edit, Zone / DNS / Edit, Zone / Zone / Read 권한을 추가한다. 사용할 계정과 도메인으로 범위를 제한한다.")}</li>
          <li>{t("Create the token and paste it below. Firecrab uses it only during setup.", "토큰을 발급한 뒤 아래에 입력한다. Firecrab은 연결 작업에만 토큰을 사용한다.")}</li></ol>
      </details>
      <form className="create-grid named-tunnel-form" onSubmit={event => { event.preventDefault(); void connect(); }}>
        <label className="field"><span>Cloudflare API token</span><input aria-label="Cloudflare API token" type="password" value={token} autoComplete="off" spellCheck={false} disabled={busy} onChange={event => { setToken(event.target.value); setZones([]); setZoneId(""); setHost(""); }} /></label>
        <div className="field field-submit"><button className="btn" type="button" disabled={busy || !token.trim()} onClick={() => void load()}>{loading ? t("Loading domains…", "도메인 확인 중…") : t("Load domains", "도메인 불러오기")}</button></div>
        {zones.length > 0 && <>
          <label className="field"><span>{t("Cloudflare domain", "Cloudflare 도메인")}</span><select aria-label={t("Cloudflare domain", "Cloudflare 도메인")} value={zoneId} disabled={busy} onChange={event => { setZoneId(event.target.value); setHost(""); }}>{zones.map(zone => <option key={zone.id} value={zone.id}>{zone.name}</option>)}</select></label>
          <label className="field"><span>{t("Route domain", "라우트 도메인")}</span><select aria-label={t("Route domain", "라우트 도메인")} value={host} disabled={busy || !hosts.length} required onChange={event => setHost(event.target.value)}><option value="">{t("Select a saved route domain", "저장된 라우트 도메인 선택")}</option>{hosts.map(host => <option key={host} value={host}>{host}</option>)}</select></label>
          {!hosts.length && <p className="field-error named-tunnel-note">{t("Save an Nginx route using this Cloudflare domain before connecting.", "이 Cloudflare 도메인으로 Nginx 라우트를 먼저 저장한다.")}</p>}
          {host && <p className="poll-note named-tunnel-note">{config.routes.filter(route => route.host === host).map(route => route.pathPrefix).join(", ")}</p>}
        </>}
        {!!config.quickTunnel && <p id="named-tunnel-quick-transition" className="poll-note named-tunnel-note">{t("Before publishing, open the existing Quick Tunnel's actions → Edit → Save. Its temporary URL changes once. You can enter your API token and load domains now.", "공개 연결 전에 기존 Quick Tunnel의 ⋯ → 수정 → 저장을 진행한다. 기존 임시 주소가 한 번 바뀐다. API 토큰 입력과 도메인 불러오기는 지금 사용할 수 있다.")}</p>}
        <div className="pipe-controls named-tunnel-note"><button className="btn primary" type="submit" aria-describedby={config.quickTunnel ? "named-tunnel-quick-transition" : undefined} disabled={busy || !!config.quickTunnel || !snapshot?.nginxAvailable || !zone || !hosts.includes(host)}>{busy && !loading ? t("Connecting…", "연결 중…") : t("Connect and publish", "연결하고 공개")}</button><button className="btn" type="button" disabled={busy || !token} onClick={clear}>{t("Clear input", "입력 초기화")}</button></div>
        <p className="poll-note named-tunnel-note">{t("Creates a tunnel and DNS record for the selected hostname. Existing DNS records are preserved.", "선택한 주소의 터널과 DNS 기록을 만든다. 기존 DNS 기록은 유지한다.")}</p>
      </form>
    </>}
  </section>;
}
