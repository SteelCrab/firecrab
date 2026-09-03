import { useCallback, useEffect, useRef, useState, type FormEvent } from "react";
import type {
  BootstrapResponse,
  BootstrapStep,
  BootstrapStepRun,
  ImageInstallResponse,
  ImageResponse,
  KernelResponse,
  MicroRegistryRegisterResponse,
  MicroRegistryResponse,
  OciInspectResponse,
  VmResponse,
} from "../bindings";
import {
  ApiClientError,
  cancelBootstrap,
  deleteImage,
  deleteStagedPackage,
  deleteVm,
  getActiveBootstrap,
  getBootstrap,
  getImageInstall,
  getImagePackage,
  getMicroRegistry,
  getMicroRegistryRegisterJob,
  getOciImport,
  inspectOciImage,
  listImages,
  listKernels,
  listVms,
  startBootstrap,
  startImageInstall,
  startImagePackage,
  startMicroRegistryRegister,
  startOciImport,
  stopVm,
  updateImageKernel,
} from "../api/client";
import { logDownloadFilename } from "../lib/textExport";
import Banner from "./Banner";
import LogExportActions from "./LogExportActions";
import InlineConsole from "./InlineConsole";
import { useI18n } from "../i18n";

const KNOWN_TEMPLATES = [
  { alias: "alpine-3.24.1", label: "Alpine Linux", logoSrc: "https://www.alpinelinux.org/alpinelinux-logo.svg" },
  { alias: "ubuntu-26.04", label: "Ubuntu", logoSrc: "https://assets.ubuntu.com/v1/ff6a9a38-ubuntu-logo-2022.svg" },
  { alias: "rocky-9.8", label: "Rocky Linux 9.8", logoSrc: "https://raw.githubusercontent.com/rocky-linux/branding/main/logo/src/icon-primary.svg" },
] as const;

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

function packageDownloadPercent(job: ImageInstallResponse): number | null {
  const total = job.totalBytes;
  if (!total || total <= 0) return null;
  return Math.min(100, Math.round(((job.downloadedBytes ?? 0) / total) * 100));
}

function formatTransferRate(bytesPerSecond: number | null): string {
  if (bytesPerSecond === null || !Number.isFinite(bytesPerSecond)) return "—";
  if (bytesPerSecond >= 1024 ** 3) return `${(bytesPerSecond / 1024 ** 3).toFixed(1)} GiB/s`;
  if (bytesPerSecond >= 1024 ** 2) return `${(bytesPerSecond / 1024 ** 2).toFixed(1)} MiB/s`;
  if (bytesPerSecond >= 1024) return `${(bytesPerSecond / 1024).toFixed(1)} KiB/s`;
  return `${Math.round(bytesPerSecond)} B/s`;
}

/** Download-only progress. Terminal states use the normal status badge. */
function PackageDownloadProgress({
  job,
  label = "M2Image package download",
}: {
  job: ImageInstallResponse;
  label?: string;
}) {
  const measuredPercent = packageDownloadPercent(job);
  const barPercent = measuredPercent ?? 35;
  const sampleRef = useRef({
    startedAtMs: job.startedAtMs,
    downloadedBytes: job.downloadedBytes ?? 0,
    sampledAtMs: Date.now(),
  });
  const [bytesPerSecond, setBytesPerSecond] = useState<number | null>(() => {
    const elapsedMs = job.startedAtMs ? Date.now() - job.startedAtMs : 0;
    return elapsedMs > 0 ? ((job.downloadedBytes ?? 0) * 1000) / elapsedMs : null;
  });

  useEffect(() => {
    const now = Date.now();
    const downloadedBytes = job.downloadedBytes ?? 0;
    const previous = sampleRef.current;
    if (previous.startedAtMs !== job.startedAtMs) {
      const elapsedMs = job.startedAtMs ? now - job.startedAtMs : 0;
      setBytesPerSecond(elapsedMs > 0 ? (downloadedBytes * 1000) / elapsedMs : null);
    } else {
      const elapsedMs = now - previous.sampledAtMs;
      const transferredBytes = downloadedBytes - previous.downloadedBytes;
      if (elapsedMs > 0 && transferredBytes >= 0) {
        setBytesPerSecond((transferredBytes * 1000) / elapsedMs);
      }
    }
    sampleRef.current = { startedAtMs: job.startedAtMs, downloadedBytes, sampledAtMs: now };
  }, [job.downloadedBytes, job.startedAtMs]);

  return (
    <span className="package-progress">
      <span
        className={`package-progress-track${measuredPercent === null ? " indeterminate" : ""}`}
        role="progressbar"
        aria-label={label}
        aria-valuenow={measuredPercent ?? undefined}
        aria-valuemin={0}
        aria-valuemax={100}
      >
        <span className="package-progress-fill" style={{ width: `${barPercent}%` }} />
      </span>
      <span className="package-progress-speed">{formatTransferRate(bytesPerSecond)}</span>
    </span>
  );
}

/**
 * A poll may have left the browser before the POST that starts a newer job.
 * Do not let that older `idle`/`running` response erase the state returned by
 * the POST (or a terminal state for the same job).
 */
