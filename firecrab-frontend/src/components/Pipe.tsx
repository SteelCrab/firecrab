import { useCallback, useEffect, useRef, useState } from "react";
import { EMPTY_PIPE, getPipe, type PipeSnapshot } from "../api/pipe";
import { useI18n } from "../i18n";
import CloudflareConnect from "./CloudflareConnect";
import NamedCloudflareTunnel from "./NamedCloudflareTunnel";
import FirecrabTunnel from "./FirecrabTunnel";
import type { VmResponse } from "../bindings";
import { pipeApps } from "../pipeApps";
import { viewHash, type PipeCardId } from "../navigation";
import PipeCards from "./PipeCards";
import Screen from "./layout/Screen";
import PipeAuth from "./PipeAuth";
import PipeDatabases from "./PipeDatabases";
import PipeStorage from "./PipeStorage";
import PipeRegistries from "./PipeRegistries";
import PipeRouting from "./PipeRouting";

interface Props {
  card: PipeCardId | null;
  vms: VmResponse[];
  vmsLoading: boolean;
  vmsUnavailable: boolean;
  onUnsavedChange: (unsaved: boolean) => void;
}

/** Pipe lists its services as cards; only the opened card's controls are mounted. */
export default function Pipe({ card, vms, vmsLoading, vmsUnavailable, onUnsavedChange }: Props) {
  const { t } = useI18n();
  const [snapshot, setSnapshot] = useState<PipeSnapshot | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [dirty, setDirty] = useState(false);
  const revision = useRef(0);
  const mutating = useRef(false);
  const apps = pipeApps(vms);

  const reportUnsavedChange = useCallback((value: boolean) => {
    setDirty(value);
    onUnsavedChange(value);
  }, [onUnsavedChange]);

  useEffect(() => () => onUnsavedChange(false), [onUnsavedChange]);

  useEffect(() => {
    if (!dirty) return;
    const beforeUnload = (event: BeforeUnloadEvent) => {
      event.preventDefault();
      event.returnValue = "";
    };
    window.addEventListener("beforeunload", beforeUnload);
    return () => window.removeEventListener("beforeunload", beforeUnload);
  }, [dirty]);

  useEffect(() => {
    if (card === "storage") return;
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
  }, [card]);

  // A started request blocks polling so an older snapshot cannot overwrite its result.
  const onBusy = useCallback((value: boolean) => { mutating.current = value; setBusy(value); revision.current++; }, []);
  const onSnapshot = useCallback((next: PipeSnapshot) => { setSnapshot(next); setError(null); }, []);
  const shared = { snapshot, busy, onBusy, onUnsavedChange: reportUnsavedChange };
  const appProps = { apps, vmsLoading, vmsUnavailable };

  if (card === "auth") return <PipeAuth onUnsavedChange={reportUnsavedChange} />;
  if (!card) return <Screen view="pipe"><PipeCards snapshot={snapshot} error={error} /></Screen>;
  return <Screen view="pipe"><div className="stack">
    {card !== "storage" && <nav className="pipe-back" aria-label={t("Pipe navigation", "Pipe 이동")}><a href={viewHash("pipe")}><span aria-hidden="true">← </span>Pipe</a></nav>}
    {error && card !== "storage" && <div className="banner error" role="alert">{error}</div>}
    {card === "route" && <PipeRouting {...shared} {...appProps} onSnapshot={onSnapshot} />}
    {card === "registry" && <PipeRegistries onUnsavedChange={reportUnsavedChange} />}
    {card === "database" && <PipeDatabases onUnsavedChange={reportUnsavedChange} />}
    {card === "storage" && <PipeStorage onUnsavedChange={reportUnsavedChange} />}
    {card === "firecrab-tunnel" && <FirecrabTunnel {...shared} {...appProps} onConnected={onSnapshot} />}
    {card === "quick-tunnel" && <CloudflareConnect {...shared} {...appProps} config={snapshot?.config ?? EMPTY_PIPE} onConnected={onSnapshot} />}
    {card === "named-tunnel" && <NamedCloudflareTunnel {...shared} config={snapshot?.config ?? EMPTY_PIPE} onConnected={onSnapshot} />}
  </div></Screen>;
}
