import { useCallback, useEffect, useReducer, useRef, useState } from "react";
import type { VmResponse } from "./bindings";
import { deleteVm, listVms, startVm, stopVm } from "./api/client";
import type { VmAction } from "./model";
import BannerView from "./components/Banner";
import CreateVm from "./components/CreateVm";
import VmTable from "./components/VmTable";
import Console from "./components/Console";
import VmDetailModal from "./components/VmDetailModal";
import HostInfo from "./components/HostInfo";
import Images from "./components/Images";
import Kernels from "./components/Kernels";
import MicroNetworks from "./components/MicroNetworks";
import MicroStorages from "./components/MicroStorages";
import Shell from "./components/Shell";
import Shells from "./components/Shells";
import { newVmHash, useAppRoute, viewHash } from "./navigation";
import { useI18n } from "./i18n";

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
  | { type: "created"; vm: VmResponse; message: string }
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
      // Always flip `loaded` so the list panel is not stuck on "불러오는 중…"
      // after the first failed poll (common right after `npm run dev` while
      // the API is still starting, or when the proxy flaps once).
      return {
        ...state,
        loaded: true,
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
        banner: { kind: "info", text: msg.message },
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
  const { t } = useI18n();
  const [state, dispatch] = useReducer(reduce, initialState);
  // Generation token so React StrictMode's mount→unmount→remount does not
  // leave a stuck "in flight" flag that skips the real first list fetch.
  const refreshGen = useRef(0);
  // id of the VM whose detail modal is open, if any — local UI state, not
  // server-synced. The serial console is a full-page hash route instead.
  const [openDetailId, setOpenDetailId] = useState<string | null>(null);
  const { route, selectView, closeConsole } = useAppRoute();

  const runRefresh = useCallback(() => {
    const gen = ++refreshGen.current;
    (async () => {
      try {
        const vms = await listVms();
        // Ignore stale responses from an aborted StrictMode pass.
        if (gen !== refreshGen.current) return;
        dispatch({ type: "refreshed", vms });
      } catch (error) {
        if (gen !== refreshGen.current) return;
        dispatch({ type: "refreshFailed", message: (error as Error).message });
      }
    })();
  }, []);

  const slowMode = state.consecutiveFailures >= SLOW_POLL_AFTER;
  useEffect(() => {
    runRefresh();
    const millis = slowMode ? SLOW_POLL_MILLIS : POLL_MILLIS;
    const interval = setInterval(runRefresh, millis);
    return () => {
      // Invalidate in-flight work from this effect instance (StrictMode cleanup).
      refreshGen.current += 1;
      clearInterval(interval);
    };
  }, [slowMode, runRefresh]);

  const onAction = useCallback(
    (id: string, action: VmAction) => {
      if (state.busy.has(id)) return;
      if (action === "delete" && !confirmDelete(t)) return;

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
    [state.busy, runRefresh, t],
  );

  const onCreated = useCallback(
    (vm: VmResponse) => {
      dispatch({ type: "created", vm, message: t(`Created: ${vm.name} (${vm.id})`, `생성됨: ${vm.name} (${vm.id})`) });
      window.location.hash = viewHash("vms");
    },
    [t],
  );
  const onError = useCallback((message: string) => dispatch({ type: "error", message }), []);
  const dismiss = useCallback(() => dispatch({ type: "dismissBanner" }), []);

  const onOpenDetail = useCallback((id: string) => setOpenDetailId(id), []);
  const onCloseDetail = useCallback(() => setOpenDetailId(null), []);

  const pollNote = slowMode
    ? t("API unavailable — retrying every 15s", "API 연결 안 됨 — 15s 간격 재시도")
    : t(`Polling every ${POLL_MILLIS / 1000}s`, `${POLL_MILLIS / 1000}초마다 폴링`);

  // Terminal owns the whole viewport — no shell chrome, no modal clip box.
  if (route.kind === "console") {
    return <Console vmId={route.vmId} onClose={closeConsole} />;
  }

  const view = route.kind === "vm-new" ? "vms" : route.view;

  return (
    <Shell view={view} onSelectView={selectView}>
      <div className="stack">
        {state.banner && <BannerView kind={state.banner.kind} text={state.banner.text} onDismiss={dismiss} />}
        {route.kind === "vm-new" && (
          <section className="panel">
            <h2 className="panel-title">
              <span>{t("New MicroVM", "새 MicroVM")}</span>
              <a href={viewHash("vms")}>{t("Back", "뒤로")}</a>
            </h2>
            <CreateVm onCreated={onCreated} onError={onError} />
          </section>
        )}
        {route.kind === "shell" && route.view === "vms" && (
          <section className="panel">
            <h2 className="panel-title">
              <span>{t(`MicroVMs (${state.vms.length})`, `MicroVM 목록 (${state.vms.length})`)}</span>
              <span>
                <a id="vm-list-add" className="btn primary" href={newVmHash()}>
                  {t("Create", "생성")}
                </a>
                <span className="poll-note">{pollNote}</span>
              </span>
            </h2>
            {state.loaded ? (
              <VmTable
                vms={state.vms}
                busy={state.busy}
                onAction={onAction}
                onOpenDetail={onOpenDetail}
              />
            ) : (
              <div className="empty">{t("Loading…", "불러오는 중…")}</div>
            )}
          </section>
        )}
        {/* Each page mounts only while selected, so its own polling stops
            the moment you navigate away. */}
        {view === "networks" && <MicroNetworks />}
        {view === "storages" && <MicroStorages />}
        {view === "images" && <Images />}
        {view === "kernels" && <Kernels />}
        {view === "shells" && <Shells />}
        {view === "host" && <HostInfo />}
      </div>
      {openDetailId && <VmDetailModal vmId={openDetailId} vms={state.vms} onClose={onCloseDetail} />}
    </Shell>
  );
}

function confirmDelete(t: (english: string, korean: string) => string): boolean {
  return window.confirm(t("Delete this VM record and its disk?", "VM 레코드와 디스크를 삭제할까요?"));
}
