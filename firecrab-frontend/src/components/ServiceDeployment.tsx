import { useCallback, useEffect, useRef, useState } from "react";
import type { FormEvent } from "react";
import type { MicroNetworkResponse, MicroServiceDeployment, MicroServiceResponse, ServicePhase } from "../bindings";
import { ApiClientError, checkServiceDeployment, deployMicroService, getServiceDeployment, listMicroNetworks, stopMicroService } from "../api/client";
import { ArrowUpRight, Check, Info } from "lucide-react";
import { consoleHash, pipeCardHash } from "../navigation";
import { getPipeAuth, hasImageAuth } from "../api/pipeAuth";
import { reconnectServiceAuth, type ServiceDeploymentDraft } from "../lib/serviceDeploymentResume";
import { useI18n } from "../i18n";

type PlacementCheck = { key: string; fields: Record<string, string>; error: string | null };

type AuthState = "checking" | "connected" | "missing" | "unavailable";

const BUSY: ServicePhase[] = ["queued", "importing", "starting", "checking_health", "stopping"];

export default function ServiceDeployment({ service, draft, onClose, onEdit }: { service: MicroServiceResponse; draft?: ServiceDeploymentDraft; onClose: () => void; onEdit: () => void }) {
  const { t } = useI18n();
  const [deployment, setDeployment] = useState<MicroServiceDeployment | null>(null);
  const [networks, setNetworks] = useState<MicroNetworkResponse[]>([]);
  const [network, setNetwork] = useState(draft?.microNetworkId ?? "");
  const [port, setPort] = useState(draft?.hostPort ?? 30100);
  const [loaded, setLoaded] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [placement, setPlacement] = useState<PlacementCheck | null>(null);
  const [placementVersion, setPlacementVersion] = useState(0);
  const networkInput = useRef<HTMLSelectElement>(null);
  const portInput = useRef<HTMLInputElement>(null);
  const focusRestoredError = useRef(!!draft);
  const focusSubmitError = useRef(false);
  const [statusError, setStatusError] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);
  const [authState, setAuthState] = useState<AuthState>("checking");
  const authRequest = useRef<AbortController | null>(null);
  const generation = useRef(0);
  const polling = useRef(false);
  const acting = useRef(false);
  const busy = !!deployment && BUSY.includes(deployment.phase);
  const hasInstances = !!deployment?.instances.length;

  const unsupported = !!service.domain || Object.keys(service.secrets).length > 0;
  const canCheckPlacement = loaded && !busy && !hasInstances && !unsupported;
  const placementKey = JSON.stringify([service.id, service.replicas, network, port, placementVersion]);
  const portRangeError = !Number.isInteger(port) || port < 1024 || port + service.replicas - 1 > 65535
    ? t(`Choose a first host port from 1024 to ${65536 - service.replicas}. All replicas need consecutive ports.`, `첫 번째 호스트 포트는 1024~${65536 - service.replicas} 범위에서 선택한다. 모든 복제본에 연속 포트가 필요하다.`) : null;
  const placementFields = portRangeError ? { hostPort: portRangeError } : placement?.key === placementKey ? placement.fields : {};
  const placementChecking = canCheckPlacement && !portRangeError && placement?.key !== placementKey;
  const placementReady = canCheckPlacement && !portRangeError && placement?.key === placementKey && placement.error === null && Object.keys(placement.fields).length === 0;

  useEffect(() => {
    if (canCheckPlacement && portRangeError && focusRestoredError.current) {
      focusRestoredError.current = false;
      portInput.current?.focus();
    }
  }, [canCheckPlacement, portRangeError]);

  useEffect(() => {
    if (!canCheckPlacement || portRangeError) return;
    const controller = new AbortController();
    const timer = window.setTimeout(() => {
      const signal = AbortSignal.any([controller.signal, AbortSignal.timeout(10000)]);
      void Promise.allSettled([
        listMicroNetworks(signal),
        checkServiceDeployment(service.id, { hostPort: port, microNetworkId: network || null }, signal),
      ]).then(([catalog, check]) => {
        if (controller.signal.aborted) return;
        if (catalog.status === "fulfilled") setNetworks(catalog.value);
        const result = check.status === "rejected" ? placementFailure(placementKey, check.reason)
          : catalog.status === "rejected" ? placementFailure(placementKey, catalog.reason)
          : { key: placementKey, fields: {}, error: null };
        setPlacement(result);
        if (!focusRestoredError.current) return;
        focusRestoredError.current = false;
        if (result.fields.microNetworkId) networkInput.current?.focus();
        else if (result.fields.hostPort) portInput.current?.focus();
      });
    }, 250);
    return () => { window.clearTimeout(timer); controller.abort(); };
  }, [canCheckPlacement, portRangeError, service.id, service.replicas, port, network, placementKey]);

  useEffect(() => {
    if (submitting || !focusSubmitError.current || placement?.key !== placementKey) return;
    focusSubmitError.current = false;
    if (placement.fields.microNetworkId) networkInput.current?.focus();
    else if (placement.fields.hostPort) portInput.current?.focus();
  }, [submitting, placement, placementKey]);

  const readyInstance = !statusError && deployment?.phase === "running" ? deployment.instances.find((instance) => instance.healthy) : undefined;
  const progress = { queued: 0, importing: 0, starting: 1, checking_health: 2, running: 3, degraded: 2 };
  const activeStep = !statusError && deployment && deployment.phase in progress ? progress[deployment.phase as keyof typeof progress] : -1;

  const checkAuth = useCallback((restart = false): Promise<boolean> => {
    if (authRequest.current && !restart) return Promise.resolve(false);
    authRequest.current?.abort();
    const controller = new AbortController();
    authRequest.current = controller;
    return getPipeAuth(AbortSignal.any([controller.signal, AbortSignal.timeout(10000)]))
      .then((connections) => {
        if (controller.signal.aborted) return false;
        const connected = hasImageAuth(service.image, connections);
        setAuthState(connected ? "connected" : "missing");
        return connected;
      }).catch(() => {
        if (!controller.signal.aborted) setAuthState("unavailable");
        return false;
      }).finally(() => {
        if (authRequest.current === controller) authRequest.current = null;
      });
  }, [service.image]);

  useEffect(() => {
    void checkAuth();
    const interval = window.setInterval(() => { void checkAuth(); }, 3000);
    const checkOnReturn = () => {
      if (document.visibilityState === "visible") {
        setAuthState("checking");
        setPlacementVersion((value) => value + 1);
        void checkAuth(true);
      }
    };
    window.addEventListener("focus", checkOnReturn);
    document.addEventListener("visibilitychange", checkOnReturn);
    return () => {
      window.clearInterval(interval);
      window.removeEventListener("focus", checkOnReturn);
      document.removeEventListener("visibilitychange", checkOnReturn);
      authRequest.current?.abort();
      authRequest.current = null;
    };
  }, [checkAuth]);

  const refresh = useCallback(async () => {
    if (polling.current || acting.current) return;
    polling.current = true;
    const current = generation.current;
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
    } finally { polling.current = false; }
  }, [service.id]);

  useEffect(() => {
    refresh();
    const interval = window.setInterval(refresh, 3000);
    return () => { generation.current += 1; window.clearInterval(interval); };
  }, [refresh]);

  const recheckAuth = () => {
    setAuthState("checking");
    return checkAuth(true);
  };

  const act = async (stop: boolean) => {
    if (acting.current || (!stop && (!loaded || statusError || busy || hasInstances || unsupported || authState !== "connected" || !placementReady))) return;
    acting.current = true;
    setSubmitting(true); setError(null); generation.current += 1;
    let placementChecked = stop;
    try {
      if (!stop && !await recheckAuth()) return;
      if (!stop) await checkServiceDeployment(service.id, { hostPort: port, microNetworkId: network || null }, AbortSignal.timeout(10000));
      placementChecked = true;
      const result = stop ? await stopMicroService(service.id) : await deployMicroService(service.id, { hostPort: port, microNetworkId: network || null });
      generation.current += 1;
      setDeployment(result);
    } catch (failure) {
      const result = placementFailure(placementKey, failure);
      if (!stop && (!placementChecked || Object.keys(result.fields).length)) {
        focusSubmitError.current = true;
        setPlacement(result);
      } else setError((failure as Error).message);
      if (!stop) await recheckAuth();
    } finally { generation.current += 1; acting.current = false; setSubmitting(false); }
  };

  const submit = (event: FormEvent) => { event.preventDefault(); act(false); };
  const labels: Record<ServicePhase, string> = {
    queued: t("Queued", "대기 중"), importing: t("Importing image", "이미지 가져오는 중"),
    starting: t("Starting MicroVM", "MicroVM 시작 중"), checking_health: t("Checking app health", "앱 상태 확인 중"),
    running: t("Running", "실행 중"), degraded: t("Health check failed", "상태 검사 실패"),
    stopping: t("Stopping", "중지 중"), stopped: t("Stopped", "중지 완료"), failed: t("Deployment failed", "배포 실패"),
  };
  return (
    <section className="panel service-deployment" aria-labelledby="service-runtime-title">
      <div className="service-runtime-header">
        <h2 className="panel-title" id="service-runtime-title">{t(`Run: ${service.name}`, `실행 관리: ${service.name}`)}</h2>
        <button className="btn" onClick={onClose}>{t("Close", "닫기")}</button>
      </div>
      <p className="service-runtime-description">{t("Configuration saved. Choose how to connect, then deploy. Firecrab prepares the image, starts the app and checks its response.", "설정을 저장했다. 접속 방법을 정하고 배포한다. Firecrab이 이미지를 준비하고 앱을 실행한 뒤 응답을 확인한다.")}</p>
      {!loaded && <p className="empty">{t("Loading…", "불러오는 중…")}</p>}
      {error && <p className="field-error" role="alert">{error}</p>}
      {statusError && <p className="field-error" role="alert">{statusError}</p>}
      {deployment?.error && <p className="field-error" role="alert">{deployment.error}</p>}
      <p role="status">{statusError ? t("Status unavailable", "상태를 확인할 수 없다") : deployment ? labels[deployment.phase] : loaded ? t("Not deployed", "아직 배포하지 않았다") : t("Checking deployment…", "배포 상태 확인 중…")}</p>
      {activeStep >= 0 && <ol className="service-deploy-steps" aria-label={t("Deployment progress", "배포 진행 단계")}>{[t("Prepare image", "이미지 준비"), t("Start app", "앱 시작"), t("Check app health", "정상 응답 확인"), t("Ready to open", "접속 준비 완료")].map((label, index) => <li key={label} className={index < activeStep || (index === 3 && readyInstance) ? "done" : index === activeStep ? "active" : ""} aria-current={index === activeStep ? "step" : undefined}><span className="service-step-number">{index < activeStep || (index === 3 && readyInstance) ? <Check aria-hidden="true" /> : index + 1}</span>{label}</li>)}</ol>}
      {readyInstance && <div className="service-deploy-success"><div><strong>{t("Your app is ready", "앱을 사용할 수 있다")}</strong><p>{t("The app answered the health check.", "앱이 상태 검사에 정상 응답했다.")}</p></div><a className="btn" href={endpoint(readyInstance.hostPort)} target="_blank" rel="noreferrer">{t("Open app", "앱 열기")}<ArrowUpRight aria-hidden="true" /></a></div>}
      {(deployment?.phase === "failed" || deployment?.phase === "degraded") && <div className="service-deploy-next"><strong>{t("What to check next", "다음 항목을 확인한다")}</strong><p>{t("Check the deployment log below. Confirm that the image can run and that the app port and health path match your app. Stop remaining instances before changing settings or retrying.", "아래 배포 로그를 확인한다. 이미지가 실행 가능한지 확인한다. 앱 포트와 상태 검사 경로가 맞는지 확인한다. 설정을 바꾸거나 다시 배포하려면 남은 인스턴스를 먼저 중지한다.")}</p>{!hasInstances && !unsupported && <button className="btn" onClick={onEdit}>{t("Edit configuration", "설정 수정")}</button>}</div>}
      <div id="service-deploy-auth" aria-live="polite">
        {authState === "checking" && <p className="poll-note">{t("Checking AUTH connection…", "AUTH 연결 확인 중…")}</p>}
        {authState === "missing" && <div className="service-notice service-notice-warning"><Info aria-hidden="true" /><div>
          <strong>{t("Reconnect AUTH to deploy", "배포하려면 AUTH를 다시 연결한다")}</strong>
          <p>{t("This image has no matching AUTH connection. Connect its GitHub account or AWS account and region.", "이 이미지에 맞는 AUTH 연결이 없다. 이미지 소유자의 GitHub 계정 또는 AWS 계정과 리전을 연결한다.")}</p>
          <code className="service-auth-image">{service.image}</code>
          {hasInstances && <p>{t("Existing instances keep running. You can still stop them.", "기존 인스턴스는 계속 실행된다. 서비스 중지는 사용할 수 있다.")}</p>}
          <div className="service-auth-actions"><a className="btn" href={pipeCardHash("auth")} onClick={(event) => {
            event.preventDefault();
            reconnectServiceAuth({ serviceId: service.id, serviceName: service.name, image: service.image, microNetworkId: network, hostPort: port });
          }}>{t("Reconnect AUTH", "AUTH 다시 연결")}</a><button className="btn" disabled={submitting} onClick={() => { void recheckAuth(); }}>{t("Check again", "다시 확인")}</button></div>
        </div></div>}
        {authState === "unavailable" && <div className="service-notice service-notice-warning"><Info aria-hidden="true" /><div>
          <strong>{t("Unable to verify AUTH", "AUTH 연결을 확인할 수 없다")}</strong>
          <p>{t("Deployment is disabled until the connection can be verified. Try again when the API is available.", "연결 상태를 확인할 때까지 배포를 사용할 수 없다. API 연결 상태를 확인한 뒤 다시 시도한다.")}</p>
          <div className="service-auth-actions"><button className="btn" disabled={submitting} onClick={() => { void recheckAuth(); }}>{t("Retry AUTH check", "AUTH 연결 확인 재시도")}</button></div>
        </div></div>}
      </div>
      {canCheckPlacement && <div id="service-deploy-placement" aria-live="polite">
        {placementChecking && <p className="poll-note">{t("Checking network and host ports…", "네트워크와 호스트 포트 확인 중…")}</p>}
        {placement?.key === placementKey && placement.error !== null && <div className="service-notice service-notice-warning"><Info aria-hidden="true" /><div><strong>{t("Unable to verify deployment choices", "배포 설정을 확인할 수 없다")}</strong><p>{t("Deployment is disabled until the network and ports can be checked. Try again.", "네트워크와 포트를 확인할 때까지 배포를 사용할 수 없다. 다시 확인한다.")}</p><p>{placement.error}</p><button className="btn" onClick={() => setPlacementVersion((value) => value + 1)}>{t("Retry deployment check", "배포 설정 확인 재시도")}</button></div></div>}
      </div>}
      <form className="create-grid" onSubmit={submit}>
        <div className="field service-wide"><label htmlFor="service-deploy-network">{t("Network", "네트워크")}</label><select ref={networkInput} id="service-deploy-network" value={network} aria-invalid={canCheckPlacement && !!placementFields.microNetworkId} aria-describedby={canCheckPlacement && placementFields.microNetworkId ? "service-deploy-network-error" : undefined} disabled={submitting || busy || hasInstances} onChange={(event) => setNetwork(event.target.value)}><option value="">{t("Automatic services network", "서비스 네트워크 자동 생성")}</option>{network && !networks.some((row) => row.id === network) && <option value={network}>{canCheckPlacement && placementFields.microNetworkId ? t("Unavailable network", "사용할 수 없는 네트워크") : t("Deployment network", "배포 네트워크")} · {network.slice(0, 8)}</option>}{networks.map((row) => <option key={row.id} value={row.id}>{row.name} · {row.subnetCidr}</option>)}</select>{canCheckPlacement && placementFields.microNetworkId && <div id="service-deploy-network-error" className="field-error" role="alert"><p>{t("The selected network is unavailable. Choose another network or use the automatic services network.", "선택한 네트워크를 사용할 수 없다. 다른 네트워크를 선택하거나 서비스 네트워크 자동 생성을 사용한다.")}</p><button className="btn" type="button" onClick={() => setNetwork("")}>{t("Use automatic services network", "서비스 네트워크 자동 생성 사용")}</button></div>}</div>
        <div className="field"><label htmlFor="service-deploy-port">{t("First host port", "첫 번째 호스트 포트")}</label><input ref={portInput} id="service-deploy-port" aria-invalid={canCheckPlacement && !!placementFields.hostPort} aria-describedby={canCheckPlacement && placementFields.hostPort ? "service-deploy-port-error" : undefined} type="number" min={1024} max={65536 - service.replicas} value={Number.isNaN(port) ? "" : port} disabled={submitting || busy || hasInstances} required onChange={(event) => setPort(event.target.valueAsNumber)} />{canCheckPlacement && placementFields.hostPort && <p id="service-deploy-port-error" className="field-error" role="alert">{t("Choose another host port range.", "다른 호스트 포트 범위를 선택한다.")} {placementFields.hostPort}</p>}</div>
        <div className="field field-submit"><button className="btn primary" type="submit" aria-describedby={[authState !== "connected" ? "service-deploy-auth" : "", canCheckPlacement ? "service-deploy-placement" : ""].filter(Boolean).join(" ") || undefined} disabled={!loaded || !!statusError || submitting || busy || hasInstances || unsupported || authState !== "connected" || !placementReady}>{t("Deploy and run", "배포 및 실행")}</button></div>
      </form>
      <p className="poll-note">{t(`Each replica uses one host port, starting at ${Number.isNaN(port) ? "—" : port}. App port: ${service.port}.`, `복제본마다 ${Number.isNaN(port) ? "—" : port}번부터 호스트 포트를 하나씩 사용한다. 앱 포트는 ${service.port}번이다.`)}</p>
      <p className="service-runtime-description">{t("A host port is the external entrance to your app. The automatic network needs no manual setup.", "호스트 포트는 외부에서 앱에 접속하는 입구이다. 자동 네트워크를 선택하면 직접 구성할 필요가 없다.")}</p>
      {unsupported && <div className="service-notice service-notice-warning"><Info aria-hidden="true" /><div><p>{t("Domain routing and secret injection are not supported yet. Clear the domain and secret references to deploy using host ports.", "도메인 연결과 시크릿 주입은 아직 지원하지 않는다. 호스트 포트로 배포하려면 도메인과 시크릿 참조를 제거한다.")}</p><button className="btn" onClick={onEdit}>{t("Edit configuration", "설정 수정")}</button></div></div>}
      {hasInstances && <>
        <div className="table-scroll"><table className="vm-table service-table"><thead><tr><th id="service-instance-column-0" scope="col">MicroVM</th><th id="service-instance-column-1" scope="col">{t("State", "상태")}</th><th id="service-instance-column-2" scope="col">{t("App health", "앱 상태")}</th><th id="service-instance-column-3" scope="col">{t("Endpoint", "접속 주소")}</th></tr></thead><tbody>{deployment!.instances.map((instance) => <tr key={instance.vmId}><td headers="service-instance-column-0" data-label="MicroVM"><a href={consoleHash(instance.vmId)}>{instance.vmId.slice(0, 8)}</a></td><td headers="service-instance-column-1" data-label={t("State", "상태")}>{instance.state}</td><td headers="service-instance-column-2" data-label={t("App health", "앱 상태")}>{instance.healthy ? t("Healthy", "정상") : t("Not healthy", "정상 응답 없음")}</td><td headers="service-instance-column-3" data-label={t("Endpoint", "접속 주소")}><a href={endpoint(instance.hostPort)} target="_blank" rel="noreferrer">:{instance.hostPort}</a></td></tr>)}</tbody></table></div>
        <div className="service-actions"><button className="btn danger" disabled={submitting || busy} onClick={() => { if (window.confirm(t("Stop and remove these service instances?", "서비스 인스턴스를 중지하고 삭제할까요?"))) act(true); }}>{t("Stop service", "서비스 중지")}</button></div>
      </>}
      {deployment && <details className="service-log-details" open={deployment.phase === "failed" || deployment.phase === "degraded"}><summary>{t("Deployment log and image details", "배포 로그와 이미지 정보")}</summary>{deployment.digest && <p className="service-digest">{deployment.digest}</p>}<pre className="service-deploy-log" aria-label={t("Deployment log", "배포 로그")}>{deployment.log.join("\n")}</pre></details>}
    </section>
  );
}

function endpoint(port: number): string {
  const url = new URL(window.location.origin);
  url.protocol = "http:"; url.port = String(port); url.pathname = "/";
  return url.toString();
}

function placementFailure(key: string, failure: unknown): PlacementCheck {
  const fields: Record<string, string> = {};
  if (failure instanceof ApiClientError) {
    for (const field of ["microNetworkId", "hostPort"]) {
      const message = failure.fieldError(field);
      if (message) fields[field] = message;
    }
  }
  return { key, fields, error: Object.keys(fields).length ? null : failure instanceof Error ? failure.message || "Deployment check failed" : String(failure) };
}
