import { useEffect, useState } from "react";
import { KeyRound, Sparkles } from "lucide-react";
import { listAiConnections } from "../api/pipeAi";
import type { PipeSnapshot } from "../api/pipe";
import { getPipeAuth } from "../api/pipeAuth";
import { listPipeDatabases } from "../api/pipeDatabases";
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
    case "ai":
      return <Sparkles className="pipe-card-icon" aria-hidden="true" />;
    case "auth":
      return <KeyRound className="pipe-card-icon" aria-hidden="true" />;
    case "route":
      // One request path forking to two apps.
      return <svg className="pipe-card-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true"><path d="M3 12h6l5-6h6M9 12l5 6h6" /><path d="M17 3l3 3-3 3M17 15l3 3-3 3" /></svg>;
    case "registry":
      // Shipping container: images held in a container registry.
      return <svg className="pipe-card-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true"><rect x="2" y="6" width="20" height="12" rx="1.5" /><path d="M6.5 9.5v5M10.5 9.5v5M14.5 9.5v5M18.5 9.5v5" strokeWidth="1.6" /></svg>;
    case "database":
      // Stacked disks: a database cylinder.
      return <svg className="pipe-card-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true"><ellipse cx="12" cy="5.5" rx="8" ry="3" /><path d="M4 5.5v13c0 1.66 3.58 3 8 3s8-1.34 8-3v-13" /><path d="M4 12c0 1.66 3.58 3 8 3s8-1.34 8-3" /></svg>;
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
  const [auths, setAuths] = useState<number | null | undefined>(undefined);
  const [ai, setAi] = useState<number | null | undefined>(undefined);
  const [registries, setRegistries] = useState<number | null | undefined>(undefined);
  const [databases, setDatabases] = useState<number | null | undefined>(undefined);

  useEffect(() => {
    let cancelled = false;
    listAiConnections().then(
      connections => { if (!cancelled) setAi(connections.length); },
      () => { if (!cancelled) setAi(null); },
    );
    getPipeAuth().then(
      connections => { if (!cancelled) setAuths(connections.length); },
      () => { if (!cancelled) setAuths(null); },
    );
    getRegistries().then(
      connections => { if (!cancelled) setRegistries(connections.length); },
      () => { if (!cancelled) setRegistries(null); },
    );
    listPipeDatabases().then(
      connections => { if (!cancelled) setDatabases(connections.length); },
      () => { if (!cancelled) setDatabases(null); },
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
  function auth() {
    if (auths === undefined) return show(loading);
    if (auths === null) return show(unavailable, "error");
    return auths ? show(t(`${auths} connected`, `${auths}개 연결`), "running") : unconfigured;
  }
  function registry() {
    if (registries === undefined) return show(loading);
    if (registries === null) return show(unavailable, "error");
    return registries ? show(t(`${registries} connected`, `${registries}개 연결`), "running") : unconfigured;
  }
  function database() {
    if (databases === undefined) return show(loading);
    if (databases === null) return show(unavailable, "error");
    return databases ? show(t(`${databases} saved`, `${databases}개 저장됨`), "running") : unconfigured;
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
    { id: "auth", title: "Pipe Auth", description: "AWS · GitHub", ...auth() },
    { id: "ai", title: "Pipe AI", description: t("AI account connections", "AI 계정 연결 관리"), ...(ai === undefined ? show(loading) : ai === null ? show(unavailable, "error") : ai ? show(t(`${ai} connected`, `${ai}개 연결`), "running") : unconfigured) },
    { id: "route", title: "Route", description: t("Domain · path", "도메인 · 경로"), ...(unknown ?? route()) },
    { id: "registry", title: "Pipe Registry", description: t("Image pull", "이미지 가져오기"), ...registry() },
    { id: "database", title: "Database", description: t("Connections · browse", "연결 · 조회"), ...database() },
    { id: "firecrab-tunnel", title: "Firecrab Tunnel", description: t("Fixed address", "고정 주소"), ...(unknown ?? firecrab()) },
    { id: "quick-tunnel", title: "Cloudflare Quick Tunnel", description: t("Temporary URL", "임시 주소"), ...(unknown ?? quick()) },
    { id: "named-tunnel", title: "Cloudflare Named Tunnel", description: t("Custom domain", "사용자 도메인"), ...(unknown ?? named()) },
  ];

  return <section className="panel" aria-label="Pipe">
    <h2 className="panel-title"><span>Pipe</span></h2>
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
