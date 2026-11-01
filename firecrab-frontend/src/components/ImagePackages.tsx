import { useCallback, useEffect, useRef, useState } from "react";
import {
  ApiClientError,
  fetchApiResponse,
  getImageInstall,
  getImagePackage,
  startImageInstall,
  startImagePackage,
} from "../api/client";
import type { ImageInstallResponse, ImageResponse } from "../bindings";
import { useI18n } from "../i18n";

const placeholders = new Set(["alpine-3.24.1", "rocky-9.8", "ubuntu-26.04"]);
export const visibleImage = (image: ImageResponse) =>
  !placeholders.has(image.alias) || image.installed || image.packageStaged;
export function ImagePackageActions({
  image,
  onChanged,
}: {
  image: ImageResponse;
  onChanged: () => Promise<void>;
}) {
  const { t } = useI18n();
  const [busy, setBusy] = useState(false),
    [error, setError] = useState(""),
    [jobs, setJobs] = useState<{
      package: ImageInstallResponse | null;
      install: ImageInstallResponse | null;
    }>({ package: null, install: null });
  const wasRunning = useRef(false);
  const refresh = useCallback(async () => {
    try {
      const [download, install] = await Promise.all([
        getImagePackage(image.alias),
        getImageInstall(image.alias),
      ]);
      setJobs({ package: download, install });
      const running =
        download.status === "running" || install.status === "running";
      if (wasRunning.current && !running) void onChanged();
      wasRunning.current = running;
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, [image.alias, onChanged]);
  useEffect(() => {
    void refresh();
    const timer = window.setInterval(() => void refresh(), 3000);
    return () => window.clearInterval(timer);
  }, [refresh]);
  const running =
    jobs.package?.status === "running" || jobs.install?.status === "running";
  const action = async (kind: "package" | "install" | "delete") => {
    if (busy || running) return;
    setBusy(true);
    setError("");
    try {
      if (kind === "package") await startImagePackage(image.alias);
      else if (kind === "install") await startImageInstall(image.alias);
      else {
        if (
          !window.confirm(
            t("Delete this installation file?", "이 설치 파일을 삭제할까?"),
          )
        )
          return;
        await fetchApiResponse(
          `/api/images/${encodeURIComponent(image.alias)}/package`,
          { method: "DELETE" },
        );
      }
      await Promise.all([refresh(), onChanged()]);
    } catch (e) {
      if (e instanceof ApiClientError && e.status === 409) await refresh();
      else setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };
  const current =
    jobs.install?.status === "running"
      ? jobs.install
      : jobs.package?.status === "running"
        ? jobs.package
        : null;
  return (
    <section aria-label={t("Installation files", "설치 파일 관리")}>
      <h3>{t("Installation file", "설치 파일")}</h3>
      <p className="detail-note">
        {image.installed
          ? t(
              "Keep this compressed archive to reinstall the image. Deleting the file keeps the installed image.",
              "이미지를 다시 설치할 때 쓰는 압축 파일이다. 파일을 삭제해도 설치된 이미지는 유지된다.",
            )
          : t(
              "Prepare the compressed archive, then install the image from it.",
              "압축 파일을 준비한 뒤 이미지로 설치한다.",
            )}
      </p>
      {error && <p role="alert">{error}</p>}
      {current && (
        <p>
          {t("Working…", "진행 중…")}{" "}
          {current.downloadedBytes
            ? `${(current.downloadedBytes / 1024 / 1024).toFixed(1)} MiB`
            : ""}
        </p>
      )}
      {!image.installed && !image.packageStaged && image.packageUrl && (
        <button
          className="btn"
          disabled={busy || running || !jobs.package || !jobs.install}
          onClick={() => void action("package")}
        >
          {t("Download installation file", "설치 파일 받기")}
        </button>
      )}
      {!image.installed && (
        <button
          className="btn"
          disabled={
            busy ||
            running ||
            !jobs.package ||
            !jobs.install ||
            !image.packageStaged
          }
          onClick={() => void action("install")}
        >
          {t("Install", "설치")}
        </button>
      )}
      {image.packageStaged && (
        <button
          className="btn danger"
          disabled={busy || running}
          onClick={() => void action("delete")}
        >
          {t("Delete installation file", "설치 파일 삭제")}
        </button>
      )}
      {(current?.log ||
        jobs.install?.status === "failed" ||
        jobs.package?.status === "failed") && (
        <details>
          <summary>{t("Detailed log", "상세 로그")}</summary>
          <pre className="kernel-react-log">
            {current?.log || jobs.install?.log || jobs.package?.log}
          </pre>
        </details>
      )}
    </section>
  );
}

export function ImagePackageSection(
  props: Parameters<typeof ImagePackageActions>[0],
) {
  const { t } = useI18n();
  const [open, setOpen] = useState(false);
  if (props.image.installed && !props.image.packageStaged) return null;
  if (!props.image.installed) return <ImagePackageActions {...props} />;
  return (
    <details
      className="package-management"
      onToggle={(event) => setOpen(event.currentTarget.open)}
    >
      <summary>{t("Installation files", "설치 파일 관리")}</summary>
      {open && <ImagePackageActions {...props} />}
    </details>
  );
}
