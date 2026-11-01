import {
  useCallback,
  useEffect,
  useMemo,
  useReducer,
  useRef,
  useState,
} from "react";
import type { VmResponse } from "./bindings";
import {
  deleteVm,
  listVms,
  startVm,
  stopVm,
  pauseVm,
  resumeVm,
} from "./api/client";
import type { VmAction } from "./model";
import BannerView from "./components/Banner";
import CreateVm from "./components/CreateVm";
import Console from "./components/Console";
import VmScreen from "./components/vm/VmScreen";
import FirecrabPanel from "./components/FirecrabPanel";
import HostInfo from "./components/HostInfo";
import Cluster from "./components/Cluster";
import AiSandbox from "./components/AiSandbox";
import Images from "./components/Images";
import Kernels from "./components/Kernels";
import Queues from "./components/Queues";
import Schedules from "./components/Schedules";
import Pools from "./components/Pools";
import Secrets from "./components/Secrets";
import Stacks from "./components/Stacks";
import Volumes from "./components/Volumes";
import MicroNetworks from "./components/MicroNetworks";
import SecurityGroups from "./components/SecurityGroups";
import MicroStorages from "./components/MicroStorages";
import MicroServices from "./components/MicroServices";
import Shell from "./components/Shell";
import Pipe from "./components/Pipe";
import Shells from "./components/Shells";
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
  kind: "error";
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
  | { type: "created"; vm: VmResponse }
  | { type: "error"; message: string }
  | { type: "dismissBanner" }
  | { type: "select"; id: string | null };

