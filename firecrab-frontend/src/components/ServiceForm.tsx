import { useEffect, useRef, useState, type FormEvent } from "react";
import type {
  MicroNetworkResponse,
  MicroServiceResponse,
  MicroServiceSpec,
  SecurityGroupResponse,
} from "../bindings";
import {
  ApiClientError,
  fetchJson,
  createMicroService,
  updateMicroService,
  listMicroNetworks,
  listSecurityGroups,
} from "../api/client";
import { useI18n } from "../i18n";
import { listStorage, type StorageConnection } from "../api/pipeStorage";
import { getPipeAuth } from "../api/pipeAuth";
import ServiceAppPicker, { hasServiceImageAuth } from "./ServiceAppPicker";
const DEFAULT_SPEC: MicroServiceSpec = {
  name: "",
  image: "",
  runtime: "vm",
  port: 8080,
  healthPath: "/health",
  domain: null,
  ram: 512,
  cpu: 1,
  replicas: 1,
  startupTimeout: 300,
  drainTimeout: 30,
  env: {},
  secrets: {},
  securityGroupId: null,
};

type Pair = { key: string; value: string };

export default function ServiceForm({
  service,
  onCancel,
  onSaved,
  onUnsavedChange,
  onDeploy,
}: {
  service: MicroServiceResponse | null;
  onCancel: () => void;
  onSaved: (saved?: MicroServiceResponse) => Promise<void>;
  onUnsavedChange: (dirty: boolean) => void;
  onDeploy?: (
    spec: MicroServiceSpec,
    strategy: "rolling" | "blue_green",
  ) => Promise<void>;
}) {
  const { t } = useI18n();
  const [step, setStep] = useState(0),
    [strategy, setStrategy] = useState<"rolling" | "blue_green">("rolling");
  const [imageAuthReady, setImageAuthReady] = useState(false),
    [manualRevision, setManualRevision] = useState(0);
  const formRef = useRef<HTMLFormElement>(null);
  const steps = [
    t("App", "앱 선택"),
    t("Access", "접속 방법"),
    t("Size", "실행 크기"),
    t("Connections", "연결"),
    t("Review", "확인"),
  ];
  const [spec, setSpec] = useState<MicroServiceSpec>(service ?? DEFAULT_SPEC);
  const [env, setEnv] = useState<Pair[]>(
    Object.entries(spec.env).map(([key, value]) => ({ key, value })),
  );
  const [secrets, setSecrets] = useState<Pair[]>(
    Object.entries(spec.secrets).map(([key, value]) => ({ key, value })),
  );
  const [queueRows, setQueueRows] = useState(() =>
    Object.entries(spec.queues || {}).map(([key, value]) => ({
      key,
      ...value,
    })),
  );
  const snapshot = JSON.stringify({
    spec: service ? spec : { ...spec, image: "" },
    env,
    secrets,
    queueRows,
  });
  const [original] = useState(snapshot);
  const dirty = snapshot !== original;
  useEffect(() => {
    onUnsavedChange(dirty);
    return () => onUnsavedChange(false);
  }, [dirty, onUnsavedChange]);
  const [queueOptions, setQueueOptions] = useState<
    { id: string; name: string; phase: string }[]
  >([]);
  const [queueError, setQueueError] = useState("");
  useEffect(() => {
    let active = true;
    fetchJson<{ id: string; name: string; phase: string }[]>("/api/queues")
      .then((rows) => {
        if (active) setQueueOptions(rows);
      })
      .catch((e) => {
        if (active) setQueueError(e instanceof Error ? e.message : String(e));
      });
    return () => {
      active = false;
    };
  }, []);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [fields, setFields] = useState<Record<string, string>>({});
  const [storageConnections, setStorageConnections] = useState<
    StorageConnection[]
  >([]);
  const [storageError, setStorageError] = useState<string | null>(null);
  useEffect(() => {
    const controller = new AbortController();
    void listStorage(controller.signal)
      .then((rows) => {
        if (!controller.signal.aborted) setStorageConnections(rows);
      })
      .catch((failure) => {
        if (!controller.signal.aborted)
          setStorageError((failure as Error).message);
      });
    return () => controller.abort();
  }, []);
  const [groups, setGroups] = useState<SecurityGroupResponse[] | null>(null);
  const [networks, setNetworks] = useState<MicroNetworkResponse[]>([]);
  const [groupError, setGroupError] = useState<string | null>(null);

  useEffect(() => {
    let active = true;
    Promise.all([listSecurityGroups(), listMicroNetworks()])
      .then(([rows, nets]) => {
        if (active) {
          setGroups(rows);
          setNetworks(nets);
        }
      })
      .catch((failure) => {
        if (active) setGroupError((failure as Error).message);
      });
    return () => {
      active = false;
    };
  }, []);

  const reloadGroups = async () => {
    setGroupError(null);
    try {
      const [rows, nets] = await Promise.all([
        listSecurityGroups(),
        listMicroNetworks(),
      ]);
      setGroups(rows);
      setNetworks(nets);
    } catch (failure) {
      setGroupError((failure as Error).message);
    }
  };

  const change = <K extends keyof MicroServiceSpec>(
    key: K,
    value: MicroServiceSpec[K],
  ) => setSpec((previous) => ({ ...previous, [key]: value }));
  const fieldError = (key: string) =>
    fields[key] && (
      <span className="field-error" role="alert">
        {fields[key]}
      </span>
    );

  const nextStep = async () => {
    if (saving) return;
    const controls = formRef.current?.querySelectorAll<
      HTMLInputElement | HTMLSelectElement | HTMLTextAreaElement
    >(
      `[data-service-step="${step}"] input,[data-service-step="${step}"] select,[data-service-step="${step}"] textarea`,
    );
    if (controls && [...controls].some((control) => !control.reportValidity()))
      return;
    setError(null);
    if (step === 0) {
      setSaving(true);
      try {
        if (!hasServiceImageAuth(spec.image, await getPipeAuth())) {
          setError(
            t(
              "This image must belong to an account connected in Pipe Auth.",
              "Pipe Auth에 연결한 계정의 이미지를 선택한다.",
            ),
          );
          return;
        }
      } catch (cause) {
        setError(String(cause));
        return;
      } finally {
        setSaving(false);
      }
    }
    setStep((value) => Math.min(4, value + 1));
  };
  const submit = async (event: FormEvent) => {
    event.preventDefault();
    if (saving || groups === null || groupError) return;
    if (step < 4) {
      await nextStep();
      return;
    }
    setError(null);
    setFields({});
    const maps: Record<string, Record<string, string>> = {};
    for (const [field, rows] of [
      ["env", env],
      ["secrets", secrets],
    ] as const) {
      const map: Record<string, string> = Object.create(null);
      for (const row of rows) {
        const key = row.key.trim();
        if (!key || Object.hasOwn(map, key)) {
          setFields({
            [field]: t(
              "Each row needs a unique variable name.",
              "각 행에 서로 다른 변수 이름이 필요하다.",
            ),
          });
          return;
        }
        map[key] = row.value;
      }
      maps[field] = map;
    }
    const queues: NonNullable<MicroServiceSpec["queues"]> = Object.create(null);
    for (const row of queueRows) {
      const key = row.key.trim();
      if (!key || !row.queueId || Object.hasOwn(queues, key)) {
        setFields({
          queues: t(
            "Each queue needs a unique variable and queue.",
            "각 큐에 서로 다른 변수 이름과 큐가 필요하다.",
          ),
        });
        return;
      }
      queues[key] = { queueId: row.queueId, access: row.access };
    }
    const request: MicroServiceSpec = {
      name: spec.name.trim(),
      image: spec.image.trim(),
      runtime: spec.runtime,
      port: spec.port,
      healthPath: spec.healthPath,
      domain: normalizedDomain(spec.domain),
      ram: spec.ram,
      cpu: spec.cpu,
      replicas: spec.replicas,
      startupTimeout: spec.startupTimeout,
      drainTimeout: spec.drainTimeout,
      env: maps.env!,
      secrets: maps.secrets!,
      queues,
      ...(spec.storageConnectionId
        ? { storageConnectionId: spec.storageConnectionId }
        : {}),
      securityGroupId: spec.securityGroupId,
      ...(spec.autoDeploy !== undefined ? { autoDeploy: spec.autoDeploy } : {}),
    };
    setSaving(true);
    try {
      if (!hasServiceImageAuth(request.image, await getPipeAuth())) {
        setStep(0);
        setError(
          t(
            "This AUTH account is no longer connected. Select a connected account again.",
            "AUTH 계정 연결이 해제되었다. 연결된 계정을 다시 선택한다.",
          ),
        );
        return;
      }
      let saved: MicroServiceResponse | undefined;
      if (onDeploy) await onDeploy(request, strategy);
      else if (service) saved = await updateMicroService(service.id, request);
      else saved = await createMicroService(request);
      await onSaved(saved);
    } catch (failure) {
      setError((failure as Error).message);
      if (failure instanceof ApiClientError) {
        setFields(failure.apiError?.fields ?? {});
        const key = Object.keys(failure.apiError?.fields ?? {})[0];
        if (key)
          setStep(
            ["name", "image"].includes(key)
              ? 0
              : ["port", "healthPath", "domain", "securityGroupId"].includes(
                    key,
                  )
                ? 1
                : [
                      "ram",
                      "cpu",
                      "replicas",
                      "startupTimeout",
                      "drainTimeout",
                    ].includes(key)
                  ? 2
                  : [
                        "env",
                        "secrets",
                        "queues",
                        "storageConnectionId",
                      ].includes(key)
                    ? 3
                    : 4,
          );
      }
    } finally {
      setSaving(false);
    }
  };

  return (
    <section
      className="panel service-form-editor"
      aria-labelledby="service-editor-heading"
      data-editing
    >
      <h2 className="panel-title" id="service-editor-heading">
        {onDeploy
          ? t("Deploy update", "업데이트 배포")
          : service
            ? t("Edit MicroService", "MicroService 편집")
            : t("New MicroService", "새 MicroService")}
      </h2>
      {error && (
        <p className="field-error" role="alert">
          {error}
        </p>
      )}
      {groupError && (
        <div>
          <p className="field-error" role="alert">
            {groupError}
          </p>
          <button type="button" className="btn" onClick={reloadGroups}>
            {t("Retry loading security groups", "보안그룹 다시 불러오기")}
          </button>
        </div>
      )}
      <form
        id="editor"
        ref={formRef}
        className="service-form service-wizard"
        noValidate
        onSubmit={submit}
      >
        <nav
          id="service-steps"
          data-step={step}
          aria-label={t("Setup steps", "설정 단계")}
        >
          {steps.map((label, index) => (
            <button
              key={label}
              type="button"
              className="btn"
              data-action="service-step"
              data-id={index}
              aria-current={step === index ? "step" : undefined}
              disabled={saving || index > step}
              onClick={() => setStep(index)}
            >
              {index + 1}. {label}
            </button>
          ))}
        </nav>
        <div
          className="create-grid service-step-panel"
          data-service-step="0"
          hidden={step !== 0}
        >
          <ServiceAppPicker
            manualRevision={manualRevision}
            onAuthReady={setImageAuthReady}
            image={spec.image}
            onChange={(image) => change("image", image)}
            disabled={saving}
          />
          <div
            id="service-wizard-error"
            role="status"
            className="service-full"
          />
          <div className="field">
            <label htmlFor="service-name">{t("Name", "이름")}</label>
            <input
              id="service-name"
              readOnly={!!onDeploy}
              value={spec.name}
              onChange={(event) => change("name", event.target.value)}
              placeholder="orders"
              required
              maxLength={63}
              pattern={"[a-z0-9]([a-z0-9\\-]*[a-z0-9])?"}
              disabled={saving}
            />
            {fieldError("name")}
          </div>
          <div className="field service-wide">
            <label htmlFor="service-image">
              {t("OCI image", "OCI 이미지")}
            </label>
            <input
              id="service-image"
              value={spec.image}
              onChange={(event) => {
                setManualRevision((value) => value + 1);
                change("image", event.target.value);
              }}
              placeholder="ghcr.io/team/orders:1.0"
              required
              maxLength={2048}
              disabled={saving}
            />
            {fieldError("image")}
          </div>
        </div>
        <div
          className="create-grid service-step-panel"
          data-service-step="1"
          hidden={step !== 1}
        >
          <div className="field">
            <label htmlFor="service-runtime">{t("Runtime", "실행 환경")}</label>
            <select id="service-runtime" defaultValue="vm" disabled>
              <option value="vm">MicroVM</option>
            </select>
            {fieldError("runtime")}
          </div>
          <div className="field service-full">
            <label htmlFor="service-security-group">
              {t("Security group", "보안그룹")}
            </label>
            <select
              id="service-security-group"
              value={spec.securityGroupId ?? ""}
              disabled={saving || groups === null || !!groupError || !!onDeploy}
              onChange={(event) =>
                change("securityGroupId", event.target.value || null)
              }
              aria-describedby="service-security-group-note"
            >
              <option value="">
                {groups === null
                  ? t("Loading…", "불러오는 중…")
                  : t(
                      "None — use network defaults",
                      "없음 — 네트워크 기본 설정 사용",
                    )}
              </option>
              {spec.securityGroupId &&
                !groups?.some((group) => group.id === spec.securityGroupId) && (
                  <option value={spec.securityGroupId}>
                    {t("Unavailable group", "그룹을 확인할 수 없다")} ·{" "}
                    {spec.securityGroupId}
                  </option>
                )}
              {groups?.map((group) => (
                <option key={group.id} value={group.id}>
                  {group.name} ·{" "}
                  {networks.find(
                    (network) => network.id === group.microNetworkId,
                  )?.name ?? group.microNetworkId}
                </option>
              ))}
            </select>
            {fieldError("securityGroupId")}
            <p className="poll-note" id="service-security-group-note">
              {t(
                "New replica VMs inherit the selected group and use its MicroNetwork. Changes apply on the next deployment.",
                "새 복제 VM은 선택한 그룹을 상속하고 해당 MicroNetwork를 사용한다. 변경은 다음 배포에 적용한다.",
              )}
            </p>
            {groups?.length === 0 && (
              <p className="poll-note">
                {t(
                  "Create a group from Security groups in the network menu.",
                  "네트워크 메뉴의 보안그룹 화면에서 그룹을 생성한다.",
                )}
              </p>
            )}
          </div>
          <div className="field">
            <label htmlFor="service-port">{t("App port", "앱 포트")}</label>
            <input
              id="service-port"
              type="number"
              min={1}
              max={65535}
              value={spec.port}
              onChange={(event) => change("port", event.target.valueAsNumber)}
              required
              disabled={saving}
            />
            {fieldError("port")}
          </div>
          <div className="field">
            <label htmlFor="service-health">
              {t("Health path", "상태 검사 경로")}
            </label>
            <input
              id="service-health"
              value={spec.healthPath}
              onChange={(event) => change("healthPath", event.target.value)}
              placeholder="/actuator/health"
              required
              disabled={saving}
            />
            {fieldError("healthPath")}
          </div>
          <div className="field service-wide">
            <label htmlFor="service-domain">
              {t("Domain (optional)", "도메인 (선택)")}
            </label>
            <input
              id="service-domain"
              readOnly={!!onDeploy}
              aria-describedby="service-domain-help"
              autoCapitalize="none"
              autoCorrect="off"
              spellCheck={false}
              value={spec.domain ?? ""}
              onChange={(event) => change("domain", event.target.value)}
              placeholder="orders.example.com"
              maxLength={253}
              disabled={saving}
            />
            <p id="service-domain-help" className="poll-note">
              {t(
                "Enter your domain. Healthy replicas connect automatically; no host port or manual route is needed. Public access uses Pipe Tunnel and your DNS.",
                "도메인을 입력하면 정상 인스턴스를 자동 연결한다. 호스트 포트나 경로를 따로 설정할 필요가 없다. 외부 접속에는 Pipe Tunnel과 DNS 연결이 필요하다.",
              )}
            </p>
            {fieldError("domain")}
          </div>
        </div>
        <div
          className="create-grid service-step-panel"
          data-service-step="2"
          hidden={step !== 2}
        >
          <div className="field">
            <label htmlFor="service-ram">
              {t("Memory (MiB)", "메모리 (MiB)")}
            </label>
            <input
              id="service-ram"
              type="number"
              min={128}
              max={65536}
              step={1}
              list="service-memory-presets"
              value={spec.ram}
              onChange={(event) => change("ram", event.target.valueAsNumber)}
              required
              disabled={saving}
            />
            <datalist id="service-memory-presets">
              {[128, 256, 512, 1024, 2048, 4096, 8192, 16384, 32768].map(
                (size) => (
                  <option value={size} key={size} />
                ),
              )}
            </datalist>
            {fieldError("ram")}
          </div>
          <div className="field">
            <label htmlFor="service-cpu">CPU</label>
            <input
              id="service-cpu"
              type="number"
              min={1}
              max={32}
              value={spec.cpu}
              onChange={(event) => change("cpu", event.target.valueAsNumber)}
              required
              disabled={saving}
            />
            {fieldError("cpu")}
          </div>
          <div className="field">
            <label htmlFor="service-replicas">{t("Replicas", "복제 수")}</label>
            <input
              id="service-replicas"
              type="number"
              min={1}
              max={100}
              value={spec.replicas}
              onChange={(event) =>
                change("replicas", event.target.valueAsNumber)
              }
              required
              disabled={saving}
            />
            {fieldError("replicas")}
          </div>
          <div className="field">
            <label htmlFor="service-startup">
              {t("Startup timeout (s)", "시작 제한 시간 (초)")}
            </label>
            <input
              id="service-startup"
              type="number"
              min={1}
              max={3600}
              value={spec.startupTimeout}
              onChange={(event) =>
                change("startupTimeout", event.target.valueAsNumber)
              }
              required
              disabled={saving}
            />
            {fieldError("startupTimeout")}
          </div>
          <div className="field">
            <label htmlFor="service-drain">
              {t("Drain timeout (s)", "요청 종료 대기 (초)")}
            </label>
            <input
              id="service-drain"
              type="number"
              min={0}
              max={3600}
              value={spec.drainTimeout}
              onChange={(event) =>
                change("drainTimeout", event.target.valueAsNumber)
              }
              required
              disabled={saving}
            />
            {fieldError("drainTimeout")}
          </div>
        </div>
        <div
          className="create-grid service-step-panel"
          data-service-step="3"
          hidden={step !== 3}
        >
          <VariableRows
            kind="env"
            rows={env}
            setRows={setEnv}
            disabled={saving}
          />
          <div className="field service-full">
            <label htmlFor="service-storage">
              {t("S3 connection (optional)", "S3 연결 (선택)")}
            </label>
            <select
              id="service-storage"
              disabled={saving}
              value={spec.storageConnectionId ?? ""}
              onChange={(event) =>
                change("storageConnectionId", event.target.value || null)
              }
            >
              <option value="">{t("No S3 connection", "S3 연결 없음")}</option>
              {!!spec.storageConnectionId &&
                !storageConnections.some(
                  (row) => row.id === spec.storageConnectionId,
                ) && (
                  <option value={spec.storageConnectionId}>
                    {spec.storageConnectionId}
                  </option>
                )}
              {storageConnections.map((connection) => (
                <option key={connection.id} value={connection.id}>
                  {connection.name} · {connection.bucket}/{connection.prefix}
                </option>
              ))}
            </select>
            {storageError && (
              <span role="alert" className="field-error">
                {storageError}
              </span>
            )}
            <small>
              {t(
                "AWS SDK credentials are provided at boot. Use FIRECRAB_S3_BUCKET and FIRECRAB_S3_PREFIX in the app. Restrict access with AWS IAM.",
                "부팅할 때 AWS SDK 자격 증명을 전달한다. 앱에서 FIRECRAB_S3_BUCKET과 FIRECRAB_S3_PREFIX를 사용한다. AWS IAM으로 접근 권한을 제한한다.",
              )}
            </small>
          </div>
          <div className="service-full">{fieldError("env")}</div>
          <VariableRows
            kind="secrets"
            rows={secrets}
            setRows={setSecrets}
            disabled={saving}
          />
          <div className="service-full">{fieldError("secrets")}</div>
          <fieldset
            className="service-variables service-full"
            disabled={saving}
          >
            <legend>{t("Queue connections", "큐 연결")}</legend>
            {queueError && (
              <p className="field-error" role="alert">
                {queueError}
              </p>
            )}
            {queueRows.map((row, index) => (
              <div className="service-variable-row" key={index}>
                <label>
                  {t("Variable", "변수")}{" "}
                  <input
                    required
                    value={row.key}
                    onChange={(e) =>
                      setQueueRows((rows) =>
                        rows.map((item, i) =>
                          i === index ? { ...item, key: e.target.value } : item,
                        ),
                      )
                    }
                  />
                </label>
                <label>
                  Queue{" "}
                  <select
                    required
                    value={row.queueId}
                    onChange={(e) =>
                      setQueueRows((rows) =>
                        rows.map((item, i) =>
                          i === index
                            ? { ...item, queueId: e.target.value }
                            : item,
                        ),
                      )
                    }
                  >
                    <option value="">{t("Select queue", "큐 선택")}</option>
                    {row.queueId &&
                      !queueOptions.some((q) => q.id === row.queueId) && (
                        <option value={row.queueId}>{row.queueId}</option>
                      )}
                    {queueOptions.map((q) => (
                      <option key={q.id} value={q.id}>
                        {q.name}
                      </option>
                    ))}
                  </select>
                </label>
                <label>
                  {t("Access", "접근 권한")}{" "}
                  <select
                    value={row.access}
                    onChange={(e) =>
                      setQueueRows((rows) =>
                        rows.map((item, i) =>
                          i === index
                            ? {
                                ...item,
                                access: e.target.value as "publish" | "consume",
                              }
                            : item,
                        ),
                      )
                    }
                  >
                    <option value="publish">{t("Send", "전송")}</option>
                    <option value="consume">{t("Receive", "수신")}</option>
                  </select>
                </label>
                <button
                  type="button"
                  className="btn"
                  onClick={() =>
                    setQueueRows((rows) => rows.filter((_, i) => i !== index))
                  }
                >
                  {t("Remove", "삭제")}
                </button>
              </div>
            ))}
            <button
              type="button"
              className="btn"
              disabled={!!queueError}
              onClick={() =>
                setQueueRows((rows) => [
                  ...rows,
                  { key: "", queueId: "", access: "publish" },
                ])
              }
            >
              {t("Add queue", "큐 추가")}
            </button>
            {fieldError("queues")}
          </fieldset>
        </div>
        <section
          id="service-review"
          className="service-step-panel"
          hidden={step !== 4}
          aria-label={t("Configuration review", "설정 확인")}
        >
          <h3>{t("Review configuration", "설정 확인")}</h3>
          {onDeploy && (
            <fieldset
              className="network-choice"
              role="radiogroup"
              aria-label={t("Deployment strategy", "배포 전략")}
              disabled={saving}
            >
              <legend>{t("Deployment strategy", "배포 전략")}</legend>
              {[
                {
                  value: "rolling" as const,
                  label: t(
                    "Rolling — one spare replica",
                    "롤링 — 여유 복제본 1개",
                  ),
                },
                {
                  value: "blue_green" as const,
                  label: t(
                    "Blue-green — a full extra generation",
                    "블루그린 — 새 복제본 세트",
                  ),
                },
              ].map((option) => (
                <label key={option.value}>
                  <input
                    type="radio"
                    name="service-update-strategy"
                    checked={strategy === option.value}
                    onChange={() => setStrategy(option.value)}
                  />
                  <span>{option.label}</span>
                </label>
              ))}
            </fieldset>
          )}

          <dl className="props">
            <dt>{t("Name", "이름")}</dt>
            <dd>{spec.name}</dd>
            <dt>{t("Image", "이미지")}</dt>
            <dd className="mono">{spec.image}</dd>
            <dt>{t("App port", "앱 포트")}</dt>
            <dd>
              {spec.port} · {spec.healthPath}
            </dd>
            <dt>{t("Access", "접근")}</dt>
            <dd>{spec.domain || t("Internal network", "내부 네트워크")}</dd>
            <dt>{t("Size", "크기")}</dt>
            <dd>
              {spec.cpu} vCPU · {spec.ram / 1024} GiB · {spec.replicas}{" "}
              {t("replicas", "복제본")}
            </dd>
            <dt>{t("Connections", "연결")}</dt>
            <dd>
              {queueRows.length} Queue · {secrets.length}{" "}
              {t("secret references", "시크릿 참조")}
              {spec.storageConnectionId ? " · S3" : ""}
            </dd>
          </dl>
        </section>
        <div className="form-footer">
          <button
            className="btn ghost"
            type="button"
            disabled={saving}
            onClick={onCancel}
          >
            {t("Cancel", "취소")}
          </button>
          <div className="form-footer-actions">
            <button
              className="btn"
              type="button"
              disabled={saving || step === 0}
              onClick={() => setStep((value) => value - 1)}
            >
              {t("Back", "이전")}
            </button>
            {step < 4 ? (
              <button
                key="next"
                className="btn primary"
                type="button"
                disabled={
                  saving ||
                  groups === null ||
                  !!groupError ||
                  (step === 0 && !imageAuthReady)
                }
                onClick={(event) => {
                  event.preventDefault();
                  void nextStep();
                }}
              >
                {t("Next", "다음")}
              </button>
            ) : (
              <button
                key="save"
                type="submit"
                className="btn primary"
                disabled={saving || groups === null || !!groupError}
              >
                {saving
                  ? t("Saving…", "저장 중…")
                  : onDeploy
                    ? t("Deploy update", "업데이트 배포")
                    : t("Save configuration", "설정 저장")}
              </button>
            )}
          </div>
        </div>
      </form>
    </section>
  );
}

