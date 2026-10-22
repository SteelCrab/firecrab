import { useEffect, useState } from "react";
import type { ReactNode } from "react";
import { getHostStatus } from "./api/client";
import type { HostStatusResponse } from "./bindings";
import { HostStatusContext } from "./hostStatusContext";

const POLL_MILLIS = 10_000;

/**
 * Polls the host once for the whole frame: the sidebar card and the MicroVM
 * summary read the same answer. `null` until the first answer and after a
 * failed request.
 */
export function HostStatusProvider({ children }: { children: ReactNode }) {
  const [status, setStatus] = useState<HostStatusResponse | null>(null);

  useEffect(() => {
    let cancelled = false;
    const tick = () => {
      getHostStatus().then(
        (next) => { if (!cancelled) setStatus(next); },
        () => { if (!cancelled) setStatus(null); },
      );
    };
    tick();
    const interval = setInterval(tick, POLL_MILLIS);
    return () => {
      cancelled = true;
      clearInterval(interval);
    };
  }, []);

  return <HostStatusContext.Provider value={status}>{children}</HostStatusContext.Provider>;
}
