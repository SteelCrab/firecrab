import { useCallback, useEffect, useRef, useState } from "react";
import { Image as ImageIcon } from "lucide-react";
import type {
  ImageResponse,
  KernelInstallResponse,
  KernelResponse,
} from "../bindings";
import {
  ApiClientError,
  deleteKernel,
  getKernelInstall,
  listImages,
  listKernels,
  startKernelInstall,
} from "../api/client";
import { useI18n } from "../i18n";

type Jobs = Record<string, KernelInstallResponse | null>;
function stage(job: KernelInstallResponse): number {
  if (job.status === "succeeded") return 4;
  if (
    job.log.includes("verifying built kernel") ||
    job.log.includes("verified and cached")
  )
    return 3;
  if (job.log.includes("building Linux ")) return 2;
  if (job.log.includes("waiting for the host kernel builder")) return 1;
  return 0;
}

export default function Kernels() {
  const { t } = useI18n();
  const [kernels, setKernels] = useState<KernelResponse[] | null>(null);
  const [images, setImages] = useState<ImageResponse[]>([]);
  const [jobs, setJobs] = useState<Jobs>({});
  const [selected, setSelected] = useState("");
  const [busy, setBusy] = useState("");
  const [error, setError] = useState("");
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
      setJobs(
        Object.fromEntries(
          list.map((kernel, index) => [kernel.version, snapshots[index]]),
        ),
      );
      setError("");
    } catch (cause) {
      if (active.current && id === generation.current)
        setError(cause instanceof Error ? cause.message : String(cause));
    }
  }, []);
  useEffect(() => {
    active.current = true;
    let timer = 0;
    const tick = async () => {
      if (!busyRef.current) await refresh();
      if (active.current) timer = window.setTimeout(() => void tick(), 3000);
    };
    void tick();
    return () => {
      active.current = false;
      generation.current++;
      window.clearTimeout(timer);
    };
  }, [refresh]);
  const install = async (version: string) => {
    if (
      busyRef.current ||
      !jobs[version] ||
      jobs[version]?.status === "running"
    )
      return;
    busyRef.current = true;
    setBusy(version);
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
        await refresh();
      }
    }
  };
  const remove = async (kernel: KernelResponse) => {
    if (
      busyRef.current ||
      !kernel.installed ||
      kernel.inUse ||
      !jobs[kernel.version] ||
      jobs[kernel.version]?.status === "running"
    )
      return;
    if (
      !window.confirm(
        t(
          `Delete kernel ${kernel.version}?`,
          `커널 ${kernel.version} 버전을 삭제할까요?`,
        ),
      )
    )
      return;
    busyRef.current = true;
    setBusy(kernel.version);
    setError("");
    generation.current++;
    try {
      await deleteKernel(kernel.version);
      await refresh();
    } catch (cause) {
      if (active.current)
        setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      busyRef.current = false;
      if (active.current) setBusy("");
    }
  };
  const label = (kernel: KernelResponse) => {
    const job = jobs[kernel.version];
    if (job?.status === "running")
      return [
        t("Downloading", "다운로드 중"),
        t("Waiting for build", "빌드 대기"),
        t("Building kernel", "커널 빌드 중"),
        t("Verifying", "검증 중"),
      ][stage(job)];
    if (job?.status === "failed") return t("Installation failed", "설치 실패");
    if (kernel.installed) return t("Installed", "설치됨");
    return job
      ? t("Not installed", "설치 안 됨")
      : t("Checking status…", "상태 확인 중…");
  };
  const current = kernels?.find((kernel) => kernel.version === selected);
  const relevantImages = images.filter(
    (image) => image.installed && image.kernelVersion === selected,
  );
  return (
    <div className="kernel-react">
      {error && <p role="alert">{error}</p>}
      {!kernels ? (
        <p>{t("Loading kernel catalog…", "커널 목록을 불러오는 중…")}</p>
      ) : (
        <>
          {Object.entries(jobs)
            .filter(
              ([, job]) =>
                job?.status === "running" || job?.status === "failed",
            )
            .map(
              ([version, job]) =>
                job && (
                  <section className="panel" key={version}>
                    <button
                      className="btn"
                      onClick={() => setSelected(version)}
                    >
                      {version}
                    </button>{" "}
                    {label(
                      kernels.find((kernel) => kernel.version === version)!,
                    )}
                    <progress
                      max={4}
                      value={stage(job)}
                      aria-label={t("Installation stages", "설치 진행 단계")}
                    />
                    <span>{stage(job)} / 4</span>
                    <ol className="kernel-react-steps">
                      <li>{t("Download", "다운로드")}</li>
                      <li>{t("Waiting", "대기")}</li>
                      <li>{t("Build", "빌드")}</li>
                      <li>{t("Verify", "검증")}</li>
                    </ol>
                  </section>
                ),
            )}
          <section className="panel">
            <table className="vm-table">
              <thead>
                <tr>
                  <th>{t("Version", "버전")}</th>
                  <th>{t("Status", "상태")}</th>
                  <th>{t("Architecture", "아키텍처")}</th>
                </tr>
              </thead>
              <tbody>
                {kernels.map((kernel) => (
                  <tr key={kernel.version}>
                    <td>
                      <button
                        className="btn"
                        onClick={() => setSelected(kernel.version)}
                      >
                        {kernel.version}
                      </button>
                    </td>
                    <td>{label(kernel)}</td>
                    <td>{kernel.architecture}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </section>
          {current && (
            <section
              className="panel"
              aria-label={t("Kernel detail", "커널 상세")}
            >
              <h2>{current.version}</h2>
              <p>{current.architecture}</p>
              <p>{label(current)}</p>
              {current.installed && current.inUse && (
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
              )}
              <button
                className="btn"
                disabled={
                  !!busy ||
                  !jobs[current.version] ||
                  jobs[current.version]?.status === "running"
                }
                onClick={() => void install(current.version)}
              >
                {t("Build kernel", "커널 빌드")}
              </button>
              {current.installed && (
                <button
                  className="btn danger"
                  disabled={
                    !!busy ||
                    current.inUse ||
                    !jobs[current.version] ||
                    jobs[current.version]?.status === "running"
                  }
                  onClick={() => void remove(current)}
                >
                  {t("Delete", "삭제")}
                </button>
              )}
              {jobs[current.version]?.log && (
                <details>
                  <summary>{t("Detailed log", "상세 로그")}</summary>
                  <pre className="kernel-react-log">
                    {jobs[current.version]?.log}
                  </pre>
                </details>
              )}
            </section>
          )}
        </>
      )}
    </div>
  );
}
