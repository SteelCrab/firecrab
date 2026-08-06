import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { BootstrapResponse, BuildResponse, ImageInstallResponse, ImageResponse, VmResponse } from "../bindings";
import {
  ApiClientError,
  buildPackages,
  cancelBootstrap,
  cancelBuild,
  deleteImage,
  deleteVm,
  finalizeBuild,
  getBootstrap,
  getBuild,
  getImageInstall,
  getImagePackage,
  listImages,
  listVms,
  startBootstrap,
  startBuild,
  startImageInstall,
  startImagePackage,
  stopVm,
} from "../api/client";
import { logDownloadFilename } from "../lib/textExport";
import LogExportActions from "./LogExportActions";

const KNOWN_TEMPLATES = [
  { alias: "alpine-3.24", label: "Alpine Linux", logoSrc: "https://www.alpinelinux.org/alpinelinux-logo.svg" },
  { alias: "ubuntu-26.04", label: "Ubuntu", logoSrc: "https://assets.ubuntu.com/v1/ff6a9a38-ubuntu-logo-2022.svg" },
  { alias: "rocky-9", label: "Rocky Linux", logoSrc: "https://raw.githubusercontent.com/rocky-linux/branding/main/logo/src/icon-primary.svg" },
] as const;

/** Last path segment of an official package URL for the table cell. */
function packageBasename(url: string): string {
  try {
    const path = new URL(url).pathname;
    const seg = path.split("/").filter(Boolean).pop();
    return seg || url;
  } catch {
    return url;
  }
}

/** Human size for the real rootfs artifact (not the ceiled min-disk floor). */
function formatRootfsSize(bytes: number | undefined | null): string {
  const n = typeof bytes === "number" ? bytes : Number(bytes);
  if (!Number.isFinite(n) || n <= 0) return "—";
  const gib = n / 1024 ** 3;
  if (gib >= 1) {
    const rounded = gib >= 10 || Number.isInteger(gib) ? gib.toFixed(0) : gib.toFixed(2);
    return `${rounded} GiB`;
  }
  const mib = n / 1024 ** 2;
  const rounded = mib >= 10 || Number.isInteger(mib) ? mib.toFixed(0) : mib.toFixed(1);
  return `${rounded} MiB`;
}

/**
 * A poll may have left the browser before the POST that starts a newer job.
 * Do not let that older `idle`/`running` response erase the state returned by
 * the POST (or a terminal state for the same job).
 */
function keepNewestJobSnapshot(
  current: ImageInstallResponse | null | undefined,
  incoming: ImageInstallResponse,
): ImageInstallResponse {
  if (!current) return incoming;
  if (incoming.status === "idle" && current.status !== "idle") return current;
  const currentStarted = current.startedAtMs;
  const incomingStarted = incoming.startedAtMs;
  if (currentStarted !== undefined && incomingStarted !== undefined && incomingStarted < currentStarted) {
    return current;
  }
  const currentIsTerminal = current.status === "succeeded" || current.status === "failed";
  if (currentStarted !== undefined && incomingStarted === currentStarted && currentIsTerminal && incoming.status === "running") {
    return current;
  }
  return incoming;
}

/**
 * Build modal: boot a builder VM off `sourceAlias`, install/remove packages
 * on its console, then save the result as a new or updated template.
 */