function VariableRows({
  kind,
  rows,
  setRows,
  disabled,
}: {
  kind: "env" | "secrets";
  rows: Pair[];
  setRows: (rows: Pair[]) => void;
  disabled: boolean;
}) {
  const { t } = useI18n();
  const secret = kind === "secrets";
  const bulkField = useRef<HTMLTextAreaElement>(null);
  const [bulk, setBulk] = useState(false),
    [raw, setRaw] = useState("");
  const serialized = rows.map((row) => `${row.key}=${row.value}`).join("\n");
  const bulkLabel = secret
    ? t(
        "Secret references (NAME=PipeSecrets ID)",
        "시크릿 참조 (이름=PipeSecrets ID)",
      )
    : t("Environment (NAME=value)", "환경 변수 (이름=값)");
  return (
    <fieldset className="service-variables service-full" disabled={disabled}>
      <legend>
        {secret
          ? t("Secret references", "시크릿 참조")
          : t("Environment variables", "환경 변수")}
      </legend>
      {secret && (
        <p className="poll-note">
          {t(
            "Enter a PipeSecrets ID for each variable. Values are injected into every replica at deployment. Secret changes apply on the next deployment.",
            "변수마다 PipeSecrets ID를 입력한다. 배포할 때 모든 복제본에 값을 주입한다. 시크릿 변경은 다음 배포에 반영한다.",
          )}
        </p>
      )}
      <button
        className="btn"
        type="button"
        onClick={() => {
          if (!bulk) {
            setRaw(serialized);
            bulkField.current?.setCustomValidity(
              rows.some((row) => !row.key.trim())
                ? t(
                    "Use one NAME=value entry per line.",
                    "한 줄에 이름=값을 입력한다.",
                  )
                : "",
            );
          }
          setBulk((value) => !value);
        }}
      >
        {bulk
          ? t("Edit as rows", "행으로 편집")
          : secret
            ? t("Bulk edit secret references", "시크릿 참조 일괄 편집")
            : t("Bulk edit environment", "환경 변수 일괄 편집")}
      </button>
      <textarea
        ref={bulkField}
        disabled={!bulk || disabled}
        hidden={!bulk}
        aria-label={bulkLabel}
        value={bulk ? raw : serialized}
        rows={6}
        spellCheck={false}
        onChange={(event) => {
          const text = event.target.value;
          setRaw(text);
          const lines = text.split("\n").filter((line) => line.trim());
          event.currentTarget.setCustomValidity(
            lines.some((line) => line.indexOf("=") < 1)
              ? t(
                  "Use one NAME=value entry per line.",
                  "한 줄에 이름=값을 입력한다.",
                )
              : "",
          );
          setRows(
            lines.map((line) => {
              const split = line.indexOf("=");
              return {
                key: split < 0 ? line : line.slice(0, split).trim(),
                value: split < 0 ? "" : line.slice(split + 1),
              };
            }),
          );
        }}
      />
      <fieldset
        className="service-variable-fields"
        disabled={bulk || disabled}
        hidden={bulk}
      >
        {rows.map((row, index) => (
          <div className="service-variable-row" key={index}>
            <div className="field">
              <label htmlFor={`service-${kind}-key-${index}`}>
                {t("Variable", "변수")}
              </label>
              <input
                id={`service-${kind}-key-${index}`}
                value={row.key}
                maxLength={256}
                autoComplete="off"
                onChange={(event) =>
                  setRows(
                    rows.map((pair, i) =>
                      i === index ? { ...pair, key: event.target.value } : pair,
                    ),
                  )
                }
                required
              />
            </div>
            <div className="field">
              <label htmlFor={`service-${kind}-value-${index}`}>
                {secret ? "PipeSecrets ID" : t("Value", "값")}
              </label>
              <input
                id={`service-${kind}-value-${index}`}
                value={row.value}
                maxLength={secret ? 36 : 4096}
                autoComplete="off"
                onChange={(event) =>
                  setRows(
                    rows.map((pair, i) =>
                      i === index
                        ? { ...pair, value: event.target.value }
                        : pair,
                    ),
                  )
                }
                required={secret}
              />
            </div>
            <button
              type="button"
              className="btn danger"
              aria-label={t(
                `Remove ${kind} row ${index + 1}`,
                `${kind} ${index + 1}행 삭제`,
              )}
              onClick={() => setRows(rows.filter((_, i) => i !== index))}
            >
              {t("Remove", "삭제")}
            </button>
          </div>
        ))}
        <button
          type="button"
          className="btn"
          disabled={rows.length >= 64}
          onClick={() => setRows([...rows, { key: "", value: "" }])}
        >
          {secret
            ? t("Add secret reference", "시크릿 참조 추가")
            : t("Add variable", "변수 추가")}
        </button>
      </fieldset>
    </fieldset>
  );
}

/** Accept a pasted site address without silently accepting a path or query. */
function normalizedDomain(value: string | null): string | null {
  const input = value?.trim().toLowerCase();
  if (!input) return null;
  if (/^https?:\/\//.test(input)) {
    try {
      const url = new URL(input);
      if (
        url.pathname === "/" &&
        !url.search &&
        !url.hash &&
        !url.port &&
        !url.username &&
        !url.password
      )
        return url.hostname.replace(/\.$/, "");
    } catch {
      /* Keep invalid input for the API's domain field error. */
    }
  }
  return input.replace(/\.$/, "");
}
