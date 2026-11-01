import { useCallback, useEffect, useRef, useState } from "react";
import { EMPTY_PIPE, getPipe, type PipeSnapshot } from "../api/pipe";
import CloudflareConnect from "./CloudflareConnect";
import NamedCloudflareTunnel from "./NamedCloudflareTunnel";
import FirecrabTunnel from "./FirecrabTunnel";
import type { VmResponse } from "../bindings";
import { pipeApps } from "../pipeApps";
import { type PipeCardId } from "../navigation";
import PipeNavigation from "./PipeNavigation";
import PipeOverview from "./PipeOverview";
import Screen from "./layout/Screen";
import PipeAuth from "./PipeAuth";
import PipeAi from "./PipeAi";
import PipeStorage from "./PipeStorage";
import PipeDatabases from "./PipeDatabases";
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
export default function Pipe({
  card,
  vms,
  vmsLoading,
  vmsUnavailable,
  onUnsavedChange,
}: Props) {
  const [snapshot, setSnapshot] = useState<PipeSnapshot | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const revision = useRef(0);
  const mutating = useRef(false);
  const apps = pipeApps(vms);
  const needsIngress =
    !card ||
    ["route", "quick-tunnel", "named-tunnel", "firecrab-tunnel"].includes(card);

  const reportDirty = onUnsavedChange;
  useEffect(() => () => onUnsavedChange(false), [onUnsavedChange]);

  useEffect(() => {
    if (!needsIngress) return;
    let cancelled = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    let controller: AbortController | undefined;
    const refresh = async () => {
      if (mutating.current) {
        timer = setTimeout(refresh, 8_000);
        return;
      }
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
    return () => {
      cancelled = true;
      clearTimeout(timer);
      controller?.abort();
    };
  }, [needsIngress]);

  // A started request blocks polling so an older snapshot cannot overwrite its result.
  const onBusy = useCallback((value: boolean) => {
    mutating.current = value;
    setBusy(value);
    revision.current++;
  }, []);
  const onSnapshot = useCallback((next: PipeSnapshot) => {
    setSnapshot(next);
    setError(null);
  }, []);
  const shared = { snapshot, busy, onBusy, onUnsavedChange: reportDirty };
  const appProps = { apps, vmsLoading, vmsUnavailable };

  if (card === "auth")
    return (
      <PipeAuth
        onUnsavedChange={reportDirty}
        navigation={<PipeNavigation active="auth" />}
      />
    );
  if (!card)
    return (
      <Screen view="pipe" navigation={<PipeNavigation active={card} />}>
        <PipeOverview snapshot={snapshot} error={error} />
      </Screen>
    );
  return (
    <Screen view="pipe" navigation={<PipeNavigation active={card} />}>
      <div className="stack">
        {error && needsIngress && (
          <div className="banner error" role="alert">
            {error}
          </div>
        )}
        {card === "ai" && <PipeAi onUnsavedChange={reportDirty} />}
        {card === "storage" && <PipeStorage onUnsavedChange={reportDirty} />}
        {card === "route" && (
          <PipeRouting {...shared} {...appProps} onSnapshot={onSnapshot} />
        )}
        {card === "registry" && (
          <PipeRegistries onUnsavedChange={reportDirty} />
        )}
        {card === "database" && <PipeDatabases onUnsavedChange={reportDirty} />}
        {card === "firecrab-tunnel" && (
          <FirecrabTunnel {...shared} {...appProps} onConnected={onSnapshot} />
        )}
        {card === "quick-tunnel" && (
          <CloudflareConnect
            {...shared}
            {...appProps}
            config={snapshot?.config ?? EMPTY_PIPE}
            onConnected={onSnapshot}
          />
        )}
        {card === "named-tunnel" && (
          <NamedCloudflareTunnel
            {...shared}
            config={snapshot?.config ?? EMPTY_PIPE}
            onConnected={onSnapshot}
          />
        )}
      </div>
    </Screen>
  );
}
