import { useEffect, useRef, useState } from "react";
import { fetchJson } from "../api/client";
import type {
  MicroServiceDeployment,
  MicroServiceResponse,
  MicroServiceSpec,
} from "../bindings";
import { useI18n } from "../i18n";

export type ServiceRelease = {
  id: string;
  spec: MicroServiceSpec;
  digest: string | null;
  phase?: string;
};
type Release = ServiceRelease;
export default function ServiceReleases({
  service,
  deployment,
  onChanged,
  onList,
  selectedRelease,
  onUpdate,
}: {
  service: MicroServiceResponse;
  deployment: MicroServiceDeployment;
  onChanged: (result?: MicroServiceDeployment) => Promise<void>;
  onList?: (rows: Release[]) => void;
  selectedRelease?: string;
  onUpdate?: () => void;
}) {
  const { t } = useI18n();
  const [releases, setReleases] = useState<Release[] | null>(null),
    [error, setError] = useState(""),
    [busy, setBusy] = useState(false),
    [selected, setSelected] = useState(""),
    [historyRetry, setHistoryRetry] = useState(0),
    [strategy, setStrategy] = useState<"rolling" | "blue_green">("rolling");
  useEffect(() => {
    if (selectedRelease) setSelected(selectedRelease);
  }, [selectedRelease]);
  const pending = useRef<{ key: string; id: string } | null>(null);
  const base = `/api/micro-services/${encodeURIComponent(service.id)}`;
  const servingRelease =
    deployment.update && !deployment.update.completed
      ? deployment.update.previous?.id
      : ["running", "degraded"].includes(deployment.phase)
        ? deployment.id
        : undefined;
  useEffect(() => {
    let live = true;
    if (!deployment.spec.domain) return;
    if (!servingRelease) {
      setReleases([]);
      return;
    }
    fetchJson<Release[]>(`${base}/releases`)
      .then((rows) => {
        if (!Array.isArray(rows))
          throw new Error(
            t(
              "Invalid release list response.",
              "릴리스 목록 응답이 올바르지 않습니다.",
            ),
          );
        if (live) {
          setReleases(rows);
          onList?.(rows);
          setError("");
        }
      })
      .catch((e) => {
        if (live) setError(e instanceof Error ? e.message : String(e));
      });
    return () => {
      live = false;
    };
  }, [base, servingRelease, deployment.spec.domain, onList, historyRetry]);
  const settled =
    ["running", "degraded"].includes(deployment.phase) &&
    (!deployment.update || deployment.update.completed);
  const submit = async (kind: "update" | "rollback" | "retry") => {
    if (busy) return;
    if (
      kind === "rollback" &&
      !window.confirm(
        t(
          "Restore the selected release? External data is not restored.",
          "선택한 릴리스로 되돌릴까요? 외부 데이터는 복원되지 않습니다.",
        ),
      )
    )
      return;
    const key = `${kind}:${deployment.id}:${selected}:${strategy}:${JSON.stringify(service)}`;
    if (!pending.current || pending.current.key !== key)
      pending.current = { key, id: crypto.randomUUID() };
    const operationId = pending.current.id;
    let url = "",
      body: object | undefined;
    if (kind === "retry") {
      url = `${base}/deployment/retry`;
      body = { operationId: deployment.id };
    }
    if (kind === "rollback") {
      url = `${base}/rollback`;
      body = {
        operationId,
        expectedReleaseId: deployment.id,
        releaseId: selected,
        strategy,
      };
    }
    if (kind === "update") {
      url = `${base}/updates`;
      const { id: _id, ...spec } = service;
      body = { operationId, expectedReleaseId: deployment.id, spec, strategy };
    }
    setBusy(true);
    setError("");
    try {
      const result = await fetchJson<MicroServiceDeployment>(url, {
        method: "POST",
        ...(body
          ? {
              headers: { "Content-Type": "application/json" },
              body: JSON.stringify(body),
            }
          : {}),
      });
      pending.current = null;
      await onChanged(result);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };
  return (
    <section id="deploy-panel" className="service-releases">
      <h3>{t("Releases", "릴리스")}</h3>
      {error && (
        <div>
          <p role="alert">{error}</p>
          {releases === null && (
            <button
              className="btn"
              onClick={() => setHistoryRetry((value) => value + 1)}
            >
              {t("Retry release history", "릴리스 목록 다시 조회")}
            </button>
          )}
        </div>
      )}
      {!deployment.spec.domain ? (
        <p>
          {t(
            "Releases require a domain.",
            "릴리스 기능을 사용하려면 도메인을 설정하세요.",
          )}
        </p>
      ) : (
        <>
          {releases === null ? (
            <p>
              {error
                ? t(
                    "Release history is unavailable.",
                    "릴리스 목록을 확인할 수 없습니다.",
                  )
                : t("Loading releases…", "릴리스를 불러오는 중…")}
            </p>
          ) : releases.length === 0 ? (
            <p>{t("No retained releases.", "보관 중인 릴리스가 없습니다.")}</p>
          ) : (
            <div className="table-scroll">
              <table className="vm-table release-table">
                <thead>
                  <tr>
                    <th>{t("Release", "릴리스")}</th>
                    <th>{t("Image", "이미지")}</th>
                    <th>{t("State", "상태")}</th>
                  </tr>
                </thead>
                <tbody>
                  {releases.map((release) => (
                    <tr key={release.id}>
                      <td>
                        <span className="mono" title={release.id}>
                          {release.id.slice(0, 8)}
                        </span>
                      </td>
                      <td>
                        <span
                          className="service-image"
                          title={release.spec.image}
                        >
                          {release.spec.image}
                        </span>
                        {release.digest && (
                          <span
                            className="cell-sub mono"
                            title={release.digest}
                          >
                            {release.digest.slice(0, 22)}…
                          </span>
                        )}
                      </td>
                      <td>
                        {release.id === servingRelease
                          ? t("Active", "사용 중")
                          : t("Archived", "보관됨")}
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}
          {onUpdate && (
            <button
              id="deploy-update"
              className="btn primary"
              disabled={busy || !settled}
              onClick={onUpdate}
            >
              {t("Deploy update", "업데이트 배포")}
            </button>
          )}
          <div className="field">
            <label htmlFor="deploy-strategy">{t("Strategy", "전략")}</label>
            <select
              id="deploy-strategy"
              value={strategy}
              disabled={busy || !settled}
              onChange={(e) =>
                setStrategy(e.target.value as "rolling" | "blue_green")
              }
            >
              <option value="rolling">{t("Rolling", "롤링")}</option>
              <option value="blue_green">{t("Blue-green", "블루그린")}</option>
            </select>
          </div>
          <button
            className="btn"
            disabled={busy || !settled}
            onClick={() => void submit("update")}
          >
            {t("Deploy saved configuration", "저장된 설정 배포")}
          </button>
          <div className="field">
            <label htmlFor="deploy-rollback">
              {t("Rollback release", "롤백할 릴리스")}
            </label>
            <select
              id="deploy-rollback"
              disabled={busy || !settled}
              value={selected}
              onChange={(e) => setSelected(e.target.value)}
            >
              <option value="">{t("Select release", "릴리스 선택")}</option>
              {releases
                ?.filter(
                  (release) => release.id !== deployment.id && !!release.digest,
                )
                .map((release) => (
                  <option key={release.id} value={release.id}>
                    {release.id.slice(0, 8)} · {release.spec.image}
                  </option>
                ))}
            </select>
          </div>
          <button
            className="btn"
            disabled={
              busy ||
              !settled ||
              !releases?.some(
                (release) =>
                  release.id === selected &&
                  release.id !== deployment.id &&
                  !!release.digest,
              )
            }
            onClick={() => void submit("rollback")}
          >
            {t("Roll back to selected release", "선택한 릴리스로 롤백")}
          </button>
          {deployment.phase === "failed" &&
            !!deployment.update &&
            !deployment.update.completed && (
              <button
                className="btn"
                disabled={busy}
                onClick={() => void submit("retry")}
              >
                {t("Retry deployment", "배포 재시도")}
              </button>
            )}
        </>
      )}
    </section>
  );
}
