import { useCallback, useEffect, useRef, useState } from "react";
import type { ScheduleResponse } from "../../bindings";
import { listSchedules } from "../../api/client";

/** Only the visible screen polls; stale responses cannot replace a newer refresh. */
export function useSchedules() {
  const [items, setItems] = useState<ScheduleResponse[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const generation = useRef(0);
  const cancel = useCallback(() => { ++generation.current; }, []);
  const reload = useCallback(async () => {
    const current = ++generation.current;
    try {
      const next = await listSchedules();
      if (current === generation.current) { setItems(next); setError(null); }
    } catch (failure) {
      if (current === generation.current) setError((failure as Error).message);
    }
  }, []);
  useEffect(() => {
    let stopped = false;
    let timer: ReturnType<typeof setTimeout>;
    const poll = async () => { await reload(); if (!stopped) timer = setTimeout(poll, 5000); };
    void poll();
    return () => { stopped = true; cancel(); clearTimeout(timer); };
  }, [reload, cancel]);
  return { items, error, reload };
}
