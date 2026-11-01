import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type FormEvent,
} from "react";
import { fetchJson } from "../api/client";
import { useI18n } from "../i18n";
import Screen from "./layout/Screen";

type Network = { id: string; name: string };
type Engine = { phase: string; error?: string | null; log?: string | null };
type Queue = {
  id: string;
  name: string;
  phase: string;
  deliveryLimit: number;
  maxBytes: number;
  error?: string | null;
  services: { id: string; name: string }[];
};
type Metrics = {
  ready: number;
  unacknowledged: number;
  consumers: number;
  failed: number;
};
type Connection = { url: string };
type Snapshot = { engine: Engine | null; queues: Queue[]; networks: Network[] };

async function mutate<T>(
  path: string,
  method: "POST" | "DELETE",
  body?: object,
): Promise<T | null> {
  const response = await fetch(path, {
    method,
    ...(body
      ? {
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify(body),
        }
      : {}),
  });
  if (!response.ok) {
    const result = (await response.json().catch(() => ({}))) as {
      error?: { message?: string; fields?: Record<string, string> };
    };
    const fields = result.error?.fields;
    throw new Error(
      fields && Object.keys(fields).length
        ? Object.entries(fields)
            .map(([key, value]) => `${key}: ${value}`)
            .join(" · ")
        : result.error?.message || `HTTP ${response.status}`,
    );
  }
  return response.status === 204 ? null : ((await response.json()) as T);
}