function keepNewestJobSnapshot<T extends ImageInstallResponse>(
  current: T | null | undefined,
  incoming: T,
): T {
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

function bootstrapStepLabel(
  step: BootstrapStep,
  t: (english: string, korean: string) => string,
): string {
  switch (step) {
    case "startingBuilderVm": return t("Preparing builder VM", "빌더 VM 준비");
    case "installingSystem": return t("Installing system", "시스템 설치");
    case "packaging": return t("Packaging", "패키징");
    case "finalizing": return t("Finalizing", "마무리");
  }
}


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
  const { t } = useI18n();
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
          <span className="step-label">{bootstrapStepLabel(step, t)}</span>
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

/** 선택한 로컬 이미지의 상세 정보를 표 아래에 표시한다. */
function computeMenuItems(
  image: ImageResponse,
  ctx: {
    t: (english: string, korean: string) => string;
    busyAlias: string | null;
    onDelete: (alias: string) => Promise<void>;
  },
): { label: string; onClick: () => void; disabled: boolean; danger?: boolean }[] {
  const { t, busyAlias, onDelete } = ctx;

  const deleteLabel = busyAlias === image.alias ? t("Deleting…", "삭제 중…") : t("Delete", "삭제");

  return [
    {
      label: deleteLabel,
      disabled: !image.installed || busyAlias === image.alias,
      onClick: () => void onDelete(image.alias),
      danger: true,
    },
  ];
}

function ImageDetail({
  image,
  registryMinDiskGb,
  kernels,
  kernelUpdateBusy,
  onUpdateKernel,
  usedByVms,
  usedByError,
}: {
  image: ImageResponse;
  registryMinDiskGb?: number;
  kernels: KernelResponse[];
  kernelUpdateBusy: boolean;
  onUpdateKernel: (version: string) => Promise<void>;
  usedByVms: VmResponse[] | null;
  usedByError: string | null;
}) {
  const { t } = useI18n();
  const minDiskGb = Math.max(image.minDiskGb, registryMinDiskGb ?? 0);
  const [targetKernel, setTargetKernel] = useState(image.kernelVersion ?? "");
  useEffect(() => {
    setTargetKernel(image.kernelVersion ?? "");
  }, [image.alias, image.kernelVersion]);
  const installedKernels = kernels.filter((kernel) => kernel.installed);
  const canUpdateKernel =
    image.installed &&
    targetKernel !== "" &&
    targetKernel !== image.kernelVersion &&
    !kernelUpdateBusy;

  return (
    <div className="subpanel">
      <dl className="detail-fields mono">
        <dt>alias</dt>
        <dd>{image.alias}</dd>

        <dt>{t("Version", "버전")}</dt>
        <dd>{image.version}</dd>

        <dt>{t("Kernel version", "커널 버전")}</dt>
        <dd>{image.kernelVersion ?? t("Distro-provided kernel", "배포판 제공 커널")}</dd>

        <dt>{t("Kernel image", "커널 이미지")}</dt>
        <dd>{image.kernelImage || "—"}</dd>

        <dt>{t("Kernel SHA256", "커널 SHA256")}</dt>
        <dd>{image.kernelSha256 || "—"}</dd>

        <dt>{t("Rootfs SHA256", "rootfs SHA256")}</dt>
        <dd>{image.rootfsSha256 || "—"}</dd>

        {image.initrdSha256 && (
          <>
            <dt>initrd SHA256</dt>
            <dd>{image.initrdSha256}</dd>
          </>
        )}

        <dt>{t("Minimum disk", "최소 디스크")}</dt>
        <dd>{minDiskGb > 0 ? `${minDiskGb} GiB` : "—"}</dd>

        <dt>{t("Rootfs size", "rootfs 크기")}</dt>
        <dd>{formatRootfsSize(image.rootfsSizeBytes)}</dd>

        <dt>{t("Status", "상태")}</dt>
        <dd>
          {image.installed
            ? t("Installed", "설치됨")
            : t("Not installed", "미설치")}
        </dd>

        <dt>{t("VMs using it", "사용 중인 VM")}</dt>
        <dd>
          {usedByError
            ? usedByError
            : usedByVms === null
              ? t("Loading…", "불러오는 중…")
              : usedByVms.length === 0
                ? t("None", "없음")
                : usedByVms.map((vm) => `${vm.name} [${vm.state}]`).join(", ")}
        </dd>
      </dl>
      {image.installed && (
        <div className="kernel-update-control">
          <label htmlFor={`image-kernel-${image.alias}`}>
            {t("Update image kernel", "이미지 커널 업데이트")}
          </label>
          <div className="kernel-update-row">
            <select
              id={`image-kernel-${image.alias}`}
              value={targetKernel}
              onChange={(event) => setTargetKernel(event.target.value)}
              disabled={kernelUpdateBusy || installedKernels.length === 0}
            >
              <option value="">
                {installedKernels.length === 0
                  ? t("Install a kernel first", "먼저 커널을 설치하세요")
                  : t("Select installed kernel", "설치된 커널을 선택하세요")}
              </option>
              {installedKernels.map((kernel) => (
                <option key={kernel.version} value={kernel.version}>
                  {kernel.version}{kernel.version === image.kernelVersion ? t(" · current", " · 현재") : ""}
                </option>
              ))}
            </select>
            <button
              type="button"
              className="btn primary"
              disabled={!canUpdateKernel}
              onClick={() => void onUpdateKernel(targetKernel)}
            >
              {kernelUpdateBusy ? t("Updating…", "업데이트 중…") : t("Update kernel", "커널 업데이트")}
            </button>
          </div>
          <small>
            {t(
              "Only images with no VM references can change their kernel.",
              "VM이 연결되지 않은 이미지만 커널을 변경할 수 있습니다.",
            )}
          </small>
        </div>
      )}
    </div>
  );
}

function ImageJobLog({
  job,
  kind,
}: {
  job: ImageInstallResponse;
  kind: "download" | "import" | "oci" | "register";
}) {
  const { t } = useI18n();
  const label = kind === "download"
    ? t("Package download log", "패키지 다운로드 로그")
    : kind === "oci"
      ? t("OCI import log", "OCI 가져오기 로그")
      : kind === "register"
        ? t("Register log", "등록 로그")
        : t("Image import log", "이미지 가져오기 로그");

  const logRef = useRef<HTMLPreElement>(null);
  useEffect(() => {
    const el = logRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [job.log]);

  return (
    <div className="subpanel">
      <div className="log-export-bar">
        <span className="log-export-bar-label">{label} — {job.alias}</span>
        <LogExportActions
          text={job.log}
          filename={logDownloadFilename(`m2image-${kind}`, job.alias)}
          buttonClassName="btn console-bar-btn"
          disabled={!job.log}
        />
      </div>
      <pre className="detail-log image-install-log" ref={logRef}>{job.log}</pre>
    </div>
  );
}

function MicroBootPanel({
  images,
  session,
  install,
  startingAlias,
  error,
  onStart,
  onCancel,
  onInstall,
  onDeletePackage,
}: {
  images: ImageResponse[];
  session: BootstrapResponse | null;
  install: ImageInstallResponse | null;
  startingAlias: string | null;
  error: string | null;
  onStart: (alias: string) => Promise<void>;
  onCancel: (bootstrapId: string) => Promise<void>;
  onInstall: (alias: string) => Promise<void>;
  onDeletePackage: (alias: string) => Promise<void>;
}) {
  const { t } = useI18n();
  const sessionActive =
    session !== null && session.status !== "succeeded" && session.status !== "failed";
  const bootstrapBusy = startingAlias !== null || sessionActive;
  const sessionLogRef = useRef<HTMLPreElement>(null);
  useEffect(() => {
    const el = sessionLogRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [session?.log]);

  return (
    <section className="panel microboot-panel">
      <h2 className="panel-title">MicroBoot</h2>
      <p className="panel-intro">
        {t(
          "Build an M2Image from an official distribution inside an isolated builder microVM. The finished package stays on this host until you install or delete it.",
          "격리된 빌더 microVM에서 공식 배포판으로 M2Image를 만듭니다. 완성된 패키지는 설치하거나 삭제할 때까지 이 호스트에 보관됩니다.",
        )}
      </p>
      {error && <div className="field-error">{error}</div>}
      <div className="table-scroll">
        <table className="vm-table microboot-table">
          <thead>
            <tr>
              <th>{t("Target image", "대상 이미지")}</th>
              <th>{t("Build status", "빌드 상태")}</th>
              <th className="actions">{t("Action", "동작")}</th>
            </tr>
          </thead>
          <tbody>
            {images.map((image) => {
              const isMine = startingAlias === image.alias || session?.alias === image.alias;
              const canCancel =
                isMine &&
                session !== null &&
                (session.status === "booting" || session.status === "running");
              const building = isMine && bootstrapBusy;
              const microBootPackageReady =
                image.packageStaged && image.packageOrigin === "microBoot";
              const packageOwnedElsewhere = image.packageStaged && !microBootPackageReady;
              const installing = install?.alias === image.alias && install.status === "running";
              const known = KNOWN_TEMPLATES.find((template) => template.alias === image.alias);
              const statusLabel = building
                ? session?.status ?? t("Starting", "시작 중")
                : microBootPackageReady
                  ? t("Package ready", "패키지 준비됨")
                  : packageOwnedElsewhere
                    ? t("Package managed by MicroRegistry", "MicroRegistry에서 관리 중")
                  : image.installed
                    ? t("Installed", "설치됨")
                    : t("Ready to build", "빌드 가능");
              const statusClass = building
                ? " starting"
                : image.packageStaged || image.installed
                  ? " running"
                  : "";
              const actionLabel = canCancel
                ? t("Cancel build", "빌드 취소")
                : packageOwnedElsewhere
                  ? t("Managed by MicroRegistry", "MicroRegistry에서 관리")
                  : building
                    ? t("Building…", "빌드 중…")
                    : image.installed
                      ? t("No build needed", "빌드 필요 없음")
                      : bootstrapBusy
                        ? t("Another build is running", "다른 빌드 진행 중")
                        : t("Build", "빌드");
              const actionDisabled = canCancel
                ? false
                : packageOwnedElsewhere
                  ? true
                  : image.installed || bootstrapBusy;
              const handleAction = () => {
                if (canCancel && session) {
                  if (!window.confirm(t(
                    "Cancel the build in progress?\nThe builder VM will be deleted and its progress will be lost.",
                    "진행 중인 빌드를 취소할까요?\n빌더 VM을 삭제하며, 지금까지 진행된 내용은 저장되지 않습니다.",
                  ))) return;
                  void onCancel(session.bootstrapId);
                  return;
                }
                void onStart(image.alias);
              };

              return (
                <tr key={image.alias}>
                  <td className="mono">
                    {known && <img className="image-template-logo" src={known.logoSrc} alt="" />}
                    {image.alias}
                  </td>
                  <td><span className={`state-badge${statusClass}`}>{statusLabel}</span></td>
                  <td className="actions">
                    {microBootPackageReady && !canCancel ? (
                      <>
                        <button
                          type="button"
                          className="btn"
                          disabled={installing || bootstrapBusy}
                          onClick={() => void onInstall(image.alias)}
                        >
                          {installing ? t("Installing…", "설치 중…") : t("Install", "설치")}
                        </button>
                        <button
                          type="button"
                          className="btn danger"
                          disabled={installing || bootstrapBusy}
                          onClick={() => {
                            if (!window.confirm(t(
                              `Delete the built package '${image.alias}'?`,
                              `'${image.alias}' 구운 패키지를 삭제할까요?`,
                            ))) return;
                            void onDeletePackage(image.alias);
                          }}
                        >
                          {t("Delete package", "패키지 삭제")}
                        </button>
                      </>
                    ) : (
                      <button
                        type="button"
                        className={`btn${canCancel ? " danger" : ""}`}
                        disabled={actionDisabled}
                        onClick={handleAction}
                      >
                        {actionLabel}
                      </button>
                    )}
                  </td>
                </tr>
              );
            })}
          </tbody>
        </table>
      </div>

      {session && (
        <div className="subpanel microboot-session">
          <div className="log-export-bar">
            <span className="log-export-bar-label">
              {t("MicroBoot session", "MicroBoot 세션")} — {session.alias}
            </span>
            <LogExportActions
              text={session.log}
              filename={logDownloadFilename("microboot", session.alias)}
              buttonClassName="btn console-bar-btn"
              disabled={!session.log}
            />
          </div>
          <BootstrapStepper timeline={session.stepTimeline} />
          {session.status === "booting" || session.status === "running" ? (
            <InlineConsole vmId={session.vmId} />
          ) : (
            <p className="inline-console-ended">
              {t(
                "The builder VM was cleaned up, so its console connection ended.",
                "빌더 VM이 정리되어 콘솔 연결이 종료되었습니다.",
              )}
            </p>
          )}
          <pre className="detail-log" ref={sessionLogRef}>{session.log}</pre>
        </div>
      )}
      {install && install.status !== "idle" && (
        <ImageJobLog job={install} kind="import" />
      )}
    </section>
  );
}

function apiErrorText(error: unknown): string {
  if (error instanceof ApiClientError) {
    return error.fieldError("reference") ?? error.message;
  }
  return (error as Error).message;
}

function lastLogLine(log: string, fallback: string): string {
  const line = log.trim().split("\n").filter(Boolean).at(-1);
  return line || fallback;
}

function jobStatusClass(status: ImageInstallResponse["status"]): string {
  if (status === "running") return " starting";
  if (status === "succeeded") return " running";
  if (status === "failed") return " error";
  return "";
}

/**
 * Inspect an OCI reference, then start/poll `POST/GET /api/oci/import`
 * with the same job snapshot the M2Image install path already uses.
 */
function OciImportPanel({
  images,
  onImported,
}: {
  images: ImageResponse[];
  onImported: (alias: string) => Promise<void>;
}) {
  const { t } = useI18n();
  const [reference, setReference] = useState("");
  const [inspectedReference, setInspectedReference] = useState<string | null>(null);
  const [inspect, setInspect] = useState<OciInspectResponse | null>(null);
  const [inspecting, setInspecting] = useState(false);
  const [starting, setStarting] = useState(false);
  const [job, setJob] = useState<ImageInstallResponse | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [info, setInfo] = useState<string | null>(null);
  const mountedRef = useRef(true);
  const pollingAliasRef = useRef<string | null>(null);

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
    };
  }, []);

  const pollImport = useCallback((alias: string) => {
    if (pollingAliasRef.current === alias) return;
    pollingAliasRef.current = alias;
    const stop = () => {
      if (pollingAliasRef.current === alias) pollingAliasRef.current = null;
    };
    const tick = async () => {
      if (!mountedRef.current) return stop();
      try {
        const latest = await getOciImport(alias);
        if (!mountedRef.current) return stop();
        setJob((current) => keepNewestJobSnapshot(current, latest));
        if (latest.status === "running") {
          setTimeout(() => void tick(), 500);
          return;
        }
        stop();
        if (latest.status === "succeeded") {
          setError(null);
          setInfo(t(
            `Imported as ${latest.alias}. It now appears in M2Image.`,
            `${latest.alias}(으)로 가져왔습니다. M2Image 목록에 나타납니다.`,
          ));
          await onImported(latest.alias);
        } else if (latest.status === "failed") {
          setInfo(null);
          setError(lastLogLine(latest.log, t("OCI import failed.", "OCI 가져오기에 실패했습니다.")));
        }
      } catch {
        if (mountedRef.current) setTimeout(() => void tick(), 500);
        else stop();
      }
    };
    void tick();
  }, [onImported, t]);

  const resumeImport = useCallback(async (alias: string) => {
    try {
      const latest = await getOciImport(alias);
      if (!mountedRef.current) return;
      setJob((current) => keepNewestJobSnapshot(current, latest));
      if (latest.status === "running") {
        pollImport(alias);
      } else if (latest.status === "succeeded") {
        setInfo(t(
          `Imported as ${latest.alias}. It now appears in M2Image.`,
          `${latest.alias}(으)로 가져왔습니다. M2Image 목록에 나타납니다.`,
        ));
        await onImported(latest.alias);
      } else if (latest.status === "failed") {
        setError(lastLogLine(latest.log, t("OCI import failed.", "OCI 가져오기에 실패했습니다.")));
      }
    } catch {
      // Idle or a missing job is the ordinary case after a fresh inspect.
    }
  }, [onImported, pollImport, t]);

  const handleInspect = async (event: FormEvent) => {
    event.preventDefault();
    const trimmed = reference.trim();
    if (!trimmed) {
      setInspect(null);
      setInspectedReference(null);
      setInfo(null);
      setError(t("Enter an image reference.", "이미지 참조를 입력하세요."));
      return;
    }
    setInspecting(true);
    setError(null);
    setInfo(null);
    try {
      const next = await inspectOciImage(trimmed);
      if (!mountedRef.current) return;
      setInspect(next);
      setInspectedReference(trimmed);
      if (next.alias) await resumeImport(next.alias);
    } catch (err) {
      if (!mountedRef.current) return;
      setInspect(null);
      setInspectedReference(null);
      setError(apiErrorText(err));
    } finally {
      if (mountedRef.current) setInspecting(false);
    }
  };

  const handleStartImport = async () => {
    if (!inspect || starting || job?.status === "running") return;
    const trimmed = reference.trim();
    if (!trimmed || trimmed !== inspectedReference) return;
    setStarting(true);
    setError(null);
    setInfo(null);
    try {
      const started = await startOciImport({ reference: trimmed });
      if (!mountedRef.current) return;
      setJob((current) => keepNewestJobSnapshot(current, started));
      pollImport(started.alias);
    } catch (err) {
      if (!mountedRef.current) return;
      const code = err instanceof ApiClientError ? err.apiError?.code : undefined;
      if (code === "import_in_progress" && inspect.alias) {
        await resumeImport(inspect.alias);
        return;
      }
      setError(apiErrorText(err));
    } finally {
      if (mountedRef.current) setStarting(false);
    }
  };

  const handleReferenceChange = (value: string) => {
    setReference(value);
    if (inspectedReference !== null && value.trim() !== inspectedReference) {
      setInspect(null);
      setInspectedReference(null);
      setError(null);
      setInfo(null);
      if (job && job.status !== "running") setJob(null);
    }
  };

  const inspectMatchesInput = inspect !== null && inspectedReference === reference.trim();
  const registered = job?.status === "succeeded"
    ? images.find((image) => image.alias === job.alias) ?? null
    : null;
  // Succeeded only blocks Start Import while the alias is still installed.
  // After the operator deletes that template the in-memory job stays
  // `succeeded`, and the same reference must be importable again (E2E
  // cleanup + a long-lived API process).
  const canStart =
    inspectMatchesInput &&
    Boolean(inspect?.alias) &&
    !inspecting &&
    !starting &&
    job?.status !== "running" &&
    registered === null;

  const jobStatusLabel = job
    ? job.status === "running"
      ? t("Importing…", "가져오는 중…")
      : job.status === "succeeded"
        ? t("Imported", "가져옴")
        : job.status === "failed"
          ? t("Import failed", "가져오기 실패")
          : t("Idle", "대기")
    : null;

  return (
    <section className="panel">
      <h2 className="panel-title">OCI</h2>
      <p className="panel-intro">
        {t(
          "Inspect a container image, then import it as a bootable template on this host. The registered alias appears in M2Image.",
          "컨테이너 이미지를 검사한 뒤 이 호스트의 부팅 가능한 템플릿으로 가져옵니다. 등록된 별칭은 M2Image에 나타납니다.",
        )}
      </p>
      {(error || info) && (
        <div style={{ marginBottom: "1rem" }}>
          {error && <Banner kind="error" text={error} onDismiss={() => setError(null)} />}
          {info && <Banner kind="info" text={info} onDismiss={() => setInfo(null)} />}
        </div>
      )}
      <form className="create-grid" onSubmit={(event) => void handleInspect(event)}>
        <div className="field" style={{ gridColumn: "1 / -1" }}>
          <label htmlFor="oci-reference">{t("Reference", "참조")}</label>
          <input
            id="oci-reference"
            name="reference"
            placeholder="nginx:1.27"
            value={reference}
            onChange={(event) => handleReferenceChange(event.target.value)}
            autoComplete="off"
            spellCheck={false}
            disabled={inspecting || starting || job?.status === "running"}
          />
          <span className="field-error" aria-hidden />
        </div>
        <div className="field">
          <label htmlFor="oci-inspect">&nbsp;</label>
          <button
            id="oci-inspect"
            className="btn"
            type="submit"
            disabled={inspecting || starting || job?.status === "running"}
          >
            {inspecting ? t("Inspecting…", "검사 중…") : t("Inspect", "검사")}
          </button>
          <span className="field-error" aria-hidden />
        </div>
        <div className="field">
          <label htmlFor="oci-import">&nbsp;</label>
          <button
            id="oci-import"
            className="btn primary"
            type="button"
            disabled={!canStart}
            onClick={() => void handleStartImport()}
          >
            {starting || job?.status === "running"
              ? t("Importing…", "가져오는 중…")
              : t("Start Import", "가져오기 시작")}
          </button>
          <span className="field-error" aria-hidden />
        </div>
      </form>

      {inspectMatchesInput && inspect && (
        <div className="subpanel">
          <dl className="detail-fields mono">
            <dt>{t("Compatibility", "호환성")}</dt>
            <dd>
              {t(
                "Compatible with this host.",
                "이 호스트와 호환됩니다.",
              )}
              {" "}
              {inspect.singlePlatform
                ? t(
                    `Single-platform image for ${inspect.architecture}.`,
                    `${inspect.architecture} 단일 플랫폼 이미지입니다.`,
                  )
                : t(
                    `This host selected the ${inspect.architecture} platform from a multi-arch index.`,
                    `이 호스트가 다중 아키텍처 인덱스에서 ${inspect.architecture} 플랫폼을 선택했습니다.`,
                  )}
            </dd>
            <dt>{t("Digest", "다이제스트")}</dt>
            <dd>{inspect.digest}</dd>
            <dt>{t("Architecture", "아키텍처")}</dt>
            <dd>{inspect.architecture}</dd>
            <dt>alias</dt>
            <dd>{inspect.alias}</dd>
            <dt>{t("Version", "버전")}</dt>
            <dd>
              {inspect.version}
              {inspect.immutable
                ? ` · ${t("Pinned digest", "고정된 다이제스트")}`
                : ` · ${t("Tag may move", "태그가 바뀔 수 있음")}`}
            </dd>
            <dt>{t("Registry", "레지스트리")}</dt>
            <dd>{inspect.registry}/{inspect.repository}</dd>
          </dl>
        </div>
      )}

      {job && job.status !== "idle" && (
        <>
          <div className="subpanel">
            <dl className="detail-fields mono">
              <dt>{t("Import status", "가져오기 상태")}</dt>
              <dd>
                <span className={`state-badge${jobStatusClass(job.status)}`}>{jobStatusLabel}</span>
              </dd>
              {job.status === "running" && (
                <>
                  <dt>{t("Progress", "진행")}</dt>
                  <dd>
                    <PackageDownloadProgress
                      job={job}
                      label={t("OCI import progress", "OCI 가져오기 진행")}
                    />
                  </dd>
                </>
              )}
              {job.status === "succeeded" && (
                <>
                  <dt>{t("Registered image", "등록된 이미지")}</dt>
                  <dd>
                    {registered
                      ? `${registered.alias} · ${t("Installed", "설치됨")}${registered.version ? ` · ${registered.version}` : ""}`
                      : `${job.alias} · ${t("Installed", "설치됨")}`}
                  </dd>
                </>
              )}
            </dl>
          </div>
          <ImageJobLog job={job} kind="oci" />
        </>
      )}
    </section>
  );
}

