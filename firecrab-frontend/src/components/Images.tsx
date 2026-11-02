import PackageRegistration from "./PackageRegistration";
import PipeServiceMenu from "./PipeServiceMenu";
import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type FormEvent,
  type ReactNode,
} from "react";
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
  getOciImport,
  inspectOciImage,
  listImages,
  listKernels,
  listVms,
  startOciImport,
  updateImageKernel,
} from "../api/client";
import { logDownloadFilename } from "../lib/textExport";
import {
  getRegistryImages,
  pinnedRegistryReference,
  type RegistryImage,
} from "../api/registries";
import Banner from "./Banner";
import { ImagePackageSection, visibleImage } from "./ImagePackages";
import LogExportActions from "./LogExportActions";
import { useI18n } from "../i18n";
import { Layers } from "lucide-react";
import Screen from "./layout/Screen";
import { useScreenStatus } from "./layout/screenStatus";
import ImageDetail from "./ImageDetail";

function packageDownloadPercent(job: ImageInstallResponse): number | null {
  const total = job.totalBytes;
  if (!total || total <= 0) return null;
  return Math.min(100, Math.round(((job.downloadedBytes ?? 0) / total) * 100));
}

function formatTransferRate(bytesPerSecond: number | null): string {
  if (bytesPerSecond === null || !Number.isFinite(bytesPerSecond)) return "—";
  if (bytesPerSecond >= 1024 ** 3)
    return `${(bytesPerSecond / 1024 ** 3).toFixed(1)} GiB/s`;
  if (bytesPerSecond >= 1024 ** 2)
    return `${(bytesPerSecond / 1024 ** 2).toFixed(1)} MiB/s`;
  if (bytesPerSecond >= 1024)
    return `${(bytesPerSecond / 1024).toFixed(1)} KiB/s`;
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
    return elapsedMs > 0
      ? ((job.downloadedBytes ?? 0) * 1000) / elapsedMs
      : null;
  });

  useEffect(() => {
    const now = Date.now();
    const downloadedBytes = job.downloadedBytes ?? 0;
    const previous = sampleRef.current;
    if (previous.startedAtMs !== job.startedAtMs) {
      const elapsedMs = job.startedAtMs ? now - job.startedAtMs : 0;
      setBytesPerSecond(
        elapsedMs > 0 ? (downloadedBytes * 1000) / elapsedMs : null,
      );
    } else {
      const elapsedMs = now - previous.sampledAtMs;
      const transferredBytes = downloadedBytes - previous.downloadedBytes;
      if (elapsedMs > 0 && transferredBytes >= 0) {
        setBytesPerSecond((transferredBytes * 1000) / elapsedMs);
      }
    }
    sampleRef.current = {
      startedAtMs: job.startedAtMs,
      downloadedBytes,
      sampledAtMs: now,
    };
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
        <span
          className="package-progress-fill"
          style={{ width: `${barPercent}%` }}
        />
      </span>
      <span className="package-progress-speed">
        {formatTransferRate(bytesPerSecond)}
      </span>
    </span>
  );
}

