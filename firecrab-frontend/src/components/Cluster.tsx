import { useCallback, useEffect, useState, type FormEvent } from "react";
import { ApiClientError, fetchJson } from "../api/client";
import { useI18n } from "../i18n";
import Screen from "./layout/Screen";

type Network = { id: string; name: string };
type Storage = { id: string };
type Image = { alias: string; version: string; rootfsSha256: string };
type Host = {
  online: boolean;
  draining: boolean;
  reserved: { cpu: number; ramMib: number };
  report: {
    id: string;
    name: string;
    capacity: { cpu: number; ramMib: number };
    networks: Network[];
    storages: Storage[];
    images: Image[];
  };
};
type Placement = {
  id: string;
  phase: string;
  error?: string;
  spec: { name: string };
  vm?: { hostId: string };
  observed?: { state: string };
};
type Service = {
  id: string;
  generation: number;
  releaseGeneration: number;
  desired: string;
  observed: string;
  pending: number;
  degraded: number;
  spec: {
    service: {
      name: string;
      image: string;
      port: number;
      healthPath: string;
      cpu: number;
      ram: number;
      env: Record<string, string>;
    };
    state: object;
    credentials: object;
    targets: Target[];
  };
};
type Target = {
  hostId: string;
  replicas: number;
  request: { microNetworkId: string; hostPort: number };
};
type Pending = { path: string; body: object; key: string };
type Choice = {
  enabled: boolean;
  network: string;
  storage: string;
  replicas: number;
  port: number;
};
const placementKey = "firecrab.cluster.placement.v1";
const serviceKey = "firecrab.cluster.service.v1";
function readPending(key: string): Pending | null {
  try {
    const value = JSON.parse(sessionStorage.getItem(key) || "null");
    if (!value || typeof value.key !== "string" || !value.key) return null;
    if (key === placementKey && value.spec && typeof value.spec === "object")
      return {
        path: "/api/cluster/placements",
        body: value.spec,
        key: value.key,
      };
    if (
      typeof value.path !== "string" ||
      !value.body ||
      typeof value.body !== "object"
    )
      return null;
    if (key === placementKey && value.path !== "/api/cluster/placements")
      return null;
    if (
      key === serviceKey &&
      !/^\/api\/cluster\/services(?:\/[^/]+\/(?:stop|update))?$/.test(
        value.path,
      )
    )
      return null;
    return value;
  } catch {
    return null;
  }
}
function object(text: string): object {
  const value: unknown = JSON.parse(text);
  if (!value || typeof value !== "object" || Array.isArray(value))
    throw Error("Expected a JSON object");
  return value;
}
function randomKey() {
  return Array.from(crypto.getRandomValues(new Uint8Array(16)), (n) =>
    n.toString(16).padStart(2, "0"),
  ).join("");
}
function path(id: string) {
  return encodeURIComponent(id);
}

