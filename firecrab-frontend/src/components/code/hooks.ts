import { useCallback, useEffect, useState } from "react";
import { fetchJson } from "../../api/client";
import {
  CODE,
  codeError,
  equal,
  runtime,
  uuid,
  workspaces,
} from "../../api/code";
import type { Account, Adapter, Runtime, Workspace } from "../../api/code";
import pinned from "../../../../firecrab-api/src/code_agent_auth/adapters.json";

export function useCodeSnapshot(id: string | null) {
  const [value, set] = useState<{
    rows: Workspace[];
    runtime: Runtime | null;
    loaded: boolean;
    error: string;
  }>({ rows: [], runtime: null, loaded: false, error: "" });
  const [epoch, update] = useState(0);
  const refresh = useCallback(() => update((n) => n + 1), []);
  useEffect(() => {
    const controller = new AbortController();
    let timer: ReturnType<typeof setTimeout>;
    async function poll() {
      try {
        const rows = await workspaces(controller.signal);
        const selected =
          rows.find((w) => w.id === id) ?? (!id ? rows[0] : undefined);
        const r = selected
          ? await runtime(selected.id, controller.signal)
          : null;
        if (!controller.signal.aborted)
          set((old) => {
            const next = { rows, runtime: r, loaded: true, error: "" };
            return equal(old, next) ? old : next;
          });
      } catch (e) {
        if (!controller.signal.aborted)
          set({ rows: [], runtime: null, loaded: true, error: codeError(e) });
      } finally {
        if (!controller.signal.aborted)
          timer = setTimeout(() => void poll(), 3000);
      }
    }
    void poll();
    return () => {
      controller.abort();
      clearTimeout(timer);
    };
  }, [id, epoch]);
  return { ...value, refresh };
}
export function useAccounts(epoch: number) {
  const [state, set] = useState<{
    accounts: Account[];
    adapters: Adapter[];
    loading: boolean;
    error: boolean;
  }>({ accounts: [], adapters: [], loading: true, error: false });
  useEffect(() => {
    const controller = new AbortController();
    let timer: ReturnType<typeof setTimeout>;
    async function load() {
      try {
        const accounts = await fetchJson<Account[]>(
          "/api/pipe/ai/connections",
          { signal: controller.signal },
        );
        if (
          !Array.isArray(accounts) ||
          accounts.some(
            (a, i) =>
              !uuid(a.id) || accounts.findIndex((v) => v.id === a.id) !== i,
          )
        )
          throw new Error("invalid");
        let adapters: Adapter[] = [];
        try {
          const raw = await fetchJson<Adapter[]>(
            "/api/code/agent-auth/adapters",
            { signal: controller.signal },
          );
          if (Array.isArray(raw))
            adapters = raw.filter((a) =>
              pinned.some(
                (p) =>
                  p.id === a.id &&
                  p.agent === a.agent &&
                  p.provider === a.provider &&
                  p.authentication === a.authentication &&
                  p.cliVersion === a.cliVersion &&
                  p.supported &&
                  a.supported,
              ),
            );
        } catch {
          /* Account management remains available during a recipe outage. */
        }
        if (!controller.signal.aborted)
          set((old) => {
            const next = { accounts, adapters, loading: false, error: false };
            return equal(old, next) ? old : next;
          });
      } catch {
        if (!controller.signal.aborted)
          set({ accounts: [], adapters: [], loading: false, error: true });
      } finally {
        if (!controller.signal.aborted)
          timer = setTimeout(() => void load(), 3000);
      }
    }
    void load();
    return () => {
      controller.abort();
      clearTimeout(timer);
    };
  }, [epoch]);
  return state;
}
export async function current(w: Workspace) {
  const latest = await fetchJson<Workspace>(`${CODE}/${w.id}`);
  if (
    latest.id !== w.id ||
    latest.revision !== w.revision ||
    !equal(latest.spec, w.spec)
  )
    throw new Error("changed");
  return latest;
}
