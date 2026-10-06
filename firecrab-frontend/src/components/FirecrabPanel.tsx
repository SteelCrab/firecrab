import { useEffect, useState } from "react";
import type { FirecrabInfo } from "../bindings";
import { getFirecrabInfo } from "../api/client";
import { useI18n } from "../i18n";

/**
 * The Firecrab running on this host: its version, and what `firecrab info`
 * prints, which is where it is installed. It is fixed for as long as the API
 * runs, so it is read once rather than polled.
 */
export default function FirecrabPanel() {
  const { t } = useI18n();
  const [info, setInfo] = useState<FirecrabInfo | null>(null);
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    let cancelled = false;
    getFirecrabInfo()
      .then((next) => {
        if (!cancelled) setInfo(next);
      })
      .catch(() => {
        if (!cancelled) setFailed(true);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  return (
    <section className="panel host-info" aria-label="Firecrab">
      <h2 className="panel-title">Firecrab</h2>
      {info ? (
        <dl className="detail-fields mono">
          <dt>version</dt>
          <dd>{info.version}</dd>
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
