import { useCallback, useEffect, useId, useRef, useState } from "react";
import { createPortal } from "react-dom";
import type { UpdateCheckResponse, UpdatePhase, UpdateProgressResponse } from "../bindings";
import { ApiClientError, getUpdateProgress, startUpdate } from "../api/client";
import { useI18n } from "../i18n";
import ProgressGauge from "./ProgressGauge";
import ReleaseNotes from "./ReleaseNotes";

const POLL_MILLIS = 1000;
/** How long to wait for the updater's first word before saying it never started. */
const START_GRACE_MILLIS = 20 * 1000;
/**
 * How long a run may take before the dialog says it could not confirm it.
 *
 * Twice the helper's own apply timeout (300s in `firecrab-cli/src/update/helper.rs`)
 * plus room for the download that precedes it, which comfortably covers a real
 * run while bounding how long anyone stares at a gauge that is not going to move.
 */
const RUN_TIMEOUT_MILLIS = 10 * 60 * 1000;

type Step = "review" | "running" | "done" | "failed";
type StageState = "done" | "active" | "failed" | "pending";
/** Why a run is called failed: the updater said so, or the dialog gave up waiting. */
type Failure = { kind: "reported"; text: string | null } | { kind: "unconfirmed" } | { kind: "not-started" };

const STAGES: { phase: UpdatePhase; label: [string, string] }[] = [
  { phase: "checking", label: ["Check the release", "릴리스 확인"] },
  { phase: "downloading", label: ["Download the bundle", "번들 내려받기"] },
  { phase: "verifying", label: ["Verify the checksum", "체크섬 검증"] },
  { phase: "applying", label: ["Replace the binaries", "실행 파일 교체"] },
  { phase: "restarting", label: ["Restart the services", "서비스 재시작"] },
];

/**
 * Which stage a failed run stopped in, from how far its gauge got. The updater
 * keeps no stage for a failure, only the percent, and these are where each
 * stage starts on its scale (`span` in `firecrab-cli/src/update/progress.rs`).
 */
function failedStage(percent: number): number {
  if (percent < 5) return 0;
  if (percent < 80) return 1;
  if (percent < 85) return 2;
  if (percent < 97) return 3;
  return 4;
}

