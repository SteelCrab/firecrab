import { useCallback, useEffect, useRef, useState } from "react";
import type { ImageInstallStatus, KernelInstallResponse, KernelResponse } from "../bindings";
import {
  deleteKernel,
  getKernelInstall,
  listKernels,
  startKernelInstall,
} from "../api/client";
import { useI18n } from "../i18n";
import Banner from "./Banner";
import LogExportActions from "./LogExportActions";
import { logDownloadFilename } from "../lib/textExport";

function statusClass(status: ImageInstallStatus): string {
  if (status === "running") return " starting";
  if (status === "succeeded") return " running";
  if (status === "failed") return " error";
  return "";
}

function KernelJobLog({ job }: { job: KernelInstallResponse }) {
  const { t } = useI18n();
  const logRef = useRef<HTMLPreElement>(null);
  useEffect(() => {
    const element = logRef.current;
    if (element) element.scrollTop = element.scrollHeight;
  }, [job.log]);

  return (
    <div className="subpanel">
      <div className="log-export-bar">
        <span className="log-export-bar-label">
          {t("Kernel install log", "커널 설치 로그")} — {job.version}
        </span>
        <LogExportActions
          text={job.log}
          filename={logDownloadFilename("kernel-install", job.version)}
          buttonClassName="btn console-bar-btn"
          disabled={!job.log}
        />
      </div>
      <pre className="detail-log image-install-log" ref={logRef}>{job.log}</pre>
    </div>
  );
}

