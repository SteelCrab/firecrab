import { useCallback, useEffect, useReducer, useRef, useState } from "react";
import type { VmResponse } from "./bindings";
import { deleteVm, listVms, startVm, stopVm } from "./api/client";
import type { VmAction } from "./model";
import BannerView from "./components/Banner";
import CreateVm from "./components/CreateVm";
import VmTable from "./components/VmTable";
import Console from "./components/Console";

const POLL_MILLIS = 3_000;
// After repeated failures assume the API is down and poll gently.
const SLOW_POLL_MILLIS = 15_000;
const SLOW_POLL_AFTER = 3;

interface BannerState {
  kind: "error" | "info";
  text: string;
}

interface Dashboard {
  vms: VmResponse[];
  busy: Set<string>;
  banner: BannerState | null;
  loaded: boolean;
  consecutiveFailures: number;
}

type Msg =
  | { type: "refreshed"; vms: VmResponse[] }
  | { type: "refreshFailed"; message: string }
  | { type: "actionStarted"; id: string }
  // `vm` is null when the VM was deleted.
  | { type: "actionSucceeded"; id: string; vm: VmResponse | null }
  | { type: "actionFailed"; id: string; message: string }
  | { type: "created"; vm: VmResponse }
  | { type: "error"; message: string }
  | { type: "dismissBanner" };

/** Keeps the server's list order: name ascending, ties by id. */
function upsert(vms: VmResponse[], vm: VmResponse): VmResponse[] {
  const exists = vms.some((existing) => existing.id === vm.id);
  const next = exists ? vms.map((existing) => (existing.id === vm.id ? vm : existing)) : [...vms, vm];
  return [...next].sort((a, b) => a.name.localeCompare(b.name) || a.id.localeCompare(b.id));
}

function reduce(state: Dashboard, msg: Msg): Dashboard {
  switch (msg.type) {
    case "refreshed":
      return { ...state, vms: msg.vms, loaded: true, consecutiveFailures: 0 };
    case "refreshFailed":
      return {
        ...state,
        consecutiveFailures: state.consecutiveFailures + 1,
        banner: { kind: "error", text: msg.message },
      };
    case "actionStarted": {
      const busy = new Set(state.busy);
      busy.add(msg.id);
      return { ...state, busy };
    }
    case "actionSucceeded": {
      const busy = new Set(state.busy);
      busy.delete(msg.id);
      const vms = msg.vm ? upsert(state.vms, msg.vm) : state.vms.filter((vm) => vm.id !== msg.id);
      return { ...state, busy, vms };
    }
    case "actionFailed": {
      const busy = new Set(state.busy);
      busy.delete(msg.id);
      return { ...state, busy, banner: { kind: "error", text: msg.message } };
    }
    case "created":
      return {
        ...state,
        banner: { kind: "info", text: `생성됨: ${msg.vm.name} (${msg.vm.id})` },
        vms: upsert(state.vms, msg.vm),
      };
    case "error":
      return { ...state, banner: { kind: "error", text: msg.message } };
    case "dismissBanner":
      return { ...state, banner: null };
  }
}

const initialState: Dashboard = {
  vms: [],
  busy: new Set(),
  banner: null,
  loaded: false,
  consecutiveFailures: 0,
};

export default function App() {
  const [state, dispatch] = useReducer(reduce, initialState);
  const refreshInFlight = useRef(false);
  // (id, name) of the console currently attached, if any. Separate from
  // `Dashboard` since it's local UI state, not server-synced data.
  const [openConsole, setOpenConsole] = useState<{ id: string; name: string } | null>(null);

  const runRefresh = useCallback(() => {
    if (refreshInFlight.current) return;
    refreshInFlight.current = true;
    (async () => {
      try {
        dispatch({ type: "refreshed", vms: await listVms() });
      } catch (error) {
        dispatch({ type: "refreshFailed", message: (error as Error).message });
      } finally {
        refreshInFlight.current = false;
      }
    })();
  }, []);

  const slowMode = state.consecutiveFailures >= SLOW_POLL_AFTER;
  useEffect(() => {
    runRefresh();
    const millis = slowMode ? SLOW_POLL_MILLIS : POLL_MILLIS;
    const interval = setInterval(runRefresh, millis);
    return () => clearInterval(interval);
  }, [slowMode, runRefresh]);

  const onAction = useCallback(
    (id: string, action: VmAction) => {
      if (state.busy.has(id)) return;
      if (action === "delete" && !confirmDelete()) return;

      dispatch({ type: "actionStarted", id });
      (async () => {
        try {
          let vm: VmResponse | null;
          if (action === "start") vm = await startVm(id);
          else if (action === "stop") vm = await stopVm(id);
          else {
            await deleteVm(id);
            vm = null;
          }
          dispatch({ type: "actionSucceeded", id, vm });
        } catch (error) {
          dispatch({ type: "actionFailed", id, message: (error as Error).message });
          // 409 means our view was stale, and a failed start leaves the VM
          // in error state — resync right away.
          runRefresh();
        }
      })();
    },
    [state.busy, runRefresh],
  );

  const onCreated = useCallback((vm: VmResponse) => dispatch({ type: "created", vm }), []);
  const onError = useCallback((message: string) => dispatch({ type: "error", message }), []);
  const dismiss = useCallback(() => dispatch({ type: "dismissBanner" }), []);

  const onOpenConsole = useCallback(
    (id: string) => {
      const name = state.vms.find((vm) => vm.id === id)?.name ?? "";
      setOpenConsole({ id, name });
    },
    [state.vms],
  );
  const onCloseConsole = useCallback(() => setOpenConsole(null), []);

  const pollNote = slowMode ? "API 연결 안 됨 — 15s 간격 재시도" : `${POLL_MILLIS / 1000}s polling`;

  return (
    <div className="wrap">
      <header className="hero">
        <p className="eyebrow">private microvm cloud</p>
        <h1 className="wordmark">
          firecrab
          <span className="cursor">_</span>
        </h1>
      </header>
      <div className="stack">
        {state.banner && <BannerView kind={state.banner.kind} text={state.banner.text} onDismiss={dismiss} />}
        <section className="panel">
          <h2 className="panel-title">vm 생성</h2>
          <CreateVm onCreated={onCreated} onError={onError} />
        </section>
        <section className="panel">
          <h2 className="panel-title">
            <span>{`vm 목록 (${state.vms.length})`}</span>
            <span className="poll-note">{pollNote}</span>
          </h2>
          {state.loaded ? (
            <VmTable vms={state.vms} busy={state.busy} onAction={onAction} onOpenConsole={onOpenConsole} />
          ) : (
            <div className="empty">불러오는 중…</div>
          )}
        </section>
      </div>
      {openConsole && <Console vmId={openConsole.id} vmName={openConsole.name} onClose={onCloseConsole} />}
    </div>
  );
}

function confirmDelete(): boolean {
  return window.confirm("VM 레코드와 디스크를 삭제할까요?");
}
