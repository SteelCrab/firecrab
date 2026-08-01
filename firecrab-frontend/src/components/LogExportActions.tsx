import { useCallback, useRef, useState } from "react";
import { copyText, downloadText } from "../lib/textExport";

export type TextSource = string | (() => string | Promise<string>);

interface LogExportActionsProps {
  /** Plain text, or a getter (sync/async) so callers can fetch live logs on click. */
  text: TextSource;
  /** Suggested download filename (e.g. from `logDownloadFilename`). */
  filename: string;
  disabled?: boolean;
  className?: string;
  /** Extra class on the small buttons (console bar uses `console-bar-btn`). */
  buttonClassName?: string;
  copyLabel?: string;
  downloadLabel?: string;
}

type Feedback = "idle" | "busy" | "copied" | "saved" | "failed" | "empty";

async function resolveText(source: TextSource): Promise<string> {
  if (typeof source === "function") {
    return await source();
  }
  return source;
}

/**
 * Compact "복사" / "다운로드" pair for log panels.
 * Brief status text appears next to the buttons after an action.
 */
export default function LogExportActions({
  text,
  filename,
  disabled = false,
  className = "",
  buttonClassName = "btn",
  copyLabel = "복사",
  downloadLabel = "다운로드",
}: LogExportActionsProps) {
  const [feedback, setFeedback] = useState<Feedback>("idle");
  const clearTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  const flash = useCallback((next: Feedback) => {
    setFeedback(next);
    if (clearTimer.current !== null) clearTimeout(clearTimer.current);
    if (next === "idle" || next === "busy") return;
    clearTimer.current = setTimeout(() => setFeedback("idle"), 2_000);
  }, []);

  const run = useCallback(
    async (mode: "copy" | "download") => {
      if (disabled) return;
      setFeedback("busy");
      try {
        const body = (await resolveText(text)).replace(/\s+$/u, "");
        if (!body) {
          flash("empty");
          return;
        }
        if (mode === "download") {
          downloadText(`${body}\n`, filename);
          flash("saved");
          return;
        }
        const ok = await copyText(body);
        flash(ok ? "copied" : "failed");
      } catch {
        flash("failed");
      }
    },
    [disabled, filename, flash, text],
  );

  const statusText =
    feedback === "busy"
      ? "…"
      : feedback === "empty"
        ? "내용 없음"
        : feedback === "failed"
          ? "실패"
          : feedback === "copied"
            ? "복사됨"
            : feedback === "saved"
              ? "저장됨"
              : null;

  return (
    <div className={`log-export-actions ${className}`.trim()} role="group" aria-label="로그 내보내기">
      <button
        type="button"
        className={buttonClassName}
        disabled={disabled || feedback === "busy"}
        onClick={() => void run("copy")}
        title="로그 전체를 클립보드에 복사"
      >
        {copyLabel}
      </button>
      <button
        type="button"
        className={buttonClassName}
        disabled={disabled || feedback === "busy"}
        onClick={() => void run("download")}
        title="로그 전체를 텍스트 파일로 저장"
      >
        {downloadLabel}
      </button>
      {statusText && (
        <span className="log-export-status" role="status" aria-live="polite">
          {statusText}
        </span>
      )}
    </div>
  );
}
