import { useEffect, useState } from "react";
import { PIPE_GROUPS, PIPE_PAGES } from "../lib/pipePages";
import PipeIcon from "./PipeIcon";
import type { PipeSnapshot } from "../api/pipe";
import { listAiConnections } from "../api/pipeAi";
import { listStorage } from "../api/pipeStorage";
import { getPipeAuth } from "../api/pipeAuth";
import { listPipeDatabases } from "../api/pipeDatabases";
import { getRegistries } from "../api/registries";
import { useI18n } from "../i18n";
import { pipeCardHash, type PipeCardId } from "../navigation";

type Tone = "running" | "error" | "";
interface Card {
  id: PipeCardId;
  title: string;
  description: string;
  status: string;
  tone: Tone;
}

interface Props {
  snapshot: PipeSnapshot | null;
  error: string | null;
}

/** Service overview: each card links to the page where that service is used. */
export default function PipeOverview({ snapshot, error }: Props) {
  const { t } = useI18n();
  const [ais, setAis] = useState<number | null | undefined>();
  const [storage, setStorage] = useState<number | null | undefined>();
  const [auths, setAuths] = useState<number | null | undefined>(undefined);
  const [registries, setRegistries] = useState<number | null | undefined>(
    undefined,
  );
  const [databases, setDatabases] = useState<number | null | undefined>(
    undefined,
  );

  useEffect(() => {
    let cancelled = false;
    listAiConnections().then(
      (rows) => {
        if (!cancelled) setAis(rows.length);
      },
      () => {
        if (!cancelled) setAis(null);
      },
    );
    listStorage().then(
      (rows) => {
        if (!cancelled) setStorage(rows.length);
      },
      () => {
        if (!cancelled) setStorage(null);
      },
    );
    getPipeAuth().then(
      (connections) => {
        if (!cancelled) setAuths(connections.length);
      },
      () => {
        if (!cancelled) setAuths(null);
      },
    );
    getRegistries().then(
      (connections) => {
        if (!cancelled) setRegistries(connections.length);
      },
      () => {
        if (!cancelled) setRegistries(null);
      },
    );
    listPipeDatabases().then(
      (connections) => {
        if (!cancelled) setDatabases(connections.length);
      },
      () => {
        if (!cancelled) setDatabases(null);
      },
    );
    return () => {
      cancelled = true;
    };
  }, []);

  const show = (status: string, tone: Tone = "") => ({ status, tone });
  const loading = t("Checking…", "확인 중…");
  const unavailable = t("Unavailable", "확인 불가");
  const unconfigured = show(t("Not configured", "미설정"));
  // Pipe-backed cards cannot report anything until a snapshot arrives.
  const unknown = snapshot ? null : show(error ? unavailable : loading);
  const unitState = (unit: string) =>
    snapshot?.units.find((value) => value.unit === unit)?.activeState ??
    "unknown";
  const unitTone = (state: string) =>
    state === "active" ? "running" : state === "failed" ? "error" : "";

  function route() {
    const state = unitState("firecrab-ingress.service");
    const routes = snapshot?.config?.routes.length ?? 0;
    return show(
      t(
        `Nginx ${state} · ${routes} ${routes === 1 ? "route" : "routes"}`,
        `Nginx ${state} · 라우트 ${routes}개`,
      ),
      unitTone(state),
    );
  }
  function countStatus(count: number | null | undefined) {
    if (count === undefined) return show(loading);
    if (count === null) return show(unavailable, "error");
    return count
      ? show(t(`${count} connected`, `${count}개 연결`), "running")
      : unconfigured;
  }
  function auth() {
    if (auths === undefined) return show(loading);
    if (auths === null) return show(unavailable, "error");
    return auths
      ? show(t(`${auths} connected`, `${auths}개 연결`), "running")
      : unconfigured;
  }
  function registry() {
    if (registries === undefined) return show(loading);
    if (registries === null) return show(unavailable, "error");
    return registries
      ? show(t(`${registries} connected`, `${registries}개 연결`), "running")
      : unconfigured;
  }
  function database() {
    if (databases === undefined) return show(loading);
    if (databases === null) return show(unavailable, "error");
    return databases
      ? show(t(`${databases} saved`, `${databases}개 저장됨`), "running")
      : unconfigured;
  }
  function firecrab() {
    const addresses = snapshot?.firecrabTunnels ?? [];
    if (!addresses.length) return unconfigured;
    const connected = addresses.filter((address) => address.connected).length;
    return show(
      t(
        `${connected}/${addresses.length} connected`,
        `${connected}/${addresses.length}개 연결됨`,
      ),
      addresses.some((address) => address.error)
        ? "error"
        : connected === addresses.length
          ? "running"
          : "",
    );
  }
  function quick() {
    const tunnels = snapshot?.quickTunnels ?? [];
    if (!tunnels.length) return unconfigured;
    const running = tunnels.filter(
      (tunnel) => tunnel.unit.activeState === "active",
    ).length;
    const failed = tunnels.some(
      (tunnel) => tunnel.error || tunnel.unit.activeState === "failed",
    );
    return show(
      t(
        `${running}/${tunnels.length} running`,
        `${running}/${tunnels.length}개 실행 중`,
      ),
      failed ? "error" : running === tunnels.length ? "running" : "",
    );
  }
  function named() {
    if (!snapshot?.config?.tunnel) return unconfigured;
    const state = unitState("firecrab-tunnel.service");
    return show(
      t(`Connector ${state}`, `연결 프로그램 ${state}`),
      unitTone(state),
    );
  }

  const statuses = {
    auth: auth(),
    ai: countStatus(ais),
    storage: countStatus(storage),
    registry: registry(),
    database: database(),
    route: unknown ?? route(),
    "firecrab-tunnel": unknown ?? firecrab(),
    "quick-tunnel": unknown ?? quick(),
    "named-tunnel": unknown ?? named(),
  };
  const cards: Card[] = PIPE_PAGES.map((page) => ({
    id: page.id,
    title: t(...page.title),
    description: t(...page.description),
    ...statuses[page.id],
  }));

  return (
    <section className="pipe-overview" aria-label="Pipe">
      {error && (
        <div className="banner error" role="alert">
          {error}
        </div>
      )}
      {PIPE_GROUPS.map((group) => (
        <section
          className="pipe-group"
          key={group.label[0]}
          aria-label={t(group.label[0], group.label[1])}
        >
          <h2>{t(group.label[0], group.label[1])}</h2>
          {group.pages.map((id) => {
            const card = cards.find((card) => card.id === id)!;
            return (
              <a
                className="pipe-row"
                key={id}
                href={pipeCardHash(id)}
                aria-labelledby={`pipe-page-${id}`}
                aria-describedby={`pipe-page-${id}-description pipe-page-${id}-status`}
              >
                <span className="pipe-row-icon">
                  <PipeIcon id={id} />
                </span>
                <span className="pipe-row-text">
                  <b id={`pipe-page-${id}`}>{card.title}</b>
                  <span id={`pipe-page-${id}-description`}>
                    {card.description}
                  </span>
                </span>
                <span
                  className={`pipe-row-status state-badge${card.tone ? " " + card.tone : ""}`}
                  id={`pipe-page-${id}-status`}
                >
                  {card.status}
                </span>
                <span className="pipe-row-chevron" aria-hidden="true">
                  ›
                </span>
              </a>
            );
          })}
        </section>
      ))}
    </section>
  );
}
