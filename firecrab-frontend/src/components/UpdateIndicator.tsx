import { useCallback, useEffect, useState } from "react";
import type { UpdateCheckResponse, UpdatePhase, UpdateProgressResponse } from "../bindings";
import { getUpdateCheck, getUpdateProgress } from "../api/client";
import { useI18n } from "../i18n";
import UpdateDialog from "./UpdateDialog";

/**
 * Idle poll interval. GitHub's unauthenticated rate limit is 60/hour per IP and
 * the API caches a check for 30 minutes, so 15 minutes costs at most 2-4 GitHub
 * calls an hour however many tabs are open.
 */
const POLL_MILLIS = 15 * 60 * 1000;

/** The phases of a run that has not ended. */
const RUNNING: UpdatePhase[] = ["checking", "downloading", "verifying", "applying", "restarting"];

/**
 * Bottom-of-the-nav update indicator. Renders nothing at all in the common
 * case — no update, or a check that could not reach GitHub — so a background
 * widget never clutters the shell. Its button opens the update dialog, which
 * shows what the release changes and then follows the update.
 */
export default function UpdateIndicator() {
  const { t } = useI18n();
  const [check, setCheck] = useState<UpdateCheckResponse | null>(null);
  // Open on the release notes, or following a run that was already going.
  const [dialog, setDialog] = useState<{ adopt?: UpdateProgressResponse } | null>(null);

  useEffect(() => {
    let cancelled = false;

    const tick = async () => {
      try {
        const next = await getUpdateCheck();
        if (!cancelled) setCheck(next);
      } catch {
        // Keep the last answer; the next tick tries again.
      }
    };

    void tick();
    const interval = setInterval(tick, POLL_MILLIS);
    return () => {
      cancelled = true;
      clearInterval(interval);
    };
  }, []);

  // A page opened, or reloaded, in the middle of an update follows it.
  useEffect(() => {
    let cancelled = false;
    getUpdateProgress()
      .then((record) => {
        if (!cancelled && RUNNING.includes(record.phase)) setDialog({ adopt: record });
      })
      .catch(() => {
        // No answer means no run to follow.
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const close = useCallback(() => setDialog(null), []);

  // `updateAvailable` is only ever true when the API resolved a `latest`, but
  // the type still allows it to be absent — fall back rather than render
  // "vundefined".
  const available = check && !check.error && check.updateAvailable;
  const latest = check?.latest ?? "?";

  return (
    <>
      {available && !dialog && (
        <div className="update-indicator">
          <span className="update-indicator-label">
            {t(`Update available v${latest}`, `업데이트 가능 v${latest}`)}
          </span>
          <button type="button" className="update-indicator-action" onClick={() => setDialog({})}>
            {t("Update", "업데이트")}
          </button>
        </div>
      )}
      {dialog && <UpdateDialog check={check} adopt={dialog.adopt} onClose={close} />}
    </>
  );
}
