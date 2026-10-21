import { useCallback, useEffect, useRef, useState, type FormEvent, type ReactNode } from "react";
import type {
  ImageInstallResponse,
  ImageResponse,
  KernelResponse,
  OciInspectResponse,
  VmResponse,
} from "../bindings";
import {
  ApiClientError,
  deleteImage,
  deleteVm,
  getOciImport,
  inspectOciImage,
  listImages,
  listKernels,
  listVms,
  startOciImport,
  stopVm,
  updateImageKernel,
} from "../api/client";
import { logDownloadFilename } from "../lib/textExport";
import { getRegistryImages, pinnedRegistryReference, type RegistryImage } from "../api/registries";
import Banner from "./Banner";
import LogExportActions from "./LogExportActions";
import { useI18n } from "../i18n";
import { Layers } from "lucide-react";
import Screen from "./layout/Screen";
import { useScreenStatus } from "./layout/screenStatus";
import { DetailHead, DetailProp, DetailSection } from "./ui/DetailSection";
import StatusBadge from "./ui/StatusBadge";

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

/**
 * Minimal dropdown menu that opens on click and closes on outside click or Esc.
 * The project has no other dropdown pattern, so this implementation is specific
 * to this location. There are no plans to generalize it for other screens.
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
      <button type="button" className="options-menu-trigger" aria-haspopup="menu" aria-expanded={open} onClick={() => setOpen((current) => !current)}>
        ⋯
      </button>
      {open && (
        <ul className="options-menu-list" role="menu">
          {items.map((item, index) => (
            <li key={index} role="none">
              <button
                type="button"
                role="menuitem"
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

/** Show details of the selected local image below the table. */
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
  kernels,
  kernelUpdateBusy,
  onUpdateKernel,
  usedByVms,
  usedByError,
}: {
  image: ImageResponse;
  kernels: KernelResponse[];
  kernelUpdateBusy: boolean;
  onUpdateKernel: (version: string) => Promise<void>;
  usedByVms: VmResponse[] | null;
  usedByError: string | null;
}) {
  const { t } = useI18n();
  const minDiskGb = image.minDiskGb;
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
    <>
      <DetailHead
        title={image.alias}
        meta={(
          <>
            <span>{image.version}</span>
            {image.installed ? <span className="badge running">{t("Installed", "설치됨")}</span> : <span className="badge">{t("Not installed", "미설치")}</span>}
          </>
        )}
      />
      <div className="detail-body">
        <DetailSection title={t("Properties", "속성")}>
          <dl className="props">
            <DetailProp label={t("Version", "버전")}>{image.version}</DetailProp>
            <DetailProp label={t("Kernel", "커널")}>{image.kernelVersion ?? t("Distro-provided kernel", "배포판 제공 커널")}</DetailProp>
            <DetailProp label={t("Kernel image", "커널 이미지")}><span className="mono">{image.kernelImage || "—"}</span></DetailProp>
            <DetailProp label={t("Kernel SHA256", "커널 SHA256")} copy={image.kernelSha256 || undefined}><span className="mono">{image.kernelSha256 || "—"}</span></DetailProp>
            <DetailProp label={t("Rootfs SHA256", "rootfs SHA256")} copy={image.rootfsSha256 || undefined}><span className="mono">{image.rootfsSha256 || "—"}</span></DetailProp>
            {image.initrdSha256 && (
              <DetailProp label="initrd SHA256" copy={image.initrdSha256}><span className="mono">{image.initrdSha256}</span></DetailProp>
            )}
            <DetailProp label={t("Minimum disk", "최소 디스크")}>{minDiskGb > 0 ? `${minDiskGb} GiB` : "—"}</DetailProp>
            <DetailProp label={t("Rootfs size", "rootfs 크기")}>{formatRootfsSize(image.rootfsSizeBytes)}</DetailProp>
          </dl>
        </DetailSection>

        <DetailSection title={t("VMs using it", "사용 중인 VM")}>
          {usedByError ? (
            <p className="field-error">{usedByError}</p>
          ) : usedByVms === null ? (
            <p className="detail-note">{t("Loading…", "불러오는 중…")}</p>
          ) : usedByVms.length === 0 ? (
            <p className="detail-note">{t("None", "없음")}</p>
          ) : (
            <ul className="detail-list">
              {usedByVms.map((vm) => (
                <li key={vm.id}>
                  <span className="detail-list-main">{vm.name}</span>
                  <StatusBadge state={vm.state} />
                </li>
              ))}
            </ul>
          )}
        </DetailSection>

        {image.installed && (
          <DetailSection title={t("Update image kernel", "이미지 커널 업데이트")}>
            <div className="field detail-field">
              <label htmlFor={`image-kernel-${image.alias}`} className="sr-only">{t("Update image kernel", "이미지 커널 업데이트")}</label>
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
            </div>
            <div className="detail-sec-buttons">
              <button type="button" className="btn primary" disabled={!canUpdateKernel} onClick={() => void onUpdateKernel(targetKernel)}>
                {kernelUpdateBusy ? t("Updating…", "업데이트 중…") : t("Update kernel", "커널 업데이트")}
              </button>
            </div>
            <p className="detail-note">
              {t("Only images with no VM references can change their kernel.", "VM이 연결되지 않은 이미지만 커널을 변경할 수 있습니다.")}
            </p>
          </DetailSection>
        )}
      </div>
    </>
  );
}

