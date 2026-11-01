import { useEffect, useRef, useState } from "react";
import { X } from "lucide-react";
import type { MicroServiceDeployment } from "../bindings";
import { fetchJson } from "../api/client";
import { useI18n } from "../i18n";

/** Cancelling targets the exact queued operation, including private initial deployments. */
export default function ServiceCancel({
  serviceId,
  deployment,
  onChanged,
}: {
  serviceId: string;
  deployment: MicroServiceDeployment;
  onChanged: (result?: MicroServiceDeployment) => Promise<void>;
}) {
  const { t } = useI18n();
  const [busy, setBusy] = useState(false),
    [error, setError] = useState("");
  const mounted = useRef(true),
    acting = useRef(false);
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);
  const update = deployment.update;
  const allowed =
    deployment.phase === "queued" ||
    !!(
      update &&
      !update.cancelled &&
      !update.completed &&
      !update.restoring &&
      !update.routedNew &&
      !update.transition &&
      Array.isArray(update.retiring) &&
      !update.retiring.length
    );
  const cancel = async () => {
    if (acting.current || !allowed) return;
    acting.current = true;
    setBusy(true);
    setError("");
    let result: MicroServiceDeployment | undefined;
    try {
      result = await fetchJson<MicroServiceDeployment>(
        `/api/micro-services/${encodeURIComponent(serviceId)}/deployments/${encodeURIComponent(deployment.id)}/cancel`,
        { method: "POST" },
      );
    } catch (cause) {
      if (mounted.current)
        setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      if (mounted.current) {
        await onChanged(result);
        if (mounted.current) setBusy(false);
      }
      acting.current = false;
    }
  };
  return (
    <>
      {allowed && (
        <button
          id="deploy-cancel"
          className="icon-btn"
          type="button"
          title={t("Cancel", "취소")}
          aria-label={t("Cancel", "취소")}
          disabled={busy}
          onClick={() => void cancel()}
        >
          <X aria-hidden="true" />
        </button>
      )}
      {error && (
        <p className="field-error" role="alert">
          {error}
        </p>
      )}
    </>
  );
}
