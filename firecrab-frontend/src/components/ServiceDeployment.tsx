import ServiceConnections from "./ServiceConnections";
import ServiceForm from "./ServiceForm";
import { fetchJson } from "../api/client";
import { useCallback, useEffect, useRef, useState } from "react";
import type { FormEvent } from "react";
import type {
  MicroNetworkResponse,
  MicroServiceDeployment,
  MicroServiceResponse,
  MicroServiceSpec,
  SecurityGroupResponse,
  ServicePhase,
} from "../bindings";
import {
  ApiClientError,
  deployMicroService,
  getSecurityGroup,
  getServiceDeployment,
  listMicroNetworks,
  stopMicroService,
} from "../api/client";
import {
  readServiceDeployDraft,
  saveServiceDeployDraft,
  clearServiceDeployDraft,
} from "../lib/serviceDeployDraft";
import { getPipe, type PipeSnapshot } from "../api/pipe";
import { consoleHash } from "../navigation";
import ServiceAutomation from "./ServiceAutomation";
import ServiceObservability from "./ServiceObservability";
import ServiceCancel from "./ServiceCancel";
import ServiceReleases, { type ServiceRelease } from "./ServiceReleases";
import { DetailHead, DetailProp, DetailSection } from "./ui/DetailSection";
import PipeServiceMenu from "./PipeServiceMenu";
import {
  useServicePlacement,
  verifyServicePlacement,
} from "./useServicePlacement";
import { useI18n } from "../i18n";

const BUSY: ServicePhase[] = [
  "queued",
  "importing",
  "starting",
  "checking_health",
  "connecting_domain",
  "switching_traffic",
  "draining",
  "rolling_back",
  "stopping",
];