function BuildModal({
  sourceAlias,
  installedAliases,
  onClose,
  onFinalized,
}: {
  sourceAlias: string;
  installedAliases: string[];
  onClose: () => void;
  onFinalized: () => void;
}) {
  const [build, setBuild] = useState<BuildResponse | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [installInput, setInstallInput] = useState("");
  const [removeInput, setRemoveInput] = useState("");
  const [busy, setBusy] = useState(false);
  const [saveMode, setSaveMode] = useState<"update" | "derive">("update");
  const [newAlias, setNewAlias] = useState("");

  // A build session owns a real (hidden) VM, so it must never be left
  // running with nothing tracking it. These refs carry that ownership
  // outside React state, which the effect-scoped `cancelled` flags below
  // deliberately stop updating once the modal is gone.
  const buildIdRef = useRef<string | null>(null);
  /** The session no longer needs tearing down (cancelled, or finalized). */
  const settledRef = useRef(false);
  /** The modal is gone; nothing will render or poll this session again. */
  const closedRef = useRef(false);

  /**
   * Claims responsibility for tearing the session down, returning its id
   * once. Stays un-settled while there is no id yet, so a close that races
   * `startBuild`'s own response hands the job to that response instead of
   * swallowing it.
   */
  const disownBuild = useCallback(() => {
    if (settledRef.current || !buildIdRef.current) return null;
    settledRef.current = true;
    return buildIdRef.current;
  }, []);

  useEffect(
    () => () => {
      closedRef.current = true;
      // Unmount safety net: closing the modal any way other than 취소 (an
      // App-shell tab switch, say) would otherwise strand the builder VM.
      const orphaned = disownBuild();
      if (orphaned) void cancelBuild(orphaned).catch(() => undefined);
    },
    [disownBuild],
  );

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const started = await startBuild(sourceAlias);
        buildIdRef.current = started.buildId;
        if (cancelled || closedRef.current) {
          // The modal closed while this POST was still in flight, so the
          // unmount cleanup above ran before there was any id to cancel.
          const orphaned = disownBuild();
          if (orphaned) void cancelBuild(orphaned).catch(() => undefined);
          return;
        }
        setBuild(started);
      } catch (err) {
        if (!cancelled) setError((err as Error).message);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [sourceAlias, disownBuild]);

  useEffect(() => {
    if (!build || build.status === "succeeded" || build.status === "failed") return;
    let cancelled = false;
    const timer = setTimeout(async () => {
      try {
        const snapshot = await getBuild(build.buildId);
        if (!cancelled) setBuild(snapshot);
      } catch {
        /* keep last snapshot */
      }
    }, 1000);
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, [build]);

  // Every long step (boot, package install, finalize) reports through the
  // session rather than its own HTTP response, so the terminal state only
  // ever arrives via the poll above — including the one that closes this
  // modal after a successful save.
  const finishedRef = useRef(false);
  useEffect(() => {
    if (finishedRef.current || !build) return;
    if (build.status === "succeeded") {
      finishedRef.current = true;
      // finalize already stopped and deleted the builder VM.
      settledRef.current = true;
      onFinalized();
      onClose();
    } else if (build.status === "failed") {
      finishedRef.current = true;
      setBusy(false);
      // Left un-settled on purpose: a failed session still has a builder VM
      // record behind it, which the unmount cleanup should remove.
    }
  }, [build, onFinalized, onClose]);

  const runPackages = async (action: "install" | "remove", input: string) => {
    if (!build) return;
    const packages = input.split(/\s+/).filter(Boolean);
    if (packages.length === 0) return;
    setBusy(true);
    setError(null);
    try {
      // Returns the `installing` snapshot, not the finished one — the guest's
      // package manager runs for minutes. The poll above shows its output and
      // flips back to `ready` when it's done.
      const accepted = await buildPackages(build.buildId, action, packages);
      setBuild(accepted);
      if (action === "install") setInstallInput("");
      if (action === "remove") setRemoveInput("");
    } catch (err) {
      setError((err as Error).message);
    } finally {
      setBusy(false);
    }
  };

  const handleFinalize = async () => {
    if (!build) return;
    if (saveMode === "derive" && !newAlias.trim()) {
      setError("새 이미지 이름을 입력하세요.");
      return;
    }
    setBusy(true);
    setError(null);
    try {
      // Accepted, not finished: copying a multi-GB rootfs, fscking it and
      // registering it all happen after this responds. Stay open on the live
      // status — the terminal effect above closes the modal once the session
      // actually reaches `succeeded`.
      const accepted = await finalizeBuild(
        build.buildId,
        saveMode === "derive" ? newAlias.trim() : undefined,
      );
      setBuild(accepted);
    } catch (err) {
      setError((err as Error).message);
      setBusy(false);
    }
  };

  const handleCancel = async () => {
    // The overlay's own click lands here, so an accidental click outside the
    // modal must not silently destroy work — confirm the same way the image
    // delete action does, but only once there is something to lose.
    const atRisk = build !== null && (build.hadPackageAction || build.status !== "booting");
    if (
      atRisk &&
      !window.confirm("이 빌드를 취소할까요?\n빌더 VM을 삭제하며, 설치한 패키지는 저장되지 않습니다.")
    ) {
      return;
    }
    const orphaned = disownBuild();
    if (orphaned) {
      try {
        await cancelBuild(orphaned);
      } catch {
        /* best-effort */
      }
    }
    onClose();
  };

  const ready = build?.status === "ready";
  const saving = build?.status === "finalizing";
  const aliasTaken = newAlias.trim().length > 0 && installedAliases.includes(newAlias.trim());

  return (
    <div className="modal-overlay" onClick={handleCancel}>
      <div className="modal" onClick={(event) => event.stopPropagation()}>
        <h2 className="panel-title">M2Image-builder — {sourceAlias}</h2>
        {error && <div className="field-error">{error}</div>}
        <div className="state-badge">{build?.status ?? "시작 중…"}</div>
        <pre className="detail-log">{build?.log ?? ""}</pre>
        <div className="package-row">
          <input
            type="text"
            placeholder="설치할 패키지 (공백으로 구분)"
            value={installInput}
            onChange={(event) => setInstallInput(event.target.value)}
            disabled={!ready || busy}
          />
          <button type="button" className="btn" disabled={!ready || busy || !installInput.trim()} onClick={() => void runPackages("install", installInput)}>
            설치
          </button>
        </div>
        <div className="package-row">
          <input
            type="text"
            placeholder="삭제할 패키지 (공백으로 구분)"
            value={removeInput}
            onChange={(event) => setRemoveInput(event.target.value)}
            disabled={!ready || busy}
          />
          <button type="button" className="btn danger" disabled={!ready || busy || !removeInput.trim()} onClick={() => void runPackages("remove", removeInput)}>
            삭제
          </button>
        </div>
        <fieldset className="package-row">
          <label>
            <input type="radio" checked={saveMode === "update"} onChange={() => setSaveMode("update")} />
            같은 이미지 갱신 ({sourceAlias})
          </label>
          <label>
            <input type="radio" checked={saveMode === "derive"} onChange={() => setSaveMode("derive")} />
            새 이미지로 저장
          </label>
          {saveMode === "derive" && (
            <input
              type="text"
              placeholder="새 이미지 이름"
              value={newAlias}
              onChange={(event) => setNewAlias(event.target.value)}
            />
          )}
        </fieldset>
        {aliasTaken && <div className="field-error">이미 사용 중인 이름입니다.</div>}
        <div className="package-row">
          <button type="button" className="btn" disabled={saving} onClick={() => void handleCancel()}>
            취소
          </button>
          <button type="button" className="btn primary" disabled={!ready || busy || aliasTaken} onClick={() => void handleFinalize()}>
            {saving || busy ? "저장 중…" : "이미지로 저장"}
          </button>
        </div>
      </div>
    </div>
  );
}

