import { useCallback, useEffect, useRef, useState } from "react";
import type { FormEvent } from "react";
import type { MicroNetworkResponse, MicroServiceDeployment, MicroServiceResponse, ServicePhase } from "../bindings";
import { deployMicroService, getServiceDeployment, listMicroNetworks, stopMicroService } from "../api/client";
import { consoleHash } from "../navigation";
import { useI18n } from "../i18n";

const BUSY: ServicePhase[] = ["queued", "importing", "starting", "checking_health", "stopping"];

export default function ServiceDeployment({ service, onClose }: { service: MicroServiceResponse; onClose: () => void }) {
  const { t } = useI18n();
  const [deployment, setDeployment] = useState<MicroServiceDeployment | null>(null);
  const [networks, setNetworks] = useState<MicroNetworkResponse[]>([]);
  const [network, setNetwork] = useState("");
  const [port, setPort] = useState(30100);
  const [loaded, setLoaded] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [statusError, setStatusError] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);
  const generation = useRef(0);
  const busy = !!deployment && BUSY.includes(deployment.phase);
  const hasInstances = !!deployment?.instances.length;

  const refresh = useCallback(async () => {
    const current = ++generation.current;
    try {
      const result = await getServiceDeployment(service.id);
      if (generation.current !== current) return;
      setDeployment(result); setLoaded(true); setStatusError(null);
      if (result && (BUSY.includes(result.phase) || result.instances.length > 0)) {
        setPort(result.request.hostPort);
        setNetwork(result.request.microNetworkId ?? "");
      }
    } catch (failure) {
      if (generation.current !== current) return;
      setStatusError((failure as Error).message); setLoaded(true);
    }
  }, [service.id]);

  useEffect(() => {
    refresh();
    const interval = window.setInterval(refresh, 3000);
    let active = true;
    listMicroNetworks().then((rows) => { if (active) setNetworks(rows); }).catch(() => {});
    return () => { active = false; generation.current += 1; window.clearInterval(interval); };
  }, [refresh]);

  const act = async (stop: boolean) => {
    if (submitting) return;
    setSubmitting(true); setError(null); generation.current += 1;
    try {
      const result = stop ? await stopMicroService(service.id) : await deployMicroService(service.id, { hostPort: port, microNetworkId: network || null });
      generation.current += 1;
      setDeployment(result);
    } catch (failure) {
      setError((failure as Error).message);
    } finally { setSubmitting(false); }
  };

  const submit = (event: FormEvent) => { event.preventDefault(); act(false); };
  const labels: Record<ServicePhase, string> = {
    queued: t("Queued", "대기 중"), importing: t("Importing image", "이미지 가져오는 중"),
    starting: t("Starting MicroVM", "MicroVM 시작 중"), checking_health: t("Checking app health", "앱 상태 확인 중"),
    running: t("Running", "실행 중"), degraded: t("Health check failed", "상태 검사 실패"),
    stopping: t("Stopping", "중지 중"), stopped: t("Stopped", "중지 완료"), failed: t("Deployment failed", "배포 실패"),
  };
  return (
    <section className="panel" aria-labelledby="service-runtime-title">
      <div className="service-runtime-header">
        <h2 className="panel-title" id="service-runtime-title">{t(`Run: ${service.name}`, `실행 관리: ${service.name}`)}</h2>
        <button className="btn" onClick={onClose}>{t("Close", "닫기")}</button>
      </div>
      {!loaded && <p className="empty">{t("Loading…", "불러오는 중…")}</p>}
      {error && <p className="field-error" role="alert">{error}</p>}
      {statusError && <p className="field-error" role="alert">{statusError}</p>}
      {deployment?.error && <p className="field-error" role="alert">{deployment.error}</p>}
      <p role="status">{statusError ? t("Status unavailable", "상태를 확인할 수 없다") : deployment ? labels[deployment.phase] : loaded ? t("Not deployed", "아직 배포하지 않았다") : t("Checking deployment…", "배포 상태 확인 중…")}</p>
      {deployment?.digest && <p className="service-digest">{deployment.digest}</p>}
      <form className="create-grid" onSubmit={submit}>
        <div className="field service-wide"><label htmlFor="service-deploy-network">{t("Network", "네트워크")}</label><select id="service-deploy-network" value={network} disabled={submitting || busy || hasInstances} onChange={(event) => setNetwork(event.target.value)}><option value="">{t("Automatic services network", "서비스 네트워크 자동 생성")}</option>{network && !networks.some((row) => row.id === network) && <option value={network}>{t("Deployment network", "배포 네트워크")} · {network.slice(0, 8)}</option>}{networks.map((row) => <option key={row.id} value={row.id}>{row.name} · {row.subnetCidr}</option>)}</select></div>
        <div className="field"><label htmlFor="service-deploy-port">{t("First host port", "첫 번째 호스트 포트")}</label><input id="service-deploy-port" type="number" min={1024} max={65536 - service.replicas} value={port} disabled={submitting || busy || hasInstances} required onChange={(event) => setPort(event.target.valueAsNumber)} /></div>
        <div className="field field-submit"><label>&nbsp;</label><button className="btn primary" type="submit" disabled={!loaded || submitting || busy || hasInstances}>{t("Deploy and run", "배포 및 실행")}</button></div>
      </form>
      <p className="poll-note">{t(`Each replica uses one host port, starting at ${port}. App port: ${service.port}.`, `복제본마다 ${port}번부터 호스트 포트를 하나씩 사용한다. 앱 포트는 ${service.port}번이다.`)}</p>
      {(service.domain || Object.keys(service.secrets).length > 0) && <p className="field-error">{t("This deployment supports host ports and plain environment variables. Domain routing and CrabSecrets injection require later work.", "현재 배포는 호스트 포트와 일반 환경 변수를 지원한다. 도메인 연결과 CrabSecrets 주입은 아직 지원하지 않는다.")}</p>}
      {hasInstances && <>
        <div className="table-scroll"><table className="vm-table service-table"><thead><tr><th scope="col">MicroVM</th><th scope="col">{t("State", "상태")}</th><th scope="col">{t("App health", "앱 상태")}</th><th scope="col">{t("Endpoint", "접속 주소")}</th></tr></thead><tbody>{deployment!.instances.map((instance) => <tr key={instance.vmId}><td data-label="MicroVM"><a href={consoleHash(instance.vmId)}>{instance.vmId.slice(0, 8)}</a></td><td data-label={t("State", "상태")}>{instance.state}</td><td data-label={t("App health", "앱 상태")}>{instance.healthy ? t("Healthy", "정상") : t("Not healthy", "정상 응답 없음")}</td><td data-label={t("Endpoint", "접속 주소")}><a href={endpoint(instance.hostPort)} target="_blank" rel="noreferrer">:{instance.hostPort}</a></td></tr>)}</tbody></table></div>
        <div className="service-actions"><button className="btn danger" disabled={submitting || busy} onClick={() => { if (window.confirm(t("Stop and remove these service instances?", "서비스 인스턴스를 중지하고 삭제할까요?"))) act(true); }}>{t("Stop service", "서비스 중지")}</button></div>
      </>}
      {deployment && <pre className="service-deploy-log" aria-label={t("Deployment log", "배포 로그")}>{deployment.log.join("\n")}</pre>}
    </section>
  );
}

function endpoint(port: number): string {
  const url = new URL(window.location.origin);
  url.protocol = "http:"; url.port = String(port); url.pathname = "/";
  return url.toString();
}
