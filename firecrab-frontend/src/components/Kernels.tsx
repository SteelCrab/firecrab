import { useCallback, useEffect, useRef, useState } from "react";
import {
  Image as ImageIcon,
  ChevronDown,
  RotateCw,
  ArrowDown,
  ArrowUp,
  ArrowDownUp,
  Square,
  Trash2,
  Download,
} from "lucide-react";
import type {
  ImageResponse,
  KernelInstallResponse,
  KernelResponse,
} from "../bindings";
import {
  ApiClientError,
  cancelKernelInstall,
  deleteKernel,
  getKernelInstall,
  listImages,
  listKernels,
  startKernelInstall,
} from "../api/client";
import { useI18n } from "../i18n";
import { VIEW_ICONS } from "../viewIcons";
import {
  KERNEL_STEPS,
  kernelBytes,
  kernelStage,
  kernelSteps,
  kernelDownloadSource,
  registryInstall,
} from "../lib/kernelInstall";
import KernelInstallation, { KernelElapsed } from "./KernelInstallation";
import Screen from "./layout/Screen";
import { useScreenStatus } from "./layout/screenStatus";
import { DetailHead, DetailProp, DetailSection } from "./ui/DetailSection";
import "../styles/kernels.css";

type Jobs = Record<string, KernelInstallResponse | null>;
const KernelIcon = VIEW_ICONS.kernels;

