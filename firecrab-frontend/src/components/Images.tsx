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
  cancelBootstrap,
  deleteImage,
  deleteStagedPackage,
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

/** Last path segment of an official package URL, shown on the "가져오기" button. */
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
 * 클릭하면 열리는 최소 드롭다운 메뉴. 바깥 클릭 또는 Esc로 닫힌다.
 * 이 프로젝트에 다른 드롭다운 패턴이 없어 이 자리 전용으로 최소 구현했다 —
 * 범용화해서 다른 화면에 재사용할 계획은 없다.
 */
function OptionsMenu({
  items,
}: {
  items: { label: string; onClick: () => void; disabled: boolean; danger?: boolean }[];
}) {
  const [open, setOpen] = useState(false);
  const rootRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    const onDocClick = (event: MouseEvent) => {
      if (rootRef.current && !rootRef.current.contains(event.target as Node)) setOpen(false);
    };
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") setOpen(false);
    };
    document.addEventListener("mousedown", onDocClick);
    document.addEventListener("keydown", onKeyDown);
    return () => {
      document.removeEventListener("mousedown", onDocClick);
      document.removeEventListener("keydown", onKeyDown);
    };
  }, [open]);

  return (
    <div className="options-menu" ref={rootRef}>
      <button type="button" className="options-menu-trigger" onClick={() => setOpen((current) => !current)}>
        ⋯
      </button>
      {open && (
        <ul className="options-menu-list">
          {items.map((item, index) => (
            <li key={index}>
              <button
                type="button"
                className={`options-menu-item${item.danger ? " danger" : ""}`}
                disabled={item.disabled}
                onClick={() => {
                  setOpen(false);
                  item.onClick();
                }}
              >
                {item.label}
              </button>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

/**
 * 선택된 이미지 하나의 상세 정보 + 액션. 표 아래 인라인으로 열리며,
 * MicroNetworks/MicroStorages의 행 클릭 → 상세 패턴과 동일하다.
 */
/**
 * Builds the "⋯" menu's 4 items for one image — shared by the always-visible
 * per-row trigger (table) and, indirectly, by whichever row is expanded
 * (`ImageDetail` no longer owns this: the menu now lives in the table row so
 * it's visible without expanding anything).
 */
function computeMenuItems(
  image: ImageResponse,
  ctx: {
    packageJob: ImageInstallResponse | undefined;
    busyAlias: string | null;
    bootstrapSession: BootstrapResponse | null;
    bootstrapStartingAlias: string | null;
    onInstallStaged: (alias: string) => Promise<void>;
    onFetchPackage: (alias: string) => Promise<void>;
    onDelete: (alias: string) => Promise<void>;
    onStartBootstrap: (alias: string) => Promise<void>;
    onCancelBootstrap: (bootstrapId: string) => Promise<void>;
    onDeleteStagedPackage: (alias: string) => Promise<void>;
  },
): { label: string; onClick: () => void; disabled: boolean; danger?: boolean }[] {
  const {
    packageJob,
    busyAlias,
    bootstrapSession,
    bootstrapStartingAlias,
    onInstallStaged,
    onFetchPackage,
    onDelete,
    onStartBootstrap,
    onCancelBootstrap,
    onDeleteStagedPackage,
  } = ctx;
  const fetching = packageJob?.status === "running";
  // Bootstrapping this alias would spend ~30 minutes producing a package
  // the install step then refuses (`already_installed`) or that is already
  // sitting on disk waiting to be installed.
  const blockedByStatus = image.installed || image.packageStaged;
  const bootstrapBusy =
    bootstrapStartingAlias !== null ||
    (bootstrapSession !== null && bootstrapSession.status !== "succeeded" && bootstrapSession.status !== "failed");
  const bootstrapIsMine =
    bootstrapStartingAlias === image.alias || bootstrapSession?.alias === image.alias;

  const bakeLabel = blockedByStatus
    ? "구울 필요 없음"
    : bootstrapIsMine && bootstrapBusy
      ? "굽는 중…"
      : bootstrapBusy
        ? "다른 배포판 굽는 중"
        : "굽기";

  // Ahead of the packageUrl branch on purpose: when both are available, a
  // package already on this host wins over re-downloading the remote one —
  // which would overwrite a just-bootstrapped local build.
  const installLabel = image.installed
    ? "설치됨"
    : image.packageStaged
      ? busyAlias === image.alias
        ? "설치 중…"
        : "로컬 패키지 설치"
      : image.packageUrl
        ? fetching
          ? "가져오는 중…"
          : `가져오기 (${packageBasename(image.packageUrl)})`
        : "패키지 URL 없음";
  const installDisabled = image.installed
    ? true
    : image.packageStaged
      ? busyAlias === image.alias
      : image.packageUrl
        ? fetching || busyAlias === image.alias
        : true;
  const handleInstallClick = () => {
    if (image.packageStaged) void onInstallStaged(image.alias);
    else if (image.packageUrl) void onFetchPackage(image.alias);
  };

  const deleteLabel = busyAlias === image.alias ? "삭제 중…" : "삭제";

  // "굽기삭제"는 상태에 따라 서로 다른 두 동작을 겸한다: 이 alias에 대해
  // 지금 실행 중인 세션 취소, 또는 완료돼 스테이징된 패키지 삭제. 둘은
  // 실제로 배타적이다 — 세션이 `packageStaged`를 참으로 만들 수 있는
  // 시점(성공 종료)엔 이미 `bootstrapBusy`가 검사하는 비종결 상태를
  // 벗어난 뒤다.
  // Only safe to cancel while the builder VM is still booting or running the
  // guest script — matches the same gate `InlineConsole` in `ImageDetail`
  // uses. Once packaging starts, `package_bootstrap` (backend) is reading
  // the builder VM's disk and stopping/deleting it mid-read can publish a
  // truncated archive; `finalizing` means the package is already staged and
  // safe, so there's nothing left to "cancel" in the sense this button means.
  const canCancelBootstrap =
    bootstrapIsMine &&
    bootstrapSession !== null &&
    (bootstrapSession.status === "booting" || bootstrapSession.status === "running");
  const canDeleteStagedPackage = image.packageStaged && !canCancelBootstrap;
  const bakeDeleteLabel = canCancelBootstrap ? "부트스트랩 취소" : "구운 패키지 삭제";
  const handleBakeDeleteClick = () => {
    if (canCancelBootstrap && bootstrapSession) {
      if (
        !window.confirm(
          "진행 중인 부트스트랩을 취소할까요?\n빌더 VM을 삭제하며, 지금까지 진행된 내용은 저장되지 않습니다.",
        )
      ) {
        return;
      }
      void onCancelBootstrap(bootstrapSession.bootstrapId);
    } else if (canDeleteStagedPackage) {
      if (!window.confirm(`'${image.alias}' 구운 패키지를 삭제할까요?`)) return;
      void onDeleteStagedPackage(image.alias);
    }
  };

  return [
    {
      label: bakeLabel,
      disabled: blockedByStatus || bootstrapBusy,
      onClick: () => void onStartBootstrap(image.alias),
    },
    {
      label: installLabel,
      disabled: installDisabled,
      onClick: handleInstallClick,
    },
    {
      label: deleteLabel,
      disabled: !image.installed || busyAlias === image.alias,
      onClick: () => void onDelete(image.alias),
      danger: true,
    },
    {
      label: bakeDeleteLabel,
      disabled: !canCancelBootstrap && !canDeleteStagedPackage,
      onClick: handleBakeDeleteClick,
      danger: true,
    },
  ];
}

function ImageDetail({
  image,
  usedByVms,
  usedByError,
  packageJob,
  install,
  bootstrapSession,
  bootstrapStartingAlias,
  bootstrapError,
}: {
  image: ImageResponse;
  usedByVms: VmResponse[] | null;
  usedByError: string | null;
  packageJob: ImageInstallResponse | undefined;
  install: ImageInstallResponse | null;
  bootstrapSession: BootstrapResponse | null;
  bootstrapStartingAlias: string | null;
  bootstrapError: string | null;
}) {
  // The "⋯" actions menu now lives in the table row (always visible, next to
  // the 상태 badge) instead of here — this panel is info + in-progress
  // output only. `bootstrapIsMine` still gates that output to the session
  // that actually belongs to this alias.
  const bootstrapIsMine =
    bootstrapStartingAlias === image.alias || bootstrapSession?.alias === image.alias;

  return (
    <div className="subpanel">
      <dl className="detail-fields mono">
        <dt>alias</dt>
        <dd>{image.alias}</dd>

        <dt>버전</dt>
        <dd>{image.version}</dd>

        <dt>최소 디스크</dt>
        <dd>{image.minDiskGb} GiB</dd>

        <dt>rootfs 크기</dt>
        <dd>{formatRootfsSize(image.rootfsSizeBytes)}</dd>

        <dt>상태</dt>
        <dd>
          {image.installed
            ? "설치됨"
            : packageJob?.status === "succeeded" || image.packageStaged
              ? "패키지 준비됨"
              : "미설치"}
        </dd>

        <dt>설명</dt>
        <dd>{image.description || "—"}</dd>

        {image.packageUrl && (
          <>
            <dt>패키지 URL</dt>
            <dd>{image.packageUrl}</dd>
          </>
        )}

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

      {/* `bootstrapIsMine`으로 걸지 않는다: `startBootstrap` POST 자체가
          실패하면 이 alias의 세션이 아예 생기지 않아 `bootstrapIsMine`이
          항상 거짓이 된다. `ImageDetail`은 한 번에 하나의 alias에만
          렌더링되고, `Images()`의 `selectedAlias`-change effect가 선택이
          바뀔 때마다 `bootstrapError`를 리셋하므로 이 상태는 항상
          "지금 열려있는 alias의 에러"다. */}
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
   * Set from the click itself, not from the response — mirrors
   * `handleInstallStaged` 등의 `busyAlias` 가드와 같은 이유: 응답이
   * 오기 전 더블클릭이 두 번째 POST를 쏴서 빌더 VM이 두 개 뜨는 것을
   * 막는다. 백엔드도 세션 하나만 허용하므로(409) 이중 방어다. alias를
   * 함께 들고 있는 이유: `startBootstrap` 자체가 실패하면
   * `bootstrapSession`이 이 alias로 채워지지 않으므로, "지금 이 alias의
   * 요청이 진행 중"이라는 사실을 세션과 무관하게 알아야 굽기 버튼이
   * 자기 자신의 요청을 "다른 배포판 굽는 중"으로 잘못 표시하지 않는다.
   */
  const [bootstrapStartingAlias, setBootstrapStartingAlias] = useState<string | null>(null);

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

  const refreshUsedByVms = useCallback((alias: string) => {
    setUsedByVms(null);
    setUsedByError(null);
    listVms()
      .then((vms) => setUsedByVms(vms.filter((vm) => vm.template === alias)))
      .catch((error) => setUsedByError((error as Error).message));
  }, []);

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
    refreshUsedByVms(selectedAlias);
  }, [selectedAlias, refreshUsedByVms]);

  // `Images` is conditionally mounted by the App shell (only while the
  // "images" tab is active), so a poll started here can easily outlive the
  // component if the user navigates away mid-install or mid-bootstrap.
  // Every tick must check this before touching state. Deliberately does NOT
  // cancel an in-flight bootstrap session on unmount: cancelling mid-Packaging
  // deletes the builder VM's disk out from under the concurrently-running
  // packaging step, which can publish a truncated archive. A bootstrap simply
  // keeps running on the backend and this panel resumes polling it next time
  // Images mounts.
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

  // 404는 취소로 삭제된 세션이라는 확정 신호(그만 폴링), 그 외 에러는
  // 일시적일 수 있으니 계속 폴링한다.
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
    if (bootstrapStartingAlias !== null) return;
    setBootstrapStartingAlias(alias);
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
      if (mountedRef.current) setBootstrapStartingAlias(null);
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
      if (
        bootstrapSession?.alias === alias &&
        (bootstrapSession.status === "succeeded" || bootstrapSession.status === "failed")
      ) {
        setBootstrapSession(null);
      }
      if (selectedAlias === alias) refreshUsedByVms(alias);
    } catch (error) {
      setActionError((error as Error).message);
    } finally {
      setBusyAlias(null);
    }
  };

  const handleDeleteStagedPackage = async (alias: string) => {
    setBusyAlias(alias);
    setActionError(null);
    try {
      await deleteStagedPackage(alias);
      await refreshList();
      setPackageJobs((current) => {
        const next = { ...current };
        delete next[alias];
        return next;
      });
      if (
        bootstrapSession?.alias === alias &&
        (bootstrapSession.status === "succeeded" || bootstrapSession.status === "failed")
      ) {
        setBootstrapSession(null);
      }
    } catch (error) {
      setActionError((error as Error).message);
    } finally {
      setBusyAlias(null);
    }
  };

  // 취소 실패는 부트스트랩 자체의 진행 실패가 아니라 사용자가 시작한 별도
  // 액션이므로, 세션 전용 `bootstrapError`가 아니라 일반 `actionError`
  // 배너에 표시한다. 성공 시 세션을 즉시 지운다 — 다음 폴링 틱을 기다리면
  // (`pollBootstrap`의 404 처리가 결국 같은 일을 하긴 하지만) 최대 1초
  // 동안 이미 취소된 세션이 화면에 남는다.
  const handleCancelBootstrap = async (bootstrapId: string) => {
    setActionError(null);
    try {
      await cancelBootstrap(bootstrapId);
      setBootstrapSession(null);
      setBootstrapError(null);
    } catch (error) {
      setActionError((error as Error).message);
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
                  <td className="state-cell">
                    <span className={`state-badge${image.installed ? " running" : ""}`}>{statusLabel}</span>
                    {/* Stops the row's own onClick (select/deselect) from firing
                        when the user is just opening or using this menu. */}
                    <span onClick={(event) => event.stopPropagation()}>
                      <OptionsMenu
                        items={computeMenuItems(image, {
                          packageJob: job,
                          busyAlias,
                          bootstrapSession,
                          bootstrapStartingAlias,
                          onInstallStaged: handleInstallStaged,
                          onFetchPackage: handleFetchPackage,
                          onDelete: handleDelete,
                          onStartBootstrap: handleStartBootstrap,
                          onCancelBootstrap: handleCancelBootstrap,
                          onDeleteStagedPackage: handleDeleteStagedPackage,
                        })}
                      />
                    </span>
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
            install={install}
            bootstrapSession={bootstrapSession}
            bootstrapStartingAlias={bootstrapStartingAlias}
            bootstrapError={bootstrapError}
          />
        )}
      </section>
    </div>
  );
}
