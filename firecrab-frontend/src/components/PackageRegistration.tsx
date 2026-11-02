import { useEffect, useRef, useState } from "react";
import {
  ApiClientError,
  getMicroRegistry,
  getMicroRegistryRegisterJob,
  startMicroRegistryRegister,
} from "../api/client";
import type {
  MicroRegistryImageResponse,
  MicroRegistryRegisterResponse,
} from "../bindings";
import { useI18n } from "../i18n";

const JOB = "firecrab.image-register-job";
const suggestion = (alias: string) => /^(alpine|rocky|ubuntu)-\d/.test(alias);
/** The package catalog opens only on request; built-in suggestions stay hidden. */
export default function PackageRegistration({
  onChanged,
}: {
  onChanged: () => Promise<void>;
}) {
  const { t } = useI18n(),
    dialog = useRef<HTMLDialogElement>(null),
    changed = useRef(onChanged);
  changed.current = onChanged;
  const [open, setOpen] = useState(() => !!sessionStorage.getItem(JOB)),
    [items, setItems] = useState<MicroRegistryImageResponse[] | null>(null),
    [job, setJob] = useState<MicroRegistryRegisterResponse | null>(null),
    [jobId, setJobId] = useState(() => sessionStorage.getItem(JOB)),
    [error, setError] = useState(""),
    [busy, setBusy] = useState(false);
  const mounted = useRef(true);
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);
  useEffect(() => {
    if (open) dialog.current?.showModal();
    else dialog.current?.close();
  }, [open]);
  useEffect(() => {
    if (!open) return;
    let active = true;
    setError("");
    getMicroRegistry()
      .then((data) => {
        if (!Array.isArray(data.images))
          throw new Error("Invalid package catalog");
        if (active)
          setItems(data.images.filter((item) => !suggestion(item.alias)));
      })
      .catch((cause) => {
        if (active) setError(String(cause));
      });
    return () => {
      active = false;
    };
  }, [open]);
  useEffect(() => {
    if (!jobId) return;
    let active = true,
      timer: ReturnType<typeof setTimeout> | undefined;
    const tick = async () => {
      try {
        const value = await getMicroRegistryRegisterJob(jobId);
        if (!active) return;
        setJob(value);
        if (value.status === "running")
          timer = setTimeout(() => void tick(), 1500);
        else {
          sessionStorage.removeItem(JOB);
          await changed.current();
          if (active) {
            setJobId(null);
            if (value.status === "succeeded") setOpen(false);
          }
        }
      } catch (cause) {
        if (active) {
          setError(String(cause));
          if (cause instanceof ApiClientError && cause.status === 404) {
            sessionStorage.removeItem(JOB);
            setJobId(null);
          } else timer = setTimeout(() => void tick(), 3000);
        }
      }
    };
    void tick();
    return () => {
      active = false;
      clearTimeout(timer);
    };
  }, [jobId]);
  const register = async (item: MicroRegistryImageResponse) => {
    if (busy || jobId) return;
    setBusy(true);
    setError("");
    try {
      const value = await startMicroRegistryRegister({
        alias: item.alias,
        version: item.version,
      });
      if (!mounted.current) return;
      sessionStorage.setItem(JOB, value.jobId);
      setJob(value);
      setJobId(value.jobId);
    } catch (cause) {
      if (mounted.current) setError(String(cause));
    } finally {
      if (mounted.current) setBusy(false);
    }
  };
  return (
    <>
      <button className="btn" type="button" onClick={() => setOpen(true)}>
        {t("Register package", "패키지 등록")}
      </button>
      <dialog
        ref={dialog}
        className="package-registration"
        aria-label={t("Register an image package", "이미지 패키지 등록")}
        onCancel={(event) => {
          event.preventDefault();
          if (!busy) setOpen(false);
        }}
      >
        <h2>{t("Register an image package", "이미지 패키지 등록")}</h2>
        <p>
          {t(
            "Register a published image package, then select it in the image list to install it.",
            "공개된 이미지 패키지를 등록하면 이미지 목록에서 선택해 설치할 수 있습니다.",
          )}
        </p>
        {error && (
          <p role="alert" className="field-error">
            {error}
          </p>
        )}
        {items === null ? (
          !error && <p>{t("Loading…", "불러오는 중…")}</p>
        ) : items.length ? (
          <ul className="detail-list">
            {items.map((item) => (
              <li key={`${item.alias}:${item.version}`}>
                <span>
                  {item.alias} · {item.version}
                </span>
                <button
                  className="btn primary"
                  disabled={busy || !!jobId || !item.downloadable}
                  onClick={() => void register(item)}
                >
                  {t("Register", "등록")}
                </button>
              </li>
            ))}
          </ul>
        ) : (
          <p>
            {t(
              "No additional packages are available.",
              "추가로 등록할 패키지가 없습니다.",
            )}
          </p>
        )}
        {job && (
          <p role="status">
            {job.alias} · {job.status}
          </p>
        )}
        {job?.log && (
          <details>
            <summary>{t("Log", "로그")}</summary>
            <pre className="job-log">{job.log}</pre>
          </details>
        )}
        <button
          className="btn ghost"
          type="button"
          disabled={busy}
          onClick={() => setOpen(false)}
        >
          {t("Close", "닫기")}
        </button>
      </dialog>
    </>
  );
}