export default function Kernels() {
  const { t, locale } = useI18n();
  const { setDetailOpen } = useScreenStatus();
  const [kernels, setKernels] = useState<KernelResponse[] | null>(null);
  const [dateOrder, setDateOrder] = useState<"ascending" | "descending" | null>(
    null,
  );
  const [images, setImages] = useState<ImageResponse[]>([]);
  const [jobs, setJobs] = useState<Jobs>({});
  const [unavailable, setUnavailable] = useState<Set<string>>(new Set());
  const [selected, setSelected] = useState("");
  const [busy, setBusy] = useState("");
  const [busyAction, setBusyAction] = useState<
    "install" | "cancel" | "delete" | null
  >(null);
  const [error, setError] = useState("");
  const selectionMade = useRef(false);
  const active = useRef(false);
  const generation = useRef(0);
  const busyRef = useRef(false);
  const refresh = useCallback(async () => {
    const id = ++generation.current;
    try {
      const list = await listKernels();
      const [snapshots, installedImages] = await Promise.all([
        Promise.all(
          list.map((kernel) =>
            getKernelInstall(kernel.version).catch(() => null),
          ),
        ),
        listImages().catch(() => []),
      ]);
      if (!active.current || id !== generation.current) return;
      setKernels(list);
      setImages(installedImages);
      setJobs((previous) =>
        Object.fromEntries(
          list.map((kernel, index) => [
            kernel.version,
            snapshots[index] ?? previous[kernel.version] ?? null,
          ]),
        ),
      );
      setUnavailable(
        new Set(
          list
            .filter((_, index) => !snapshots[index])
            .map((kernel) => kernel.version),
        ),
      );
      if (!selectionMade.current) {
        const automatic =
          list.find(
            (_, index) =>
              snapshots[index]?.status === "running" &&
              kernelStage(snapshots[index]!) === 3,
          ) ??
          list.find((_, index) => snapshots[index]?.status === "running") ??
          list.find((_, index) => snapshots[index]?.status === "failed");
        if (automatic) {
          selectionMade.current = true;
          setSelected(automatic.version);
          setDetailOpen(true);
        } else if (snapshots.every(Boolean)) selectionMade.current = true;
      }
      setError("");
    } catch (cause) {
      if (active.current && id === generation.current)
        setError(cause instanceof Error ? cause.message : String(cause));
    }
  }, [setDetailOpen]);
  useEffect(() => {
    active.current = true;
    let timer = 0;
    let stopped = false;
    const tick = async () => {
      if (stopped) return;
      if (!busyRef.current) await refresh();
      if (!stopped) timer = window.setTimeout(() => void tick(), 3000);
    };
    void tick();
    return () => {
      stopped = true;
      active.current = false;
      window.clearTimeout(timer);
    };
  }, [refresh]);
  const select = (version: string) => {
    selectionMade.current = true;
    setSelected(version);
    setDetailOpen(true);
  };
  const install = async (version: string) => {
    if (
      busyRef.current ||
      !jobs[version] ||
      unavailable.has(version) ||
      jobs[version]?.status === "running"
    )
      return;
    busyRef.current = true;
    setBusy(version);
    setBusyAction("install");
    setError("");
    generation.current++;
    try {
      const job = await startKernelInstall(version);
      if (active.current)
        setJobs((current) => ({ ...current, [version]: job }));
    } catch (cause) {
      if (
        !(
          cause instanceof ApiClientError &&
          cause.status === 409 &&
          cause.apiError?.code === "kernel_install_in_progress"
        )
      ) {
        if (active.current)
          setError(cause instanceof Error ? cause.message : String(cause));
      }
    } finally {
      busyRef.current = false;
      if (active.current) {
        setBusy("");
        setBusyAction(null);
        await refresh();
      }
    }
  };
  const cancel = async (version: string) => {
    if (
      busyRef.current ||
      unavailable.has(version) ||
      jobs[version]?.status !== "running" ||
      jobs[version]?.cancelling
    )
      return;
    busyRef.current = true;
    setBusy(version);
    setBusyAction("cancel");
    setError("");
    generation.current++;
    try {
      const job = await cancelKernelInstall(version);
      if (active.current)
        setJobs((current) => ({ ...current, [version]: job }));
    } catch (cause) {
      if (active.current)
        setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      busyRef.current = false;
      if (active.current) {
        setBusy("");
        setBusyAction(null);
        await refresh();
      }
    }
  };
  const remove = async (kernel: KernelResponse) => {
    const attempt = jobs[kernel.version];
    const running = attempt?.status === "running";
    if (
      busyRef.current ||
      (!kernel.installed &&
        !["running", "failed", "cancelled"].includes(attempt?.status ?? "")) ||
      kernel.inUse ||
      !jobs[kernel.version] ||
      unavailable.has(kernel.version) ||
      (running && attempt?.cancelling === undefined) ||
      attempt?.cancelling
    )
      return;
    if (
      !window.confirm(
        running
          ? t(
              `Stop installation and delete local files for kernel ${kernel.version}?`,
              `설치를 중단하고 커널 ${kernel.version}의 로컬 파일을 삭제할까?`,
            )
          : t(
              `Delete kernel ${kernel.version}?`,
              `커널 ${kernel.version}을 삭제할까?`,
            ),
      )
    )
      return;
    busyRef.current = true;
    setBusy(kernel.version);
    setBusyAction("delete");
    setError("");
    generation.current++;
    if (running && attempt)
      setJobs((current) => ({
        ...current,
        [kernel.version]: { ...attempt, cancelling: true },
      }));
    try {
      await deleteKernel(kernel.version);
    } catch (cause) {
      if (active.current)
        setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      busyRef.current = false;
      if (active.current) {
        setBusy("");
        setBusyAction(null);
        await refresh();
      }
    }
  };
  const label = (kernel: KernelResponse) => {
    const job = jobs[kernel.version];
    if (job?.cancelling) return t("Cancelling installation…", "설치 취소 중…");
    if (
      job?.status === "running" &&
      (kernel.installSource === "registry" || registryInstall(job))
    )
      return t(
        kernelSteps(job, true)[kernelStage(job)]![0],
        kernelSteps(job, true)[kernelStage(job)]![1],
      );
    if (job?.status === "running")
      return [
        t("Preparing installation", "설치 준비 중"),
        t("Downloading", "다운로드 중"),
        t("Waiting for build", "빌드 대기"),
        t("Building kernel", "커널 빌드 중"),
        t("Verifying", "검증 중"),
      ][kernelStage(job)];
    if (job?.status === "failed") return t("Installation failed", "설치 실패");
    if (kernel.installed) return t("Installed", "설치됨");
    if (job?.status === "cancelled")
      return t("Installation cancelled", "설치 취소됨");
    return job
      ? t("Not installed", "설치 안 됨")
      : t("Checking status…", "상태 확인 중…");
  };
  const current = kernels?.find((kernel) => kernel.version === selected);
  const job = current ? jobs[current.version] : null;
  const running = job?.status === "running";
  const failed = job?.status === "failed";
  const wasCancelled = job?.status === "cancelled";
  const cancelling =
    !!job?.cancelling ||
    (busy === current?.version &&
      (busyAction === "cancel" || busyAction === "delete"));
  const supportsCancellation = job?.cancelling !== undefined;
  const cancelLabel = cancelling
    ? t("Cancelling…", "취소 중…")
    : t("Cancel installation", "설치 취소");
  const deleteLabel =
    busyAction === "delete" && busy === current?.version
      ? t("Deleting…", "삭제 중…")
      : running
        ? t("Cancel and delete", "취소 후 삭제")
        : t("Delete", "삭제");
  const builderVersion = kernels?.find((kernel) => {
    const snapshot = jobs[kernel.version];
    return (
      snapshot?.status === "running" &&
      !registryInstall(snapshot) &&
      kernel.installSource !== "registry" &&
      kernelStage(snapshot) === 3
    );
  })?.version;
  const relevantImages = images.filter(
    (image) => image.installed && image.kernelVersion === selected,
  );
  const registryCatalog = kernels?.some(
    (kernel) => kernel.installSource === "registry",
  );
  const downloadSources = [
    ...new Map(
      (kernels ?? []).flatMap((kernel) => {
        const source = kernelDownloadSource(kernel.packageUrl);
        return source ? [[source.origin, source] as const] : [];
      }),
    ).values(),
  ];
  const currentSource = kernelDownloadSource(current?.packageUrl);
  const activeCount = Object.values(jobs).filter(
    (snapshot) => snapshot?.status === "running",
  ).length;
  const status = (kernel: KernelResponse) => {
    const snapshot = jobs[kernel.version];
    const state =
      snapshot?.status === "running"
        ? !registryInstall(snapshot) &&
          kernel.installSource !== "registry" &&
          kernelStage(snapshot) === 2
          ? "waiting"
          : "running"
        : snapshot?.status === "failed"
          ? "failed"
          : kernel.installed
            ? "installed"
            : "idle";
    return (
      <span className="kernel-status" data-state={state}>
        <span aria-hidden="true" />
        {label(kernel)}
      </span>
    );
  };
  const orderedKernels = [...(kernels ?? [])];
  if (dateOrder) {
    orderedKernels.sort((left, right) => {
      if (!(left.publishedAt ?? left.releasedAt))
        return (right.publishedAt ?? right.releasedAt) ? 1 : 0;
      if (!(right.publishedAt ?? right.releasedAt)) return -1;
      const order = (left.publishedAt ?? left.releasedAt ?? "").localeCompare(
        right.publishedAt ?? right.releasedAt ?? "",
      );
      return dateOrder === "ascending" ? order : -order;
    });
  }
  const eolBadge = (
    <span
      className="kernel-eol"
      title={t(
        "Upstream maintenance has ended",
        "업스트림 유지보수가 종료된 버전",
      )}
    >
      EOL
    </span>
  );
  const detail = current ? (
    <div key={current.version} className="kernel-detail">
      <DetailHead
        title={current.version}
        meta={
          <>
            {status(current)}
            <span className="mono">{current.architecture}</span>
            {current.endOfLife && eolBadge}
          </>
        }
        actions={
          <>
            {running ? (
              <button
                type="button"
                className="icon-btn kernel-action"
                aria-label={cancelLabel}
                disabled={
                  !!busy ||
                  cancelling ||
                  !supportsCancellation ||
                  unavailable.has(current.version)
                }
                title={
                  !supportsCancellation
                    ? t(
                        "Requires an updated Firecrab service",
                        "업데이트된 Firecrab 서비스가 필요하다",
                      )
                    : cancelLabel
                }
                onClick={() => void cancel(current.version)}
              >
                <Square aria-hidden="true" />
              </button>
            ) : (
              !current.installed && (
                <button
                  type="button"
                  className="btn primary"
                  disabled={
                    !!busy ||
                    !job ||
                    unavailable.has(current.version) ||
                    running
                  }
                  onClick={() => void install(current.version)}
                >
                  {(failed || wasCancelled) && <RotateCw aria-hidden="true" />}
                  {failed || wasCancelled
                    ? t("Retry installation", "설치 재시도")
                    : current.installSource === "registry"
                      ? t("Download kernel", "커널 다운로드")
                      : t("Build kernel", "커널 빌드")}
                </button>
              )
            )}
            {(current.installed || running || failed || wasCancelled) && (
              <button
                type="button"
                className="icon-btn kernel-action danger"
                aria-label={deleteLabel}
                disabled={
                  !!busy ||
                  current.inUse ||
                  !job ||
                  unavailable.has(current.version) ||
                  cancelling ||
                  (!current.installed && !supportsCancellation)
                }
                title={
                  current.inUse
                    ? t("In use by an image", "이미지가 사용 중인 커널")
                    : !current.installed && !supportsCancellation
                      ? t(
                          "Requires an updated Firecrab service",
                          "업데이트된 Firecrab 서비스가 필요하다",
                        )
                      : deleteLabel
                }
                onClick={() => void remove(current)}
              >
                <Trash2 aria-hidden="true" />
              </button>
            )}
          </>
        }
      />
      <div className="detail-body">
        {unavailable.has(current.version) && (
          <p className="kernel-stale" role="status">
            {t(
              job
                ? "Could not refresh this job. Showing the last known status and retrying automatically."
                : "Could not load installation status. Retrying automatically.",
              job
                ? "진행 상태를 갱신하지 못했다. 마지막 상태를 표시하고 자동으로 다시 확인한다."
                : "설치 상태를 불러오지 못했다. 자동으로 다시 확인한다.",
            )}
          </p>
        )}
        {job && (running || failed) && (
          <KernelInstallation
            job={job}
            builderVersion={builderVersion}
            registry={current.installSource === "registry"}
          />
        )}
        {wasCancelled && (
          <p className="kernel-detail-intro" role="status">
            {t(
              "Installation cancelled. You can retry when ready.",
              "설치를 취소했다. 다시 설치할 수 있다.",
            )}
          </p>
        )}
        {!current.installed && job?.status === "idle" && (
          <p className="kernel-detail-intro">
            {t(
              current.installSource === "registry"
                ? "Download the prebuilt kernel from the registry, verify it, and install it for your VMs."
                : "Download the Linux source, build a kernel for this host, then verify and install it for your VMs.",
              current.installSource === "registry"
                ? "레지스트리에서 빌드된 커널을 내려받는다. 검증을 마친 커널은 VM에서 사용할 수 있다."
                : "Linux 소스를 다운로드하고 호스트에 맞게 커널을 빌드한다. 검증을 마친 커널은 VM에서 사용할 수 있다.",
            )}
          </p>
        )}
        <DetailSection title={t("Installation details", "설치 정보")}>
          <dl className="props">
            <DetailProp
              label={
                kernels?.some((kernel) => kernel.installSource === "registry")
                  ? t("Published", "게시 날짜")
                  : t("Released", "릴리스 날짜")
              }
            >
              {(current.publishedAt ?? current.releasedAt) ? (
                <time
                  dateTime={(current.publishedAt ?? current.releasedAt)?.slice(
                    0,
                    10,
                  )}
                >
                  {(current.publishedAt ?? current.releasedAt)?.slice(0, 10)}
                </time>
              ) : (
                "—"
              )}
            </DetailProp>
            <DetailProp label={t("Architecture", "아키텍처")}>
              {current.architecture}
            </DetailProp>
            {job?.startedAtMs && (
              <DetailProp label={t("Started", "시작 시각")}>
                {new Date(job.startedAtMs).toLocaleString(
                  locale === "ko" ? "ko-KR" : "en-US",
                )}
              </DetailProp>
            )}
            {job?.startedAtMs && !running && !failed && (
              <DetailProp label={t("Elapsed", "경과 시간")}>
                <KernelElapsed job={job} />
              </DetailProp>
            )}
            {job?.downloadedBytes !== undefined && (
              <DetailProp label={t("Downloaded", "다운로드 용량")}>
                {kernelBytes(job.downloadedBytes)}
                {job.totalBytes ? ` / ${kernelBytes(job.totalBytes)}` : ""}
              </DetailProp>
            )}
            {current.sizeBytes !== undefined && (
              <DetailProp label={t("Installed size", "설치 크기")}>
                {kernelBytes(current.sizeBytes)}
              </DetailProp>
            )}
            {currentSource && (
              <DetailProp
                label={
                  current.installSource === "registry"
                    ? t("Registry package", "레지스트리 패키지")
                    : t("Source archive", "소스 파일")
                }
                wrap
              >
                <a href={currentSource.href} target="_blank" rel="noreferrer">
                  {currentSource.href}
                </a>
              </DetailProp>
            )}
          </dl>
        </DetailSection>
        {current.installed && current.inUse && (
          <DetailSection
            title={t("Images using this kernel", "사용 중인 이미지")}
          >
            <div
              className="kernel-react-images"
              aria-label={t("Images using this kernel", "사용 중인 이미지")}
            >
              {relevantImages.length ? (
                relevantImages.map((image) => (
                  <span key={image.alias}>
                    <ImageIcon aria-hidden="true" />
                    {image.alias}
                  </span>
                ))
              ) : (
                <span>
                  <ImageIcon aria-hidden="true" />
                  {t("Image in use", "사용 중인 이미지")}
                </span>
              )}
            </div>
          </DetailSection>
        )}
        {job?.log && (
          <details className="kernel-log-disclosure">
            <summary>
              <span>{t("Detailed log", "상세 로그")}</span>
              <ChevronDown aria-hidden="true" />
            </summary>
            <pre className="kernel-log" tabIndex={0}>
              {job.log}
            </pre>
          </details>
        )}
      </div>
    </div>
  ) : null;
  return (
    <Screen
      view="kernels"
      detail={detail}
      detailLabel={t("Kernel detail", "커널 상세")}
      detailKind={
        <span className="kernel-panel-kind">
          <KernelIcon aria-hidden="true" />
          {t("Kernel", "커널")}
        </span>
      }
      onCloseDetail={() => setSelected("")}
    >
      <div className="kernel-catalog">
        {error && (
          <p className="field-error" role="alert">
            {error}
          </p>
        )}
        <div className="kernel-catalog-heading">
          <div>
            <h2>{t("Linux kernels", "Linux 커널")}</h2>
            <p>
              {activeCount
                ? t(
                    registryCatalog
                      ? "Download prebuilt kernels. Select a version to follow its installation."
                      : "Kernel builds share one builder. Select a version to follow its installation.",
                    registryCatalog
                      ? "빌드된 커널을 내려받는다. 버전을 선택하면 설치 단계를 확인할 수 있다."
                      : "커널은 빌드 자원을 함께 사용한다. 버전을 선택하면 설치 단계를 확인할 수 있다.",
                  )
                : t(
                    registryCatalog
                      ? "Select a version to view its details or download a kernel for your VMs."
                      : "Select a version to view its details or build a kernel for your VMs.",
                    registryCatalog
                      ? "버전을 선택해 상세 정보를 확인하거나 VM에서 사용할 커널을 내려받는다."
                      : "버전을 선택해 상세 정보를 확인하거나 VM에서 사용할 커널을 빌드한다.",
                  )}
            </p>
          </div>
          {activeCount > 0 && (
            <span className="kernel-active-count">
              {t(`${activeCount} in progress`, `${activeCount}개 진행 중`)}
            </span>
          )}
        </div>
        {!kernels ? (
          <p className="detail-note">
            {t("Loading kernel catalog…", "커널 목록을 불러오는 중…")}
          </p>
        ) : (
          <table className="vm-table kernel-table">
            <thead>
              <tr>
                <th>{t("Version", "버전")}</th>
                <th>{t("Status", "상태")}</th>
                <th aria-sort={dateOrder ?? "none"}>
                  <button
                    type="button"
                    className="kernel-date-sort"
                    onClick={() =>
                      setDateOrder(
                        dateOrder === "descending" ? "ascending" : "descending",
                      )
                    }
                  >
                    {kernels?.some(
                      (kernel) => kernel.installSource === "registry",
                    )
                      ? t("Published", "게시 날짜")
                      : t("Released", "릴리스 날짜")}
                    {dateOrder === "descending" ? (
                      <ArrowDown aria-hidden="true" />
                    ) : dateOrder === "ascending" ? (
                      <ArrowUp aria-hidden="true" />
                    ) : (
                      <ArrowDownUp aria-hidden="true" />
                    )}
                  </button>
                </th>
                <th>{t("Architecture", "아키텍처")}</th>
              </tr>
            </thead>
            <tbody>
              {orderedKernels.map((kernel) => {
                const snapshot = jobs[kernel.version];
                return (
                  <tr
                    key={kernel.version}
                    aria-selected={selected === kernel.version}
                    onClick={() => select(kernel.version)}
                  >
                    <td>
                      <span className="kernel-release">
                        <button type="button" className="kernel-version">
                          {kernel.version}
                        </button>
                        {kernel.endOfLife && eolBadge}
                      </span>
                    </td>
                    <td>
                      {status(kernel)}
                      {snapshot?.status === "running" && (
                        <small className="kernel-row-step">
                          {t(
                            `Step ${kernelStage(snapshot) + 1} of ${KERNEL_STEPS.length}`,
                            `${KERNEL_STEPS.length}단계 중 ${kernelStage(snapshot) + 1}단계`,
                          )}
                        </small>
                      )}
                    </td>
                    <td className="kernel-date-cell">
                      {(kernel.publishedAt ?? kernel.releasedAt) ? (
                        <time
                          dateTime={(
                            kernel.publishedAt ?? kernel.releasedAt
                          )?.slice(0, 10)}
                        >
                          {(kernel.publishedAt ?? kernel.releasedAt)?.slice(
                            0,
                            10,
                          )}
                        </time>
                      ) : (
                        "—"
                      )}
                    </td>
                    <td className="mono">{kernel.architecture}</td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        )}
        {kernels && (
          <footer
            className="kernel-source-footer"
            aria-label={t("Kernel download source", "커널 다운로드 출처")}
          >
            <Download aria-hidden="true" />
            <span>{t("Download source", "다운로드 출처")}</span>
            {downloadSources.length > 0 ? (
              downloadSources.map((source) => (
                <a
                  key={source.origin}
                  href={source.origin}
                  target="_blank"
                  rel="noreferrer"
                >
                  {source.host}
                </a>
              ))
            ) : (
              <span>
                {t(
                  "Not reported by this service",
                  "서비스가 출처를 제공하지 않는다",
                )}
              </span>
            )}
          </footer>
        )}
      </div>
    </Screen>
  );
}
