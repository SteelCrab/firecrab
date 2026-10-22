import { useCallback, useEffect, useMemo, useReducer, useRef, useState } from "react";
import type { VmResponse } from "./bindings";
import { deleteVm, listVms, startVm, stopVm } from "./api/client";
import type { VmAction } from "./model";
import BannerView from "./components/Banner";
import CreateVm from "./components/CreateVm";
import Console from "./components/Console";
import VmScreen from "./components/vm/VmScreen";
import FirecrabPanel from "./components/FirecrabPanel";
import HostInfo from "./components/HostInfo";
import Images from "./components/Images";
import Kernels from "./components/Kernels";
import MicroNetworks from "./components/MicroNetworks";
import MicroStorages from "./components/MicroStorages";
import MicroServices from "./components/MicroServices";
import Shell from "./components/Shell";
import Pipe from "./components/Pipe";
import Shells from "./components/Shells";
import Secrets from "./components/Secrets";
import CommandPalette from "./components/CommandPalette";
import Screen from "./components/layout/Screen";
import { ScreenStatus } from "./components/layout/screenStatus";
import { HostStatusProvider } from "./hostStatus";
import { useDetailPanelOpen, useDetailPanelWidth } from "./lib/detailPanel";
import { useAppRoute, viewHash } from "./navigation";
import { useI18n } from "./i18n";

const POLL_MILLIS = 3_000;
// After repeated failures assume the API is down and poll gently.
const SLOW_POLL_MILLIS = 15_000;
const SLOW_POLL_AFTER = 3;

interface BannerState {
  kind: "error" | "info";
  text: string;
  source: "poll" | "action";
}