/**
 * Bootstrap panel: triggers a from-scratch distro bootstrap and shows its
 * live log. Only one bootstrap runs at a time (backend-enforced, 409 on a
 * second start) — this component's own busy state mirrors that by polling
 * `getBootstrap` and disabling every alias's button while any session it
 * knows about is non-terminal.
 */
function BootstrapPanel({
  onFinished,
  unavailableAliases,
}: {
  onFinished: () => void;
  /** Already installed, or already holding a staged package — either way
   *  `POST /api/images/{alias}/install`'s own guard would reject a bootstrap
   *  result for it, so don't offer to spend 30 minutes producing one. */
  unavailableAliases: string[];
}) {
  const [session, setSession] = useState<BootstrapResponse | null>(null);
  const [error, setError] = useState<string | null>(null);
  /**
   * True from the click itself, not from the response. `busy` below only
   * becomes true once `startBootstrap` has resolved, which leaves a window
   * where a double-click fires a second POST — and two POSTs that both pass
   * the backend's single-session check boot two builder VMs.
   */
  const [starting, setStarting] = useState(false);

  // BootstrapPanel lives inside Images, which the App shell only mounts
  // while the "images" tab is active — a bootstrap can run for minutes, so
  // its poll (and even the initial startBootstrap POST) can easily outlive
  // the component if the user switches tabs mid-run. Mirror Images's own
  // mountedRef/pollInstall discipline (see above) rather than the
  // effect-scoped `cancelled` flag, since a bare `cancelled` flag only
  // guards the one scheduled tick it closes over, not the recursive chain
  // of ticks a real bootstrap session needs.
  const mountedRef = useRef(true);
  /**
   * The live session's id, held outside React state so the unmount cleanup
   * can still reach it — mirrors `BuildModal`'s `buildIdRef`/`settledRef`
   * pair. Cleared as soon as the session settles on its own, so a finished
   * bootstrap is never "cancelled" retroactively.
   */
  const bootstrapIdRef = useRef<string | null>(null);
  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
      // Navigating away mid-bootstrap would otherwise leave a session (and
      // its builder VM) running with nothing in the UI tracking it.
      const orphaned = bootstrapIdRef.current;
      bootstrapIdRef.current = null;
      if (orphaned) void cancelBootstrap(orphaned).catch(() => undefined);
    };
  }, []);

  const pollBootstrap = (bootstrapId: string) => {
    const tick = async () => {
      if (!mountedRef.current) return;
      try {
        const snapshot = await getBootstrap(bootstrapId);
        if (!mountedRef.current) return;
        setSession(snapshot);
        if (snapshot.status === "succeeded") {
          // Settled: the backend already stopped, packaged and deleted the
          // builder VM, so there is nothing left for unmount to cancel.
          bootstrapIdRef.current = null;
          onFinished();
        } else if (snapshot.status !== "failed") {
          setTimeout(() => void tick(), 1000);
        } else {
          // Failed sessions have already torn their builder VM down too.
          bootstrapIdRef.current = null;
        }
        // "failed" is a confirmed terminal state too — stop without retrying.
      } catch {
        // A fetch failure is not positive confirmation the job reached a
        // terminal state, so keep polling rather than freezing the log on a
        // one-off network blip (unless the component is already gone).
        if (mountedRef.current) setTimeout(() => void tick(), 1000);
      }
    };
    void tick();
  };

  const start = async (alias: string) => {
    if (starting) return;
    setStarting(true);
    setError(null);
    try {
      const started = await startBootstrap(alias);
      bootstrapIdRef.current = started.bootstrapId;
      if (!mountedRef.current) {
        // Unmounted while the POST was in flight, so the cleanup above ran
        // before there was an id to cancel — hand it back here.
        bootstrapIdRef.current = null;
        void cancelBootstrap(started.bootstrapId).catch(() => undefined);
        return;
      }
      setSession(started);
      pollBootstrap(started.bootstrapId);
    } catch (err) {
      if (!mountedRef.current) return;
      setError((err as Error).message);
    } finally {
      if (mountedRef.current) setStarting(false);
    }
  };

  const busy =
    starting || (session !== null && session.status !== "succeeded" && session.status !== "failed");

  return (
    <section className="panel">
      <h2
        className="panel-title"
        title="공식 배포판을 처음부터 준비할 때 보통 docker나 sudo가 필요한데, 여기서는 그 대신 이미 있는 microVM 빌더를 재사용합니다 — 게스트 안에서는 이미 진짜 root라 별도 컨테이너나 권한 상승 없이 안전하게 처리됩니다."
      >
        배포판 부트스트랩
      </h2>
      {error && <div className="field-error">{error}</div>}
      <div className="package-row">
        {(["alpine-3.24", "ubuntu-26.04", "rocky-9"] as const).map((alias) => {
          const unavailable = unavailableAliases.includes(alias);
          return (
            <button
              key={alias}
              type="button"
              className="btn"
              disabled={busy || unavailable}
              title={unavailable ? "이미 설치됐거나 설치할 패키지가 이미 준비되어 있습니다." : undefined}
              onClick={() => void start(alias)}
            >
              {busy && session?.alias === alias ? `${alias} 부트스트랩 중…` : `${alias} 부트스트랩`}
            </button>
          );
        })}
      </div>
      {session && (
        <>
          <div className="state-badge">{session.status}</div>
          <pre className="detail-log">{session.log}</pre>
        </>
      )}
    </section>
  );
}