/**
 * M2Image inventory, MicroBoot builder, MicroRegistry, and OCI import are
 * intentionally separate panels so local images, local builds, remote
 * packages, and container imports do not read as one mixed catalog.
 */
export default function Images() {
  const { t } = useI18n();
  const [images, setImages] = useState<ImageResponse[] | null>(null);
  const [listError, setListError] = useState<string | null>(null);
  const [registry, setRegistry] = useState<MicroRegistryResponse | null>(null);
  const [registryError, setRegistryError] = useState<string | null>(null);
  const [kernels, setKernels] = useState<KernelResponse[]>([]);
  const [registerAlias, setRegisterAlias] = useState("");
  const [registerVersion, setRegisterVersion] = useState("");
  const [registerJob, setRegisterJob] = useState<MicroRegistryRegisterResponse | null>(null);
  const [registerError, setRegisterError] = useState<string | null>(null);
  const [registerStarting, setRegisterStarting] = useState(false);
  const registerPollingRef = useRef<string | null>(null);
  const [busyAlias, setBusyAlias] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  const [packageJobs, setPackageJobs] = useState<Record<string, ImageInstallResponse>>({});
  const packagePollingRef = useRef(new Set<string>());
  /** Session id currently being polled, so only ever one loop runs. */
  const bootstrapPollingRef = useRef<string | null>(null);
  const [install, setInstall] = useState<ImageInstallResponse | null>(null);
  const [installOrigin, setInstallOrigin] = useState<"microRegistry" | "microBoot" | null>(null);
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

  const handleOciImported = useCallback(async (alias: string) => {
    await refreshList();
    setSelectedAlias(alias);
  }, [refreshList]);

  const refreshRegistry = useCallback(async () => {
    try {
      const next = await getMicroRegistry();
      setRegistry(next);
      setRegistryError(null);
      // `GET /api/images/{alias}/package` is backed by the server-side job
      // tracker. Rehydrate it after a browser refresh so a running package
      // transfer immediately regains both its bar and its poll loop.
      const snapshots = await Promise.allSettled(
        next.images
          .filter((image) => image.downloadable)
          .map((image) => getImagePackage(image.alias)),
      );
      setPackageJobs((current) => {
        const merged = { ...current };
        for (const result of snapshots) {
          if (result.status === "fulfilled") {
            merged[result.value.alias] = keepNewestJobSnapshot(merged[result.value.alias], result.value);
          }
        }
        return merged;
      });
    } catch (error) {
      setRegistryError((error as Error).message);
    }
  }, []);

  const refreshKernels = useCallback(async () => {
    try {
      setKernels(await listKernels());
    } catch (error) {
      setActionError((error as Error).message);
    }
  }, []);

  useEffect(() => {
    void refreshList();
    void refreshRegistry();
    void refreshKernels();
  }, [refreshList, refreshRegistry, refreshKernels]);

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
  useEffect(() => {
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

  const pollPackage = useCallback((alias: string) => {
    if (packagePollingRef.current.has(alias)) return;
    packagePollingRef.current.add(alias);
    const tick = async () => {
      if (!mountedRef.current) {
        packagePollingRef.current.delete(alias);
        return;
      }
      try {
        const latest = await getImagePackage(alias);
        if (!mountedRef.current) {
          packagePollingRef.current.delete(alias);
          return;
        }
        setPackageJobs((current) => ({ ...current, [alias]: keepNewestJobSnapshot(current[alias], latest) }));
        if (latest.status === "running") {
          setTimeout(() => void tick(), 500);
          return;
        }
        packagePollingRef.current.delete(alias);
        if (latest.status === "succeeded") await Promise.all([refreshList(), refreshRegistry()]);
      } catch (error) {
        packagePollingRef.current.delete(alias);
        if (mountedRef.current) setActionError((error as Error).message);
      }
    };
    void tick();
  }, [refreshList, refreshRegistry]);

  useEffect(() => {
    for (const job of Object.values(packageJobs)) {
      if (job.status === "running") pollPackage(job.alias);
    }
  }, [packageJobs, pollPackage]);

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
          await Promise.all([refreshList(), refreshRegistry()]);
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

  const pollRegister = (jobId: string) => {
    if (registerPollingRef.current === jobId) return;
    registerPollingRef.current = jobId;
    const stop = () => {
      if (registerPollingRef.current === jobId) registerPollingRef.current = null;
    };
    const tick = async () => {
      if (!mountedRef.current) return stop();
      try {
        const latest = await getMicroRegistryRegisterJob(jobId);
        if (!mountedRef.current) return stop();
        setRegisterJob((current) => keepNewestJobSnapshot(current, latest));
        if (latest.status === "running") {
          setTimeout(() => void tick(), 400);
          return;
        }
        stop();
        if (latest.status === "succeeded") {
          setRegisterError(null);
          await refreshRegistry();
        } else if (latest.status === "failed") {
          setRegisterError(lastLogLine(latest.log, t("Register failed.", "등록에 실패했습니다.")));
        }
      } catch {
        if (mountedRef.current) setTimeout(() => void tick(), 400);
        else stop();
      }
    };
    void tick();
  };

  // 404는 취소로 삭제된 세션이라는 확정 신호(그만 폴링), 그 외 에러는
  // 일시적일 수 있으니 계속 폴링한다.
  const pollBootstrap = (bootstrapId: string) => {
    // One loop per session: the resume effect below and an explicit start
    // can both ask for the same id, and StrictMode runs that effect twice in
    // development, so without this guard the timers stack up.
    if (bootstrapPollingRef.current === bootstrapId) return;
    bootstrapPollingRef.current = bootstrapId;
    const stop = () => {
      if (bootstrapPollingRef.current === bootstrapId) bootstrapPollingRef.current = null;
    };
    const tick = async () => {
      if (!mountedRef.current) return stop();
      try {
        const snapshot = await getBootstrap(bootstrapId);
        if (!mountedRef.current) return stop();
        setBootstrapSession(snapshot);
        if (snapshot.status === "succeeded") {
          stop();
          await Promise.all([refreshList(), refreshRegistry()]);
        } else if (snapshot.status !== "failed") {
          setTimeout(() => void tick(), 1000);
        } else {
          // "failed" is a confirmed terminal state too — stop without retrying.
          stop();
        }
      } catch (err) {
        if (err instanceof ApiClientError && err.status === 404) {
          stop();
          if (mountedRef.current) setBootstrapSession(null);
          return;
        }
        if (mountedRef.current) setTimeout(() => void tick(), 1000);
        else stop();
      }
    };
    void tick();
  };

  // Makes good on what the `mountedRef` comment above already promises —
  // that a bootstrap "keeps running on the backend and this panel resumes
  // polling it next time Images mounts". It could not, until now: the
  // session id arrives only in the `startBootstrap` response and lives only
  // in this component's state, so a reload or a walk to another tab left the
  // build running with no panel and no console until it finished. Ask the
  // server which bootstrap is live and pick that id back up.
  useEffect(() => {
    let cancelled = false;
    getActiveBootstrap()
      .then((session) => {
        if (cancelled || !mountedRef.current || !session) return;
        setBootstrapSession(session);
        pollBootstrap(session.bootstrapId);
      })
      // Having nothing to resume is the ordinary case, and a failure here
      // must not stop the image list itself from rendering.
      .catch(() => {});
    return () => {
      cancelled = true;
    };
    // Once, on mount. `pollBootstrap` is rebuilt every render, but the ref
    // guard inside it is what keeps duplicate loops out.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

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
  const handleInstallStaged = async (
    alias: string,
    origin: "microRegistry" | "microBoot",
  ) => {
    setBusyAlias(alias);
    setActionError(null);
    setInstallOrigin(origin);
    try {
      const started = await startImageInstall(alias);
      setInstall(started);
      pollInstall(alias);
    } catch (error) {
      if (error instanceof ApiClientError && error.apiError?.code === "already_installed") {
        await Promise.all([refreshList(), refreshRegistry()]);
        setInstall(null);
        setInstallOrigin(null);
        return;
      }
      if (
        origin === "microRegistry" &&
        error instanceof ApiClientError &&
        error.apiError?.code === "package_required"
      ) {
        await refreshRegistry();
        await handleDownloadPackage(alias);
        return;
      }
      setActionError((error as Error).message);
    } finally {
      setBusyAlias(null);
    }
  };

  const handleDownloadPackage = async (alias: string) => {
    setBusyAlias(alias);
    setActionError(null);
    try {
      const snap = await startImagePackage(alias);
      setPackageJobs((current) => ({ ...current, [alias]: snap }));
      pollPackage(alias);
    } catch (error) {
      setActionError((error as Error).message);
    } finally {
      setBusyAlias(null);
    }
  };

  const handleUpdateKernel = async (alias: string, version: string) => {
    setBusyAlias(alias);
    setActionError(null);
    try {
      await updateImageKernel(alias, { kernelVersion: version });
      await Promise.all([refreshList(), refreshKernels()]);
    } catch (error) {
      if (error instanceof ApiClientError && error.apiError?.code === "in_use") {
        const count = error.apiError.fields?.count ?? "";
        const vms = error.apiError.fields?.vms ?? "";
        setActionError(
          t(
            `'${alias}' has ${count} VM(s) still using it, so its kernel cannot change: ${vms}. Delete those VMs first.`,
            `'${alias}' 이미지를 쓰는 VM ${count}개가 있어 커널을 바꿀 수 없습니다: ${vms}. 먼저 해당 VM을 삭제하세요.`,
          ),
        );
      } else {
        setActionError((error as Error).message);
      }
    } finally {
      setBusyAlias(null);
    }
  };

  const handleRegister = async (event: FormEvent) => {
    event.preventDefault();
    const alias = registerAlias;
    const version = registerVersion.trim();
    if (!alias || !version || registerStarting || registerJob?.status === "running") return;
    setRegisterStarting(true);
    setRegisterError(null);
    try {
      const started = await startMicroRegistryRegister({ alias, version });
      if (!mountedRef.current) return;
      setRegisterJob((current) => keepNewestJobSnapshot(current, started));
      if (started.status === "failed") {
        setRegisterError(lastLogLine(started.log, t("Register failed.", "등록에 실패했습니다.")));
      }
      pollRegister(started.jobId);
    } catch (error) {
      if (!mountedRef.current) return;
      setRegisterError(error instanceof ApiClientError ? error.message : (error as Error).message);
    } finally {
      if (mountedRef.current) setRegisterStarting(false);
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
      await Promise.all([refreshList(), refreshRegistry()]);
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
      await Promise.all([refreshList(), refreshRegistry()]);
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
    return <div className="empty">{t("Loading image catalog…", "이미지 목록 불러오는 중…")}</div>;
  }

  const registrySource = registry?.source ?? "https://registry.firecrab.dev/catalog.json";
  const registryPanel = (
    <section className="panel microregistry-panel">
        <h2 className="panel-title">
          <span>MicroRegistry</span>
          <span className="microregistry-title-actions">
            <span className={`registry-health${registry ? " online" : registryError ? " offline" : " checking"}`}>
              {registry
                ? t("Online", "정상")
                : registryError
                  ? t("Offline", "연결 실패")
                  : t("Checking", "확인 중")}
            </span>
            <a
              className="microregistry-source"
              href={registrySource}
              target="_blank"
              rel="noreferrer"
              title={registrySource}
            >
              {registrySource}
            </a>
            <button type="button" className="btn microregistry-refresh" onClick={() => void refreshRegistry()}>
              {t("Refresh", "새로고침")}
            </button>
          </span>
        </h2>
        <p className="microregistry-intro">
          {t(
            "Published M2Image packages. Download verifies the package on this host; install registers a prepared local package.",
            "공개된 M2Image 패키지입니다. 다운로드는 이 호스트에서 검증하고, 설치는 준비된 로컬 패키지를 등록합니다.",
          )}
        </p>
        <form className="create-grid" onSubmit={(event) => void handleRegister(event)}>
          <div className="field">
            <label htmlFor="microregistry-register-alias">{t("Image", "이미지")}</label>
            <select
              id="microregistry-register-alias"
              value={registerAlias}
              onChange={(event) => setRegisterAlias(event.target.value)}
              disabled={registerStarting || registerJob?.status === "running"}
            >
              <option value="">
                {(images ?? []).some((image) => image.installed)
                  ? t("Select an installed image", "설치된 이미지를 선택하세요")
                  : t("No installed images", "설치된 이미지가 없습니다")}
              </option>
              {(images ?? []).filter((image) => image.installed).map((image) => (
                <option key={image.alias} value={image.alias}>
                  {image.alias}{image.version ? ` · ${image.version}` : ""}
                </option>
              ))}
            </select>
            <span className="field-error" aria-hidden />
          </div>
          <div className="field">
            <label htmlFor="microregistry-register-version">{t("Version", "버전")}</label>
            <input
              id="microregistry-register-version"
              name="version"
              value={registerVersion}
              onChange={(event) => setRegisterVersion(event.target.value)}
              autoComplete="off"
              spellCheck={false}
              disabled={registerStarting || registerJob?.status === "running"}
            />
            <span className="field-error" aria-hidden />
          </div>
          <div className="field">
            <label htmlFor="microregistry-register-submit">&nbsp;</label>
            <button
              id="microregistry-register-submit"
              className="btn"
              type="submit"
              disabled={!registerAlias || !registerVersion.trim() || registerStarting || registerJob?.status === "running"}
            >
              {registerStarting || registerJob?.status === "running"
                ? t("Registering…", "등록 중…")
                : t("Register", "등록")}
            </button>
            <span className="field-error" aria-hidden />
          </div>
        </form>
        {registerError && (
          <div style={{ margin: "1rem 0" }}>
            <Banner kind="error" text={registerError} onDismiss={() => setRegisterError(null)} />
          </div>
        )}
        {registerJob && registerJob.status !== "idle" && (
          <>
            <div className="subpanel">
              <dl className="detail-fields mono">
                <dt>{t("Register status", "등록 상태")}</dt>
                <dd>
                  <span className={`state-badge${jobStatusClass(registerJob.status)}`}>
                    {registerJob.status === "running"
                      ? t("Registering…", "등록 중…")
                      : registerJob.status === "succeeded"
                        ? t("Registered", "등록됨")
                        : registerJob.status === "failed"
                          ? t("Register failed", "등록 실패")
                          : t("Idle", "대기")}
                  </span>
                </dd>
              </dl>
            </div>
            <ImageJobLog job={registerJob} kind="register" />
          </>
        )}
        {registryError && <div className="field-error">{registryError}</div>}
        {registry === null && !registryError && (
          <div className="empty microregistry-empty">{t("Loading MicroRegistry…", "MicroRegistry 불러오는 중…")}</div>
        )}
        {registry && registry.images.length === 0 && (
          <div className="empty microregistry-empty">{t("No published packages yet.", "아직 게시된 패키지가 없습니다.")}</div>
        )}
        {registry && registry.images.length > 0 && (
          <div className="table-scroll">
            <table className="vm-table microregistry-table">
              <thead>
                <tr>
                  <th>{t("Image", "이미지")}</th>
                  <th>{t("Version", "버전")}</th>
                  <th>{t("Minimum disk", "최소 디스크")}</th>
                  <th>{t("Status", "상태")}</th>
                  <th className="actions">{t("Action", "동작")}</th>
                </tr>
              </thead>
              <tbody>
                {registry.images.map((entry) => {
                  const packageJob = packageJobs[entry.alias];
                  // Legacy staged archives predate origin sidecars. Treat
                  // them as registry downloads; all new MicroBoot builds are
                  // explicitly marked and therefore never land here.
                  const registryPackageReady =
                    entry.packageStaged && entry.packageOrigin !== "microBoot";
                  const microBootPackageReady =
                    entry.packageStaged && entry.packageOrigin === "microBoot";
                  const installing = install?.alias === entry.alias && install.status === "running";
                  const downloading = packageJob?.status === "running";
                  const downloadFailed = packageJob?.status === "failed";
                  const actionBusy = busyAlias === entry.alias || installing || downloading;
                  const downloadPercent = packageJob ? packageDownloadPercent(packageJob) : null;
                  const statusLabel = entry.installed
                    ? t("Installed", "설치됨")
                    : downloading
                      ? downloadPercent === null
                        ? t("Downloading…", "다운로드 중…")
                        : `${t("Downloading", "다운로드 중")} ${downloadPercent}%`
                    : downloadFailed
                      ? t("Download failed", "다운로드 실패")
                    : registryPackageReady
                      ? t("Package ready", "패키지 준비됨")
                      : microBootPackageReady
                        ? t("Built locally", "로컬 빌드 완료")
                      : entry.downloadable
                        ? t("Available", "사용 가능")
                        : t("Unsupported", "지원 안 됨");
                  const actionLabel = entry.installed
                    ? t("Installed", "설치됨")
                    : registryPackageReady
                      ? actionBusy
                        ? t("Installing…", "설치 중…")
                        : t("Install", "설치")
                      : microBootPackageReady
                        ? t("Managed by MicroBoot", "MicroBoot에서 관리")
                      : downloading
                        ? t("Downloading…", "다운로드 중…")
                        : t("Download", "다운로드");
                  const actionDisabled =
                    entry.installed || actionBusy || microBootPackageReady || !entry.downloadable;
                  const doAction = () => {
                    if (registryPackageReady) {
                      void handleInstallStaged(entry.alias, "microRegistry");
                    } else {
                      void handleDownloadPackage(entry.alias);
                    }
                  };
                  return (
                    <tr key={entry.alias}>
                      <td className="mono microregistry-image" title={entry.package}>{entry.alias}</td>
                      <td className="mono">{entry.version}</td>
                      <td className="mono">{entry.minDiskGb} GiB</td>
                      <td className="microregistry-status">
                        <span className={`state-badge${entry.installed ? " running" : downloading ? " starting" : downloadFailed ? " error" : ""}`}>
                          {statusLabel}
                        </span>
                        {downloading && packageJob && <PackageDownloadProgress job={packageJob} />}
                      </td>
                      <td className="actions">
                        <button
                          type="button"
                          className="btn"
                          disabled={actionDisabled}
                          onClick={doAction}
                          title={entry.downloadable ? entry.package : t("This Firecrab version cannot install this alias.", "이 Firecrab 버전은 이 별칭을 설치할 수 없습니다.")}
                        >
                          {actionLabel}
                        </button>
                        {registryPackageReady && (
                          <button
                            type="button"
                            className="btn danger"
                            disabled={actionBusy}
                            onClick={() => {
                              if (!window.confirm(t(
                                `Delete the downloaded package '${entry.alias}'?`,
                                `'${entry.alias}' 다운로드 패키지를 삭제할까요?`,
                              ))) return;
                              void handleDeleteStagedPackage(entry.alias);
                            }}
                          >
                            {t("Delete package", "패키지 삭제")}
                          </button>
                        )}
                      </td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
          </div>
        )}
        {Object.values(packageJobs)
          .filter((job) => job.status !== "idle")
          .map((job) => <ImageJobLog key={`download-${job.alias}`} job={job} kind="download" />)}
        {installOrigin === "microRegistry" && install && install.status !== "idle" && (
          <ImageJobLog job={install} kind="import" />
        )}
    </section>
  );

  return (
    <div className="stack">
      {actionError && <div className="field-error">{actionError}</div>}
      <section className="panel">
        <h2 className="panel-title">M2Image</h2>
        <p className="panel-intro">
          {t(
            "Bootable microVM images available on this Firecrab host. Select an image to inspect its local installation, disk requirements, and VM usage.",
            "이 Firecrab 호스트에서 사용할 수 있는 부팅 가능한 microVM 이미지입니다. 이미지를 선택하면 로컬 설치 상태, 디스크 요구량, 사용 중인 VM을 확인할 수 있습니다.",
          )}
        </p>
        {listError && <div className="field-error">{listError}</div>}
        <table className="vm-table image-table">
          <thead>
            <tr>
              <th>{t("Image", "이미지")}</th>
              <th>{t("Minimum disk", "최소 디스크")}</th>
              <th>{t("Status", "상태")}</th>
            </tr>
          </thead>
          <tbody>
            {(images ?? []).map((image) => {
              const registryMinDiskGb = registry?.images.find((entry) => entry.alias === image.alias)?.minDiskGb;
              const minDiskGb = Math.max(image.minDiskGb, registryMinDiskGb ?? 0);
              const statusLabel = image.installed
                ? t("Installed", "설치됨")
                : t("Not installed", "미설치");
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
                  <td className="mono">{minDiskGb > 0 ? `${minDiskGb} GiB` : "—"}</td>
                  <td className="state-cell">
                    <span className={`state-badge${image.installed ? " running" : ""}`}>{statusLabel}</span>
                    {/* Stops the row's own onClick (select/deselect) from firing
                        when the user is just opening or using this menu. */}
                    {image.installed && (
                      <span onClick={(event) => event.stopPropagation()}>
                        <OptionsMenu
                          items={computeMenuItems(image, {
                            t,
                            busyAlias,
                            onDelete: handleDelete,
                          })}
                        />
                      </span>
                    )}
                  </td>
                </tr>
              );
            })}
          </tbody>
        </table>
        {selectedImage && (
          <ImageDetail
            image={selectedImage}
            registryMinDiskGb={registry?.images.find((entry) => entry.alias === selectedImage.alias)?.minDiskGb}
            kernels={kernels}
            kernelUpdateBusy={busyAlias === selectedImage.alias}
            onUpdateKernel={(version) => handleUpdateKernel(selectedImage.alias, version)}
            usedByVms={usedByVms}
            usedByError={usedByError}
          />
        )}
      </section>
      <OciImportPanel
        images={images ?? []}
        onImported={handleOciImported}
      />
      {registryPanel}
      <MicroBootPanel
        images={images ?? []}
        session={bootstrapSession}
        install={installOrigin === "microBoot" ? install : null}
        startingAlias={bootstrapStartingAlias}
        error={bootstrapError}
        onStart={handleStartBootstrap}
        onCancel={handleCancelBootstrap}
        onInstall={(alias) => handleInstallStaged(alias, "microBoot")}
        onDeletePackage={handleDeleteStagedPackage}
      />
    </div>
  );
}