export default function Queues() {
  const { t } = useI18n();
  const [snapshot, setSnapshot] = useState<Snapshot | null>(null);
  const [connectionError, setConnectionError] = useState("");
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const [network, setNetwork] = useState("");
  const [cpu, setCpu] = useState(4);
  const [ram, setRam] = useState(4096);
  const [disk, setDisk] = useState(8);
  const [name, setName] = useState("");
  const [delivery, setDelivery] = useState(5);
  const [capacity, setCapacity] = useState(64);
  const [selected, setSelected] = useState("");
  const [metrics, setMetrics] = useState<Metrics | null>(null);
  const [secret, setSecret] = useState("");
  const requestId = useRef(0);
  const revealId = useRef(0);
  const active = useRef(false);

  const refresh = useCallback(async () => {
    const request = ++requestId.current;
    try {
      const [engine, queues, networks] = await Promise.all([
        fetchJson<Engine | null>("/api/queue/engine"),
        fetchJson<Queue[]>("/api/queues"),
        fetchJson<Network[]>("/api/micro-networks"),
      ]);
      if (!active.current || request !== requestId.current) return;
      setSnapshot({ engine, queues, networks });
      setConnectionError("");
    } catch (cause) {
      if (active.current && request === requestId.current)
        setConnectionError(
          cause instanceof Error ? cause.message : String(cause),
        );
    }
  }, []);
  useEffect(() => {
    active.current = true;
    void refresh();
    const timer = window.setInterval(() => void refresh(), 3000);
    return () => {
      active.current = false;
      requestId.current++;
      revealId.current++;
      window.clearInterval(timer);
    };
  }, [refresh]);
  useEffect(() => {
    revealId.current++;
    if (!selected) return;
    let live = true;
    const poll = async () => {
      try {
        const result = await fetchJson<Metrics>(
          `/api/queues/${encodeURIComponent(selected)}/metrics`,
        );
        if (live) setMetrics(result);
      } catch {
        if (live) setMetrics(null);
      }
    };
    void poll();
    const timer = window.setInterval(() => void poll(), 3000);
    return () => {
      live = false;
      window.clearInterval(timer);
    };
  }, [selected]);
  useEffect(() => {
    const hide = () => {
      revealId.current++;
      setSecret("");
    };
    const onVisibilityChange = () => {
      if (document.hidden) hide();
    };
    window.addEventListener("blur", hide);
    document.addEventListener("visibilitychange", onVisibilityChange);
    return () => {
      window.removeEventListener("blur", hide);
      document.removeEventListener("visibilitychange", onVisibilityChange);
    };
  }, []);
  useEffect(() => {
    if (!secret) return;
    const timer = window.setTimeout(() => {
      revealId.current++;
      setSecret("");
    }, 30000);
    return () => window.clearTimeout(timer);
  }, [secret]);

  const perform = async (
    path: string,
    method: "POST" | "DELETE",
    body?: object,
  ) => {
    if (busy || connectionError) return false;
    setBusy(true);
    setError("");
    try {
      await mutate(path, method, body);
      await refresh();
      return true;
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
      return false;
    } finally {
      if (active.current) setBusy(false);
    }
  };
  const install = (event: FormEvent) => {
    event.preventDefault();
    if (ram < 128 || ram > 32768 || (ram & (ram - 1)) !== 0) {
      setError(
        t(
          "Memory must be a power of two between 128 and 32768 MiB.",
          "메모리는 128~32768 MiB 사이의 2의 거듭제곱이어야 한다.",
        ),
      );
      return;
    }
    void perform("/api/queue/engine", "POST", {
      microNetworkId: network,
      cpu,
      ram,
      diskGb: disk,
    });
  };
  const create = async (event: FormEvent) => {
    event.preventDefault();
    if (
      await perform("/api/queues", "POST", {
        name,
        deliveryLimit: delivery,
        maxBytes: capacity * 1024 * 1024,
      })
    )
      setName("");
  };
  const reveal = async (access: "publish" | "consume") => {
    if (!selected || busy || connectionError) return;
    setSecret("");
    setError("");
    const request = ++revealId.current;
    try {
      const result = await mutate<Connection>(
        `/api/queues/${encodeURIComponent(selected)}/connection/${access}`,
        "POST",
      );
      if (active.current && request === revealId.current)
        setSecret(result?.url || "");
    } catch (cause) {
      if (active.current && request === revealId.current)
        setError(cause instanceof Error ? cause.message : String(cause));
    }
  };
  const queue = snapshot?.queues.find((item) => item.id === selected);
  const canAct = !busy && !connectionError;
  return (
    <Screen view="queues" live={false}>
      <div className="queue-migration">
        {connectionError && <p role="alert">{connectionError}</p>}
        {error && <p role="alert">{error}</p>}
        {!snapshot ? (
          <p>{t("Loading…", "불러오는 중…")}</p>
        ) : !snapshot.engine ? (
          <section className="panel">
            <h2>{t("Set up your queue engine", "큐 엔진 설치")}</h2>
            <p>
              {t(
                "RabbitMQ runs in a dedicated MicroVM.",
                "RabbitMQ는 전용 MicroVM에서 실행된다.",
              )}
            </p>
            <form onSubmit={install}>
              <fieldset disabled={!canAct}>
                <label>
                  MicroNetwork{" "}
                  <select
                    required
                    value={network}
                    onChange={(event) => setNetwork(event.target.value)}
                  >
                    <option value="">
                      {t("Select a network", "네트워크 선택")}
                    </option>
                    {snapshot.networks.map((item) => (
                      <option key={item.id} value={item.id}>
                        {item.name}
                      </option>
                    ))}
                  </select>
                </label>
                <label>
                  CPU{" "}
                  <input
                    type="number"
                    required
                    min={1}
                    max={32}
                    value={cpu}
                    onChange={(event) => setCpu(Number(event.target.value))}
                  />
                </label>
                <label>
                  {t("Memory (MiB)", "메모리 (MiB)")}{" "}
                  <input
                    type="number"
                    required
                    min={128}
                    max={32768}
                    value={ram}
                    onChange={(event) => setRam(Number(event.target.value))}
                  />
                </label>
                <label>
                  {t("Disk (GiB)", "디스크 (GiB)")}{" "}
                  <input
                    type="number"
                    required
                    min={4}
                    max={1024}
                    value={disk}
                    onChange={(event) => setDisk(Number(event.target.value))}
                  />
                </label>
                <button type="submit" className="btn primary">
                  {t("Install RabbitMQ", "RabbitMQ 설치")}
                </button>
              </fieldset>
            </form>
          </section>
        ) : (
          <>
            <section
              className="panel"
              aria-label={t("Queue engine", "큐 엔진")}
            >
              <h2>RabbitMQ</h2>
              <p>{snapshot.engine.phase}</p>
              {snapshot.engine.error && (
                <p role="alert">{snapshot.engine.error}</p>
              )}
              {snapshot.engine.log && (
                <details>
                  <summary>{t("Installation log", "설치 로그")}</summary>
                  <pre className="detail-log">{snapshot.engine.log}</pre>
                </details>
              )}
              {["failed", "stopped"].includes(snapshot.engine.phase) && (
                <button
                  className="btn"
                  disabled={!canAct}
                  onClick={() =>
                    void perform("/api/queue/engine/start", "POST")
                  }
                >
                  {t("Start / recover", "시작 / 복구")}
                </button>
              )}
              {snapshot.engine.phase === "running" && (
                <button
                  className="btn"
                  disabled={!canAct}
                  onClick={() => void perform("/api/queue/engine/stop", "POST")}
                >
                  {t("Stop", "중지")}
                </button>
              )}
            </section>
            <section className="panel">
              <h2>{t("Create a queue", "큐 생성")}</h2>
              <form onSubmit={(event) => void create(event)}>
                <fieldset
                  disabled={!canAct || snapshot.engine.phase !== "running"}
                >
                  <label>
                    {t("Queue name", "큐 이름")}{" "}
                    <input
                      required
                      pattern="[a-z0-9-]{1,40}"
                      maxLength={40}
                      value={name}
                      onChange={(event) => setName(event.target.value)}
                    />
                  </label>
                  <label>
                    {t("Delivery limit", "전달 한도")}{" "}
                    <input
                      type="number"
                      required
                      min={1}
                      max={100}
                      value={delivery}
                      onChange={(event) =>
                        setDelivery(Number(event.target.value))
                      }
                    />
                  </label>
                  <label>
                    {t("Capacity per queue (MiB)", "큐 용량 (MiB)")}{" "}
                    <input
                      type="number"
                      required
                      min={1}
                      max={1024}
                      value={capacity}
                      onChange={(event) =>
                        setCapacity(Number(event.target.value))
                      }
                    />
                  </label>
                  <button type="submit" className="btn primary">
                    {t("Create Queue", "큐 생성")}
                  </button>
                </fieldset>
              </form>
            </section>
            <section className="panel">
              <h2>{t("Queues", "큐")}</h2>
              {snapshot.queues.length === 0 ? (
                <p>{t("No queues yet.", "아직 큐가 없다.")}</p>
              ) : (
                <ul>
                  {snapshot.queues.map((item) => (
                    <li key={item.id}>
                      <button
                        className="btn"
                        onClick={() => {
                          revealId.current++;
                          setSecret("");
                          setMetrics(null);
                          setSelected(item.id);
                        }}
                      >
                        {item.name}
                      </button>{" "}
                      {item.phase}
                      {item.error && <span>{item.error}</span>}
                      {item.phase === "failed" && (
                        <button
                          className="btn"
                          disabled={!canAct}
                          onClick={() =>
                            void perform(
                              `/api/queues/${encodeURIComponent(item.id)}/retry`,
                              "POST",
                            )
                          }
                        >
                          {t("Retry", "재시도")}
                        </button>
                      )}
                      <button
                        className="btn"
                        disabled={!canAct || item.services.length > 0}
                        onClick={() => {
                          if (
                            window.confirm(
                              t(
                                `Delete queue ${item.name}?`,
                                `큐 ${item.name}을 삭제할까?`,
                              ),
                            )
                          )
                            void perform(
                              `/api/queues/${encodeURIComponent(item.id)}`,
                              "DELETE",
                            );
                        }}
                      >
                        {t("Delete", "삭제")}
                      </button>
                    </li>
                  ))}
                </ul>
              )}
            </section>
            {queue && (
              <section
                className="panel"
                aria-label={t("Queue detail", "큐 상세")}
              >
                <h2>{queue.name}</h2>
                {metrics && (
                  <p>
                    {t("Waiting", "대기")}: {metrics.ready} ·{" "}
                    {t("Unacknowledged", "처리 중")}: {metrics.unacknowledged} ·{" "}
                    {t("Consumers", "수신자")}: {metrics.consumers} ·{" "}
                    {t("Failed", "실패")}: {metrics.failed}
                  </p>
                )}
                <button
                  className="btn"
                  disabled={!canAct}
                  onClick={() => void reveal("publish")}
                >
                  {t("Show sender connection", "전송 연결 정보 보기")}
                </button>
                <button
                  className="btn"
                  disabled={!canAct}
                  onClick={() => void reveal("consume")}
                >
                  {t("Show receiver connection", "수신 연결 정보 보기")}
                </button>
                <button
                  className="btn"
                  onClick={() => {
                    revealId.current++;
                    setSecret("");
                  }}
                >
                  {t("Hide", "숨기기")}
                </button>
                {secret && <pre className="detail-log">{secret}</pre>}
              </section>
            )}
          </>
        )}
      </div>
    </Screen>
  );
}