export default function ServiceDeployment({
  service,
  onEdit,
  onDelete,
  initialDeploy = false,
  onUnsavedChange,
}: {
  service: MicroServiceResponse;
  onEdit: () => void;
  onDelete: () => void;
  initialDeploy?: boolean;
  onUnsavedChange: (dirty: boolean) => void;
}) {
  const { t } = useI18n();
  const tabs = [
    { id: "overview", label: t("Overview", "개요") },
    { id: "instances", label: t("Instances", "인스턴스") },
    { id: "releases", label: t("Releases", "릴리스") },
    { id: "automation", label: t("Settings", "설정") },
  ] as const;
  const [tab, setTab] = useState<(typeof tabs)[number]["id"]>("overview");
  const [deployment, setDeployment] = useState<MicroServiceDeployment | null>(
    null,
  );
  const [pipe, setPipe] = useState<PipeSnapshot | null>(null);
  const [networks, setNetworks] = useState<MicroNetworkResponse[]>([]);
  const [resumeDraft] = useState(() => {
    const saved = readServiceDeployDraft();
    return saved?.serviceId === service.id ? saved : null;
  });
  const [network, setNetwork] = useState(resumeDraft?.network ?? "");
  const [port, setPort] = useState(resumeDraft?.port ?? "");
  const [loaded, setLoaded] = useState(false);
  const [deployForm, setDeployForm] = useState(initialDeploy);
  const [releases, setReleases] = useState<ServiceRelease[]>([]);
  const [selectedRelease, setSelectedRelease] = useState("");
  const [updateDraft, setUpdateDraft] = useState<{
    service: MicroServiceResponse;
    releaseId: string;
  } | null>(null);
  const updateRequest = useRef<{ key: string; id: string } | null>(null);
  const [groupRefresh, setGroupRefresh] = useState(0);
  const [error, setError] = useState<string | null>(null);
  const [statusError, setStatusError] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);
  const [group, setGroup] = useState<SecurityGroupResponse | null>(null);
  const [groupError, setGroupError] = useState<string | null>(null);
  const generation = useRef(0);
  const pendingRead = useRef<Promise<void> | null>(null);
  const acting = useRef(false);
  const busy = !!deployment && BUSY.includes(deployment.phase);
  const hasInstances = !!deployment?.instances.length;
  const ownsResources =
    hasInstances ||
    !!deployment?.domainRouteOwned ||
    !!deployment?.internalRouteOwned;
  const domain = service.domain;
  const targetSpec = deployment?.spec ?? service;
  const activeSpec =
    deployment?.update && !deployment.update.completed
      ? (deployment.update.previous?.spec ?? service)
      : (deployment?.spec ?? service);
  const routed =
    !!deployment?.domainRouteOwned &&
    ["running", "degraded"].includes(deployment.phase);
  const groupPending = !!service.securityGroupId && group === null;
  const deployNetwork =
    busy || ownsResources ? network : (group?.microNetworkId ?? network);
  useEffect(() => {
    if (updateDraft) return;
    onUnsavedChange(
      deployForm &&
        (port !== (resumeDraft?.port ?? "") ||
          network !== (resumeDraft?.network ?? "")),
    );
    return () => onUnsavedChange(false);
  }, [deployForm, port, network, resumeDraft, updateDraft, onUnsavedChange]);

  const placement = useServicePlacement({
    id: service.id,
    image: service.image,
    network: deployNetwork,
    port,
    domain: service.domain,
    replicas: service.replicas,
    enabled: deployForm && loaded && !ownsResources && !busy && !groupPending,
  });
  useEffect(() => {
    if (!service.securityGroupId) return;
    let active = true;
    getSecurityGroup(service.securityGroupId)
      .then((result) => {
        if (active) {
          setGroup(result);
          setGroupError(null);
        }
      })
      .catch((failure) => {
        if (active) setGroupError((failure as Error).message);
      });
    return () => {
      active = false;
    };
  }, [service.securityGroupId, groupRefresh]);

  const refresh = useCallback(async () => {
    if (pendingRead.current) return pendingRead.current;
    if (acting.current) return;
    const current = generation.current;
    const request = (async () => {
      try {
        const result = await getServiceDeployment(service.id);
        if (generation.current !== current) return;
        if (
          result &&
          (!Array.isArray(result.instances) ||
            !Array.isArray(result.log) ||
            !result.request ||
            !result.spec)
        )
          throw new Error("Invalid deployment status response.");
        setDeployment(result);
        setLoaded(true);
        setStatusError(null);
        if (
          result &&
          (result.instances.length ||
            result.internalRouteOwned ||
            result.domainRouteOwned)
        )
          setDeployForm(false);
        if (
          result &&
          (BUSY.includes(result.phase) || result.instances.length > 0)
        ) {
          setPort(
            result.request.hostPort ? String(result.request.hostPort) : "",
          );
          setNetwork(result.request.microNetworkId ?? "");
        }
      } catch (failure) {
        if (generation.current === current) {
          setStatusError((failure as Error).message);
          setLoaded(true);
        }
      }
    })();
    pendingRead.current = request;
    try {
      await request;
    } finally {
      if (pendingRead.current === request) pendingRead.current = null;
    }
  }, [service.id]);
  const deploymentChanged = useCallback(
    async (result?: MicroServiceDeployment) => {
      generation.current++;
      if (
        result &&
        Array.isArray(result.instances) &&
        result.spec &&
        result.request
      )
        setDeployment(result);
      await pendingRead.current;
      await refresh();
    },
    [refresh],
  );

  useEffect(() => {
    refresh();
    const interval = window.setInterval(refresh, 3000);
    let active = true;
    listMicroNetworks()
      .then((rows) => {
        if (active) setNetworks(rows);
      })
      .catch(() => {});
    return () => {
      active = false;
      generation.current += 1;
      window.clearInterval(interval);
    };
  }, [refresh]);

  useEffect(() => {
    if (!domain) return;
    let active = true;
    getPipe()
      .then((result) => {
        if (active) setPipe(result);
      })
      .catch(() => {
        if (active) setPipe(null);
      });
    return () => {
      active = false;
    };
  }, [domain, deployment?.phase]);

  const act = async (stop: boolean) => {
    if (acting.current || (!stop && groupPending)) return;
    acting.current = true;
    setSubmitting(true);
    setError(null);
    generation.current += 1;
    try {
      if (!stop)
        await verifyServicePlacement(service.id, service.image, {
          ...(domain ? {} : { hostPort: Number(port) || 0 }),
          microNetworkId: deployNetwork || null,
        });
      const result = stop
        ? await stopMicroService(service.id)
        : await deployMicroService(service.id, {
            ...(domain ? {} : { hostPort: Number(port) || 0 }),
            microNetworkId: deployNetwork || null,
          });
      generation.current += 1;
      setDeployment(result);
      setDeployForm(false);
      clearServiceDeployDraft();
    } catch (failure) {
      const fields =
        failure instanceof ApiClientError
          ? Object.values(failure.apiError?.fields ?? {})
          : [];
      setError(fields.length ? fields.join(" ") : (failure as Error).message);
    } finally {
      generation.current += 1;
      acting.current = false;
      setSubmitting(false);
    }
  };

  const openUpdate = () => {
    if (!deployment || busy) return;
    updateRequest.current = null;
    setUpdateDraft({
      service: structuredClone({
        ...service,
        ...deployment.spec,
        id: service.id,
      }),
      releaseId: deployment.id,
    });
    setTab("releases");
  };
  const update = async (
    spec: MicroServiceSpec,
    strategy: "rolling" | "blue_green",
  ) => {
    if (!updateDraft) return;
    const key = JSON.stringify({ spec, strategy });
    if (updateRequest.current?.key !== key)
      updateRequest.current = { key, id: crypto.randomUUID() };
    const result = await fetchJson<MicroServiceDeployment>(
      `/api/micro-services/${encodeURIComponent(service.id)}/updates`,
      {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          operationId: updateRequest.current.id,
          expectedReleaseId: updateDraft.releaseId,
          spec,
          strategy,
        }),
      },
    );
    await deploymentChanged(result);
  };
  const submit = (event: FormEvent) => {
    event.preventDefault();
    act(false);
  };
  const labels: Record<ServicePhase, string> = {
    queued: t("Queued", "대기 중"),
    importing: t("Importing image", "이미지 가져오는 중"),
    starting: t("Starting MicroVM", "MicroVM 시작 중"),
    checking_health: t("Checking app health", "앱 상태 확인 중"),
    connecting_domain: t("Connecting domain", "도메인 연결 중"),
    switching_traffic: t("Switching Nginx traffic", "Nginx 트래픽 전환 중"),
    draining: t("Draining", "기존 연결 종료 중"),
    rolling_back: t("Rolling back", "롤백 중"),
    cancelled: t("Cancelled", "취소됨"),
    running: t("Running", "실행 중"),
    degraded: t("Health check failed", "상태 검사 실패"),
    stopping: t("Stopping", "중지 중"),
    stopped: t("Stopped", "중지 완료"),
    failed: t("Deployment failed", "배포 실패"),
  };
  const networkField = (
    <div className="field service-wide">
      <label htmlFor="service-deploy-network">{t("Network", "네트워크")}</label>
      <select
        id="service-deploy-network"
        value={deployNetwork}
        disabled={
          submitting || busy || ownsResources || !!service.securityGroupId
        }
        onChange={(event) => setNetwork(event.target.value)}
      >
        <option value="">
          {t("Automatic services network", "서비스 네트워크 자동 생성")}
        </option>
        {deployNetwork && !networks.some((row) => row.id === deployNetwork) && (
          <option value={deployNetwork}>
            {t("Deployment network", "배포 네트워크")} ·{" "}
            {deployNetwork.slice(0, 8)}
          </option>
        )}
        {networks.map((row) => (
          <option key={row.id} value={row.id}>
            {row.name} · {row.subnetCidr}
          </option>
        ))}
      </select>
    </div>
  );
  if (updateDraft)
    return (
      <ServiceForm
        service={updateDraft.service}
        onDeploy={update}
        onSaved={async () => {
          setUpdateDraft(null);
          setTab("releases");
        }}
        onCancel={() => {
          setUpdateDraft(null);
          setTab("releases");
        }}
        onUnsavedChange={onUnsavedChange}
      />
    );
  return (
    <section className="service-runtime" aria-label={service.name}>
      <DetailHead
        title={
          deployForm
            ? t("Deploy MicroService", "MicroService 배포")
            : service.name
        }
        meta={
          deployForm ? (
            service.name
          ) : (
            <>
              <span>
                {deployment
                  ? labels[deployment.phase]
                  : loaded
                    ? t("Not deployed", "아직 배포하지 않았다")
                    : t("Checking deployment…", "배포 상태 확인 중…")}
              </span>
              {deployment && (
                <>
                  {" "}
                  ·{" "}
                  <span className="mono" title={deployment.id}>
                    {deployment.id.slice(0, 8)}
                  </span>
                </>
              )}
              {deployment?.update && (
                <>
                  {" "}
                  ·{" "}
                  {deployment.update.strategy === "blue_green"
                    ? t("Blue-green", "블루그린")
                    : t("Rolling", "롤링")}
                </>
              )}
            </>
          )
        }
        actions={
          <>
            {!deployForm && (!ownsResources || domain) && (
              <button
                className="btn"
                disabled={!loaded || !!statusError || submitting || busy}
                onClick={() => {
                  if (ownsResources && domain) openUpdate();
                  else {
                    setDeployForm(true);
                    setTab("overview");
                  }
                }}
              >
                {ownsResources && domain
                  ? t("Deploy update", "업데이트 배포")
                  : t("Deploy", "배포")}
              </button>
            )}
            {ownsResources && domain && (
              <button
                className="btn"
                disabled={submitting || busy}
                onClick={() => setTab("releases")}
              >
                {t("Roll back", "롤백")}
              </button>
            )}
            <PipeServiceMenu
              label={t("More", "더 보기")}
              disabled={submitting}
              items={[
                ...(ownsResources || busy
                  ? [
                      {
                        label: t("Stop service", "서비스 중지"),
                        disabled: busy,
                        onSelect: () => {
                          if (
                            window.confirm(
                              t(
                                "Stop and remove these service instances?",
                                "서비스 인스턴스를 중지하고 삭제할까요?",
                              ),
                            )
                          )
                            void act(true);
                        },
                      },
                    ]
                  : []),
                {
                  label: t("Edit configuration", "설정 수정"),
                  disabled: busy,
                  onSelect: onEdit,
                },
                {
                  label: t("Delete", "삭제"),
                  danger: true,
                  disabled: busy || ownsResources,
                  onSelect: onDelete,
                },
              ]}
            />
          </>
        }
      />
      {!loaded && <p className="empty">{t("Loading…", "불러오는 중…")}</p>}
      {error && (
        <p className="field-error" role="alert">
          {error}
        </p>
      )}
      {statusError && (
        <p className="field-error" role="alert">
          {statusError}
        </p>
      )}
      {groupError && (
        <p className="field-error" role="alert">
          {groupError}
        </p>
      )}
      {deployment?.error && (
        <p className="field-error" role="alert">
          {deployment.error}
        </p>
      )}

      <section
        className="service-deployment-status"
        aria-label={t("Deployment", "배포")}
      >
        <div id="service-status">
          <p role="status">
            {statusError
              ? t("Status unavailable", "상태를 확인할 수 없다")
              : deployment
                ? labels[deployment.phase]
                : loaded
                  ? t("Not deployed", "아직 배포하지 않았다")
                  : t("Checking deployment…", "배포 상태 확인 중…")}
          </p>
          {deployment && (
            <ServiceCancel
              key={deployment.id}
              serviceId={service.id}
              deployment={deployment}
              onChanged={deploymentChanged}
            />
          )}
          {deployment?.digest && (
            <p className="service-digest mono" title={deployment.digest}>
              {deployment.digest}
            </p>
          )}
          {deployment && busy && (
            <>
              <p className="service-health">
                {
                  deployment.instances.filter((instance) => instance.healthy)
                    .length
                }
                /{targetSpec.replicas} {t("healthy", "정상")}
              </p>
              {deployment.update?.previous && (
                <p className="service-update">
                  {deployment.update.previous.id.slice(0, 8)} →{" "}
                  {deployment.id.slice(0, 8)} ·{" "}
                  {deployment.update.strategy === "blue_green"
                    ? t("Blue-green", "블루그린")
                    : t("Rolling", "롤링")}
                </p>
              )}
              <div
                className="service-progress"
                role="progressbar"
                aria-label={t("Healthy replicas", "정상 복제본")}
                aria-valuemin={0}
                aria-valuenow={
                  deployment.instances.filter((instance) => instance.healthy)
                    .length
                }
                aria-valuemax={targetSpec.replicas}
              >
                {Array.from({ length: targetSpec.replicas }, (_, index) => (
                  <i
                    key={index}
                    data-state={
                      deployment.instances[index]?.healthy ? "ok" : "busy"
                    }
                  />
                ))}
              </div>
            </>
          )}
        </div>
        {deployment && deployment.log.length > 0 && (
          <details className="service-log-disclosure">
            <summary>{t("Deployment log", "배포 로그")}</summary>
            <pre
              id="service-log"
              className="service-deploy-log"
              aria-label={t("Deployment log", "배포 로그")}
            >
              {deployment.log.join("\n")}
            </pre>
          </details>
        )}
      </section>
      <div
        className="tabs detail-tabs"
        role="tablist"
        aria-label={t("MicroService sections", "MicroService 구역")}
        onKeyDown={(event) => {
          if (!["ArrowLeft", "ArrowRight", "Home", "End"].includes(event.key))
            return;
          event.preventDefault();
          const index = tabs.findIndex((item) => item.id === tab),
            size = tabs.length;
          const next =
            event.key === "Home"
              ? 0
              : event.key === "End"
                ? size - 1
                : (index + (event.key === "ArrowRight" ? 1 : -1) + size) % size;
          setTab(tabs[next].id);
          event.currentTarget
            .querySelectorAll<HTMLButtonElement>('[role="tab"]')
            [next]?.focus();
        }}
      >
        {tabs.map((item) => (
          <button
            type="button"
            key={item.id}
            id={`service-tab-${item.id}`}
            aria-controls={`service-panel-${item.id}`}
            className="tab"
            role="tab"
            tabIndex={tab === item.id ? 0 : -1}
            aria-selected={tab === item.id}
            onClick={() => setTab(item.id)}
          >
            {item.label}
            {item.id === "instances" && deployment?.instances.length
              ? ` ${deployment.instances.length}`
              : ""}
          </button>
        ))}
      </div>
      <div
        role="tabpanel"
        id="service-panel-overview"
        aria-labelledby="service-tab-overview"
        hidden={tab !== "overview"}
      >
        {domain && !deployForm && (
          <div className="service-domain-panel">
            <strong>{domain}</strong>
            <p>
              {routed
                ? t("Domain routing is ready.", "도메인 경로가 준비되었다.")
                : t(
                    "Healthy replicas will connect to this domain automatically.",
                    "정상 인스턴스를 이 도메인에 자동 연결한다.",
                  )}
            </p>
            {routed && (
              <a
                className="btn"
                href={`https://${domain}`}
                target="_blank"
                rel="noreferrer"
              >
                {t("Open domain", "도메인 열기")}
              </a>
            )}
            {pipe?.config?.tunnel ? (
              <p className="poll-note">
                {t(
                  "For public HTTPS, point this domain's DNS CNAME to",
                  "외부 HTTPS 접속을 위해 도메인의 DNS CNAME을 다음 주소로 연결한다.",
                )}{" "}
                <code>{pipe.config.tunnel.id}.cfargotunnel.com</code>.{" "}
                {t(
                  "Keep Cloudflare proxy enabled.",
                  "Cloudflare 프록시를 활성화한다.",
                )}
              </p>
            ) : (
              <p className="poll-note">
                {t(
                  "For public HTTPS, connect your domain in",
                  "외부 HTTPS 접속을 위해 다음 화면에서 도메인을 연결한다.",
                )}{" "}
                <a href="#/pipe/named-tunnel">Pipe Tunnel</a>.
              </p>
            )}
          </div>
        )}
        {!deployForm && (
          <DetailSection title={t("Properties", "속성")}>
            <dl className="props">
              <DetailProp
                label={t("Image", "이미지")}
                copy={activeSpec.image}
                wrap
              >
                {activeSpec.image}
              </DetailProp>
              <DetailProp label={t("Port", "포트")}>
                {activeSpec.port}
              </DetailProp>
              <DetailProp label={t("Replicas", "복제 수")}>
                {activeSpec.replicas}
              </DetailProp>
              <DetailProp label={t("Health path", "상태 검사 경로")}>
                {activeSpec.healthPath}
              </DetailProp>
              <DetailProp label={t("Size", "크기")}>
                {activeSpec.cpu} vCPU · {activeSpec.ram / 1024} GiB
              </DetailProp>
              <ServiceConnections spec={activeSpec} />
            </dl>
          </DetailSection>
        )}
        {placement.error && (
          <div className="service-placement-error" role="alert">
            <p>{placement.error}</p>
            {placement.error.includes("Pipe Auth") && (
              <a
                href="#/pipe/auth"
                onClick={() => {
                  onUnsavedChange(false);
                  saveServiceDeployDraft({
                    serviceId: service.id,
                    image: service.image,
                    port,
                    network: deployNetwork,
                  });
                }}
              >
                {t("Reconnect AUTH", "AUTH 다시 연결")}
              </a>
            )}
            <button type="button" className="btn" onClick={placement.retry}>
              {t("Check again", "다시 확인")}
            </button>
          </div>
        )}
        {groupError && (
          <button
            className="btn"
            onClick={() => setGroupRefresh((value) => value + 1)}
          >
            {t("Retry security group", "보안그룹 다시 조회")}
          </button>
        )}
        {!deployForm && deployment?.spec.domain && (
          <DetailSection title={t("Recent releases", "최근 릴리스")}>
            <ul className="recent-releases">
              {[
                deployment,
                ...releases.filter((release) => release.id !== deployment.id),
              ]
                .slice(0, 3)
                .map((release) => {
                  const active =
                    deployment.update &&
                    !deployment.update.completed &&
                    deployment.update.previous
                      ? deployment.update.previous.id
                      : deployment.id;
                  const archived =
                    release.id !== active && release.id !== deployment.id;
                  return (
                    <li key={release.id} data-release={release.id}>
                      <div>
                        <span className="mono" title={release.id}>
                          {release.id.slice(0, 8)}
                        </span>{" "}
                        ·{" "}
                        <span>
                          {release.id === deployment.id && busy
                            ? t("In progress", "진행 중")
                            : release.id === active
                              ? t("Active", "사용 중")
                              : t("Archived", "보관됨")}
                        </span>
                        <span
                          className="service-image"
                          title={release.spec.image}
                        >
                          {release.spec.image}
                        </span>
                      </div>
                      {archived && release.digest && (
                        <button
                          className="btn"
                          aria-label={t(
                            "Roll back to this release",
                            "이 릴리스로 롤백",
                          )}
                          disabled={busy || submitting}
                          onClick={() => {
                            setSelectedRelease(release.id);
                            setTab("releases");
                          }}
                        >
                          ↶
                        </button>
                      )}
                    </li>
                  );
                })}
            </ul>
          </DetailSection>
        )}
        {!ownsResources && deployForm && (
          <form
            id="service-deploy-form"
            className="create-grid"
            onSubmit={submit}
          >
            {domain ? (
              <details className="service-full">
                <summary>{t("Advanced settings", "고급 설정")}</summary>
                <div className="create-grid">{networkField}</div>
              </details>
            ) : (
              <>
                {networkField}
                <div className="field">
                  <label htmlFor="service-deploy-port">
                    {t("First host port", "첫 번째 호스트 포트")}
                  </label>
                  <input
                    id="service-deploy-port"
                    type="number"
                    min={1024}
                    max={65536 - service.replicas}
                    value={port}
                    disabled={submitting || busy || ownsResources}
                    onChange={(event) => setPort(event.target.value)}
                  />
                </div>
              </>
            )}
            {!domain && !ownsResources && (
              <p className="poll-note">
                {port
                  ? t(
                      `Each replica uses one host port, starting at ${port}. App port: ${service.port}.`,
                      `복제본마다 ${port}번부터 호스트 포트를 하나씩 사용한다. 앱 포트는 ${service.port}번이다.`,
                    )
                  : t(
                      `Leave the host port empty for internal access. App port: ${service.port}.`,
                      "호스트 포트를 비우면 같은 MicroNetwork 안에서만 서비스에 연결한다.",
                    )}
              </p>
            )}
            {Object.keys(service.secrets).length > 0 && (
              <p className="poll-note">
                {t(
                  "Secret values are injected into every replica. Changes apply on the next deployment.",
                  "모든 복제본에 시크릿 값을 주입한다. 변경은 다음 배포에 반영한다.",
                )}
              </p>
            )}
            <div className="form-footer service-full">
              <button
                className="btn ghost"
                type="button"
                disabled={submitting}
                onClick={() => {
                  setDeployForm(false);
                  clearServiceDeployDraft();
                }}
              >
                {t("Cancel", "취소")}
              </button>
              <button
                className="btn primary"
                type="submit"
                disabled={
                  !loaded ||
                  !placement.ready ||
                  !!statusError ||
                  submitting ||
                  busy ||
                  ownsResources ||
                  groupPending
                }
              >
                {t("Deploy and run", "배포 및 실행")}
              </button>
            </div>
          </form>
        )}
        {!domain &&
          deployment?.internalRouteOwned &&
          ["running", "degraded"].includes(deployment.phase) && (
            <p>
              {t(
                "Available to apps in the same MicroNetwork.",
                "같은 MicroNetwork의 앱에서 사용할 수 있다.",
              )}{" "}
              <code>{`http://${deployment.spec.name}.internal`}</code>
            </p>
          )}

        {group && (
          <p className="poll-note">
            {t(
              `New replicas inherit security group "${group.name}". Its MicroNetwork is selected automatically.`,
              `새 복제 VM은 보안그룹 "${group.name}"을(를) 상속한다. 해당 MicroNetwork를 자동으로 선택한다.`,
            )}
          </p>
        )}
      </div>
      <div
        role="tabpanel"
        id="service-panel-instances"
        aria-labelledby="service-tab-instances"
        hidden={tab !== "instances"}
      >
        {!hasInstances && (
          <p>{t("No service instances.", "서비스 인스턴스가 없다.")}</p>
        )}
        {hasInstances && (
          <>
            <div className="table-scroll">
              <table className="vm-table service-table service-instances">
                <thead>
                  <tr>
                    <th id="service-instance-column-0" scope="col">
                      MicroVM
                    </th>
                    <th id="service-instance-column-1" scope="col">
                      {t("State", "상태")}
                    </th>
                    <th id="service-instance-column-2" scope="col">
                      {t("App health", "앱 상태")}
                    </th>
                    <th id="service-instance-column-3" scope="col">
                      {t("Endpoint", "접속 주소")}
                    </th>
                  </tr>
                </thead>
                <tbody>
                  {deployment!.instances.map((instance) => (
                    <tr key={instance.vmId}>
                      <td
                        headers="service-instance-column-0"
                        data-label="MicroVM"
                      >
                        <a
                          title={instance.vmId}
                          href={consoleHash(instance.vmId)}
                        >
                          {instance.vmId.slice(0, 8)}
                        </a>
                      </td>
                      <td
                        headers="service-instance-column-1"
                        data-label={t("State", "상태")}
                      >
                        {instance.state}
                      </td>
                      <td
                        headers="service-instance-column-2"
                        title={
                          instance.healthy
                            ? t("Healthy", "정상")
                            : t("Not healthy", "정상 응답 없음")
                        }
                        data-label={t("App health", "앱 상태")}
                      >
                        {instance.healthy
                          ? t("Healthy", "정상")
                          : t("Not healthy", "정상 응답 없음")}
                      </td>
                      <td
                        headers="service-instance-column-3"
                        data-label={t("Endpoint", "접속 주소")}
                      >
                        {instance.hostPort > 0 ? (
                          <a
                            href={endpoint(instance.hostPort)}
                            target="_blank"
                            rel="noreferrer"
                          >
                            :{instance.hostPort}
                          </a>
                        ) : (
                          <span>
                            {domain
                              ? t("Via domain", "도메인으로 접속")
                              : `${instance.ipv4}:${deployment.spec.port}`}
                          </span>
                        )}
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          </>
        )}
        <ServiceObservability
          key={service.id}
          serviceId={service.id}
          active={tab === "instances"}
        />
      </div>
      <div
        role="tabpanel"
        id="service-panel-releases"
        aria-labelledby="service-tab-releases"
        hidden={tab !== "releases"}
      >
        {deployment && (
          <ServiceReleases
            service={service}
            deployment={deployment}
            onChanged={deploymentChanged}
            onList={setReleases}
            onUpdate={openUpdate}
            selectedRelease={selectedRelease}
          />
        )}
        {!deployment && (
          <p>
            {t(
              "Deploy the service to start its release history.",
              "서비스를 배포하면 릴리스 기록이 표시된다.",
            )}
          </p>
        )}
      </div>
      <div
        role="tabpanel"
        id="service-panel-automation"
        aria-labelledby="service-tab-automation"
        hidden={tab !== "automation"}
      >
        <button className="btn" disabled={busy} onClick={onEdit}>
          {t("Edit configuration", "설정 수정")}
        </button>
        {deployment?.spec.domain ? (
          <ServiceAutomation
            serviceId={service.id}
            domain={deployment.spec.domain}
            disabled={busy}
          />
        ) : (
          <p>
            {t(
              "Automation requires a deployed service with a domain.",
              "자동화는 도메인을 사용하는 서비스 배포가 필요하다.",
            )}
          </p>
        )}
      </div>
    </section>
  );
}

function endpoint(port: number): string {
  const url = new URL(window.location.origin);
  url.protocol = "http:";
  url.port = String(port);
  url.pathname = "/";
  return url.toString();
}
