import { useCallback, useEffect, useRef, useState } from "react";
import type {
  BootstrapResponse,
  BootstrapStep,
  BootstrapStepRun,
  ImageInstallResponse,
  ImageResponse,
  VmResponse,
} from "../bindings";
import {
  ApiClientError,
  deleteImage,
  deleteVm,
  getBootstrap,
  getImageInstall,
  getImagePackage,
  listImages,
  listVms,
  startBootstrap,
  startImageInstall,
  startImagePackage,
  stopVm,
} from "../api/client";
import { logDownloadFilename } from "../lib/textExport";
import LogExportActions from "./LogExportActions";
import InlineConsole from "./InlineConsole";

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

const BOOTSTRAP_STEPS: BootstrapStep[] = [
  "startingBuilderVm",
  "installingSystem",
  "packaging",
  "finalizing",
];

const BOOTSTRAP_STEP_LABEL: Record<BootstrapStep, string> = {
  startingBuilderVm: "빌더 VM 준비",
  installingSystem: "시스템 설치",
  packaging: "패키징",
  finalizing: "마무리",
};

/** Guards against a single unbroken line (no `\n` to split on at all) still
 *  blowing up the step box the same way the unsplit case would. */
const STEP_DETAIL_PREVIEW_MAX = 160;

/**
 * Short label for a failed step's box. On the primary failure path
 * (`bootstrap.rs::run_bootstrap_script`), `run.detail` is `"bootstrap
 * script exited with code {n}"` followed by a newline and then up to
 * `OUTPUT_TAIL_CAP` (8 KiB) of echoed guest script plus console output —
 * that full text is already shown, correctly, in the `.detail-log` `<pre>`
 * below the stepper, so this box only ever needs the first line. Capped in
 * length too, in case a future producer of `detail` hands back one very
 * long line with no newline at all.
 */
function stepDetailPreview(detail: string): string {
  const firstLine = detail.split("\n", 1)[0];
  return firstLine.length > STEP_DETAIL_PREVIEW_MAX
    ? `${firstLine.slice(0, STEP_DETAIL_PREVIEW_MAX)}…`
    : firstLine;
}

/**
 * Four-box progress view over one bootstrap session, mirroring
 * `VmDetailModal`'s `PipelineStepper` so a VM start and a bootstrap read the
 * same way. Durations come from the server's own timestamps — the 1s poll is
 * far too coarse to time the short steps — and only the open step ticks
 * locally between polls.
 */
function BootstrapStepper({ timeline }: { timeline: BootstrapStepRun[] }) {
  const [now, setNow] = useState(() => Date.now());
  const hasOpenStep = timeline.some((run) => run.outcome === "running");
  useEffect(() => {
    if (!hasOpenStep) return;
    const tick = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(tick);
  }, [hasOpenStep]);

  const runFor = (step: BootstrapStep) => timeline.find((run) => run.step === step);

  return (
    <ol className="pipeline">
      {BOOTSTRAP_STEPS.map((step) => {
        const run = runFor(step);
        const status = run ? run.outcome : "pending";
        const elapsed = run ? (run.endedAtMs ?? now) - run.startedAtMs : null;

        return (
          <li key={step} className={`pipeline-step ${status}`}>
            <span className="step-label">{BOOTSTRAP_STEP_LABEL[step]}</span>
            <span className="step-bar">
              <span className="step-time">
                {elapsed === null ? "—" : formatElapsed(elapsed)}
              </span>
              <span className="step-mark">
                {status === "succeeded" ? "✓" : status === "failed" ? "✕" : ""}
              </span>
            </span>
            {run?.detail && (
              <span className="step-detail step-detail-clamped">
                {stepDetailPreview(run.detail)}
              </span>
            )}
          </li>
        );
      })}
    </ol>
  );
}

/** Same shape as `VmDetailModal`'s `duration()`. */
function formatElapsed(millis: number): string {
  if (millis < 1000) return `${millis}ms`;
  const seconds = Math.round(millis / 1000);
  if (seconds < 60) return `${seconds}s`;
  return `${Math.floor(seconds / 60)}m ${seconds % 60}s`;
}

