import { useCallback, useEffect, useRef, useState } from "react";
import type { UpdateCheckResponse } from "../bindings";
import { getUpdateCheck, startUpdate } from "../api/client";
import { useI18n } from "../i18n";

/**
 * Idle poll interval. GitHub's unauthenticated rate limit is 60/hour per IP and
 * the API caches a check for 30 minutes, so 15 minutes costs at most 2-4 GitHub
 * calls an hour however many tabs are open.
 */
const POLL_MILLIS = 15 * 60 * 1000;
/** While the host is restarting, poll fast enough to notice it coming back. */
const RESTART_POLL_MILLIS = 3000;

/**
 * Bottom-of-the-nav update indicator. Renders nothing at all in the common
 * case — no update, or a check that could not reach GitHub — so a background
 * widget never clutters the shell.
 */
export default function UpdateIndicator() {
  const { t } = useI18n();
  const [check, setCheck] = useState<UpdateCheckResponse | null>(null);
  const [restarting, setRestarting] = useState(false);
  const [finished, setFinished] = useState(false);
  const versionAtClick = useRef<string | null>(null);

  useEffect(() => {
    let cancelled = false;

    const tick = async () => {
      try {
        const next = await getUpdateCheck();
        if (cancelled) return;
        // The one reliable completion signal: the API came back reporting a
        // different version than the one that was running when we clicked.
        if (versionAtClick.current !== null && next.current !== versionAtClick.current) {
          setFinished(true);
          setRestarting(false);
        }
        setCheck(next);
      } catch {
        // During a restart the API is down and `fail()` in api/client.ts has
        // already normalized 502/503/504 and fetch failures into transport
        // errors. Swallowing them here is the whole retry policy — no extra
        // backoff logic needed.
      }
    };

    void tick();
    const interval = setInterval(tick, restarting ? RESTART_POLL_MILLIS : POLL_MILLIS);
    return () => {
      cancelled = true;
      clearInterval(interval);
    };
  }, [restarting]);

  const applyUpdate = useCallback(async () => {
    versionAtClick.current = check?.current ?? null;
    setRestarting(true);
    try {
      await startUpdate();
    } catch {
      // A 202 that never arrives is not fatal: the updater may already have
      // taken the API down. The poll above decides what really happened.
    }
  }, [check]);

  if (finished) {
    return (
      <div className="update-indicator">
        <button
          type="button"
          className="update-indicator-action"
          onClick={() => window.location.reload()}
        >
          {t("Update complete — reload", "업데이트 완료 — 새로고침")}
        </button>
      </div>
    );
  }

  if (restarting) {
    return (
      <div className="update-indicator">
        <span className="update-indicator-label">
          {t("Updating… waiting for restart", "업데이트 중… 재시작을 기다리는 중")}
        </span>
      </div>
    );
  }

  if (!check || check.error || !check.updateAvailable) return null;

  // `updateAvailable` is only ever true when the API resolved a `latest`, but
  // the type still allows it to be absent — fall back rather than render
  // "vundefined".
  const latest = check.latest ?? "?";

  return (
    <div className="update-indicator">
      <span className="update-indicator-label">
        {t(`Update available v${latest}`, `업데이트 가능 v${latest}`)}
      </span>
      <button type="button" className="update-indicator-action" onClick={() => void applyUpdate()}>
        {t("Update", "업데이트")}
      </button>
    </div>
  );
}