/** Keeps the server's list order: name ascending, ties by id. */
function upsert(vms: VmResponse[], vm: VmResponse): VmResponse[] {
  const exists = vms.some((existing) => existing.id === vm.id);
  const next = exists
    ? vms.map((existing) => (existing.id === vm.id ? vm : existing))
    : [...vms, vm];
  return [...next].sort(
    (a, b) => a.name.localeCompare(b.name) || a.id.localeCompare(b.id),
  );
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
      const vms = msg.vm
        ? upsert(state.vms, msg.vm)
        : state.vms.filter((vm) => vm.id !== msg.id);
      const selectedVmId =
        !msg.vm && state.selectedVmId === msg.id ? null : state.selectedVmId;
      return { ...state, busy, vms, selectedVmId };
    }
    case "actionFailed": {
      const busy = new Set(state.busy);
      busy.delete(msg.id);
      return {
        ...state,
        busy,
        banner: { kind: "error", text: msg.message, source: "action" },
      };
    }
    case "created":
      return {
        ...state,
        banner: null,
        vms: upsert(state.vms, msg.vm),
        // Land on the new VM so its start progress is in view.
        selectedVmId: msg.vm.id,
      };
    case "error":
      return {
        ...state,
        banner: { kind: "error", text: msg.message, source: "action" },
      };
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
  const unsaved = useRef(false);
  const [hasUnsavedChanges, setUnsavedState] = useState(false);
  const setHasUnsavedChanges = useCallback((dirty: boolean) => {
    unsaved.current = dirty;
    setUnsavedState(dirty);
  }, []);
  useEffect(() => {
    if (!hasUnsavedChanges) return;
    const leave = (event: BeforeUnloadEvent) => {
      if (unsaved.current) {
        event.preventDefault();
        event.returnValue = "";
      }
    };
    window.addEventListener("beforeunload", leave);
    return () => window.removeEventListener("beforeunload", leave);
  }, [hasUnsavedChanges]);
  const [paletteOpen, setPaletteOpen] = useState(false);
  const [detailOpen, setDetailOpen] = useDetailPanelOpen();
  const [detailWidth, setDetailWidth] = useDetailPanelWidth();
  const confirmLeave = useCallback(
    () =>
      !unsaved.current ||
      window.confirm(
        t(
          "Changes have not been saved. Leave without saving?",
          "저장하지 않은 변경 사항이 있습니다. 저장하지 않고 이동할까요?",
        ),
      ),
    [t],
  );
  const { route, selectView, closeConsole } = useAppRoute(confirmLeave);
  const routedVm = useRef("");
  useEffect(() => {
    if (route.kind !== "shell" || route.view !== "vms" || !route.vmId) {
      routedVm.current = "";
      return;
    }
    if (!state.loaded || routedVm.current === route.vmId) return;
    if (state.vms.some((vm) => vm.id === route.vmId)) {
      routedVm.current = route.vmId;
      dispatch({ type: "select", id: route.vmId });
      setDetailOpen(true);
    }
  }, [route, state.loaded, state.vms, setDetailOpen]);

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
      const target = state.vms.find((vm) => vm.id === id);
      if (!target || (target.purpose && target.purpose !== "instance")) return;
      if (action === "delete" && !confirmDelete(t)) return;

      dispatch({ type: "actionStarted", id });
      (async () => {
        try {
          let vm: VmResponse | null;
          if (action === "start") vm = await startVm(id);
          else if (action === "stop") vm = await stopVm(id);
          else if (action === "pause") vm = await pauseVm(id);
          else if (action === "resume") vm = await resumeVm(id);
          else {
            await deleteVm(id);
            vm = null;
          }
          dispatch({ type: "actionSucceeded", id, vm });
        } catch (error) {
          dispatch({
            type: "actionFailed",
            id,
            message: (error as Error).message,
          });
          // 409 means our view was stale, and a failed start leaves the VM
          // in error state — resync right away.
          runRefresh();
        }
      })();
    },
    [state.busy, state.vms, runRefresh, t],
  );

  const onCreated = useCallback((vm: VmResponse) => {
    dispatch({ type: "created", vm });
    window.location.hash = viewHash("vms");
  }, []);
  const onError = useCallback(
    (message: string) => dispatch({ type: "error", message }),
    [],
  );
  const dismiss = useCallback(() => dispatch({ type: "dismissBanner" }), []);

  const onSelectVm = useCallback(
    (id: string | null) => {
      if (id !== state.selectedVmId && !confirmLeave()) return false;
      dispatch({ type: "select", id });
      return true;
    },
    [state.selectedVmId, confirmLeave],
  );
  // The palette opens a VM by selecting it on the MicroVM screen.
  const onPaletteSelectVm = useCallback(
    (id: string) => {
      const sameVm =
        route.kind === "shell" &&
        route.view === "vms" &&
        state.selectedVmId === id;
      if (!sameVm && !confirmLeave()) return;
      if (!sameVm) setHasUnsavedChanges(false);
      dispatch({ type: "select", id });
      setDetailOpen(true);
      selectView("vms");
    },
    [
      route,
      state.selectedVmId,
      confirmLeave,
      setHasUnsavedChanges,
      setDetailOpen,
      selectView,
    ],
  );
  const onOpenPalette = useCallback(() => setPaletteOpen(true), []);

  const pollNote = slowMode
    ? t(
        "API unavailable — retrying every 15s",
        "API 연결 안 됨 — 15s 간격 재시도",
      )
    : "";

  const visibleVms = state.vms.filter((vm) => vm.purpose !== "builder");
  const instances = visibleVms.filter(
    (vm) => !vm.purpose || vm.purpose === "instance",
  );
  const vmErrorCount = visibleVms.filter((vm) => vm.state === "error").length;
  const banner = useMemo(
    () =>
      state.banner ? (
        <BannerView
          kind={state.banner.kind}
          text={state.banner.text}
          onDismiss={dismiss}
        />
      ) : null,
    [state.banner, dismiss],
  );
  const screenStatus = useMemo(
    () => ({
      banner,
      pollNote,
      detailOpen,
      setDetailOpen,
      detailWidth,
      setDetailWidth,
    }),
    [banner, pollNote, detailOpen, setDetailOpen, detailWidth, setDetailWidth],
  );

  // Terminal owns the whole viewport — no shell chrome, no modal clip box.
  if (route.kind === "console") {
    return (
      <Console key={route.vmId} vmId={route.vmId} onClose={closeConsole} />
    );
  }

  const view =
    route.kind === "vm-new"
      ? "vms"
      : route.kind === "pipe-card"
        ? "pipe"
        : route.view;

  return (
    <HostStatusProvider>
      <ScreenStatus.Provider value={screenStatus}>
        <Shell
          view={view}
          onSelectView={selectView}
          onOpenPalette={onOpenPalette}
          vmCount={visibleVms.length}
          vmErrorCount={vmErrorCount}
        >
          {route.kind === "vm-new" && (
            <Screen
              view="vms"
              heading={t("New MicroVM", "새 MicroVM")}
              parent="vms"
              flush
            >
              <section className="form-page">
                <header>
                  <p>
                    {t(
                      "Pick an image and resources. The MicroVM starts in a few seconds.",
                      "이미지와 사양을 선택하면 몇 초 안에 시작할 수 있습니다.",
                    )}
                  </p>
                </header>
                <CreateVm onCreated={onCreated} onError={onError} />
              </section>
            </Screen>
          )}
          {route.kind === "shell" && route.view === "vms" && (
            <VmScreen
              vms={visibleVms}
              loaded={state.loaded}
              busy={state.busy}
              selectedVmId={state.selectedVmId}
              onSelectVm={onSelectVm}
              onAction={onAction}
              onUnsavedChange={setHasUnsavedChanges}
            />
          )}
          {/* Each page mounts only while selected, so its own polling stops
          the moment you navigate away. */}
          {route.kind === "shell" && view === "schedules" && (
            <Schedules
              vms={state.vms.filter(
                (vm) => !vm.purpose || vm.purpose === "instance",
              )}
              vmsLoaded={state.loaded}
              vmId={route.vmId}
              create={route.createSchedule}
              scheduleId={route.scheduleId}
            />
          )}
          {view === "networks" && (
            <MicroNetworks onUnsavedChange={setHasUnsavedChanges} />
          )}
          {view === "security-groups" && (
            <SecurityGroups
              vms={visibleVms}
              vmsAvailable={state.consecutiveFailures === 0}
            />
          )}
          {view === "services" && (
            <MicroServices
              creating={route.kind === "shell" && !!route.createService}
              onUnsavedChange={setHasUnsavedChanges}
            />
          )}
          {view === "pipe" && (
            <Pipe
              card={route.kind === "pipe-card" ? route.card : null}
              vms={instances}
              vmsLoading={!state.loaded}
              vmsUnavailable={state.consecutiveFailures > 0}
              onUnsavedChange={setHasUnsavedChanges}
            />
          )}
          {view === "storages" && (
            <MicroStorages onUnsavedChange={setHasUnsavedChanges} />
          )}
          {view === "queues" && <Queues />}
          {view === "volumes" && <Volumes vms={visibleVms} />}
          {view === "ai-sandbox" && (
            <AiSandbox vms={instances} onUnsavedChange={setHasUnsavedChanges} />
          )}
          {view === "cluster" && <Cluster />}
          {view === "stacks" && <Stacks />}
          {view === "secrets" && (
            <Screen view="secrets" live={false}>
              <Secrets onUnsavedChange={setHasUnsavedChanges} />
            </Screen>
          )}
          {view === "pools" && (
            <Screen view="pools" live={false}>
              <Pools />
            </Screen>
          )}
          {view === "images" && <Images />}
          {view === "kernels" && (
            <Screen view="kernels">
              <Kernels />
            </Screen>
          )}
          {view === "shells" && (
            <Shells onUnsavedChange={setHasUnsavedChanges} />
          )}
          {view === "host" && (
            <Screen view="host">
              <HostInfo>
                <FirecrabPanel />
              </HostInfo>
            </Screen>
          )}
          <CommandPalette
            open={paletteOpen}
            onOpenChange={setPaletteOpen}
            vms={visibleVms}
            onSelectView={selectView}
            onSelectVm={onPaletteSelectVm}
          />
        </Shell>
      </ScreenStatus.Provider>
    </HostStatusProvider>
  );
}

function confirmDelete(
  t: (english: string, korean: string) => string,
): boolean {
  return window.confirm(
    t("Delete this VM record and its disk?", "VM과 디스크를 삭제할까요?"),
  );
}
