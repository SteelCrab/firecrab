import { useCallback, useEffect, useRef, useState } from "react";
import type { ImageResponse, MicroNetworkResponse, ShellResponse, StorageRootResponse, VmResponse } from "../../../bindings";
import { getVm, getVmLog, listImages, listMicroNetworks, listShells, listStorageRoots } from "../../../api/client";
import { STARTUP_STEPS, STARTUP_STEP_LOG_LINE, logTimestamp } from "../startup";

/** Start and stop progress lasts seconds, so transitions are polled fast. */
const FAST_POLL_MILLIS = 750;
/** A settled VM changes no faster than the list poll notices. */
const SLOW_POLL_MILLIS = 3_000;

export interface VmDetailPolling {
  /** The VM is starting or stopping. */
  fast: boolean;
  /** The log is on screen; it can be large, so it is not read otherwise. */
  withLog: boolean;
}

export interface VmDetailData {
  /** Latest answer from `GET /api/vms/<id>`; `null` until the first one. */
  vm: VmResponse | null;
  consoleLog: string;
  /** Lines derived from the start steps seen while polling. */
  pipelineLines: string[];
  /** Index into `STARTUP_STEPS` of the furthest step this attempt reached. */
  highestStepSeen: number;
  networks: MicroNetworkResponse[];
  storageRoots: StorageRootResponse[];
  shells: ShellResponse[];
  images: ImageResponse[];
  /** Adopts a VM returned by a save without waiting for the next poll. */
  setVm: (vm: VmResponse) => void;
  reloadShells: () => void;
}

/**
 * Polls one VM — every 750ms while it starts or stops, at the list's pace
 * otherwise — and its console log only while the log is shown. Also loads
 * the catalogs the detail resolves ids against; a failed load leaves the raw
 * ids on screen. Mount one per VM (the panel is keyed by VM id).
 */
export function useVmDetail(vmId: string, { fast, withLog }: VmDetailPolling): VmDetailData {
  const [vm, setVm] = useState<VmResponse | null>(null);
  const [consoleLog, setConsoleLog] = useState("");
  const [pipelineLines, setPipelineLines] = useState<string[]>([]);
  const [highestStepSeen, setHighestStepSeen] = useState(-1);
  const [networks, setNetworks] = useState<MicroNetworkResponse[]>([]);
  const [storageRoots, setStorageRoots] = useState<StorageRootResponse[]>([]);
  const [shells, setShells] = useState<ShellResponse[]>([]);
  const [images, setImages] = useState<ImageResponse[]>([]);

  const reloadShells = useCallback(() => {
    listShells().then(setShells).catch(() => setShells([]));
  }, []);

  useEffect(() => {
    listMicroNetworks().then(setNetworks).catch(() => setNetworks([]));
    listStorageRoots().then(setStorageRoots).catch(() => setStorageRoots([]));
    listImages().then(setImages).catch(() => setImages([]));
    reloadShells();
  }, [reloadShells]);

  // `startup_step` resets to `null` on every transition out of Starting,
  // including the successful one — so "how far did this attempt get" has to
  // be remembered here, not read back off the server after the fact. Refs,
  // so a polling restart (tab or pace change) keeps the progress.
  const progress = useRef({ wasStarting: false, seen: -1, lines: [] as string[] });

  useEffect(() => {
    let cancelled = false;
    let timer: ReturnType<typeof setTimeout> | undefined;

    const tick = async () => {
      try {
        const [nextVm, log] = await Promise.all([getVm(vmId), withLog ? getVmLog(vmId) : Promise.resolve(null)]);
        if (cancelled) return;
        let { wasStarting, seen, lines } = progress.current;

        // A fresh start (including a restart while the panel is open) gets a fresh pipeline log.
        if (nextVm.state === "starting" && !wasStarting) {
          seen = -1;
          lines = [];
        }
        wasStarting = nextVm.state === "starting";

        if (nextVm.startupStep) {
          const index = STARTUP_STEPS.indexOf(nextVm.startupStep);
          for (let i = seen + 1; i <= index; i++) {
            lines = [...lines, `[${logTimestamp()}] ${STARTUP_STEP_LOG_LINE[STARTUP_STEPS[i]]}`];
          }
          seen = Math.max(seen, index);
        } else if (nextVm.state === "running" && seen < STARTUP_STEPS.length - 1) {
          seen = STARTUP_STEPS.length - 1;
          lines = [...lines, `[${logTimestamp()}] Ready — VM started.`];
        }

        progress.current = { wasStarting, seen, lines };
        setVm(nextVm);
        if (log) setConsoleLog(log.consoleLog);
        setPipelineLines(lines);
        setHighestStepSeen(seen);
      } catch {
        // Transient poll miss — keep the last known state, try again next tick.
      }
      if (!cancelled) timer = setTimeout(tick, fast ? FAST_POLL_MILLIS : SLOW_POLL_MILLIS);
    };

    tick();
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, [vmId, fast, withLog]);

  return { vm, consoleLog, pipelineLines, highestStepSeen, networks, storageRoots, shells, images, setVm, reloadShells };
}