/**
 * 선택된 이미지 하나의 상세 정보 + 액션. 표 아래 인라인으로 열리며,
 * MicroNetworks/MicroStorages의 행 클릭 → 상세 패턴과 동일하다.
 */
function ImageDetail({
  image,
  usedByVms,
  usedByError,
  packageJob,
  busyAlias,
  install,
  onInstallStaged,
  onFetchPackage,
  onDelete,
  bootstrapSession,
  bootstrapStarting,
  bootstrapError,
  onStartBootstrap,
}: {
  image: ImageResponse;
  usedByVms: VmResponse[] | null;
  usedByError: string | null;
  packageJob: ImageInstallResponse | undefined;
  busyAlias: string | null;
  install: ImageInstallResponse | null;
  onInstallStaged: (alias: string) => Promise<void>;
  onFetchPackage: (alias: string) => Promise<void>;
  onDelete: (alias: string) => Promise<void>;
  bootstrapSession: BootstrapResponse | null;
  bootstrapStarting: boolean;
  bootstrapError: string | null;
  onStartBootstrap: (alias: string) => Promise<void>;
}) {
  const fetching = packageJob?.status === "running";
  const blockedByStatus = image.installed || image.packageStaged;
  const bootstrapBusy =
    bootstrapStarting ||
    (bootstrapSession !== null && bootstrapSession.status !== "succeeded" && bootstrapSession.status !== "failed");
  const bootstrapIsMine = bootstrapSession?.alias === image.alias;

  return (
    <div className="subpanel">
      <dl className="detail-fields mono">
        <dt>버전</dt>
        <dd>{image.version}</dd>

        <dt>최소 디스크</dt>
        <dd>{image.minDiskGb} GiB</dd>

        <dt>rootfs 크기</dt>
        <dd>{formatRootfsSize(image.rootfsSizeBytes)}</dd>

        <dt>설명</dt>
        <dd>{image.description || "—"}</dd>

        <dt>사용 중인 VM</dt>
        <dd>
          {usedByError
            ? usedByError
            : usedByVms === null
              ? "불러오는 중…"
              : usedByVms.length === 0
                ? "없음"
                : usedByVms.map((vm) => `${vm.name} [${vm.state}]`).join(", ")}
        </dd>
      </dl>

      <div className="package-row">
        <button
          type="button"
          className="btn"
          disabled={blockedByStatus || bootstrapBusy}
          title={
            blockedByStatus
              ? "이미 설치됐거나 설치할 패키지가 이미 준비되어 있습니다."
              : bootstrapBusy && !bootstrapIsMine
                ? "다른 배포판의 부트스트랩이 진행 중입니다."
                : "공식 배포판을 처음부터 준비합니다 — 이미 있는 microVM 빌더를 재사용해 별도 컨테이너나 권한 상승 없이 처리합니다."
          }
          onClick={() => void onStartBootstrap(image.alias)}
        >
          {bootstrapIsMine && bootstrapBusy ? "굽는 중…" : "굽기"}
        </button>

        {image.installed ? (
          <button type="button" className="btn" disabled>
            설치됨
          </button>
        ) : image.packageStaged ? (
          <button
            type="button"
            className="btn primary"
            disabled={busyAlias === image.alias}
            onClick={() => void onInstallStaged(image.alias)}
            title="이 호스트에 준비된 로컬 패키지를 바로 설치합니다."
          >
            {busyAlias === image.alias ? "설치 중…" : "로컬 패키지 설치"}
          </button>
        ) : image.packageUrl ? (
          <button
            type="button"
            className="btn primary"
            disabled={fetching || busyAlias === image.alias}
            onClick={() => void onFetchPackage(image.alias)}
            title={image.packageUrl}
          >
            {fetching ? "가져오는 중…" : `가져오기 (${packageBasename(image.packageUrl)})`}
          </button>
        ) : (
          <button type="button" className="btn" disabled>
            패키지 URL 없음
          </button>
        )}

        <button
          type="button"
          className="btn danger"
          disabled={!image.installed || busyAlias === image.alias}
          onClick={() => void onDelete(image.alias)}
        >
          {busyAlias === image.alias ? "삭제 중…" : "삭제"}
        </button>
      </div>

      {/* `bootstrapIsMine`으로 걸지 않는다: `startBootstrap` POST 자체가
          실패하면 이 alias의 세션이 아예 생기지 않아 `bootstrapIsMine`이
          항상 거짓이 된다(위 Step 2 참고). `ImageDetail`은 한 번에 하나의
          alias에만 렌더링되고, 선택이 바뀔 때마다 `bootstrapError`가
          리셋되므로(Step 2의 effect) 이 상태는 항상 "지금 열려있는
          alias의 에러"다. */}
      {bootstrapError && <div className="field-error">{bootstrapError}</div>}

      {bootstrapIsMine && bootstrapSession && (
        <>
          <div className="state-badge">{bootstrapSession.status}</div>
          <BootstrapStepper timeline={bootstrapSession.stepTimeline} />
          {/* The builder VM only exists while the session is pre-terminal:
              `packaging` is entered *after* stop_vm returns, and the VM is
              deleted at the end. Gate on status, never on vmId — that field
              keeps its value after the VM it names is gone. */}
          {bootstrapSession.status === "booting" || bootstrapSession.status === "running" ? (
            <InlineConsole vmId={bootstrapSession.vmId} />
          ) : (
            <p className="inline-console-ended">빌더 VM이 정리되어 콘솔 연결이 종료되었습니다.</p>
          )}
          <pre className="detail-log">{bootstrapSession.log}</pre>
        </>
      )}

      {install && install.alias === image.alias && install.status !== "idle" && (
        <>
          <div className="log-export-bar">
            <span className="log-export-bar-label">이미지 가져오기 로그 — {install.alias}</span>
            <LogExportActions
              text={install.log}
              filename={logDownloadFilename("m2image-import", install.alias)}
              buttonClassName="btn console-bar-btn"
              disabled={!install.log}
            />
          </div>
          <pre className="detail-log image-install-log">{install.log}</pre>
        </>
      )}
    </div>
  );
}