function OciImportProgress({ job }: { job: ImageInstallResponse }) {
  const { t } = useI18n();
  const log = job.log ?? "";
  const current =
    job.status === "succeeded"
      ? 4
      : log.includes("pairing host kernel")
        ? 3
        : log.includes("provisioning guest runtime") ||
            log.includes("writing ext4 image")
          ? 2
          : log.includes("reading image process config") ||
              log.includes("decompressing layers")
            ? 1
            : 0;
  const steps = [
    t("Download", "다운로드"),
    t("Prepare files", "파일 준비"),
    t("Build image", "이미지 생성"),
    t("Connect kernel", "커널 연결"),
  ];
  const labels = [
    t("Downloading image", "이미지 다운로드 중"),
    t("Preparing image files", "이미지 파일 준비 중"),
    t("Building image", "이미지 생성 중"),
    t("Connecting kernel", "커널 연결 중"),
  ];
  const status =
    job.status === "succeeded"
      ? t("Import complete", "가져오기 완료")
      : job.status === "failed"
        ? t("Import failed", "가져오기 실패")
        : labels[current];
  return (
    <div
      className="kernel-react"
      aria-label={t("Image import progress", "이미지 가져오기 진행 상황")}
    >
      <strong>{status}</strong>
      <progress
        max={4}
        value={current}
        aria-label={t("Installation stages", "설치 단계")}
      />
      <ol className="kernel-react-steps">
        {steps.map((step, i) => (
          <li
            key={step}
            aria-current={i === current && current < 4 ? "step" : undefined}
          >
            {step}
          </li>
        ))}
      </ol>
      {current === 0 &&
        job.status === "running" &&
        job.totalBytes != null &&
        job.totalBytes > 0 && (
          <PackageDownloadProgress
            job={job}
            label={t("OCI download progress", "OCI 다운로드 진행 상황")}
          />
        )}
    </div>
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
  if (
    currentStarted !== undefined &&
    incomingStarted !== undefined &&
    incomingStarted < currentStarted
  ) {
    return current;
  }
  const currentIsTerminal =
    current.status === "succeeded" || current.status === "failed";
  if (
    currentStarted !== undefined &&
    incomingStarted === currentStarted &&
    currentIsTerminal &&
    incoming.status === "running"
  ) {
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
  items: {
    label: string;
    onClick: () => void;
    disabled: boolean;
    danger?: boolean;
  }[];
}) {
  const { t } = useI18n();
  return (
    <PipeServiceMenu
      label={t("Image actions", "이미지 작업")}
      disabled={false}
      items={items.map((item) => ({
        label: item.label,
        onSelect: item.onClick,
        disabled: item.disabled,
        danger: item.danger,
      }))}
    />
  );
}

function ImageJobLog({ job }: { job: ImageInstallResponse }) {
  const { t } = useI18n();
  const label = t("OCI import log", "OCI 가져오기 로그");

  return (
    <div className="subpanel">
      <div className="log-export-bar">
        <span className="log-export-bar-label">
          {label} — {job.alias}
        </span>
        <LogExportActions
          text={job.log}
          filename={logDownloadFilename("m2image-oci", job.alias)}
          buttonClassName="btn sm"
          disabled={!job.log}
        />
      </div>
      <details>
        <summary>{t("Detailed log", "상세 로그")}</summary>
        <pre className="detail-log image-install-log">{job.log}</pre>
      </details>
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

function registryImageName(image: RegistryImage): string {
  const [name, digest] = image.reference
    .slice(image.reference.indexOf("/") + 1)
    .split("@");
  return digest ? `${name}@${digest.slice(0, "sha256:".length + 12)}` : name;
}

function registryProviderName(image: RegistryImage): string {
  const host = image.reference.split("/")[0];
  if (host === "docker.io" || host === "registry-1.docker.io")
    return "Docker Hub";
  if (host === "ghcr.io") return "GHCR";
  if (
    host === "public.ecr.aws" ||
    /^\d{12}\.dkr\.ecr\.[a-z0-9-]+\.amazonaws\.com(?:\.cn)?$/.test(host)
  )
    return "ECR";
  return "Harbor";
}

function PulledImageRow({
  image,
  onBuilt,
}: {
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
        setJob((current) => keepNewestJobSnapshot(current, next));
        if (next.status === "running") {
          timer = setTimeout(poll, 500);
        } else if (next.status === "succeeded") {
          await onBuilt(next.alias);
        } else if (next.status === "failed") {
          setError(
            lastLogLine(
              next.log,
              t("Image build failed.", "이미지를 빌드하지 못했습니다."),
            ),
          );
        }
      } catch (failure) {
        if (!cancelled) {
          setError(apiErrorText(failure));
          timer = setTimeout(poll, 2_000);
        }
      }
    }
    void poll();
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, [image.templateAlias, generation, onBuilt, t]);

  async function build() {
    if (running || !image.templateAlias) return;
    setStarting(true);
    setError(null);
    try {
      const started = await startOciImport({
        reference: pinnedRegistryReference(image),
      });
      setJob(started);
      setGeneration((value) => value + 1);
    } catch (failure) {
      if (
        failure instanceof ApiClientError &&
        failure.apiError?.code === "import_in_progress"
      ) {
        setGeneration((value) => value + 1);
      } else {
        setError(apiErrorText(failure));
      }
    } finally {
      setStarting(false);
    }
  }

  return (
    <>
      <tr className="pulled-image-row">
        <td className="mono" title={image.reference}>
          {registryImageName(image)}
        </td>
        <td
          className="image-registry"
          data-label={t("Registry", "레지스트리")}
          title={image.reference.split("/")[0]}
        >
          {registryProviderName(image)}
        </td>
        <td className="mono" data-label={t("Minimum disk", "최소 디스크")}>
          —
        </td>
        <td className="state-cell">
          <div className="state-cell-content">
            <span
              className={`state-badge${running ? " starting" : job?.status === "failed" ? " error" : ""}`}
            >
              {running
                ? t("Building…", "빌드 중…")
                : job?.status === "failed"
                  ? t("Build failed", "빌드 실패")
                  : t("Build required", "빌드 필요")}
            </span>
            <OptionsMenu
              items={[
                {
                  label: t("Build M2Image", "M2Image 빌드"),
                  disabled: running || !image.templateAlias,
                  onClick: () => void build(),
                },
              ]}
            />
          </div>
        </td>
      </tr>
      {(error || (job && job.status !== "idle")) && (
        <tr className="pulled-image-log">
          <td colSpan={4}>
            {error && (
              <p className="field-error" role="alert">
                {error}
              </p>
            )}
            {job && job.status !== "idle" && <ImageJobLog job={job} />}
          </td>
        </tr>
      )}
    </>
  );
}

/**
 * Inspect an OCI reference, then start/poll `POST/GET /api/oci/import`
 * with the same job snapshot the M2Image install path already uses.
 */
const OCI_ACTIVE_KEY = "firecrab.oci-import.active";
function savedOciAlias(): string | null {
  try {
    return window.sessionStorage.getItem(OCI_ACTIVE_KEY);
  } catch {
    return null;
  }
}
function saveOciAlias(alias: string | null) {
  try {
    if (alias) window.sessionStorage.setItem(OCI_ACTIVE_KEY, alias);
    else window.sessionStorage.removeItem(OCI_ACTIVE_KEY);
  } catch {
    /* Storage may be disabled. */
  }
}

function OciImportPanel({
  images,
  onImported,
}: {
  images: ImageResponse[];
  onImported: (alias: string) => Promise<void>;
}) {
  const { t } = useI18n();
  const [reference, setReference] = useState("");
  const [inspectedReference, setInspectedReference] = useState<string | null>(
    null,
  );
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

  const pollImport = useCallback(
    (alias: string) => {
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
          saveOciAlias(null);
          if (latest.status === "succeeded") {
            setError(null);
            setInfo(
              t(
                `Imported as ${latest.alias}. It now appears in M2Image.`,
                `이미지를 가져왔습니다. M2Image 목록에서 ${latest.alias} 항목을 확인하세요.`,
              ),
            );
            await onImported(latest.alias);
          } else if (latest.status === "failed") {
            setInfo(null);
            setError(
              lastLogLine(
                latest.log,
                t("OCI import failed.", "OCI 가져오기에 실패했습니다."),
              ),
            );
          }
        } catch {
          if (mountedRef.current) setTimeout(() => void tick(), 500);
          else stop();
        }
      };
      void tick();
    },
    [onImported, t],
  );

  const resumeImport = useCallback(
    async (alias: string) => {
      try {
        const latest = await getOciImport(alias);
        if (!mountedRef.current) return;
        setJob((current) => keepNewestJobSnapshot(current, latest));
        if (latest.status !== "running") saveOciAlias(null);
        if (latest.status === "running") {
          saveOciAlias(alias);
          pollImport(alias);
        } else if (latest.status === "succeeded") {
          setInfo(
            t(
              `Imported as ${latest.alias}. It now appears in M2Image.`,
              `이미지를 가져왔습니다. M2Image 목록에서 ${latest.alias} 항목을 확인하세요.`,
            ),
          );
          await onImported(latest.alias);
        } else if (latest.status === "failed") {
          setError(
            lastLogLine(
              latest.log,
              t("OCI import failed.", "OCI 가져오기에 실패했습니다."),
            ),
          );
        }
      } catch {
        // Idle or a missing job is the ordinary case after a fresh inspect.
      }
    },
    [onImported, pollImport, t],
  );

  useEffect(() => {
    const alias = savedOciAlias();
    if (alias) void resumeImport(alias);
  }, [resumeImport]);

  const handleInspect = async (event: FormEvent) => {
    event.preventDefault();
    const trimmed = reference.trim();
    if (!trimmed) {
      setInspect(null);
      setInspectedReference(null);
      setInfo(null);
      setError(
        t("Enter an image reference.", "이미지 주소나 이름을 입력하세요."),
      );
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
      saveOciAlias(started.alias);
      pollImport(started.alias);
    } catch (err) {
      if (!mountedRef.current) return;
      const code =
        err instanceof ApiClientError ? err.apiError?.code : undefined;
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

  const inspectMatchesInput =
    inspect !== null && inspectedReference === reference.trim();
  const registered =
    job?.status === "succeeded"
      ? (images.find((image) => image.alias === job.alias) ?? null)
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

  return (
    <section className="panel">
      <h2 className="panel-title">OCI</h2>
      <p className="panel-intro">
        {t(
          "Inspect a container image, then import it as a bootable template on this host. The registered alias appears in M2Image.",
          "컨테이너 이미지를 검사한 뒤 이 호스트에서 부팅할 수 있는 템플릿으로 가져옵니다. 등록한 이미지는 M2Image 목록에서 확인할 수 있습니다.",
        )}
      </p>
      {(error || info) && (
        <div style={{ marginBottom: "1rem" }}>
          {error && (
            <Banner
              kind="error"
              text={error}
              onDismiss={() => setError(null)}
            />
          )}
          {info && (
            <Banner kind="info" text={info} onDismiss={() => setInfo(null)} />
          )}
        </div>
      )}
      <form
        className="create-grid"
        onSubmit={(event) => void handleInspect(event)}
      >
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
              {t("Compatible with this host.", "이 호스트와 호환됩니다.")}{" "}
              {inspect.singlePlatform
                ? t(
                    `Single-platform image for ${inspect.architecture}.`,
                    `${inspect.architecture} 단일 플랫폼 이미지입니다.`,
                  )
                : t(
                    `This host selected the ${inspect.architecture} platform from a multi-arch index.`,
                    `이 호스트에 맞는 ${inspect.architecture} 이미지를 다중 아키텍처 목록에서 선택했습니다.`,
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
            <dd>
              {inspect.registry}/{inspect.repository}
            </dd>
          </dl>
        </div>
      )}

      {job && job.status !== "idle" && (
        <>
          <div className="subpanel">
            <OciImportProgress job={job} />
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
  const [pulledImagesError, setPulledImagesError] = useState<string | null>(
    null,
  );
  const [kernels, setKernels] = useState<KernelResponse[]>([]);
  const [busyAlias, setBusyAlias] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  const [selectedAlias, setSelectedAlias] = useState<string | null>(null);
  const [importOpen, setImportOpen] = useState(() => Boolean(savedOciAlias()));
  useEffect(() => {
    if (savedOciAlias()) setDetailOpen(true);
  }, [setDetailOpen]);

  const refreshList = useCallback(async () => {
    try {
      const next = await listImages();
      setImages(next.filter(visibleImage));
      setListError(null);
    } catch (error) {
      setListError((error as Error).message);
    }
  }, []);

  const handleOciImported = useCallback(
    async (alias: string) => {
      await refreshList();
      setSelectedAlias(alias);
    },
    [refreshList],
  );

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

  const selectedImage =
    (images ?? []).find((image) => image.alias === selectedAlias) ?? null;

  const [usage, setUsage] = useState<{
    alias: string;
    vms: VmResponse[] | null;
    error: string | null;
  } | null>(null);
  const usedByVms = usage?.alias === selectedAlias ? usage.vms : null;
  const usedByError = usage?.alias === selectedAlias ? usage.error : null;

  useEffect(() => {
    let active = true;
    setUsage(null);
    if (selectedAlias) {
      listVms()
        .then((vms) => {
          if (active)
            setUsage({
              alias: selectedAlias,
              vms: vms.filter((vm) => vm.template === selectedAlias),
              error: null,
            });
        })
        .catch((error) => {
          if (active)
            setUsage({
              alias: selectedAlias,
              vms: null,
              error: (error as Error).message,
            });
        });
    }
    return () => {
      active = false;
    };
  }, [selectedAlias]);

  const handleUpdateKernel = async (alias: string, version: string) => {
    setBusyAlias(alias);
    setActionError(null);
    try {
      await updateImageKernel(alias, { kernelVersion: version });
      await Promise.all([refreshList(), refreshKernels()]);
    } catch (error) {
      if (
        error instanceof ApiClientError &&
        error.apiError?.code === "in_use"
      ) {
        const count = error.apiError.fields?.count ?? "";
        const vms = error.apiError.fields?.vms ?? "";
        setActionError(
          t(
            `'${alias}' has ${count} VM(s) still using it, so its kernel cannot change: ${vms}. Delete those VMs first.`,
            `이 이미지를 사용하는 VM이 있어 커널을 변경할 수 없습니다. 이미지: ${alias}, VM ${count}개: ${vms}. 먼저 해당 VM을 삭제하세요.`,
          ),
        );
      } else {
        setActionError((error as Error).message);
      }
    } finally {
      setBusyAlias(null);
    }
  };

  const handleDelete = async (alias: string) => {
    if (
      !window.confirm(
        t(`Delete image ${alias}?`, `"${alias}" 이미지를 삭제할까요?`),
      )
    )
      return;
    setBusyAlias(alias);
    setActionError(null);
    try {
      await deleteImage(alias);
      await refreshList();
      if (selectedAlias === alias) setSelectedAlias(null);
    } catch (cause) {
      setActionError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setBusyAlias(null);
    }
  };

  const detailPanel = importOpen ? (
    <OciImportPanel images={images ?? []} onImported={handleOciImported} />
  ) : selectedImage ? (
    <>
      <ImageDetail
        key={`${selectedImage.alias}:${selectedImage.kernelVersion}`}
        image={selectedImage}
        kernels={kernels}
        busy={busyAlias !== null}
        onDelete={() => handleDelete(selectedImage.alias)}
        onUpdateKernel={(version) =>
          handleUpdateKernel(selectedImage.alias, version)
        }
        usedByVms={usedByVms}
        usedByError={usedByError}
      />
      <ImagePackageSection
        key={selectedImage.alias}
        image={selectedImage}
        onChanged={refreshList}
      />
    </>
  ) : null;
  const screen = (body: ReactNode) => (
    <Screen
      view="images"
      actions={
        <>
          <PackageRegistration onChanged={refreshList} />
          <button
            type="button"
            className="btn primary"
            onClick={() => {
              setSelectedAlias(null);
              setImportOpen(true);
              setDetailOpen(true);
            }}
          >
            {t("Import OCI image", "OCI 이미지 가져오기")}
          </button>
        </>
      }
      detail={detailPanel}
      detailLabel={t("Image detail", "이미지 상세")}
      detailKind={
        <>
          <Layers aria-hidden="true" />
          M2Image
        </>
      }
      onCloseDetail={() => {
        setSelectedAlias(null);
        setImportOpen(false);
      }}
    >
      {body}
    </Screen>
  );

  if (images === null && !listError) {
    return screen(
      <div className="empty">
        {t("Loading image catalog…", "이미지 목록 불러오는 중…")}
      </div>,
    );
  }

  return screen(
    <>
      {actionError && <div className="field-error">{actionError}</div>}
      <section className="panel">
        <div className="list-actions">
          <button
            className="btn"
            type="button"
            onClick={() =>
              void Promise.all([refreshList(), refreshPulledImages()])
            }
          >
            {t("Refresh", "새로 고침")}
          </button>
        </div>
        <p className="panel-intro">
          {t(
            "Manage bootable microVM images and build images pulled from Pipe Registry. Build required images become available for VM creation after building.",
            "부팅 가능한 MicroVM 이미지와 Pipe Registry에서 가져온 이미지를 관리합니다. 빌드가 필요한 이미지는 빌드를 마친 뒤 VM 생성에 사용할 수 있습니다.",
          )}
        </p>
        {listError && <div className="field-error">{listError}</div>}
        {pulledImagesError && (
          <p className="field-error" role="alert">
            {t(
              "Could not load pulled images: ",
              "가져온 이미지 목록을 불러오지 못했습니다: ",
            )}
            {pulledImagesError}
          </p>
        )}
        {images?.length === 0 &&
          pulledImages.length === 0 &&
          !listError &&
          !pulledImagesError && (
            <div className="empty">
              {t("No installed images.", "설치된 이미지가 없습니다.")}
            </div>
          )}
        {((images?.length ?? 0) > 0 ||
          (images !== null && pulledImages.length > 0)) && (
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
                const source = pulledImages.find(
                  (source) => source.templateAlias === image.alias,
                );
                return (
                  <tr
                    key={image.alias}
                    className={
                      selectedAlias === image.alias ? "is-selected" : undefined
                    }
                    aria-selected={selectedAlias === image.alias}
                    onClick={() => {
                      setImportOpen(false);
                      setSelectedAlias(image.alias);
                      setDetailOpen(true);
                    }}
                  >
                    <td className="mono" title={image.alias}>
                      {source ? registryImageName(source) : image.alias}
                    </td>
                    <td
                      className="image-registry"
                      data-label={t("Registry", "레지스트리")}
                      title={source?.reference.split("/")[0]}
                    >
                      {source ? registryProviderName(source) : "—"}
                    </td>
                    <td
                      className="mono"
                      data-label={t("Minimum disk", "최소 디스크")}
                    >
                      {minDiskGb > 0 ? `${minDiskGb} GiB` : "—"}
                    </td>
                    <td className="state-cell">
                      <div className="state-cell-content">
                        <span
                          className={`state-badge${image.installed ? " running" : ""}`}
                        >
                          {statusLabel}
                        </span>
                      </div>
                    </td>
                  </tr>
                );
              })}
              {images !== null &&
                pulledImages
                  .filter(
                    (image) =>
                      !images.some(
                        (installed) =>
                          installed.installed &&
                          installed.alias === image.templateAlias,
                      ),
                  )
                  .map((image) => (
                    <PulledImageRow
                      key={image.templateAlias}
                      image={image}
                      onBuilt={handleOciImported}
                    />
                  ))}
            </tbody>
          </table>
        )}
      </section>
    </>,
  );
}
