import { useEffect, useState } from "react";
import type { PipeSnapshot } from "../api/pipe";
import { getRegistries } from "../api/registries";
import { useI18n } from "../i18n";
import { pipeCardHash, type PipeCardId } from "../navigation";

type Tone = "running" | "error" | "";
interface Card { id: PipeCardId; title: string; description: string; status: string; tone: Tone }

interface Props {
  snapshot: PipeSnapshot | null;
  error: string | null;
}

/** Brand marks where a service has one; otherwise a line icon in the card's ink. */
function PipeCardIcon({ id }: { id: PipeCardId }) {
  switch (id) {
    case "route":
      // One request path forking to two apps.
      return <svg className="pipe-card-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true"><path d="M3 12h6l5-6h6M9 12l5 6h6" /><path d="M17 3l3 3-3 3M17 15l3 3-3 3" /></svg>;
    case "registry":
      // Shipping container: images held in a container registry.
      return <svg className="pipe-card-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true"><rect x="2" y="6" width="20" height="12" rx="1.5" /><path d="M6.5 9.5v5M10.5 9.5v5M14.5 9.5v5M18.5 9.5v5" strokeWidth="1.6" /></svg>;
    case "firecrab-tunnel":
      return <img className="pipe-card-icon" src="/firecrab.png" alt="" width={28} height={28} />;
    case "quick-tunnel":
    case "named-tunnel":
      return <img className="pipe-card-icon" src="/cloudflare.svg" alt="" width={28} height={28} />;
  }
}

/** Service overview: each card links to the page where that service is used. */
export default function PipeCards({ snapshot, error }: Props) {
  const { t } = useI18n();
  const [registries, setRegistries] = useState<number | null | undefined>(undefined);

  useEffect(() => {
    let cancelled = false;
    getRegistries().then(
      connections => { if (!cancelled) setRegistries(connections.length); },
      () => { if (!cancelled) setRegistries(null); },
    );
    return () => { cancelled = true; };
  }, []);

  const show = (status: string, tone: Tone = "") => ({ status, tone });
  const loading = t("Checking…", "확인 중…");
  const unavailable = t("Unavailable", "확인 불가");
  const unconfigured = show(t("Not configured", "미설정"));
  // Pipe-backed cards cannot report anything until a snapshot arrives.
  const unknown = snapshot ? null : show(error ? unavailable : loading);
  const unitState = (unit: string) => snapshot?.units.find(value => value.unit === unit)?.activeState ?? "unknown";
  const unitTone = (state: string) => state === "active" ? "running" : state === "failed" ? "error" : "";

  function route() {
    const state = unitState("firecrab-ingress.service");
    const routes = snapshot?.config?.routes.length ?? 0;
    return show(t(`Nginx ${state} · ${routes} ${routes === 1 ? "route" : "routes"}`, `Nginx ${state} · 라우트 ${routes}개`), unitTone(state));
  }
  function registry() {
    if (registries === undefined) return show(loading);
    if (registries === null) return show(unavailable, "error");
    return registries ? show(t(`${registries} connected`, `${registries}개 연결`), "running") : unconfigured;
  }
  function firecrab() {
    const addresses = snapshot?.firecrabTunnels ?? [];
    if (!addresses.length) return unconfigured;
    const connected = addresses.filter(address => address.connected).length;
    return show(t(`${connected}/${addresses.length} connected`, `${connected}/${addresses.length}개 연결됨`),
      addresses.some(address => address.error) ? "error" : connected === addresses.length ? "running" : "");
  }
  function quick() {
    const tunnels = snapshot?.quickTunnels ?? [];
    if (!tunnels.length) return unconfigured;
    const running = tunnels.filter(tunnel => tunnel.unit.activeState === "active").length;
    const failed = tunnels.some(tunnel => tunnel.error || tunnel.unit.activeState === "failed");
    return show(t(`${running}/${tunnels.length} running`, `${running}/${tunnels.length}개 실행 중`),
      failed ? "error" : running === tunnels.length ? "running" : "");
  }
  function named() {
    if (!snapshot?.config?.tunnel) return unconfigured;
    const state = unitState("firecrab-tunnel.service");
    return show(t(`Connector ${state}`, `연결 프로그램 ${state}`), unitTone(state));
  }

  const cards: Card[] = [
    { id: "route", title: "Route", description: t("Nginx status and domain/path routing to VM apps", "Nginx 상태와 도메인·경로별 요청 전달"), ...(unknown ?? route()) },
    { id: "registry", title: "Pipe Registry", description: t("Pull images from external container registries", "외부 컨테이너 레지스트리에서 이미지 가져오기"), ...registry() },
    { id: "firecrab-tunnel", title: "Firecrab Tunnel", description: t("Connect apps or routes to a fixed address with a token", "토큰으로 앱 또는 라우트를 고정 주소에 연결"), ...(unknown ?? firecrab()) },
    { id: "quick-tunnel", title: "Cloudflare Quick Tunnel", description: t("Publish apps or routes on temporary URLs", "앱 또는 라우트를 임시 주소로 공개"), ...(unknown ?? quick()) },
    { id: "named-tunnel", title: "Cloudflare Named Tunnel", description: t("Publish route domains through your Cloudflare domain", "내 Cloudflare 도메인으로 라우트 공개"), ...(unknown ?? named()) },
  ];

  return <section className="panel" aria-label="Pipe">
    <h2 className="panel-title"><span>Pipe</span><span className="poll-note">{t("Firecrab integrations", "Firecrab 연동 서비스")}</span></h2>
    <p className="pipe-description">{t("Open a card to use that service.", "카드를 눌러 서비스를 연다.")}</p>
    {error && <div className="banner error" role="alert">{error}</div>}
    <ul className="pipe-cards">
      {cards.map(card => <li key={card.id}>
        <a className="pipe-card" href={pipeCardHash(card.id)} aria-labelledby={`pipe-card-${card.id}`} aria-describedby={`pipe-card-${card.id}-description pipe-card-${card.id}-status`}>
          <span className="pipe-card-head"><PipeCardIcon id={card.id} /><span className="pipe-card-title" id={`pipe-card-${card.id}`}>{card.title}</span></span>
          <span className="pipe-card-description" id={`pipe-card-${card.id}-description`}>{card.description}</span>
          <span className={`state-badge${card.tone ? " " + card.tone : ""}`} id={`pipe-card-${card.id}-status`}>{card.status}</span>
        </a>
      </li>)}
    </ul>
  </section>;
}