/**
 * Single M2Image inventory table. Package download/install ("가져오기") and
 * from-scratch bootstrap ("굽기") are both actions inside the per-row detail
 * panel (`ImageDetail`) rather than separate panels.
 */
export default function Images() {
  const [images, setImages] = useState<ImageResponse[] | null>(null);
  const [listError, setListError] = useState<string | null>(null);
  const [busyAlias, setBusyAlias] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  const [packageJobs, setPackageJobs] = useState<Record<string, ImageInstallResponse>>({});
  const [install, setInstall] = useState<ImageInstallResponse | null>(null);
  const [selectedAlias, setSelectedAlias] = useState<string | null>(null);
  const [bootstrapSession, setBootstrapSession] = useState<BootstrapResponse | null>(null);
  const [bootstrapError, setBootstrapError] = useState<string | null>(null);
  /**
   * True from the click itself, not from the response — mirrors
   * `handleInstallStaged` 등의 `busyAlias` 가드와 같은 이유: 응답이
   * 오기 전 더블클릭이 두 번째 POST를 쏴서 빌더 VM이 두 개 뜨는 것을
   * 막는다. 백엔드도 세션 하나만 허용하므로(409) 이중 방어다.
   */
  const [bootstrapStarting, setBootstrapStarting] = useState(false);

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

  const selectedImage = (images ?? []).find((image) => image.alias === selectedAlias) ?? null;

  const [usedByVms, setUsedByVms] = useState<VmResponse[] | null>(null);
  const [usedByError, setUsedByError] = useState<string | null>(null);

  // MicroNetworks의 `getMicroNetwork(selectedId)`와 같은 패턴 —
  // 목록 자체엔 없는, 선택 시점의 최신 사용처만 별도로 가져온다.
  // `bootstrapError`도 함께 리셋: 세션이 아예 안 생긴 채 실패한 경우
  // `bootstrapSession.alias`로는 "누구 에러인지"를 알 수 없으므로,
  // 대신 선택이 바뀌는 시점에 이전 alias의 에러를 지우는 방식으로
  // 항상 "지금 열려있는 alias의 에러"만 남긴다.
  useEffect(() => {
    setBootstrapError(null);
    if (!selectedAlias) {
      setUsedByVms(null);
      setUsedByError(null);
      return;
    }
    setUsedByVms(null);
    setUsedByError(null);
    listVms()
      .then((vms) => setUsedByVms(vms.filter((vm) => vm.template === selectedAlias)))
      .catch((error) => setUsedByError((error as Error).message));
  }, [selectedAlias]);

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

  // 옛 BootstrapPanel.pollBootstrap과 동일한 폴링 규율 — 404는 취소로
  // 삭제된 세션이라는 확정 신호(그만 폴링), 그 외 에러는 일시적일 수
  // 있으니 계속 폴링한다.
  const pollBootstrap = (bootstrapId: string) => {
    const tick = async () => {
      if (!mountedRef.current) return;
      try {
        const snapshot = await getBootstrap(bootstrapId);
        if (!mountedRef.current) return;
        setBootstrapSession(snapshot);
        if (snapshot.status === "succeeded") {
          await refreshList();
        } else if (snapshot.status !== "failed") {
          setTimeout(() => void tick(), 1000);
        }
        // "failed" is a confirmed terminal state too — stop without retrying.
      } catch (err) {
        if (err instanceof ApiClientError && err.status === 404) {
          if (mountedRef.current) setBootstrapSession(null);
          return;
        }
        if (mountedRef.current) setTimeout(() => void tick(), 1000);
      }
    };
    void tick();
  };

  const handleStartBootstrap = async (alias: string) => {
    if (bootstrapStarting) return;
    setBootstrapStarting(true);
    setBootstrapError(null);
    try {
      const started = await startBootstrap(alias);
      if (!mountedRef.current) return;
      setBootstrapSession(started);
      pollBootstrap(started.bootstrapId);
    } catch (err) {
      if (!mountedRef.current) return;
      setBootstrapError((err as Error).message);
    } finally {
      if (mountedRef.current) setBootstrapStarting(false);
    }
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
            </tr>
          </thead>
          <tbody>
            {(images ?? []).map((image) => {
              const job = packageJobs[image.alias];
              const statusLabel = image.installed
                ? "설치됨"
                : job?.status === "succeeded" || image.packageStaged
                  ? "패키지 준비됨"
                  : "미설치";
              // Derived/web-built templates won't have a KNOWN_TEMPLATES entry —
              // fall back to plain alias text with no logo for those.
              const known = KNOWN_TEMPLATES.find((template) => template.alias === image.alias);
              return (
                <tr
                  key={image.alias}
                  className={selectedAlias === image.alias ? "is-selected" : undefined}
                  onClick={() => setSelectedAlias(selectedAlias === image.alias ? null : image.alias)}
                >
                  <td className="mono">
                    {known && <img className="image-template-logo" src={known.logoSrc} alt="" />}
                    {image.alias}
                  </td>
                  <td className="mono">{formatRootfsSize(image.rootfsSizeBytes)}</td>
                  <td>
                    <span className={`state-badge${image.installed ? " running" : ""}`}>{statusLabel}</span>
                  </td>
                </tr>
              );
            })}
          </tbody>
        </table>
        {selectedImage && (
          <ImageDetail
            image={selectedImage}
            usedByVms={usedByVms}
            usedByError={usedByError}
            packageJob={packageJobs[selectedImage.alias]}
            busyAlias={busyAlias}
            install={install}
            onInstallStaged={handleInstallStaged}
            onFetchPackage={handleFetchPackage}
            onDelete={handleDelete}
            bootstrapSession={bootstrapSession}
            bootstrapStarting={bootstrapStarting}
            bootstrapError={bootstrapError}
            onStartBootstrap={handleStartBootstrap}
          />
        )}
      </section>
    </div>
  );
}
