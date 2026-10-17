import { useState } from "react";
import { connectCloudflare, getCloudflareZones, type CloudflareZone, type PipeConfig, type PipeSnapshot } from "../api/pipe";
import { useI18n } from "../i18n";
import { canUsePipeApps, type PipeApp } from "../pipeApps";
import PipeAppPicker from "./PipeAppPicker";

interface Props {
  config: PipeConfig;
  snapshot: PipeSnapshot | null;
  busy: boolean;
  apps: PipeApp[];
  vmsLoading: boolean;
  vmsUnavailable: boolean;
  onBusy: (busy: boolean) => void;
  onConnected: (snapshot: PipeSnapshot) => void;
}

export default function CloudflareConnect({ config, snapshot, busy, apps, vmsLoading, vmsUnavailable, onBusy, onConnected }: Props) {
  const { t } = useI18n();
  const [token, setToken] = useState("");
  const [zones, setZones] = useState<CloudflareZone[]>([]);
  const [zoneId, setZoneId] = useState("");
  const [subdomain, setSubdomain] = useState("app");
  const [addresses, setAddresses] = useState<string[]>([]);
  const [step, setStep] = useState<"idle" | "domains" | "connecting">("idle");
  const [error, setError] = useState<string | null>(null);
  const [publicUrl, setPublicUrl] = useState<string | null>(null);
  const zone = zones.find(zone => zone.id === zoneId);
  const hostname = zone ? [subdomain.trim().toLowerCase(), zone.name].filter(Boolean).join(".") : "";
  const configured = snapshot?.config?.tunnel;
  const appsReady = !vmsLoading && !vmsUnavailable && canUsePipeApps(apps, addresses);

  async function loadDomains() {
    setStep("domains"); onBusy(true); setError(null); setPublicUrl(null);
    try {
      const result = await getCloudflareZones(token.trim());
      setZones(result); setZoneId(result[0]?.id ?? "");
    } catch (failure) { setError((failure as Error).message); setZones([]); }
    finally { setStep("idle"); onBusy(false); }
  }

  async function connect() {
    if (!hostname || !zoneId || busy || !appsReady) return;
    setStep("connecting"); onBusy(true); setError(null);
    const route = { host: hostname, pathPrefix: "/", upstreams: addresses };
    const next = { ...config, tunnel: null, routes: [...config.routes.filter(route => !(route.host === hostname && route.pathPrefix.replace(/\/+$/, "") === "")), route] };
    try {
      const result = await connectCloudflare(token.trim(), zoneId, hostname, next);
      setToken(""); setZones([]); setZoneId("");
      setPublicUrl(`https://${hostname}`); onConnected(result);
    } catch (failure) { setError((failure as Error).message); }
    finally { setStep("idle"); onBusy(false); }
  }

  return <section className="panel cloudflare-connect" aria-label={t("Connect Cloudflare", "Cloudflare 연결")}>
    <h2 className="panel-title">
      <span className="panel-title-with-icon">
        <img className="panel-brand-icon" src="/cloudflare.svg" alt="" width={28} height={28} />
        {t("Connect Cloudflare Tunnel", "Cloudflare Tunnel 연결")}
      </span>
    </h2>
    <p>{t("Create a public app address here. Firecrab creates the tunnel, registers DNS and starts the connector.", "공개할 앱 주소를 만든다. Firecrab이 터널 생성, DNS 등록, 연결 프로그램 실행을 처리한다.")}</p>
    {error && <div className="banner error" role="alert">{error}</div>}
    {publicUrl && <div className="banner info" role="status">{t("Cloudflare connector connected. DNS and HTTPS may take a moment to become available.", "Cloudflare 연결을 확인했다. DNS와 HTTPS 적용에는 시간이 걸릴 수 있다.")} <a href={publicUrl} target="_blank" rel="noreferrer">{publicUrl}</a></div>}
    {configured ? <p className="poll-note">{t("A tunnel is configured. Its live state is shown above. To replace it, remove its local configuration in advanced settings first. Cloudflare DNS and tunnel records are managed separately.", "터널이 설정되어 있다. 실제 실행 상태는 위 표에서 확인한다. 교체하려면 고급 설정에서 로컬 터널 설정을 먼저 제거한다. Cloudflare의 DNS와 터널 기록은 별도로 관리한다.")}</p> : <>
      <details className="cloudflare-help">
        <summary>{t("How to create an API token", "API 토큰 발급 방법")}</summary>
        <ol>
          <li>{t("Add your own domain to Cloudflare and complete its nameserver setup first.", "소유한 도메인을 Cloudflare에 등록하고 네임서버 설정을 먼저 완료한다.")}</li>
          <li><a href="https://dash.cloudflare.com/profile/api-tokens" target="_blank" rel="noreferrer">{t("Open Cloudflare API Tokens", "Cloudflare API Tokens 열기")}</a>{t(" and select Create Token → Create Custom Token.", "에서 Create Token → Create Custom Token을 선택한다.")}</li>
          <li>{t("Name the token Firecrab and add these permissions:", "이름을 Firecrab으로 정하고 아래 권한을 추가한다.")}</li>
        </ol>
        <div className="table-scroll"><table className="vm-table"><thead><tr><th>{t("Scope", "범위")}</th><th>{t("Resource", "항목")}</th><th>{t("Permission", "권한")}</th></tr></thead><tbody>
          <tr><td>Account</td><td>Cloudflare Tunnel</td><td>Edit</td></tr>
          <tr><td>Zone</td><td>DNS</td><td>Edit</td></tr>
          <tr><td>Zone</td><td>Zone</td><td>Read</td></tr>
        </tbody></table></div>
        <p>{t("Limit Account Resources to your account and Zone Resources to the domain you will publish. Select Continue to summary → Create Token, then copy the token into Firecrab.", "Account Resources는 사용할 계정으로, Zone Resources는 공개할 도메인으로 제한한다. Continue to summary → Create Token을 선택하고 발급한 토큰을 Firecrab에 붙여 넣는다.")}</p>
        <p>{t("Use a Cloudflare API token. A tunnel token or installation command is a different credential. The API token is used only during setup and is not saved.", "Cloudflare API 토큰을 사용한다. 터널 토큰이나 설치 명령은 다른 인증 정보다. API 토큰은 연결 작업에만 사용하고 저장하지 않는다.")}</p>
      </details>
      <div className="pipe-controls">
        <label className="field"><span>Cloudflare API token</span><input type="password" autoComplete="off" spellCheck={false} value={token} disabled={busy} onChange={event => { setToken(event.target.value); setZones([]); setZoneId(""); setError(null); }} /></label>
        <button className="btn" type="button" disabled={busy || !token.trim()} onClick={loadDomains}>{step === "domains" ? t("Loading domains…", "도메인 확인 중…") : t("Load domains", "도메인 불러오기")}</button>
      </div>
      {zones.length > 0 && <div className="pipe-tunnel-fields">
        <label className="field"><span>{t("Cloudflare domain", "Cloudflare 도메인")}</span><select aria-label={t("Cloudflare domain", "Cloudflare 도메인")} value={zoneId} disabled={busy} onChange={event => setZoneId(event.target.value)}>{zones.map(zone => <option key={zone.id} value={zone.id}>{zone.name}</option>)}</select></label>
        <label className="field"><span>{t("Subdomain", "서브도메인")}</span><input aria-label={t("Subdomain", "서브도메인")} value={subdomain} placeholder="app" disabled={busy} onChange={event => setSubdomain(event.target.value)} /><span className="poll-note">{hostname}</span></label>
        <PipeAppPicker id="cloudflare-apps" label={t("Public apps", "공개할 앱 선택")} apps={apps} selected={addresses} loading={vmsLoading} unavailable={vmsUnavailable} disabled={busy} onChange={setAddresses} />
        <div className="field field-submit"><button className="btn primary" type="button" disabled={busy || !snapshot?.nginxAvailable || !appsReady} onClick={connect}>{step === "connecting" ? t("Connecting…", "연결 중…") : t("Connect and publish", "연결하고 공개")}</button><span className="poll-note">{t("Creates a public DNS record. Existing DNS records are never overwritten.", "공개 DNS 기록을 만든다. 기존 DNS 기록은 덮어쓰지 않는다.")}</span></div>
      </div>}
      {step === "connecting" && <p className="poll-note" role="status">{t("Installing the connector if needed, creating the tunnel and DNS, then checking the Cloudflare connection. Keep this page open.", "필요한 연결 프로그램을 설치하고 터널과 DNS를 생성한 뒤 Cloudflare 연결을 확인한다. 이 화면을 유지한다.")}</p>}
      {snapshot && !snapshot.nginxAvailable && <p className="field-error">{t("Install Nginx on the managed host before connecting.", "연결 전에 관리 호스트에 Nginx를 설치한다.")}</p>}
    </>}
  </section>;
}
