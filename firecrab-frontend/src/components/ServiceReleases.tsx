import { useEffect, useRef, useState } from "react";
import type { DeployStrategy, MicroServiceDeployment } from "../bindings";
import { listServiceReleases, rollbackServiceRelease, retryServiceRelease } from "../api/client";
import { newDeployOperation } from "../lib/deployOperation";
import { useI18n } from "../i18n";

export default function ServiceReleases({ deployment, disabled, onUpdate, onAccepted, onBusy }: {
  deployment: MicroServiceDeployment;
  disabled: boolean;
  onUpdate: (releaseId: string) => void;
  onAccepted: (deployment: MicroServiceDeployment) => void;
  onBusy: (value: boolean) => void;
}) {
  const { t } = useI18n();
  const [open, setOpen] = useState(false);
  const [refresh, setRefresh] = useState(0);
  const [result, setResult] = useState<{ key: string; rows: MicroServiceDeployment[] } | null>(null);
  const requestKey = `${deployment.serviceId}:${deployment.id}:${deployment.phase}:${refresh}`;
  const releases = result?.key === requestKey ? result.rows : null;
  const [error, setError] = useState<string | null>(null);
  const [selected, setSelected] = useState("");
  const [strategy, setStrategy] = useState<DeployStrategy>("rolling");
  const [submitting, setSubmitting] = useState(false);
  const acting = useRef(false);
  const pending = useRef<{ key: string; id: string } | null>(null);
  const mounted = useRef(true);
  useEffect(() => { mounted.current = true; return () => { mounted.current = false; }; }, []);
  useEffect(() => {
    if (!open) return;
    const controller = new AbortController();
    listServiceReleases(deployment.serviceId, controller.signal).then(rows => {
      if (controller.signal.aborted) return;
      setResult({ key: requestKey, rows }); setError(null);
      setSelected(previous => rows.some(row => row.id === previous) ? previous : rows.find(row => row.id !== deployment.id)?.id ?? "");
    }).catch(failure => { if (!controller.signal.aborted) setError((failure as Error).message); });
    return () => controller.abort();
  }, [open, deployment.serviceId, deployment.id, deployment.phase, requestKey]);
  const recovering = !!deployment.update && !deployment.update.completed;
  const canUpdate = !recovering && ["running", "degraded"].includes(deployment.phase);
  const act = async (retry: boolean) => {
    if (acting.current || disabled || (!retry && (!selected || selected === deployment.id || !releases?.some(row => row.id === selected)))) return;
    acting.current = true; setSubmitting(true); onBusy(true); setError(null);
    try {
      let result: MicroServiceDeployment;
      if (retry) result = await retryServiceRelease(deployment.serviceId, deployment.id);
      else {
        const key = `${selected}:${strategy}`;
        if (pending.current?.key !== key) pending.current = { key, id: newDeployOperation() };
        result = await rollbackServiceRelease(deployment.serviceId, { operationId: pending.current.id, expectedReleaseId: deployment.id, releaseId: selected, strategy });
        pending.current = null;
      }
      if (mounted.current) onAccepted(result);
    } catch (failure) {
      if (mounted.current) setError((failure as Error).message);
    } finally {
      acting.current = false;
      if (mounted.current) { setSubmitting(false); onBusy(false); }
    }
  };
  return <section aria-label="Deploy" className="service-release-panel">
    <h3>Deploy</h3>
    <p>{t("Update this app through the existing Nginx route. The service address stays the same.", "기존 Nginx 경로로 앱을 업데이트한다. 서비스 주소는 유지한다.")}</p>
    <div className="service-actions">
      {canUpdate && <button type="button" className="btn primary" disabled={disabled || submitting} onClick={() => onUpdate(deployment.id)}>{t("Deploy update", "업데이트 배포")}</button>}
      {recovering && deployment.phase === "failed" && <button type="button" className="btn" disabled={disabled || submitting} onClick={() => void act(true)}>{t("Retry Deploy", "Deploy 재시도")}</button>}
      <button type="button" className="btn" aria-expanded={open} aria-controls="service-release-history" onClick={() => setOpen(value => !value)}>{t("Release history", "릴리스 이력")}</button>
    </div>
    {recovering && <p role="status">{t(`Deploy ${deployment.id.slice(0, 8)} · ${deployment.update!.strategy}. Previous generation: ${deployment.update!.previous.instances.length} retained instances.`, `Deploy ${deployment.id.slice(0, 8)} · ${deployment.update!.strategy}. 이전 세대 인스턴스 ${deployment.update!.previous.instances.length}개를 보존하고 있다.`)}</p>}
    {error && <div role="alert" className="field-error"><p>{error}</p>{open && <button type="button" className="btn" onClick={() => setRefresh(value => value + 1)}>{t("Reload releases", "릴리스 다시 불러오기")}</button>}</div>}
    {open && <div id="service-release-history">
      {!releases && !error && <p role="status">{t("Loading releases…", "릴리스 불러오는 중…")}</p>}
      {releases && <>
        {releases.length === 0 ? <p>{t("No successful releases are retained yet.", "보관된 성공 릴리스가 없다.")}</p> : <>
          <div className="table-scroll"><table className="vm-table"><thead><tr><th>{t("Release", "릴리스")}</th><th>{t("Image", "이미지")}</th><th>{t("Completed", "완료 시각")}</th></tr></thead><tbody>{releases.map(release => <tr key={release.id}><td>{release.id.slice(0, 8)}{release.id === deployment.id ? t(" · active", " · 실행 중") : ""}</td><td><span>{release.spec.image}</span><br /><code title={release.digest ?? undefined}>{release.digest?.slice(0, 23)}</code></td><td>{release.finishedAt ? new Date(release.finishedAt * 1000).toLocaleString() : "—"}</td></tr>)}</tbody></table></div>
          <div className="create-grid">
            <div className="field"><label htmlFor="service-rollback-release">{t("Rollback release", "롤백할 릴리스")}</label><select id="service-rollback-release" value={selected} disabled={disabled || submitting || recovering} onChange={event => setSelected(event.target.value)}><option value="">{t("Select a previous release", "이전 릴리스 선택")}</option>{releases.filter(release => release.id !== deployment.id).map(release => <option value={release.id} key={release.id}>{release.id.slice(0, 8)} · {release.spec.image}</option>)}</select></div>
            <div className="field"><label htmlFor="service-rollback-strategy">{t("Strategy", "배포 방식")}</label><select id="service-rollback-strategy" value={strategy} disabled={disabled || submitting || recovering} onChange={event => setStrategy(event.target.value as DeployStrategy)}><option value="rolling">{t("Rolling — one extra VM", "롤링 — 추가 VM 1개")}</option><option value="blue_green">{t("Blue-green — a full extra generation", "블루그린 — 새 세대 전체 추가")}</option></select></div>
            <div className="field field-submit"><button type="button" className="btn" disabled={disabled || submitting || !canUpdate || recovering || !selected || selected === deployment.id} onClick={() => void act(false)}>{t("Roll back", "롤백")}</button></div>
          </div>
          <p className="poll-note">{t("Rollback restores the image and service configuration. It uses current credentials and Secret values. External data is not restored.", "롤백은 이미지와 서비스 설정을 복구한다. 현재 자격 증명과 시크릿 값을 사용한다. 외부 데이터는 복구하지 않는다.")}</p>
        </>}
      </>}
    </div>}
  </section>;
}