interface Dashboard {
  vms: VmResponse[];
  busy: Set<string>;
  banner: BannerState | null;
  loaded: boolean;
  consecutiveFailures: number;
  /** VM shown in the detail panel; cleared when it leaves the list. */
  selectedVmId: string | null;
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
  | { type: "dismissBanner" }
  | { type: "select"; id: string | null };

/** Keeps the server's list order: name ascending, ties by id. */
function upsert(vms: VmResponse[], vm: VmResponse): VmResponse[] {
  const exists = vms.some((existing) => existing.id === vm.id);
  const next = exists ? vms.map((existing) => (existing.id === vm.id ? vm : existing)) : [...vms, vm];
  return [...next].sort((a, b) => a.name.localeCompare(b.name) || a.id.localeCompare(b.id));
}

function reduce(state: Dashboard, msg: Msg): Dashboard {
  switch (msg.type) {
    case "refreshed": {
      const kept = msg.vms.some((vm) => vm.id === state.selectedVmId);
      return {
        ...state,
        vms: msg.vms,
        loaded: true,
        consecutiveFailures: 0,
        selectedVmId: kept ? state.selectedVmId : null,
        banner: state.banner?.source === "poll" ? null : state.banner,
      };
    }
    case "refreshFailed":
      // Always flip `loaded` so the list panel is not stuck on "Loading…"
      // after the first failed poll (common right after `npm run dev` while
      // the API is still starting, or when the proxy flaps once).
      return {
        ...state,
        loaded: true,
        consecutiveFailures: state.consecutiveFailures + 1,
        banner: { kind: "error", text: msg.message, source: "poll" },
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
      const selectedVmId = !msg.vm && state.selectedVmId === msg.id ? null : state.selectedVmId;
      return { ...state, busy, vms, selectedVmId };
    }
    case "actionFailed": {
      const busy = new Set(state.busy);
      busy.delete(msg.id);
      return { ...state, busy, banner: { kind: "error", text: msg.message, source: "action" } };
    }
    case "created":
      return {
        ...state,
        banner: { kind: "info", text: msg.message, source: "action" },
        vms: upsert(state.vms, msg.vm),
        // Land on the new VM so its start progress is in view.
        selectedVmId: msg.vm.id,
      };
    case "error":
      return { ...state, banner: { kind: "error", text: msg.message, source: "action" } };
    case "dismissBanner":
      return { ...state, banner: null };
    case "select":
      return { ...state, selectedVmId: msg.id };
  }
}

const initialState: Dashboard = {
  vms: [],
  busy: new Set(),
  banner: null,
  loaded: false,
  consecutiveFailures: 0,
  selectedVmId: null,
};

export default function App() {
  const { t } = useI18n();
  const [state, dispatch] = useReducer(reduce, initialState);
  // Generation token so React StrictMode's mount→unmount→remount does not
  // leave a stuck "in flight" flag that skips the real first list fetch.
  const refreshGen = useRef(0);
  const [pipeHasUnsavedChanges, setPipeHasUnsavedChanges] = useState(false);
  const [paletteOpen, setPaletteOpen] = useState(false);
  const [detailOpen, setDetailOpen] = useDetailPanelOpen();
  const [detailWidth, setDetailWidth] = useDetailPanelWidth();
  const [secretsHasUnsavedChanges, setSecretsHasUnsavedChanges] = useState(false);
  const confirmLeave = useCallback(() => !(pipeHasUnsavedChanges || secretsHasUnsavedChanges) || window.confirm(
    secretsHasUnsavedChanges
      ? t("Secrets changes have not been saved or are still processing. Leave this page?", "Secrets 변경을 저장하지 않았거나 처리 중이다. 이동할까요?")
      : t("Pipe changes have not been saved. Leave without saving?", "Pipe 변경을 저장하지 않았다. 저장하지 않고 이동할까요?"),
  ), [pipeHasUnsavedChanges, secretsHasUnsavedChanges, t]);
  const { route, selectView, closeConsole } = useAppRoute(confirmLeave);

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

  const onSelectVm = useCallback((id: string | null) => dispatch({ type: "select", id }), []);
  // The palette opens a VM by selecting it on the MicroVM screen.
  const onPaletteSelectVm = useCallback((id: string) => {
    dispatch({ type: "select", id });
    setDetailOpen(true);
  }, [setDetailOpen]);
  const onToggleDetail = useCallback(() => setDetailOpen(!detailOpen), [detailOpen, setDetailOpen]);
  const onOpenPalette = useCallback(() => setPaletteOpen(true), []);

  const pollNote = slowMode
    ? t("API unavailable — retrying every 15s", "API 연결 안 됨 — 15s 간격 재시도")
    : t(`Polling every ${POLL_MILLIS / 1000}s`, `${POLL_MILLIS / 1000}초마다 폴링`);

  const vmErrorCount = state.vms.filter((vm) => vm.state === "error").length;
  const banner = useMemo(
    () => (state.banner ? <BannerView kind={state.banner.kind} text={state.banner.text} onDismiss={dismiss} /> : null),
    [state.banner, dismiss],
  );
  const screenStatus = useMemo(
    () => ({ banner, pollNote, detailOpen, setDetailOpen, detailWidth, setDetailWidth }),
    [banner, pollNote, detailOpen, setDetailOpen, detailWidth, setDetailWidth],
  );

  // Terminal owns the whole viewport — no shell chrome, no modal clip box.
  if (route.kind === "console") {
    return <Console vmId={route.vmId} onClose={closeConsole} />;
  }

  const view = route.kind === "vm-new" ? "vms" : route.kind === "pipe-card" ? "pipe" : route.view;

  return (
    <HostStatusProvider>
    <ScreenStatus.Provider value={screenStatus}>
    <Shell view={view} onSelectView={selectView} onOpenPalette={onOpenPalette} vmCount={state.vms.length} vmErrorCount={vmErrorCount}>
      {route.kind === "vm-new" && (
        <Screen view="vms">
          <section className="panel">
            <h2 className="panel-title">
              <span>{t("New MicroVM", "새 MicroVM")}</span>
              <a href={viewHash("vms")}>{t("Back", "뒤로")}</a>
            </h2>
            <CreateVm onCreated={onCreated} onError={onError} />
          </section>
        </Screen>
      )}
      {route.kind === "shell" && route.view === "vms" && (
        <VmScreen
          vms={state.vms}
          loaded={state.loaded}
          busy={state.busy}
          selectedVmId={state.selectedVmId}
          onSelectVm={onSelectVm}
          onAction={onAction}
        />
      )}
      {/* Each page mounts only while selected, so its own polling stops
          the moment you navigate away. */}
      {view === "networks" && <MicroNetworks />}
      {view === "services" && <MicroServices />}
      {view === "secrets" && <Screen view="secrets"><Secrets onUnsavedChange={setSecretsHasUnsavedChanges} /></Screen>}
      {view === "pipe" && (
        <Pipe card={route.kind === "pipe-card" ? route.card : null} vms={state.vms} vmsLoading={!state.loaded} vmsUnavailable={state.consecutiveFailures > 0} onUnsavedChange={setPipeHasUnsavedChanges} />
      )}
      {view === "storages" && <MicroStorages />}
      {view === "images" && <Images />}
      {view === "kernels" && <Screen view="kernels"><Kernels /></Screen>}
      {view === "shells" && <Shells />}
      {view === "host" && (
        <Screen view="host">
          <HostInfo />
          <FirecrabPanel />
        </Screen>
      )}
      <CommandPalette
        open={paletteOpen}
        onOpenChange={setPaletteOpen}
        vms={state.vms}
        onSelectView={selectView}
        onSelectVm={onPaletteSelectVm}
        onToggleDetail={onToggleDetail}
      />
    </Shell>
    </ScreenStatus.Provider>
    </HostStatusProvider>
  );
}

function confirmDelete(t: (english: string, korean: string) => string): boolean {
  return window.confirm(t("Delete this VM record and its disk?", "VM 레코드와 디스크를 삭제할까요?"));
}