function ImageJobLog({ job }: { job: ImageInstallResponse }) {
  const { t } = useI18n();
  const label = t("OCI import log", "OCI 가져오기 로그");

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
          filename={logDownloadFilename("m2image-oci", job.alias)}
          buttonClassName="btn console-bar-btn"
          disabled={!job.log}
        />
      </div>
      <pre className="detail-log image-install-log" ref={logRef}>{job.log}</pre>
    </div>
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

function registryImageName(image: RegistryImage): string {
  const [name, digest] = image.reference.slice(image.reference.indexOf("/") + 1).split("@");
  return digest ? `${name}@${digest.slice(0, "sha256:".length + 12)}` : name;
}

function registryProviderName(image: RegistryImage): string {
  const host = image.reference.split("/")[0];
  if (host === "docker.io" || host === "registry-1.docker.io") return "Docker Hub";
  if (host === "ghcr.io") return "GHCR";
  if (host === "public.ecr.aws" || /^\d{12}\.dkr\.ecr\.[a-z0-9-]+\.amazonaws\.com(?:\.cn)?$/.test(host)) return "ECR";
  return "Harbor";
}

function PulledImageRow({ image, onBuilt }: {
  image: RegistryImage;
  onBuilt: (alias: string) => Promise<void>;
}) {
  const { t } = useI18n();
  const [job, setJob] = useState<ImageInstallResponse | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [starting, setStarting] = useState(false);
  const [generation, setGeneration] = useState(0);
  const running = starting || job?.status === "running";

  useEffect(() => {
    if (!image.templateAlias) return;
    let cancelled = false;
    let timer: ReturnType<typeof setTimeout>;
    async function poll() {
      try {
        const next = await getOciImport(image.templateAlias);
        if (cancelled) return;
        setJob(current => keepNewestJobSnapshot(current, next));
        if (next.status === "running") {
          timer = setTimeout(poll, 500);
        } else if (next.status === "succeeded") {
          await onBuilt(next.alias);
        } else if (next.status === "failed") {
          setError(lastLogLine(next.log, t("Image build failed.", "이미지 빌드에 실패했다.")));
        }
      } catch (failure) {
        if (!cancelled) {
          setError(apiErrorText(failure));
          timer = setTimeout(poll, 2_000);
        }
      }
    }
    void poll();
    return () => { cancelled = true; clearTimeout(timer); };
  }, [image.templateAlias, generation, onBuilt, t]);

  async function build() {
    if (running || !image.templateAlias) return;
    setStarting(true);
    setError(null);
    try {
      const started = await startOciImport({ reference: pinnedRegistryReference(image) });
      setJob(started);
      setGeneration(value => value + 1);
    } catch (failure) {
      if (failure instanceof ApiClientError && failure.apiError?.code === "import_in_progress") {
        setGeneration(value => value + 1);
      } else {
        setError(apiErrorText(failure));
      }
    } finally {
      setStarting(false);
    }
  }

  return <>
    <tr className="pulled-image-row">
      <td className="mono" title={image.reference}>{registryImageName(image)}</td>
      <td className="image-registry" data-label={t("Registry", "레지스트리")} title={image.reference.split("/")[0]}>{registryProviderName(image)}</td>
      <td className="mono" data-label={t("Minimum disk", "최소 디스크")}>—</td>
      <td className="state-cell">
        <span className={`state-badge${running ? " starting" : job?.status === "failed" ? " error" : ""}`}>
          {running ? t("Building…", "빌드 중…") : job?.status === "failed" ? t("Build failed", "빌드 실패") : t("Build required", "빌드 필요")}
        </span>
        <OptionsMenu items={[{ label: t("Build M2Image", "M2Image 빌드"), disabled: running || !image.templateAlias, onClick: () => void build() }]} />
      </td>
    </tr>
    {(error || (job && job.status !== "idle")) && <tr className="pulled-image-log"><td colSpan={4}>
      {error && <p className="field-error" role="alert">{error}</p>}
      {job && job.status !== "idle" && <ImageJobLog job={job} />}
    </td></tr>}
  </>;
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
          <ImageJobLog job={job} />
        </>
      )}
    </section>
  );
}