function stageStates(phase: UpdatePhase, percent: number): StageState[] {
  if (phase === "done") return STAGES.map(() => "done");
  const current = phase === "failed" ? failedStage(percent) : STAGES.findIndex((stage) => stage.phase === phase);
  return STAGES.map((_, index) => {
    if (index < current) return "done";
    if (index > current) return "pending";
    return phase === "failed" ? "failed" : "active";
  });
}

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  const units = ["KiB", "MiB", "GiB"];
  let value = bytes;
  let unit = -1;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value.toFixed(1)} ${units[unit]}`;
}

const STAGE_MARK: Record<StageState, string> = { done: "✓", active: "●", failed: "✕", pending: "○" };

interface UpdateDialogProps {
  /** The newest release; absent when the dialog only follows a run that was already going. */
  check: UpdateCheckResponse | null;
  /** The record of a run that was already going when the page opened. */
  adopt?: UpdateProgressResponse;
  onClose: () => void;
}

/**
 * The update popup: what the new release changes, then the run itself with a
 * gauge, a percent, and the stage it is in.
 *
 * The API restarts at the end of an update, so the dialog follows the run
 * through the record the updater keeps (`GET /api/update/progress`) and treats
 * an API that does not answer as the restart it is.
 */
export default function UpdateDialog({ check, adopt, onClose }: UpdateDialogProps) {
  const { t } = useI18n();
  const titleId = useId();
  const [step, setStep] = useState<Step>(adopt ? "running" : "review");
  const [progress, setProgress] = useState<UpdateProgressResponse | null>(adopt ?? null);
  const [unreachable, setUnreachable] = useState(false);
  const [failure, setFailure] = useState<Failure | null>(null);
  // The updater this dialog follows: an earlier run's record says nothing about this one.
  const following = useRef<number | null>(adopt?.pid ?? null);
  const clickedAt = useRef(0);
  const primary = useRef<HTMLButtonElement>(null);
  const started = progress !== null;

  const start = useCallback(async () => {
    setFailure(null);
    setProgress(null);
    setUnreachable(false);
    following.current = null;
    clickedAt.current = Date.now();
    setStep("running");
    try {
      following.current = (await startUpdate()).pid;
    } catch (error) {
      // A refusal means nothing was started. No answer at all is not fatal: the
      // updater may already have taken the API down, and the polling decides.
      if (error instanceof ApiClientError && error.status !== undefined) {
        setFailure({ kind: "reported", text: error.message });
        setStep("failed");
      }
    }
  }, []);

  useEffect(() => {
    if (step !== "running") return;
    let cancelled = false;
    const tick = async () => {
      try {
        const record = await getUpdateProgress();
        if (cancelled) return;
        setUnreachable(false);
        const pid = following.current;
        // With no pid in hand (the start's answer was lost), take the first record written since the click.
        const ours =
          pid !== null ? record.pid === pid : record.phase !== "idle" && record.updatedAtMs >= clickedAt.current - 5000;
        if (!ours) return;
        if (pid === null && record.pid !== undefined) following.current = record.pid;
        setProgress(record);
        if (record.phase === "done") setStep("done");
        if (record.phase === "failed") {
          setFailure({ kind: "reported", text: record.error ?? null });
          setStep("failed");
        }
      } catch {
        // The API is down while it restarts, and that is expected.
        if (!cancelled) setUnreachable(true);
      }
    };
    void tick();
    const interval = setInterval(tick, POLL_MILLIS);
    const giveUp = setTimeout(() => {
      if (cancelled) return;
      setFailure({ kind: "unconfirmed" });
      setStep("failed");
    }, RUN_TIMEOUT_MILLIS);
    return () => {
      cancelled = true;
      clearInterval(interval);
      clearTimeout(giveUp);
    };
  }, [step]);

  // The updater writes its first record at once; silence means it never ran.
  useEffect(() => {
    if (step !== "running" || started) return;
    const silent = setTimeout(() => {
      setFailure({ kind: "not-started" });
      setStep("failed");
    }, START_GRACE_MILLIS);
    return () => clearTimeout(silent);
  }, [step, started]);

  useEffect(() => {
    primary.current?.focus();
  }, [step]);

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape" && (step === "review" || step === "failed")) onClose();
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [step, onClose]);

  const latest = progress?.target ?? check?.latest;
  const target = latest ? `v${latest}` : "";
  const phase: UpdatePhase = step === "done" ? "done" : (progress?.phase ?? "idle");
  // An API that does not answer after the helper took over is the restart.
  const shown: UpdatePhase = unreachable && (phase === "applying" || phase === "restarting") ? "restarting" : phase;
  const percent = step === "done" ? 100 : (progress?.percent ?? 0);
  const states = stageStates(step === "failed" ? "failed" : shown, percent);

  const title = {
    review: t(`Update to ${target}`, `${target}로 업데이트`),
    running: target ? t(`Updating to ${target}…`, `${target}로 업데이트 중…`) : t("Updating…", "업데이트 중…"),
    done: target ? t(`Updated to ${target}`, `${target}로 업데이트됨`) : t("Updated", "업데이트됨"),
    failed: t("Update failed", "업데이트 실패"),
  }[step];

  const failureText = (() => {
    switch (failure?.kind) {
      case "unconfirmed":
        return t(
          "The update was not confirmed. Check `journalctl -u firecrab-api -u firecrab-helper`.",
          "업데이트가 확인되지 않았습니다. `journalctl -u firecrab-api -u firecrab-helper`를 확인하세요.",
        );
      case "not-started":
        return t("The updater did not start. Check the API's journal.", "업데이트 도구가 시작되지 않았습니다. API 저널을 확인하세요.");
      case "reported":
        if (failure.text) return failure.text;
        break;
    }
    return t("The update stopped before it finished.", "업데이트가 끝나기 전에 멈췄습니다.");
  })();

  const status = (() => {
    if (step === "done") return t("The update is complete. Reload to use the new version.", "업데이트가 끝났습니다. 새 버전을 쓰려면 새로고침하세요.");
    if (step === "failed") return failureText;
    switch (shown) {
      case "checking":
        return t("Looking up the newest release…", "최신 릴리스를 확인하는 중…");
      case "downloading": {
        const done = progress?.downloadedBytes;
        const total = progress?.totalBytes;
        if (done === undefined) return t("Downloading the bundle…", "번들을 내려받는 중…");
        return t(
          `Downloading — ${formatBytes(done)}${total ? ` of ${formatBytes(total)}` : ""}`,
          `내려받는 중 — ${formatBytes(done)}${total ? ` / ${formatBytes(total)}` : ""}`,
        );
      }
      case "verifying":
        return t("Checking the bundle's SHA-256…", "번들의 SHA-256을 확인하는 중…");
      case "applying":
        return t("Replacing the installed binaries…", "설치된 실행 파일을 교체하는 중…");
      case "restarting":
        return t("Restarting the services. This page reconnects when the API is back.", "서비스를 재시작하는 중입니다. API가 돌아오면 이 페이지가 다시 연결됩니다.");
      default:
        return t("Starting the updater…", "업데이트 도구를 시작하는 중…");
    }
  })();

  return createPortal(
    <div className="update-overlay">
      <div className="update-dialog" role="dialog" aria-modal="true" aria-labelledby={titleId}>
        <div className="update-dialog-head">
          <h2 id={titleId} className="update-dialog-title">
            {title}
          </h2>
          {step === "review" && check && (
            <p className="update-dialog-versions mono">{t(`Installed v${check.current}`, `설치된 버전 v${check.current}`)}</p>
          )}
        </div>

        <div className="update-dialog-body">
          {step === "review" ? (
            <>
              <h3 className="update-dialog-section">{t("What's new", "업데이트 내용")}</h3>
              {check?.notes ? (
                <ReleaseNotes markdown={check.notes} />
              ) : (
                <p className="update-dialog-empty">{t("No release notes were published for this version.", "이 버전에는 릴리스 노트가 없습니다.")}</p>
              )}
              {check?.releaseUrl && (
                <a className="update-dialog-link" href={check.releaseUrl} target="_blank" rel="noreferrer noopener">
                  {t("View the release on GitHub", "GitHub에서 릴리스 보기")}
                </a>
              )}
            </>
          ) : (
            <>
              <ProgressGauge percent={percent} label={t("Update progress", "업데이트 진행률")} failed={step === "failed"} />
              <p className={`update-status${step === "failed" ? " is-failed" : ""}`} role="status" aria-live="polite">
                {status}
              </p>
              <ol className="update-stages">
                {STAGES.map((stage, index) => (
                  <li key={stage.phase} className={`update-stage is-${states[index]}`}>
                    <span className="update-stage-mark" aria-hidden="true">
                      {STAGE_MARK[states[index]]}
                    </span>
                    {t(...stage.label)}
                  </li>
                ))}
              </ol>
            </>
          )}
        </div>

        <div className="update-dialog-foot">
          {step === "review" && (
            <>
              <button type="button" className="btn" onClick={onClose}>
                {t("Cancel", "취소")}
              </button>
              <button type="button" className="btn primary" ref={primary} onClick={() => void start()}>
                {t("Update now", "지금 업데이트")}
              </button>
            </>
          )}
          {step === "running" && (
            <span className="update-dialog-hint">{t("Keep this page open until the update finishes.", "업데이트가 끝날 때까지 이 페이지를 열어 두세요.")}</span>
          )}
          {step === "done" && (
            <button type="button" className="btn primary" ref={primary} onClick={() => window.location.reload()}>
              {t("Reload", "새로고침")}
            </button>
          )}
          {step === "failed" && (
            <>
              <button type="button" className="btn" onClick={onClose}>
                {t("Close", "닫기")}
              </button>
              <button type="button" className="btn primary" ref={primary} onClick={() => void start()}>
                {t("Try again", "다시 시도")}
              </button>
            </>
          )}
        </div>
      </div>
    </div>,
    document.body,
  );
}