/**
 * Single M2Image inventory table plus an on-demand build modal. Package
 * download/install ("가져오기") is a per-row action here rather than a
 * separate panel — the two-stage Packer/Store split moved into BuildModal.
 */
export default function Images() {
  const [images, setImages] = useState<ImageResponse[] | null>(null);
  const [listError, setListError] = useState<string | null>(null);
  const [busyAlias, setBusyAlias] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  const [packageJobs, setPackageJobs] = useState<Record<string, ImageInstallResponse>>({});
  const [install, setInstall] = useState<ImageInstallResponse | null>(null);
  const [buildSourceAlias, setBuildSourceAlias] = useState<string | null>(null);
  // Deliberately not seeded from KNOWN_TEMPLATES: which templates are
  // actually installed is only known once `images` loads, and a `<select>`
  // whose value isn't among its options silently renders the first option
  // instead — so a hardcoded default would start a build against an alias
  // the user never saw (and the API 404s on).
  const [newBuildSource, setNewBuildSource] = useState<string>("");

  const refreshList = useCallback(async () => {
    try {
      const next = await listImages();
      setImages(next);
      setListError(null);
    } catch (error) {
      setListError((error as Error).message);
    }
  }, []);

  useEffect(() => {
    void refreshList();
  }, [refreshList]);

  const installedAliases = useMemo(
    () => (images ?? []).filter((image) => image.installed).map((image) => image.alias),
    [images],
  );

  // Bootstrapping either of these would spend ~30 minutes producing a package
  // the install step then refuses (`already_installed`) or that is already
  // sitting on disk waiting to be installed.
  const bootstrapBlockedAliases = useMemo(
    () =>
      (images ?? [])
        .filter((image) => image.installed || image.packageStaged)
        .map((image) => image.alias),
    [images],
  );

  // Keep the build-source selection in step with what is installed: pick the
  // first installed alias once the list arrives, and re-pick if the current
  // choice is deleted out from under it.
  useEffect(() => {
    setNewBuildSource((current) =>
      installedAliases.includes(current) ? current : (installedAliases[0] ?? ""),
    );
  }, [installedAliases]);

  // `Images` is conditionally mounted by the App shell (only while the
  // "images" tab is active), so a poll started here can easily outlive the
  // component if the user navigates away mid-install. Every tick must check
  // this before touching state.
  const mountedRef = useRef(true);
  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
    };
  }, []);

  // `startImageInstall` only kicks the install off — the backend always
  // answers with a single "install started" snapshot and does the real
  // extract/register work in the background, so the result must be polled
  // to a terminal status before the table can show "설치됨".
  const pollInstall = (alias: string) => {
    const tick = async () => {
      if (!mountedRef.current) return;
      try {
        const latest = await getImageInstall(alias);
        if (!mountedRef.current) return;
        setInstall((current) => keepNewestJobSnapshot(current, latest));
        if (latest.status === "running") {
          setTimeout(() => void tick(), 300);
        } else if (latest.status === "succeeded") {
          await refreshList();
        }
        // "failed" is a confirmed terminal state too — stop without retrying.
      } catch {
        // A fetch failure is not positive confirmation the job reached a
        // terminal state, so keep polling rather than freezing the log on a
        // one-off network blip (unless the component is already gone).
        if (mountedRef.current) setTimeout(() => void tick(), 300);
      }
    };
    void tick();
  };

  /**
   * Install straight from an archive already staged on this host — what a
   * finished 배포판 부트스트랩 leaves behind. Deliberately skips
   * `startImagePackage`: there is nothing to download (and on a host with no
   * `FIRECRAB_IMAGE_BASE_URL` there is nowhere to download from), so this
   * goes directly to the same install + poll the remote path ends with.
   */
  const handleInstallStaged = async (alias: string) => {
    setBusyAlias(alias);
    setActionError(null);
    try {
      const started = await startImageInstall(alias);
      setInstall(started);
      pollInstall(alias);
    } catch (error) {
      setActionError((error as Error).message);
    } finally {
      setBusyAlias(null);
    }
  };

  const handleFetchPackage = async (alias: string) => {
    setBusyAlias(alias);
    setActionError(null);
    try {
      const snap = await startImagePackage(alias);
      setPackageJobs((current) => ({ ...current, [alias]: snap }));
      // Same unmount/error discipline as `pollInstall`: this poll can outlive
      // the component (the App shell unmounts `Images` on a tab switch), and
      // an unguarded throw here would become an unhandled rejection that
      // silently ends the poll with the row stuck at "가져오는 중…".
      const poll = async () => {
        if (!mountedRef.current) return;
        try {
          const latest = await getImagePackage(alias);
          if (!mountedRef.current) return;
          setPackageJobs((current) => ({ ...current, [alias]: keepNewestJobSnapshot(current[alias], latest) }));
          if (latest.status === "running") setTimeout(() => void poll(), 500);
          else if (latest.status === "succeeded") {
            const installed = await startImageInstall(alias);
            if (!mountedRef.current) return;
            setInstall(installed);
            pollInstall(alias);
          }
        } catch (error) {
          if (mountedRef.current) setActionError((error as Error).message);
        }
      };
      void poll();
    } catch (error) {
      setActionError((error as Error).message);
    } finally {
      setBusyAlias(null);
    }
  };

  /**
   * Stop (if needed) and delete every VM that still pins this image, so the
   * image delete can proceed entirely from the dashboard.
   */
  const removeVmsUsingImage = async (users: VmResponse[]) => {
    for (const vm of users) {
      if (vm.state === "running" || vm.state === "starting") {
        await stopVm(vm.id);
        // Poll until delete-eligible (stopped / error / created).
        for (let attempt = 0; attempt < 40; attempt++) {
          await new Promise((resolve) => setTimeout(resolve, 250));
          const latest = (await listVms()).find((entry) => entry.id === vm.id);
          if (!latest) break;
          if (latest.state === "stopped" || latest.state === "error" || latest.state === "created") break;
        }
      }
      const latest = (await listVms()).find((entry) => entry.id === vm.id);
      if (!latest) continue;
      if (latest.state === "stopping" || latest.state === "starting") {
        throw new Error(`VM ${latest.name}이(가) 아직 ${latest.state} 상태입니다. 잠시 후 다시 시도하세요.`);
      }
      await deleteVm(latest.id);
    }
  };

  const handleDelete = async (alias: string) => {
    if (!window.confirm(`'${alias}' 이미지를 삭제할까요?\n레지스트리에서 제거하고 디스크 파일을 지웁니다.`)) return;
    setBusyAlias(alias);
    setActionError(null);
    try {
      try {
        await deleteImage(alias);
      } catch (error) {
        const apiError = error instanceof ApiClientError ? error : null;
        if (apiError?.apiError?.code !== "in_use") throw error;
        const users = (await listVms()).filter((vm) => vm.template === alias);
        if (users.length === 0) throw error;
        const lines = users.map((vm) => `· ${vm.name} [${vm.state}]`).join("\n");
        if (!window.confirm(`'${alias}' 이미지를 쓰는 VM ${users.length}개가 있습니다.\n웹에서 해당 VM을 지운 뒤 이미지를 삭제할까요?\n\n${lines}`)) {
          setActionError(`이미지 삭제 취소됨 — 사용 중인 VM: ${users.map((vm) => vm.name).join(", ")}`);
          return;
        }
        await removeVmsUsingImage(users);
        await deleteImage(alias);
      }
      await refreshList();
      if (install?.alias === alias) setInstall(null);
    } catch (error) {
      setActionError((error as Error).message);
    } finally {
      setBusyAlias(null);
    }
  };

  if (images === null && !listError) {
    return <div className="empty">이미지 목록 불러오는 중…</div>;
  }

  return (
    <div className="stack">
      <section className="panel">
        <h2 className="panel-title">M2Image</h2>
        {listError && <div className="field-error">{listError}</div>}
        {actionError && <div className="field-error">{actionError}</div>}
        <table className="vm-table image-table">
          <thead>
            <tr>
              <th>이미지</th>
              <th>크기</th>
              <th>상태</th>
              <th />
            </tr>
          </thead>
          <tbody>
            {(images ?? []).map((image) => {
              const job = packageJobs[image.alias];
              const fetching = job?.status === "running";
              const statusLabel = image.installed
                ? "설치됨"
                : job?.status === "succeeded" || image.packageStaged
                  ? "패키지 준비됨"
                  : "미설치";
              // Derived/web-built templates won't have a KNOWN_TEMPLATES entry —
              // fall back to plain alias text with no logo for those.
              const known = KNOWN_TEMPLATES.find((template) => template.alias === image.alias);
              return (
                <tr key={image.alias}>
                  <td className="mono">
                    {known && <img className="image-template-logo" src={known.logoSrc} alt="" />}
                    {image.alias}
                  </td>
                  <td className="mono">{formatRootfsSize(image.rootfsSizeBytes)}</td>
                  <td>
                    <span className={`state-badge${image.installed ? " running" : ""}`}>{statusLabel}</span>
                  </td>
                  <td className="actions">
                    {image.installed ? (
                      <>
                        <button type="button" className="btn" disabled={busyAlias === image.alias} onClick={() => setBuildSourceAlias(image.alias)}>
                          빌드
                        </button>
                        <button type="button" className="btn danger" disabled={busyAlias === image.alias} onClick={() => void handleDelete(image.alias)}>
                          {busyAlias === image.alias ? "삭제 중…" : "삭제"}
                        </button>
                      </>
                    ) : image.packageStaged ? (
                      // Ahead of the packageUrl branch on purpose: when both
                      // are available, a package already on this host wins
                      // over re-downloading the remote one — which would
                      // overwrite a just-bootstrapped local build.
                      <button
                        type="button"
                        className="btn primary"
                        disabled={busyAlias === image.alias}
                        onClick={() => void handleInstallStaged(image.alias)}
                        title="이 호스트에 준비된 로컬 패키지를 바로 설치합니다."
                      >
                        {busyAlias === image.alias ? "설치 중…" : "로컬 패키지 설치"}
                      </button>
                    ) : image.packageUrl ? (
                      <button type="button" className="btn primary" disabled={fetching || busyAlias === image.alias} onClick={() => void handleFetchPackage(image.alias)} title={image.packageUrl}>
                        {fetching ? "가져오는 중…" : `가져오기 (${packageBasename(image.packageUrl)})`}
                      </button>
                    ) : (
                      <span className="poll-note">패키지 URL 없음</span>
                    )}
                  </td>
                </tr>
              );
            })}
          </tbody>
        </table>
        {install && install.status !== "idle" && (
          <>
            <div className="log-export-bar">
              <span className="log-export-bar-label">이미지 가져오기 로그 — {install.alias}</span>
              <LogExportActions text={install.log} filename={logDownloadFilename("m2image-import", install.alias)} buttonClassName="btn console-bar-btn" disabled={!install.log} />
            </div>
            <pre className="detail-log image-install-log">{install.log}</pre>
          </>
        )}
        <div className="package-row">
          <select value={newBuildSource} onChange={(event) => setNewBuildSource(event.target.value)}>
            {installedAliases.map((alias) => (
              <option key={alias} value={alias}>{alias}</option>
            ))}
          </select>
          <button type="button" className="btn primary" disabled={!newBuildSource} onClick={() => setBuildSourceAlias(newBuildSource)}>
            + 새 이미지 빌드
          </button>
        </div>
      </section>

      <BootstrapPanel onFinished={() => void refreshList()} unavailableAliases={bootstrapBlockedAliases} />

      {buildSourceAlias && (
        <BuildModal
          key={buildSourceAlias}
          sourceAlias={buildSourceAlias}
          installedAliases={installedAliases}
          onClose={() => setBuildSourceAlias(null)}
          onFinalized={() => void refreshList()}
        />
      )}
    </div>
  );
}