/** M2Image lists installed templates and pulled OCI sources awaiting a build. */
export default function Images() {
  const { t } = useI18n();
  const { setDetailOpen } = useScreenStatus();
  const [images, setImages] = useState<ImageResponse[] | null>(null);
  const [listError, setListError] = useState<string | null>(null);
  const [pulledImages, setPulledImages] = useState<RegistryImage[]>([]);
  const [pulledImagesError, setPulledImagesError] = useState<string | null>(null);
  const [kernels, setKernels] = useState<KernelResponse[]>([]);
  const [busyAlias, setBusyAlias] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  const [selectedAlias, setSelectedAlias] = useState<string | null>(null);

  const refreshList = useCallback(async () => {
    try {
      const next = await listImages();
      setImages(next.filter((image) => image.installed));
      setListError(null);
    } catch (error) {
      setListError((error as Error).message);
    }
  }, []);

  const handleOciImported = useCallback(async (alias: string) => {
    await refreshList();
    setSelectedAlias(alias);
  }, [refreshList]);

  const refreshPulledImages = useCallback(async () => {
    try {
      setPulledImages(await getRegistryImages());
      setPulledImagesError(null);
    } catch (error) {
      setPulledImagesError((error as Error).message);
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
    void refreshPulledImages();
    void refreshKernels();
  }, [refreshList, refreshPulledImages, refreshKernels]);

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

  // Follow the same pattern as MicroNetworks' `getMicroNetwork(selectedId)`:
  // fetch current usage separately on selection because the list does not include it.
  useEffect(() => {
    if (!selectedAlias) {
      setUsedByVms(null);
      setUsedByError(null);
      return;
    }
    refreshUsedByVms(selectedAlias);
  }, [selectedAlias, refreshUsedByVms]);

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
      if (selectedAlias === alias) refreshUsedByVms(alias);
    } catch (error) {
      setActionError((error as Error).message);
    } finally {
      setBusyAlias(null);
    }
  };

  const detailPanel = selectedImage ? (
    <ImageDetail
      image={selectedImage}
      kernels={kernels}
      kernelUpdateBusy={busyAlias === selectedImage.alias}
      onUpdateKernel={(version) => handleUpdateKernel(selectedImage.alias, version)}
      usedByVms={usedByVms}
      usedByError={usedByError}
    />
  ) : null;
  const screen = (body: ReactNode) => (
    <Screen
      view="images"
      detail={detailPanel}
      detailLabel={t("Image detail", "이미지 상세")}
      detailKind={<><Layers aria-hidden="true" />M2Image</>}
      onCloseDetail={() => setSelectedAlias(null)}
    >
      {body}
    </Screen>
  );

  if (images === null && !listError) {
    return screen(<div className="empty">{t("Loading image catalog…", "이미지 목록 불러오는 중…")}</div>);
  }

  return screen(
    <>
      {actionError && <div className="field-error">{actionError}</div>}
      <section className="panel">
        <h2 className="panel-title"><span>M2Image</span><button className="btn" type="button" onClick={() => void Promise.all([refreshList(), refreshPulledImages()])}>{t("Refresh", "새로 고침")}</button></h2>
        <p className="panel-intro">
          {t(
            "Manage bootable microVM images and build images pulled from Pipe Registry. Build required images become available for VM creation after building.",
            "부팅 가능한 microVM 이미지와 Pipe Registry에서 가져온 이미지를 관리한다. 빌드가 필요한 이미지는 빌드 완료 후 VM 생성에 사용할 수 있다.",
          )}
        </p>
        {listError && <div className="field-error">{listError}</div>}
        {pulledImagesError && <p className="field-error" role="alert">{t("Could not load pulled images: ", "가져온 이미지 목록을 조회하지 못했다: ")}{pulledImagesError}</p>}
        {images?.length === 0 && pulledImages.length === 0 && !listError && !pulledImagesError && (
          <div className="empty">{t("No installed images.", "설치된 이미지가 없습니다.")}</div>
        )}
        {((images?.length ?? 0) > 0 || (images !== null && pulledImages.length > 0)) && (
          <table className="vm-table image-table">
            <thead>
              <tr>
                <th>{t("Image", "이미지")}</th>
                <th>{t("Registry", "레지스트리")}</th>
                <th>{t("Minimum disk", "최소 디스크")}</th>
                <th>{t("Status", "상태")}</th>
              </tr>
            </thead>
            <tbody>
              {(images ?? []).map((image) => {
                const minDiskGb = image.minDiskGb;
                const statusLabel = image.installed
                  ? t("Installed", "설치됨")
                  : t("Not installed", "미설치");
                const source = pulledImages.find(source => source.templateAlias === image.alias);
                return (
                  <tr
                    key={image.alias}
                    className={selectedAlias === image.alias ? "is-selected" : undefined}
                    aria-selected={selectedAlias === image.alias}
                    onClick={() => {
                      setSelectedAlias(image.alias);
                      setDetailOpen(true);
                    }}
                  >
                    <td className="mono" title={image.alias}>
                      {source ? registryImageName(source) : image.alias}
                    </td>
                    <td className="image-registry" data-label={t("Registry", "레지스트리")} title={source?.reference.split("/")[0]}>{source ? registryProviderName(source) : "—"}</td>
                    <td className="mono" data-label={t("Minimum disk", "최소 디스크")}>{minDiskGb > 0 ? `${minDiskGb} GiB` : "—"}</td>
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
              {images !== null && pulledImages.filter(image => !images.some(installed => installed.installed && installed.alias === image.templateAlias)).map(image => (
                <PulledImageRow key={image.templateAlias} image={image} onBuilt={handleOciImported} />
              ))}
            </tbody>
          </table>
        )}
      </section>
      <OciImportPanel
        images={images ?? []}
        onImported={handleOciImported}
      />
    </>,
  );
}
