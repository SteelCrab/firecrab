import { useEffect, useState } from "react";
import type { FirecrabInfoResponse } from "../bindings";
import { getFirecrabInfo } from "../api/client";
import { useI18n } from "../i18n";

const POLL_MILLIS = 10_000;
const REQUEST_TIMEOUT_MILLIS = 5_000;

/** Installation information and live service state on this Firecrab host. */
export default function FirecrabPanel() {
  const { t } = useI18n();
  const [info, setInfo] = useState<FirecrabInfoResponse | null>(null);
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    let cancelled = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    let controller: AbortController | undefined;
    const refresh = async () => {
      controller = new AbortController();
      const deadline = setTimeout(() => controller?.abort(), REQUEST_TIMEOUT_MILLIS);
      try {
        const next = await getFirecrabInfo(controller.signal);
        if (cancelled) return;
        setInfo(next);
        setFailed(false);
      } catch {
        if (cancelled) return;
        // Do not leave a green service state visible after losing the API.
        setInfo(null);
        setFailed(true);
      } finally {
        clearTimeout(deadline);
      }
      if (!cancelled) timer = setTimeout(refresh, POLL_MILLIS);
    };
    void refresh();
    return () => {
      cancelled = true;
      clearTimeout(timer);
      controller?.abort();
    };
  }, []);

  return (
    <section className="panel host-info" aria-label="Firecrab">
      <h2 className="panel-title">Firecrab</h2>
      {info ? (
        <dl className="detail-fields mono">
          <dt>version</dt>
          <dd>{info.version}</dd>
          <dt>firecracker</dt>
          <dd>{info.firecrackerVersion ?? t("Unavailable", "확인 불가")}</dd>
          <dt>{t("API service", "API 서비스")}</dt>
          <dd><ServiceStatus state={info.apiService} unit="firecrab-api.service" /></dd>
          <dt>{t("helper service", "helper 서비스")}</dt>
          <dd><ServiceStatus state={info.netHelperService} unit={info.helperUnit ?? "firecrab-helper.service"} /></dd>
          <dt>prefix</dt>
          <dd>{info.prefix}</dd>
          <dt>datadir</dt>
          <dd>{info.datadir}</dd>
          <dt>confdir</dt>
          <dd>{info.confdir}</dd>
          <dt>unitdir</dt>
          <dd>{info.unitdir}</dd>
          <dt>api</dt>
          <dd>{info.apiBase}</dd>
        </dl>
      ) : (
        <div className="empty">
          {failed ? t("Could not load the Firecrab info.", "Firecrab 정보를 불러오지 못했습니다.") : t("Loading…", "불러오는 중…")}
        </div>
      )}
    </section>
  );
}

function ServiceStatus({ state, unit }: { state?: string; unit: string }) {
  const { t } = useI18n();
  const labels: Record<string, [string, string]> = {
    active: ["active", "실행 중"],
    inactive: ["inactive", "중지됨"],
    failed: ["failed", "실패"],
    activating: ["activating", "시작 중"],
    deactivating: ["deactivating", "중지 중"],
    reloading: ["reloading", "다시 불러오는 중"],
    maintenance: ["maintenance", "유지 보수 중"],
    unknown: ["unknown", "확인 불가"],
  };
  const value = state ?? "unknown";
  const label = labels[value];
  const color = value === "active" ? " running" : value === "failed" ? " error" : "";
  return <span className={`state-badge${color}`} title={unit}>{label ? t(...label) : value}</span>;
}