export default function Kernels() {
  const { t } = useI18n();
  const [kernels, setKernels] = useState<KernelResponse[] | null>(null);
  const [jobs, setJobs] = useState<Record<string, KernelInstallResponse>>({});
  const [busyVersion, setBusyVersion] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const mountedRef = useRef(true);
  const pollingRef = useRef(new Set<string>());

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
    };
  }, []);

  const refresh = useCallback(async () => {
    try {
      const next = await listKernels();
      if (!mountedRef.current) return;
      setKernels(next);
      setError(null);
      const snapshots = await Promise.allSettled(next.map((kernel) => getKernelInstall(kernel.version)));
      if (!mountedRef.current) return;
      setJobs((current) => {
        const merged = { ...current };
        for (const result of snapshots) {
          if (result.status === "fulfilled" && result.value.status !== "idle") {
            merged[result.value.version] = result.value;
          }
        }
        return merged;
      });
    } catch (err) {
      if (mountedRef.current) setError((err as Error).message);
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const poll = useCallback((version: string) => {
    if (pollingRef.current.has(version)) return;
    pollingRef.current.add(version);
    const tick = async () => {
      if (!mountedRef.current) {
        pollingRef.current.delete(version);
        return;
      }
      try {
        const latest = await getKernelInstall(version);
        if (!mountedRef.current) return;
        setJobs((current) => ({ ...current, [version]: latest }));
        if (latest.status === "running") {
          window.setTimeout(() => void tick(), 500);
          return;
        }
        pollingRef.current.delete(version);
        await refresh();
      } catch {
        if (mountedRef.current) window.setTimeout(() => void tick(), 500);
      }
    };
    void tick();
  }, [refresh]);

  useEffect(() => {
    for (const job of Object.values(jobs)) {
      if (job.status === "running") poll(job.version);
    }
  }, [jobs, poll]);

  const handleInstall = async (version: string) => {
    setBusyVersion(version);
    setError(null);
    try {
      const started = await startKernelInstall(version);
      if (!mountedRef.current) return;
      setJobs((current) => ({ ...current, [version]: started }));
      poll(version);
    } catch (err) {
      if (mountedRef.current) setError((err as Error).message);
    } finally {
      if (mountedRef.current) setBusyVersion(null);
    }
  };

  const handleDelete = async (kernel: KernelResponse) => {
    if (!window.confirm(t(
      `Delete local kernel ${kernel.version}? Images using it must be updated first.`,
      `로컬 커널 ${kernel.version}을 삭제할까요? 사용 중인 이미지는 먼저 업데이트해야 합니다.`,
    ))) return;
    setBusyVersion(kernel.version);
    setError(null);
    try {
      await deleteKernel(kernel.version);
      await refresh();
      setJobs((current) => {
        const next = { ...current };
        delete next[kernel.version];
        return next;
      });
    } catch (err) {
      if (mountedRef.current) setError((err as Error).message);
    } finally {
      if (mountedRef.current) setBusyVersion(null);
    }
  };

  if (kernels === null && !error) {
    return <div className="empty">{t("Loading kernel catalog…", "커널 목록 불러오는 중…")}</div>;
  }

  return (
    <section className="panel">
      <h2 className="panel-title">
        <span>{t("Kernel management", "커널 관리")}</span>
        <button type="button" className="btn" onClick={() => void refresh()}>
          {t("Refresh", "새로고침")}
        </button>
      </h2>
      <p className="panel-intro">
        {t(
          "Install digest-pinned guest kernels independently from M2Image rootfs files. Select an installed kernel from an image's detail panel to update that image.",
          "digest가 고정된 게스트 커널을 M2Image rootfs와 별도로 설치합니다. 이미지 상세 패널에서 설치된 커널을 선택해 이미지를 업데이트할 수 있습니다.",
        )}
      </p>
      {error && <Banner kind="error" text={error} onDismiss={() => setError(null)} />}
      {kernels && (
        <div className="table-scroll">
          <table className="vm-table kernel-table">
            <thead>
              <tr>
                <th>{t("Version", "버전")}</th>
                <th>{t("Architecture", "아키텍처")}</th>
                <th>{t("Image", "커널 이미지")}</th>
                <th>{t("SHA256", "SHA256")}</th>
                <th>{t("Status", "상태")}</th>
                <th className="actions">{t("Action", "동작")}</th>
              </tr>
            </thead>
            <tbody>
              {kernels.map((kernel) => {
                const job = jobs[kernel.version];
                const installing = busyVersion === kernel.version || job?.status === "running";
                const state = installing
                  ? t("Installing…", "설치 중…")
                  : kernel.inUse
                    ? t("In use", "사용 중")
                    : kernel.installed
                      ? t("Installed", "설치됨")
                      : t("Available", "사용 가능");
                return (
                  <tr key={kernel.version}>
                    <td className="mono"><strong>{kernel.version}</strong></td>
                    <td className="mono">{kernel.architecture}</td>
                    <td className="mono">{kernel.image}</td>
                    <td className="mono kernel-digest" title={kernel.imageSha256}>{kernel.imageSha256.slice(0, 12)}…</td>
                    <td>
                      <span className={`state-badge${installing ? " starting" : job ? statusClass(job.status) : kernel.installed ? " running" : ""}`}>
                        {state}
                      </span>
                    </td>
                    <td className="actions">
                      {kernel.installed ? (
                        <button
                          type="button"
                          className="btn danger"
                          disabled={kernel.inUse || installing}
                          onClick={() => void handleDelete(kernel)}
                          title={kernel.inUse ? t("Update images using this kernel first.", "이 커널을 사용하는 이미지를 먼저 업데이트하세요.") : undefined}
                        >
                          {kernel.inUse ? t("In use", "사용 중") : installing ? t("Working…", "처리 중…") : t("Delete", "삭제")}
                        </button>
                      ) : (
                        <button
                          type="button"
                          className="btn primary"
                          disabled={installing}
                          onClick={() => void handleInstall(kernel.version)}
                        >
                          {installing ? t("Installing…", "설치 중…") : t("Install", "설치")}
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
      {Object.values(jobs)
        .filter((job) => job.status !== "idle")
        .map((job) => <KernelJobLog key={job.version} job={job} />)}
      <div className="subpanel kernel-catalog-note">
        {t(
          "Packages and unpacked images are checked against the release catalog before they become usable.",
          "패키지와 압축 해제된 커널 이미지는 사용 가능 상태가 되기 전에 릴리스 카탈로그와 대조됩니다.",
        )}
      </div>
    </section>
  );
}
