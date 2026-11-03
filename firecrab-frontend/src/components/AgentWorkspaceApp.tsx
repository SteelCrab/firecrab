import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type FormEvent,
} from "react";
import {
  Check,
  ExternalLink,
  Globe,
  Laptop,
  Play,
  RefreshCw,
  ShieldCheck,
  Square,
  Terminal,
} from "lucide-react";
import { agentRequest, type AgentSession } from "../api/agents";
import {
  getPipe,
  saveFirecrabAddress,
  type FirecrabAddress,
  type PipeSnapshot,
} from "../api/pipe";
import {
  checkWorkspaceAddress,
  type AddressProof,
  type AppConfig,
  type AppVerification,
} from "../api/workspaces";
import type { VmResponse } from "../bindings";
import { useI18n } from "../i18n";

const initialConfig: AppConfig = {
  testCommand: "",
  buildCommand: "",
  startCommand: "",
  port: 3000,
};
const publicPreview = (url?: string | null) =>
  !!url && /^https:\/\/[a-z0-9]+(?:-[a-z0-9]+)*\.trycloudflare\.com$/.test(url);
const localPreview = (url?: string | null) =>
  !!url && /^http:\/\/127\.0\.0\.1:[1-9][0-9]{0,4}\/$/.test(url);
// The local address works only in a browser on the computer that runs Firecrab.
const onFirecrabComputer = () =>
  ["localhost", "127.0.0.1", "[::1]"].includes(location.hostname);
type Proof = AddressProof & { runId: string; origin: string };
export type WorkspaceAppProgress = {
  verified: boolean;
  fixedVerified: boolean;
  previewVerified: boolean;
};