export default function Cluster() {
  const { t } = useI18n();
  const [hosts, setHosts] = useState<Host[] | null>(null),
    [placements, setPlacements] = useState<Placement[]>([]),
    [services, setServices] = useState<Service[]>([]);
  const [error, setError] = useState(""),
    [busy, setBusy] = useState(false),
    [tab, setTab] = useState<"hosts" | "placements" | "services">("hosts");
  const [pendingPlacement, setPendingPlacement] = useState(() =>
      readPending(placementKey),
    ),
    [pendingService, setPendingService] = useState(() =>
      readPending(serviceKey),
    );
  const [name, setName] = useState(""),
    [image, setImage] = useState(""),
    [cpu, setCpu] = useState(1),
    [ram, setRam] = useState(512),
    [disk, setDisk] = useState(2);
  const [choices, setChoices] = useState<Record<string, Choice>>({}),
    [serviceChoices, setServiceChoices] = useState<Record<string, Choice>>({});
  const [serviceName, setServiceName] = useState(""),
    [serviceImage, setServiceImage] = useState(""),
    [servicePort, setServicePort] = useState(8080),
    [health, setHealth] = useState("/health"),
    [serviceCpu, setServiceCpu] = useState(1),
    [serviceRam, setServiceRam] = useState(512);
  const [state, setState] = useState('{"mode":"stateless_http"}'),
    [credentials, setCredentials] = useState("{}"),
    [env, setEnv] = useState("{}");
  const [editing, setEditing] = useState<Service | null>(null),
    [history, setHistory] = useState<Record<string, unknown>>({});
  const refresh = useCallback(async () => {
    try {
      const [a, b, c] = await Promise.all([
        fetchJson<Host[]>("/api/cluster/hosts"),
        fetchJson<Placement[]>("/api/cluster/placements"),
        fetchJson<Service[]>("/api/cluster/services"),
      ]);
      setHosts(a);
      setPlacements(b);
      setServices(c);
      setError("");
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, []);
  useEffect(() => {
    let live = true;
    const tick = async () => {
      if (live) await refresh();
    };
    void tick();
    const timer = window.setInterval(() => void tick(), 5000);
    return () => {
      live = false;
      window.clearInterval(timer);
    };
  }, [refresh]);
  const choice = (h: Host): Choice =>
    choices[h.report.id] || {
      enabled:
        h.online &&
        !h.draining &&
        h.report.networks.length > 0 &&
        h.report.storages.length > 0,
      network: h.report.networks[0]?.id || "",
      storage: h.report.storages[0]?.id || "",
      replicas: 1,
      port: 30100,
    };
  const update = (h: Host, patch: Partial<Choice>) =>
    setChoices((v) => ({ ...v, [h.report.id]: { ...choice(h), ...patch } }));
  const serviceChoice = (h: Host): Choice =>
    serviceChoices[h.report.id] || {
      enabled: false,
      network: h.report.networks[0]?.id || "",
      storage: "",
      replicas: 1,
      port: 30100,
    };
  const updateService = (h: Host, patch: Partial<Choice>) =>
    setServiceChoices((v) => ({
      ...v,
      [h.report.id]: { ...serviceChoice(h), ...patch },
    }));
  const doPending = async (kind: "placement" | "service", req: Pending) => {
    if (busy) return;
    const key = kind === "placement" ? placementKey : serviceKey;
    try {
      sessionStorage.setItem(key, JSON.stringify(req));
    } catch {
      setError(
        t(
          "Session storage is required for safe retries.",
          "안전한 재시도를 위해 세션 저장소가 필요하다.",
        ),
      );
      return;
    }
    (kind === "placement" ? setPendingPlacement : setPendingService)(req);
    setBusy(true);
    setError("");
    try {
      await fetchJson(req.path, {
        method: "POST",
        headers: {
          "Content-Type": "application/json",
          "Idempotency-Key": req.key,
        },
        body: JSON.stringify(req.body),
      });
      sessionStorage.removeItem(key);
      (kind === "placement" ? setPendingPlacement : setPendingService)(null);
      if (kind === "service") setEditing(null);
      await refresh();
    } catch (e) {
      if (
        e instanceof ApiClientError &&
        (e.status === 400 ||
          (kind === "service" && e.status === 409) ||
          e.apiError?.code === "insufficient_capacity")
      ) {
        sessionStorage.removeItem(key);
        (kind === "placement" ? setPendingPlacement : setPendingService)(null);
      }
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };
  const action = async (url: string, method = "POST", body?: object) => {
    if (busy) return;
    setBusy(true);
    setError("");
    try {
      await fetchJson(url, {
        method,
        ...(body
          ? {
              headers: { "Content-Type": "application/json" },
              body: JSON.stringify(body),
            }
          : {}),
      });
      await refresh();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };
  const images = Array.from(
    new Map(
      (hosts || [])
        .flatMap((h) => h.report.images || [])
        .map((i) => [JSON.stringify(i), i]),
    ).entries(),
  );
  const place = (event: FormEvent) => {
    event.preventDefault();
    try {
      const pin = JSON.parse(image || images[0]?.[0] || "null");
      if (!pin) throw Error("Choose an installed image.");
      const targets = (hosts || [])
        .filter(
          (h) =>
            h.online &&
            !h.draining &&
            choice(h).enabled &&
            choice(h).network &&
            choice(h).storage,
        )
        .map((h) => ({
          network: { hostId: h.report.id, resourceId: choice(h).network },
          storage: { hostId: h.report.id, resourceId: choice(h).storage },
        }));
      if (!targets.length) throw Error("Select an eligible host.");
      void doPending("placement", {
        path: "/api/cluster/placements",
        key: randomKey(),
        body: { name, image: pin, cpu, ram, diskGb: disk, targets },
      });
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  };
  const saveService = (event: FormEvent) => {
    event.preventDefault();
    try {
      const targets = (hosts || [])
        .filter((h) => serviceChoice(h).enabled)
        .map((h) => ({
          hostId: h.report.id,
          replicas: serviceChoice(h).replicas,
          request: {
            microNetworkId: serviceChoice(h).network,
            hostPort: serviceChoice(h).port,
          },
        }));
      const spec = {
        mode: "replicated",
        service: {
          ...(editing?.spec.service || {}),
          name: serviceName,
          image: serviceImage,
          port: servicePort,
          healthPath: health,
          cpu: serviceCpu,
          ram: serviceRam,
          replicas: targets.reduce((n, h) => n + h.replicas, 0),
          env: object(env),
        },
        state: object(state),
        credentials: object(credentials),
        targets,
      };
      if (!targets.length) throw Error("Select a host.");
      void doPending("service", {
        path: editing
          ? `/api/cluster/services/${path(editing.id)}/update`
          : "/api/cluster/services",
        key: randomKey(),
        body: editing ? { expectedGeneration: editing.generation, spec } : spec,
      });
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  };
  const edit = (s: Service) => {
    setEditing(s);
    setServiceName(s.spec.service.name);
    setServiceImage(s.spec.service.image);
    setServicePort(s.spec.service.port);
    setHealth(s.spec.service.healthPath);
    setServiceCpu(s.spec.service.cpu);
    setServiceRam(s.spec.service.ram);
    setEnv(JSON.stringify(s.spec.service.env || {}));
    setState(JSON.stringify(s.spec.state || {}));
    setCredentials(JSON.stringify(s.spec.credentials || {}));
    setServiceChoices(
      Object.fromEntries(
        s.spec.targets.map((target) => [
          target.hostId,
          {
            enabled: true,
            network: target.request.microNetworkId,
            storage: "",
            replicas: target.replicas,
            port: target.request.hostPort,
          },
        ]),
      ),
    );
  };
  return (
    <Screen view="cluster">
      <div className="cluster-react">
        {error && <p role="alert">{error}</p>}
        <nav aria-label={t("Cluster sections", "클러스터 섹션")}>
          {(["hosts", "placements", "services"] as const).map((k) => (
            <button
              type="button"
              key={k}
              className="btn"
              aria-pressed={tab === k}
              onClick={() => setTab(k)}
            >
              {k === "hosts"
                ? t("Hosts", "호스트")
                : k === "placements"
                  ? t("VM placements", "VM 배치")
                  : t("Services", "서비스")}
            </button>
          ))}
        </nav>
        {hosts === null ? (
          <p>{t("Loading…", "불러오는 중…")}</p>
        ) : tab === "hosts" ? (
          <section>
            <h2>{t("Workload hosts", "작업 호스트")}</h2>
            {hosts.length === 0 ? (
              <p>
                {t(
                  "No workload hosts registered.",
                  "등록된 작업 호스트가 없다.",
                )}
              </p>
            ) : (
              hosts.map((h) => (
                <article className="panel" key={h.report.id}>
                  <h3>{h.report.name}</h3>
                  <p>
                    {!h.online
                      ? t("Disconnected", "연결 끊김")
                      : h.draining
                        ? t("Draining", "비우는 중")
                        : t("Healthy", "정상")}{" "}
                    · CPU {h.reserved?.cpu ?? 0} / {h.report.capacity?.cpu ?? 0}{" "}
                    · RAM {h.reserved?.ramMib ?? 0} /{" "}
                    {h.report.capacity?.ramMib ?? 0} MiB
                  </p>
                  <button
                    className="btn"
                    disabled={busy}
                    onClick={() =>
                      void action(
                        `/api/cluster/hosts/${path(h.report.id)}`,
                        "PATCH",
                        { draining: !h.draining },
                      )
                    }
                  >
                    {h.draining
                      ? t("Resume placement", "배치 재개")
                      : t("Drain host", "새 배치 중지")}
                  </button>
                  <details>
                    <summary>{t("Host information", "호스트 정보")}</summary>
                    <pre>{JSON.stringify(h.report, null, 2)}</pre>
                  </details>
                </article>
              ))
            )}
          </section>
        ) : tab === "placements" ? (
          <section>
            <h2>{t("VM placements", "VM 배치")}</h2>
            {pendingPlacement ? (
              <p>
                {t(
                  "The previous result is unknown. Retry the saved request.",
                  "이전 요청 결과를 확인하지 못했다. 저장된 요청을 재시도한다.",
                )}{" "}
                <button
                  className="btn"
                  disabled={busy}
                  onClick={() => void doPending("placement", pendingPlacement)}
                >
                  {t("Retry", "재시도")}
                </button>
              </p>
            ) : (
              hosts.length > 0 && (
                <form className="panel" onSubmit={place}>
                  <h3>{t("Place a VM", "VM 배치")}</h3>
                  <label>
                    {t("VM name", "VM 이름")}{" "}
                    <input
                      required
                      value={name}
                      onChange={(e) => setName(e.target.value)}
                    />
                  </label>
                  <label>
                    {t("Image", "이미지")}{" "}
                    <select
                      value={image || images[0]?.[0] || ""}
                      onChange={(e) => setImage(e.target.value)}
                    >
                      {images.map(([key, i]) => (
                        <option key={key} value={key}>
                          {i.alias} · {i.version}
                        </option>
                      ))}
                    </select>
                  </label>
                  <label>
                    CPU{" "}
                    <input
                      type="number"
                      min="1"
                      value={cpu}
                      onChange={(e) => setCpu(Number(e.target.value))}
                    />
                  </label>
                  <label>
                    RAM (MiB){" "}
                    <input
                      type="number"
                      min="1"
                      value={ram}
                      onChange={(e) => setRam(Number(e.target.value))}
                    />
                  </label>
                  <label>
                    Disk (GiB){" "}
                    <input
                      type="number"
                      min="1"
                      value={disk}
                      onChange={(e) => setDisk(Number(e.target.value))}
                    />
                  </label>
                  {hosts.map((h) => (
                    <fieldset key={h.report.id}>
                      <legend>
                        <label>
                          <input
                            type="checkbox"
                            disabled={
                              !h.online ||
                              h.draining ||
                              !h.report.networks.length ||
                              !h.report.storages.length
                            }
                            checked={choice(h).enabled}
                            onChange={(e) =>
                              update(h, { enabled: e.target.checked })
                            }
                          />
                          {h.report.name}
                        </label>
                      </legend>
                      <label>
                        Network{" "}
                        <select
                          value={choice(h).network}
                          onChange={(e) =>
                            update(h, { network: e.target.value })
                          }
                        >
                          {h.report.networks.map((n) => (
                            <option key={n.id} value={n.id}>
                              {n.name}
                            </option>
                          ))}
                        </select>
                      </label>
                      <label>
                        Storage{" "}
                        <select
                          value={choice(h).storage}
                          onChange={(e) =>
                            update(h, { storage: e.target.value })
                          }
                        >
                          {h.report.storages.map((s) => (
                            <option key={s.id}>{s.id}</option>
                          ))}
                        </select>
                      </label>
                    </fieldset>
                  ))}
                  <button className="btn primary" disabled={busy}>
                    {t("Place VM", "VM 배치")}
                  </button>
                </form>
              )
            )}
            {placements.map((p) => (
              <article className="panel" key={p.id}>
                <h3>{p.spec.name}</h3>
                <p>
                  {p.phase} · {p.observed?.state} ·{" "}
                  {hosts.find((h) => h.report.id === p.vm?.hostId)?.report
                    .name || p.vm?.hostId}{" "}
                  {p.error}
                </p>
                {p.phase !== "deleted" &&
                  ["start", "stop", "reconcile", "delete"]
                    .filter(
                      (op) =>
                        op === "delete" ||
                        (op === "reconcile" && p.phase !== "rejected") ||
                        (["start", "stop"].includes(op) &&
                          p.phase === "confirmed"),
                    )
                    .map((op) => (
                      <button
                        className="btn"
                        key={op}
                        disabled={busy}
                        onClick={() => {
                          if (
                            op !== "delete" ||
                            window.confirm(
                              t(
                                "Delete this placement?",
                                "이 배치를 삭제할까?",
                              ),
                            )
                          )
                            void action(
                              `/api/cluster/placements/${path(p.id)}/${op}`,
                            );
                        }}
                      >
                        {op}
                      </button>
                    ))}
              </article>
            ))}
          </section>
        ) : (
          <section>
            <h2>{t("Controller services", "컨트롤러 서비스")}</h2>
            {services.map((s) => (
              <article className="panel" key={s.id}>
                <h3>{s.spec.service.name}</h3>
                <p>
                  <span>
                    {t("Generation", "세대")}: {s.generation}
                  </span>{" "}
                  ·{" "}
                  <span>
                    {t("Desired", "목표 상태")}: {s.desired}
                  </span>
                </p>
                <p>
                  {s.id} · {s.desired} / {s.observed} · {s.spec.service.image}
                </p>
                <button
                  className="btn"
                  disabled={busy || !!pendingService}
                  onClick={() => edit(s)}
                >
                  {t("Edit release", "릴리스 수정")}
                </button>
                <button
                  className="btn"
                  disabled={busy || !!pendingService}
                  onClick={() =>
                    void doPending("service", {
                      path: `/api/cluster/services/${path(s.id)}/stop`,
                      key: randomKey(),
                      body: { expectedGeneration: s.generation },
                    })
                  }
                >
                  {t("Stop", "중지")}
                </button>
                <button
                  className="btn"
                  disabled={busy}
                  onClick={() =>
                    void action(`/api/cluster/services/${path(s.id)}/reconcile`)
                  }
                >
                  {t("Reconcile", "상태 동기화")}
                </button>
                <button
                  className="btn"
                  disabled={busy}
                  onClick={() =>
                    void fetchJson(
                      `/api/cluster/services/${path(s.id)}/operations`,
                    )
                      .then((value) =>
                        setHistory((h) => ({ ...h, [s.id]: value })),
                      )
                      .catch((e) => setError(String(e)))
                  }
                >
                  {t("Operations", "작업 기록")}
                </button>
                {history[s.id] !== undefined && (
                  <pre>{JSON.stringify(history[s.id], null, 2)}</pre>
                )}
              </article>
            ))}
            {pendingService ? (
              <p>
                {t(
                  "The previous result is unknown. Retry the saved request.",
                  "이전 요청 결과를 확인하지 못했다. 저장된 요청을 재시도한다.",
                )}{" "}
                <button
                  className="btn"
                  disabled={busy}
                  onClick={() => void doPending("service", pendingService)}
                >
                  {t("Retry", "재시도")}
                </button>
              </p>
            ) : (
              <form
                className="panel"
                aria-label={t(
                  "Controller service form",
                  "컨트롤러 서비스 양식",
                )}
                onSubmit={saveService}
              >
                <h3>
                  {editing
                    ? t("Update service release", "서비스 릴리스 수정")
                    : t("New controller service", "새 컨트롤러 서비스")}
                </h3>
                <p>
                  {t(
                    "Use an OCI image pinned by digest.",
                    "다이제스트로 고정한 OCI 이미지를 사용한다.",
                  )}
                </p>
                <label>
                  {t("Service name", "서비스 이름")}{" "}
                  <input
                    required
                    value={serviceName}
                    onChange={(e) => setServiceName(e.target.value)}
                  />
                </label>
                <label>
                  {t("Pinned OCI image", "고정 OCI 이미지")}{" "}
                  <input
                    required
                    value={serviceImage}
                    onChange={(e) => setServiceImage(e.target.value)}
                  />
                </label>
                <label>
                  Port{" "}
                  <input
                    type="number"
                    min="1"
                    max="65535"
                    value={servicePort}
                    onChange={(e) => setServicePort(Number(e.target.value))}
                  />
                </label>
                <label>
                  Health path{" "}
                  <input
                    value={health}
                    onChange={(e) => setHealth(e.target.value)}
                  />
                </label>
                <label>
                  CPU{" "}
                  <input
                    type="number"
                    min="1"
                    value={serviceCpu}
                    onChange={(e) => setServiceCpu(Number(e.target.value))}
                  />
                </label>
                <label>
                  RAM (MiB){" "}
                  <input
                    type="number"
                    min="1"
                    value={serviceRam}
                    onChange={(e) => setServiceRam(Number(e.target.value))}
                  />
                </label>
                <details>
                  <summary>
                    {t("Application configuration", "애플리케이션 설정")}
                  </summary>
                  <label>
                    State (JSON){" "}
                    <textarea
                      value={state}
                      onChange={(e) => setState(e.target.value)}
                    />
                  </label>
                  <label>
                    Credentials (JSON){" "}
                    <textarea
                      value={credentials}
                      onChange={(e) => setCredentials(e.target.value)}
                    />
                  </label>
                  <label>
                    Environment (JSON){" "}
                    <textarea
                      value={env}
                      onChange={(e) => setEnv(e.target.value)}
                    />
                  </label>
                </details>
                {hosts.map((h) => (
                  <fieldset key={h.report.id}>
                    <legend>
                      <label>
                        <input
                          type="checkbox"
                          checked={serviceChoice(h).enabled}
                          onChange={(e) =>
                            updateService(h, { enabled: e.target.checked })
                          }
                        />
                        {h.report.name}
                      </label>
                    </legend>
                    <label>
                      Network{" "}
                      <select
                        value={serviceChoice(h).network}
                        onChange={(e) =>
                          updateService(h, { network: e.target.value })
                        }
                      >
                        {h.report.networks.map((n) => (
                          <option key={n.id} value={n.id}>
                            {n.name}
                          </option>
                        ))}
                      </select>
                    </label>
                    <label>
                      Replicas{" "}
                      <input
                        type="number"
                        min="1"
                        value={serviceChoice(h).replicas}
                        onChange={(e) =>
                          updateService(h, { replicas: Number(e.target.value) })
                        }
                      />
                    </label>
                    <label>
                      Host port{" "}
                      <input
                        type="number"
                        min="1"
                        max="65535"
                        value={serviceChoice(h).port}
                        onChange={(e) =>
                          updateService(h, { port: Number(e.target.value) })
                        }
                      />
                    </label>
                  </fieldset>
                ))}
                <button className="btn primary" disabled={busy}>
                  {editing
                    ? t("Deploy release", "릴리스 배포")
                    : t("Create service", "서비스 생성")}
                </button>
                {editing && (
                  <button
                    type="button"
                    className="btn"
                    onClick={() => setEditing(null)}
                  >
                    {t("Cancel", "취소")}
                  </button>
                )}
              </form>
            )}
          </section>
        )}
      </div>
    </Screen>
  );
}