export default function AgentWorkspaceApp({
  vm,
  agents,
  controllerId,
  canControl,
  mode,
  onTask,
  onDirty,
  onAddress,
  onPreview,
  onProgress,
}: {
  vm: VmResponse;
  agents: AgentSession[];
  controllerId: string;
  canControl: boolean;
  mode: "verify" | "address" | "preview";
  onTask: () => void;
  onDirty: (dirty: boolean) => void;
  onAddress: () => void;
  onPreview: () => void;
  onProgress: (progress: WorkspaceAppProgress) => void;
}) {
  const { t } = useI18n();
  const tasks = agents.filter((agent) => agent.role !== "authentication");
  const [selected, setSelected] = useState("");
  const agentId = tasks.some((agent) => agent.id === selected)
    ? selected
    : tasks.at(-1)?.id || "";
  const endpoint = `/${encodeURIComponent(agentId)}/automation`;
  const [status, setStatus] = useState<AppVerification | null>(null);
  const [config, setConfig] = useState<AppConfig>(initialConfig);
  const [pipe, setPipe] = useState<PipeSnapshot | null>(null);
  const [busy, setBusy] = useState(""),
    [error, setError] = useState(""),
    [loadError, setLoadError] = useState("");
  const [token, setToken] = useState(""),
    [addressName, setAddressName] = useState("");
  const [proof, setProof] = useState<Proof | null>(null);
  const alive = useRef(true),
    mutating = useRef(false),
    initialized = useRef(false),
    revision = useRef(0);
  const pendingRun = useRef<string | null>(null);
  const pipeRevision = useRef(0);
  const key = `firecrab.workspaceRun.${controllerId}.${vm.id}.${agentId}`;
  const identity = useRef("");
  useEffect(() => {
    identity.current = `${controllerId}/${vm.id}/${agentId}`;
  }, [controllerId, vm.id, agentId]);
  const busyWorkspace = tasks.some(
    (agent) =>
      agent.state === "working" ||
      agent.hasPendingTask ||
      agent.taskDeliveryUncertain,
  );
  const ready =
    canControl &&
    vm.state === "running" &&
    !loadError &&
    status?.vmId === vm.id &&
    status.agentId === agentId;
  const run = status?.run;
  const appReady =
    !!ready &&
    run?.state === "passed" &&
    run.appReady &&
    run.appRunning === true;
  const origin =
    status?.preview?.running && publicPreview(status.preview.publicUrl)
      ? status.preview.publicUrl!
      : "";
  const local =
    appReady && status?.local && localPreview(status.local.url)
      ? status.local
      : null;
  const localCheck = local?.check;
  const addresses = (pipe?.firecrabTunnels || []).filter(
    (address) =>
      address.target.type === "preview" && address.target.vm_id === vm.id,
  );
  const address =
    addresses.find((address) => address.subdomain === addressName) ||
    addresses[0];
  const currentAddress =
    address?.target.type === "preview" &&
    address.target.url === origin &&
    address.connected &&
    /^https:\/\/[a-z0-9-]+\.firecrab\.dev$/.test(address.publicUrl)
      ? address
      : null;
  const verified =
    appReady &&
    proof?.ok &&
    proof.runId === run?.id &&
    proof.origin === origin &&
    (proof.url === origin || proof.url === currentAddress?.publicUrl)
      ? proof
      : null;
  const dirty =
    !!token ||
    (!!status && JSON.stringify(config) !== JSON.stringify(status.config));
  const previewVerified = !!verified,
    fixedVerified = !!verified && verified.url === currentAddress?.publicUrl;
  useEffect(() => {
    onProgress({ verified: !!appReady, fixedVerified, previewVerified });
  }, [appReady, fixedVerified, previewVerified, onProgress]);
  useEffect(() => {
    onDirty(dirty);
    return () => onDirty(false);
  }, [dirty, onDirty]);
  const inspect = useCallback(async () => {
    if (!agentId || vm.state !== "running" || mutating.current) return;
    const version = ++revision.current;
    try {
      const next = await agentRequest<AppVerification>(
        endpoint,
        undefined,
        undefined,
        vm.id,
      );
      if (!alive.current || version !== revision.current || mutating.current)
        return;
      if (next.vmId !== vm.id || next.agentId !== agentId)
        throw Error(
          t(
            "Workspace identity changed.",
            "작업 공간이 바뀌었어요. 다시 열어 주세요.",
          ),
        );
      setStatus(next);
      setLoadError("");
      if (!initialized.current) {
        setConfig(next.config);
        initialized.current = true;
      }
      if (next.run?.id === pendingRun.current && next.run.state !== "running") {
        localStorage.removeItem(key);
        pendingRun.current = null;
      }
    } catch (failure) {
      if (alive.current && version === revision.current) {
        setLoadError((failure as Error).message);
        setProof(null);
      }
    }
  }, [agentId, endpoint, key, t, vm.id, vm.state]);
  useEffect(() => {
    alive.current = true;
    initialized.current = false;
    revision.current++;
    setStatus(null);
    setProof(null);
    setError("");
    setLoadError("");
    pendingRun.current = localStorage.getItem(key);
    let stopped = false,
      timer = 0;
    const poll = async () => {
      if (stopped) return;
      if (!document.hidden) await inspect();
      if (!stopped) timer = window.setTimeout(poll, 3500);
    };
    void poll();
    return () => {
      stopped = true;
      alive.current = false;
      revision.current++;
      clearTimeout(timer);
    };
  }, [inspect, key]);
  useEffect(() => {
    let stopped = false;
    const refresh = async () => {
      if (mutating.current) return;
      const version = ++pipeRevision.current;
      try {
        const next = await getPipe();
        if (!stopped && !mutating.current && version === pipeRevision.current)
          setPipe(next);
      } catch {
        if (!stopped && !mutating.current && version === pipeRevision.current)
          setPipe(null);
      }
    };
    void refresh();
    const timer = setInterval(refresh, 5000);
    return () => {
      stopped = true;
      clearInterval(timer);
    };
  }, [vm.id]);

  async function native(
    action: string,
    id: string = crypto.randomUUID(),
  ): Promise<AppVerification & AddressProof> {
    return agentRequest(endpoint, { action, id, config }, controllerId, vm.id);
  }
  async function act(
    kind: "run" | "stop_app" | "stop_preview" | "temporary" | "fixed" | "check",
  ) {
    if (
      mutating.current ||
      !ready ||
      ((kind === "run" || kind === "fixed" || kind === "temporary") &&
        busyWorkspace)
    )
      return;
    const owner = identity.current,
      active = () => alive.current && identity.current === owner;
    mutating.current = true;
    pipeRevision.current++;
    revision.current++;
    setBusy(kind);
    setError("");
    setProof(null);
    try {
      if (kind === "run") {
        const id = pendingRun.current || crypto.randomUUID();
        pendingRun.current = id;
        localStorage.setItem(key, id);
        const next = await native("run", id);
        if (active()) setStatus(next);
        return;
      }
      if (kind === "stop_app" || kind === "stop_preview") {
        const next = await native(kind);
        if (active()) setStatus(next);
        return;
      }
      if (!appReady || !run)
        throw Error(
          t(
            "Pass verification and start the app first.",
            "검증을 통과하고 앱을 먼저 실행해 주세요.",
          ),
        );
      let next = status!;
      if (kind !== "check") next = await native("start_preview");
      const deadline = Date.now() + 45_000;
      while (
        active() &&
        (!next.preview?.running || !publicPreview(next.preview.publicUrl))
      ) {
        if (Date.now() > deadline)
          throw Error(
            t(
              "The preview is still starting. Retry to check the existing tunnel.",
              "주소를 준비하고 있어요. 다시 확인하면 기존 터널을 이어서 사용할 수 있어요.",
            ),
          );
        await new Promise((resolve) => setTimeout(resolve, 1500));
        next = await agentRequest<AppVerification>(
          endpoint,
          undefined,
          undefined,
          vm.id,
        );
      }
      if (!active()) return;
      if (
        next.vmId !== vm.id ||
        next.agentId !== agentId ||
        next.run?.id !== run.id ||
        next.run.appRunning !== true ||
        next.run.state !== "passed"
      )
        throw Error(
          t(
            "The app changed. Verify it again.",
            "앱 상태가 바뀌었어요. 검증 결과를 다시 확인해 주세요.",
          ),
        );
      setStatus(next);
      const source = next.preview!.publicUrl!;
      const sourceProof = await native("check_preview");
      if (!sourceProof.ok || sourceProof.url !== source)
        throw Error(
          t(
            "The app's HTTPS preview did not pass.",
            "앱이 HTTPS 요청에 정상적으로 응답하지 않았어요.",
          ),
        );
      if (!active()) return;
      let fixed: FirecrabAddress | undefined;
      if (kind === "fixed") {
        if (!address && !/^[a-fA-F0-9]{64}$/.test(token.trim()))
          throw Error(
            t(
              "Enter a Firecrab connection token.",
              "Firecrab에서 발급받은 연결 토큰을 입력해 주세요.",
            ),
          );
        let snapshot = await saveFirecrabAddress(
          address ? { subdomain: address.subdomain } : { token: token.trim() },
          { type: "preview", vm_id: vm.id, url: source },
        );
        if (!active()) return;
        setToken("");
        setPipe(snapshot);
        const until = Date.now() + 30_000;
        do {
          fixed = snapshot.firecrabTunnels?.find(
            (value) =>
              value.target.type === "preview" &&
              value.target.vm_id === vm.id &&
              value.target.url === source &&
              (!address || value.subdomain === address.subdomain),
          );
          if (fixed?.connected || Date.now() > until) break;
          await new Promise((resolve) => setTimeout(resolve, 1500));
          if (!active()) return;
          snapshot = await getPipe();
          setPipe(snapshot);
        } while (active());
        if (!fixed?.connected)
          throw Error(
            fixed?.error ||
              t(
                "Address registration is pending. Check it again.",
                "주소 연결을 기다리고 있어요. 잠시 후 다시 확인해 주세요.",
              ),
          );
        setAddressName(fixed.subdomain);
      } else if (kind === "check" && currentAddress) fixed = currentAddress;
      const result = fixed
        ? await checkWorkspaceAddress(fixed.subdomain)
        : sourceProof;
      if (!active()) return;
      if (result.url !== (fixed?.publicUrl || source))
        throw Error(
          t(
            "Preview identity changed.",
            "연결 주소가 바뀌었어요. 다시 확인해 주세요.",
          ),
        );
      setProof({ ...result, runId: run.id, origin: source });
      if (!result.ok)
        throw Error(
          t(
            `HTTPS returned HTTP ${result.status}.`,
            `앱 주소에서 HTTP ${result.status} 응답을 받았어요.`,
          ),
        );
    } catch (failure) {
      if (active()) setError((failure as Error).message);
    } finally {
      mutating.current = false;
      if (active()) {
        setBusy("");
        void inspect();
      }
    }
  }
  // Separate from `act`: the local view leaves the public address and its HTTPS proof alone.
  async function viewLocally(action: "start_local" | "stop_local") {
    if (mutating.current || !ready) return;
    const owner = identity.current,
      active = () => alive.current && identity.current === owner;
    mutating.current = true;
    revision.current++;
    setBusy(action);
    setError("");
    try {
      const next = await native(action);
      if (!active()) return;
      if (next.vmId !== vm.id || next.agentId !== agentId)
        throw Error(
          t(
            "Workspace identity changed.",
            "작업 공간이 바뀌었어요. 다시 열어 주세요.",
          ),
        );
      setStatus(next);
      if (action === "start_local" && !next.local?.check?.reachable)
        throw Error(
          t(
            "The app did not answer through the local connection. Check the app log, then retry.",
            "로컬 연결로 앱이 응답하지 않았어요. 앱 로그를 확인하고 다시 시도해 주세요.",
          ),
        );
    } catch (failure) {
      if (active()) setError((failure as Error).message);
    } finally {
      mutating.current = false;
      if (active()) {
        setBusy("");
        void inspect();
      }
    }
  }
  const submit = (event: FormEvent) => {
    event.preventDefault();
    void act("run");
  };
  if (!tasks.length)
    return (
      <section className="awa-empty">
        <Terminal />
        <h2>{t("Start with an AI task", "먼저 AI에게 작업을 맡겨 보세요")}</h2>
        <p>
          {t(
            "Verification and app connections use that task's VM workspace.",
            "작업을 마치면 같은 VM에서 테스트하고 앱을 실행할 수 있어요.",
          )}
        </p>
        <button className="btn primary" onClick={onTask}>
          {t("AI tasks", "AI 작업으로 이동")}
        </button>
      </section>
    );
  return (
    <section className="awa" aria-label={t("Workspace app", "작업 공간 앱")}>
      <header>
        <small>{t("Workspace app", "작업 공간 앱")}</small>
        <h2>
          {mode === "verify"
            ? t("Verify and run your app", "검증하고 앱 실행하기")
            : mode === "address"
              ? t("Connect your app address", "앱 주소 연결")
              : t("Your running app", "실행 중인 앱")}
        </h2>
        <p>
          {t(
            "Use the real app and verification results from this MicroVM.",
            "이 VM에서 실행한 앱과 실제 검증 결과를 확인해요.",
          )}
        </p>
      </header>
      {tasks.length > 1 && (
        <label>
          {t("Coding task", "코딩 작업")}
          <select
            value={agentId}
            disabled={!!busy}
            onChange={(e) => setSelected(e.target.value)}
          >
            {tasks.map((agent) => (
              <option key={agent.id} value={agent.id}>
                {agent.name}
              </option>
            ))}
          </select>
        </label>
      )}
      {(error || loadError) && (
        <p className="awc-error" role="alert">
          {error || loadError}
        </p>
      )}
      {mode === "verify" && (
        <form className="awa-card" onSubmit={submit}>
          <div className="awa-heading">
            <ShieldCheck />
            <h3>{t("Tests → build → app", "테스트 → 빌드 → 앱 실행")}</h3>
          </div>
          {run && (
            <div className={`awa-run ${run.state}`} role="status">
              <strong>
                {run.state === "running"
                  ? t("Checking", "검증 중")
                  : run.state === "passed"
                    ? t("Passed", "검증 통과")
                    : run.state === "failed"
                      ? t("Failed", "실패")
                      : t("Check required", "상태 확인 필요")}
              </strong>
              <span>
                {(
                  {
                    tests: t("Tests", "테스트"),
                    build: t("Build", "빌드"),
                    preview: t("Starting app", "앱 시작"),
                    complete: appReady
                      ? t("App running", "앱 실행 중")
                      : t("Complete", "완료"),
                  } as Record<string, string>
                )[run.phase] || t("Preparing", "준비 중")}
              </span>
              {run.exitCode !== null && run.exitCode !== 0 && (
                <span>exit {run.exitCode}</span>
              )}
            </div>
          )}
          {busyWorkspace && (
            <p>
              {t(
                "Wait for all coding tasks to finish before verification.",
                "코딩 작업이 끝나면 검증을 시작할 수 있어요.",
              )}
            </p>
          )}
          <details open={!config.testCommand || !config.startCommand}>
            <summary>{t("Execution settings", "실행 설정")}</summary>
            {(
              [
                ["testCommand", t("Test command", "테스트 명령")],
                ["buildCommand", t("Build command", "빌드 명령")],
                ["startCommand", t("App command", "앱 실행 명령")],
              ] as const
            ).map(([field, label]) => (
              <label key={field}>
                {label}
                <input
                  value={config[field]}
                  disabled={!!busy || run?.state === "running"}
                  onChange={(e) =>
                    setConfig({ ...config, [field]: e.target.value })
                  }
                />
              </label>
            ))}
            <label>
              {t("App port", "앱 포트")}
              <input
                type="number"
                min={1}
                max={65535}
                value={config.port}
                disabled={!!busy || run?.state === "running"}
                onChange={(e) =>
                  setConfig({ ...config, port: +e.target.value })
                }
              />
            </label>
            <small>
              {t(
                "Run the app in the foreground. Keep development servers stopped until verification starts.",
                "앱은 전경 실행 명령으로 준비해 주세요. 검증 전에 별도 개발 서버를 실행했다면 해당 터미널에서 먼저 중지해 주세요.",
              )}
            </small>
          </details>
          <div className="awa-actions">
            {appReady && (
              <button type="button" className="btn primary" onClick={onAddress}>
                {t("Connect app address", "앱 주소 연결하기")}
                <Globe />
              </button>
            )}
            <button
              className="btn primary"
              type="submit"
              disabled={
                !ready ||
                !!busy ||
                busyWorkspace ||
                run?.state === "running" ||
                !!run?.appRunning ||
                !config.testCommand.trim() ||
                !config.startCommand.trim()
              }
            >
              <Play />
              {t("Verify and run", "검증하고 실행")}
            </button>
            {run?.appRunning && (
              <button
                type="button"
                className="btn"
                disabled={!ready || !!busy}
                onClick={() => void act("stop_app")}
              >
                <Square />
                {t("Stop app", "앱 중지")}
              </button>
            )}
            <button
              type="button"
              className="icon-btn"
              aria-label={t("Refresh app status", "앱 상태 새로고침")}
              disabled={!!busy}
              onClick={() => void inspect()}
            >
              <RefreshCw />
            </button>
          </div>
          {run?.exitCode === 127 && (
            <p>
              {t(
                "The command was not found in this VM.",
                "이 VM에서 명령을 찾지 못했어요.",
              )}
            </p>
          )}
          {run?.log && (
            <details>
              <summary>{t("Verification log", "검증 로그")}</summary>
              <pre>{run.log}</pre>
            </details>
          )}
        </form>
      )}
      {mode !== "verify" && (
        <div className="awa-card">
          <div className="awa-heading">
            <Globe />
            <h3>{t("App address", "앱 주소")}</h3>
          </div>
          {!appReady && (
            <p>
              {t(
                "Pass verification and start the app in App execution first.",
                "먼저 ‘앱 실행’에서 검증을 통과하고 앱을 실행해 주세요.",
              )}
            </p>
          )}
          {addresses.length > 1 && (
            <label>
              {t("Saved address", "저장된 주소")}
              <select
                value={address?.subdomain || ""}
                disabled={!!busy}
                onChange={(e) => {
                  setAddressName(e.target.value);
                  setProof(null);
                }}
              >
                {addresses.map((value) => (
                  <option key={value.subdomain} value={value.subdomain}>
                    {value.publicUrl}
                  </option>
                ))}
              </select>
            </label>
          )}
          {address ? (
            <p className="awa-url">{address.publicUrl}</p>
          ) : (
            <label>
              {t("Firecrab connection token", "Firecrab 연결 토큰")}
              <input
                aria-label={t(
                  "Firecrab connection token",
                  "Firecrab 연결 토큰",
                )}
                type="password"
                autoComplete="off"
                spellCheck={false}
                maxLength={64}
                value={token}
                disabled={!!busy}
                onChange={(e) => setToken(e.target.value)}
              />
              <a
                href="https://console.firecrab.dev/"
                target="_blank"
                rel="noreferrer"
              >
                {t("Get a connection token", "연결 토큰 발급받기")}
              </a>
            </label>
          )}
          <div className="awa-actions">
            <button
              className="btn primary"
              disabled={
                !appReady ||
                !!busy ||
                busyWorkspace ||
                (!address && !/^[a-fA-F0-9]{64}$/.test(token.trim()))
              }
              onClick={() => void act("fixed")}
            >
              <Globe />
              {busy === "fixed"
                ? t("Connecting…", "연결 중…")
                : t("Connect Firecrab address", "Firecrab 주소 연결")}
            </button>
            <button
              className="btn"
              disabled={!appReady || !!busy || busyWorkspace}
              onClick={() => void act("temporary")}
            >
              {t("Use a temporary address", "임시 주소로 보기")}
            </button>
            <button
              className="btn"
              disabled={!appReady || !!busy}
              onClick={() => void viewLocally("start_local")}
            >
              <Laptop />
              {busy === "start_local"
                ? t("Connecting…", "연결 중…")
                : t("View locally", "로컬에서 보기")}
            </button>
          </div>
          {address?.error && <p className="awc-error">{address.error}</p>}
          {(origin || currentAddress) && (
            <div className="awa-actions">
              <button
                className="btn"
                disabled={!appReady || !!busy}
                onClick={() => void act("check")}
              >
                {t("Check HTTPS", "HTTPS 확인")}
              </button>
              <button
                className="btn"
                disabled={!ready || !!busy || !status?.preview?.running}
                onClick={() => void act("stop_preview")}
              >
                <Square />
                {t("Stop public access", "공개 접속 중지")}
              </button>
            </div>
          )}
          {verified && (
            <p className="awa-proof" role="status">
              <Check />
              HTTPS {verified.status} · {verified.latencyMs} ms
            </p>
          )}
          {verified && (
            <>
              <a
                className="awa-url"
                href={verified.url}
                target="_blank"
                rel="noreferrer"
              >
                {verified.url}
                <ExternalLink />
              </a>
              {mode === "preview" ? (
                new URL(verified.url).origin !== location.origin && (
                  <iframe
                    title={t("Running app preview", "실행 중인 앱 미리보기")}
                    src={verified.url}
                    sandbox="allow-scripts allow-forms allow-same-origin"
                    referrerPolicy="no-referrer"
                  />
                )
              ) : (
                <button className="btn primary" onClick={onPreview}>
                  {t("Open app preview", "앱 미리보기 열기")}
                  <ExternalLink />
                </button>
              )}
              <div className="awa-actions">
                <button className="btn" onClick={onTask}>
                  {t("Ask AI for another change", "AI에게 추가 수정 맡기기")}
                </button>
              </div>
            </>
          )}
          {local && (
            <>
              <div className="awa-heading">
                <Laptop />
                <h3>{t("Local address", "로컬 주소")}</h3>
              </div>
              {localCheck?.reachable && (
                <p className="awa-proof" role="status">
                  <Check />
                  HTTP {localCheck.status} · {localCheck.latencyMs} ms
                </p>
              )}
              {onFirecrabComputer() ? (
                <>
                  <a
                    className="awa-url"
                    href={local.url}
                    target="_blank"
                    rel="noreferrer"
                  >
                    {local.url}
                    <ExternalLink />
                  </a>
                  <small>
                    {t(
                      "This address opens only in a browser on the computer that runs Firecrab. Nothing is published to the internet.",
                      "이 주소는 Firecrab을 실행하는 컴퓨터의 브라우저에서만 열려요. 인터넷에는 공개되지 않아요.",
                    )}
                  </small>
                  {!verified &&
                    (mode === "preview" ? (
                      <iframe
                        title={t(
                          "Running app preview",
                          "실행 중인 앱 미리보기",
                        )}
                        src={local.url}
                        sandbox="allow-scripts allow-forms allow-same-origin"
                        referrerPolicy="no-referrer"
                      />
                    ) : (
                      <button className="btn primary" onClick={onPreview}>
                        {t("Open app preview", "앱 미리보기 열기")}
                        <ExternalLink />
                      </button>
                    ))}
                </>
              ) : (
                <p>
                  {t(
                    `This dashboard is open from another computer. ${local.url} opens only in a browser on the computer that runs Firecrab.`,
                    `이 대시보드는 다른 컴퓨터에서 열려 있어요. ${local.url} 주소는 Firecrab을 실행하는 컴퓨터의 브라우저에서만 열려요.`,
                  )}
                </p>
              )}
              <div className="awa-actions">
                <button
                  className="btn"
                  disabled={!ready || !!busy}
                  onClick={() => void viewLocally("stop_local")}
                >
                  <Square />
                  {t("Stop local access", "로컬 접속 중지")}
                </button>
              </div>
            </>
          )}
        </div>
      )}
    </section>
  );
}
