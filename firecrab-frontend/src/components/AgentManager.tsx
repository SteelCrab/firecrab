import {
  useCallback,
  useEffect,
  useRef,
  type ReactNode,
  useState,
  type FormEvent,
} from "react";
import {
  Check,
  Plus,
  PanelLeftClose,
  Square,
  RefreshCw,
  MoreHorizontal,
  ChevronDown,
  Folder,
  CodeXml,
  X,
} from "lucide-react";
import { useI18n } from "../i18n";
import { useHostStatus } from "../hostStatusContext";
import {
  agentRequest,
  agentEditorUrl,
  type AgentTask,
  type AgentSession,
  type AgentStatus,
  type AgentTerminal,
} from "../api/agents";
import AgentBrandIcon from "./icons/AgentBrandIcon";
import AgentTerminalView from "./AgentTerminal";
import Screen from "./layout/Screen";
import AppDialog from "./ui/AppDialog";
import AgentTaskForm from "./AgentTaskForm";
import AgentEnvironment from "./AgentEnvironment";
import AgentRepository from "./AgentRepository";
import AgentAutomation from "./AgentAutomation";
import CreateVm from "./CreateVm";
import { startVm, resumeVm } from "../api/client";
import type { VmResponse } from "../bindings";
import "../styles/agent-manager.css";

type Props = {
  vms: VmResponse[];
  onUnsavedChange: (dirty: boolean) => void;
};
const blank = (): AgentTask => ({
  id: crypto.randomUUID(),
  input: "",
  cli: "",
  model: "",
});
const labels: Record<string, [string, string]> = {
  working: ["Working", "실행 중"],
  blocked: ["Needs input", "입력 필요"],
  idle: ["Idle", "대기"],
  done: ["Done", "완료"],
  closed: ["Closed", "종료됨"],
  disconnected: ["Disconnected", "연결 끊김"],
  unknown: ["Unknown", "상태 미확인"],
  failed: ["Launch failed", "시작 실패"],
};

const AGENT_NAMES: Record<string, string> = {
  codex: "Codex",
  claude: "Claude Code",
  gemini: "Gemini CLI",
  opencode: "OpenCode",
  copilot: "GitHub Copilot",
  cursor: "Cursor Agent",
  github: "GitHub",
};
const agentName = (kind: string) => AGENT_NAMES[kind] || kind;

export default function AgentManager(props: Props) {
  const host = useHostStatus();
  const observedControllerId = host?.platform?.controllerId || "";
  const [controllerId, setControllerId] = useState(observedControllerId);
  // An unavailable health sample is not a change of host identity.
  if (observedControllerId && observedControllerId !== controllerId) {
    setControllerId(observedControllerId);
  }
  return (
    <AgentVmSelection
      key={controllerId}
      {...props}
      controllerId={controllerId}
    />
  );
}
function AgentVmSelection(props: Props & { controllerId: string }) {
  const { t } = useI18n();
  const key = `firecrab.agentVm.${props.controllerId}`;
  const [vmId, setVmId] = useState(() => localStorage.getItem(key) || "");
  const [created, setCreated] = useState<VmResponse[]>([]);
  const unsaved = useRef(false);
  const vms = [
    ...props.vms,
    ...created.filter((v) => !props.vms.some((p) => p.id === v.id)),
  ].filter((v) => !v.purpose || v.purpose === "instance");
  const onUnsavedChange = props.onUnsavedChange;
  const activeVmId = vms.some((v) => v.id === vmId) ? vmId : "";
  const onDirty = useCallback(
    (dirty: boolean) => {
      unsaved.current = dirty;
      onUnsavedChange(dirty);
    },
    [onUnsavedChange],
  );
  const choose = (id: string) => {
    if (
      unsaved.current &&
      !window.confirm(
        t("Leave the unsent draft?", "보내지 않은 입력을 두고 이동할까?"),
      )
    )
      return;
    localStorage.setItem(key, id);
    setVmId(id);
  };
  return (
    <VmAgentWorkspace
      key={activeVmId}
      {...props}
      vms={vms}
      onUnsavedChange={onDirty}
      vmId={activeVmId}
      onVm={choose}
      onCreated={(v) => {
        setCreated((old) => [...old, v]);
        localStorage.setItem(key, v.id);
        setVmId(v.id);
      }}
    />
  );
}

function VmAgentWorkspace({
  controllerId,
  vms,
  vmId,
  onVm,
  onCreated,
  onUnsavedChange,
}: Props & {
  controllerId: string;
  vmId: string;
  onVm: (id: string) => void;
  onCreated: (vm: VmResponse) => void;
}) {
  const { t } = useI18n();
  const host = useHostStatus();
  const vm = vms.find((v) => v.id === vmId);
  const cliKey = `firecrab.agentCli.${controllerId}.${vmId}`;
  const [cli, setCli] = useState(() => {
    const saved = localStorage.getItem(cliKey) || "";
    return ["codex", "claude", "copilot"].includes(saved) ? saved : "";
  });
  const [creatingVm, setCreatingVm] = useState(false);
  const [environmentBusy, setEnvironmentBusy] = useState(false);
  const [status, setStatus] = useState<AgentStatus | null>(null),
    [error, setError] = useState(""),
    [stale, setStale] = useState(false);
  const [editor, setEditor] = useState<AgentTask | null>(null),
    [creating, setCreating] = useState(false),
    [createError, setCreateError] = useState("");
  const [selection, setSelection] = useState({
      ids: [] as string[],
      initialized: false,
    }),
    [filter, setFilter] = useState("all"),
    [search, setSearch] = useState(""),
    [starter, setStarter] = useState<AgentTask>(blank);
  const selected = selection.ids;
  const alive = useRef(true),
    revision = useRef(0);
  const [repositoryDirty, setRepositoryDirty] = useState(false);
  const dirty =
    repositoryDirty || !!editor || !!(starter.input.trim() || starter.model);
  useEffect(() => {
    onUnsavedChange(dirty);
    return () => onUnsavedChange(false);
  }, [dirty, onUnsavedChange]);
  useEffect(() => {
    if (!dirty) return;
    const handler = (e: BeforeUnloadEvent) => {
      e.preventDefault();
      e.returnValue = "";
    };
    window.addEventListener("beforeunload", handler);
    return () => window.removeEventListener("beforeunload", handler);
  }, [dirty]);
  const refresh = useCallback(async () => {
    if (!vmId || vm?.state !== "running") {
      setStatus(null);
      return;
    }
    const version = ++revision.current;
    try {
      const next = await agentRequest<AgentStatus>(
        "",
        undefined,
        undefined,
        vmId,
      );
      if (!alive.current || version !== revision.current) return;
      setStatus(next);
      setStale(false);
      setError("");
      setSelection((selection) => {
        const kept = selection.ids.filter((id) =>
          next.agents.some((a) => a.id === id),
        );
        if (kept.length || selection.initialized)
          return { ...selection, ids: kept };
        const initial = next.agents
          .filter((a) => !a.closed)
          .slice(0, 2)
          .map((a) => a.id);
        return { ids: initial, initialized: initial.length > 0 };
      });
    } catch (e) {
      if (alive.current && version === revision.current) {
        setStale(true);
        setError(e instanceof Error ? e.message : String(e));
      }
    }
  }, [vmId, vm?.state]);
  useEffect(() => {
    alive.current = true;
    revision.current++;
    let cancelled = false;
    let timer = 0;
    const poll = async () => {
      if (cancelled) return;
      if (!document.hidden) await refresh();
      if (!cancelled) timer = window.setTimeout(() => void poll(), 2500);
    };
    void poll();
    return () => {
      cancelled = true;
      alive.current = false;
      clearTimeout(timer);
    };
  }, [refresh]);
  const identityMatches =
    !!status?.controllerId &&
    !!host?.platform?.controllerId &&
    status.controllerId === host.platform.controllerId &&
    status.vmId === vmId;
  const canControl = identityMatches && !stale && vm?.state === "running";
  const driver = status?.drivers.find((d) => d.id === cli);
  const cliReady =
    vm?.state === "running" &&
    status?.controllerId === controllerId &&
    status?.vmId === vmId &&
    !!cli &&
    !!driver?.available &&
    (!["codex", "claude"].includes(cli) || !!driver.authenticated) &&
    status?.preparation?.state !== "installing";
  const canLaunch =
    canControl && cliReady && status?.project?.state === "ready";
  const openEditor = () => {
    const next = starter.input.trim() ? { ...starter } : blank();
    next.cli = cli;
    setEditor(next);
    setCreateError("");
  };
  const create = async (e: FormEvent, request: AgentTask) => {
    e.preventDefault();
    if (!request.input.trim() || creating || !canLaunch) return;
    setCreating(true);
    setCreateError("");
    try {
      const agent = await agentRequest<AgentSession>(
        "/tasks",
        {
          id: request.id,
          input: request.input,
          cli,
          model: request.model,
        },
        status?.controllerId,
        vmId,
      );
      revision.current++;
      setStatus((old) =>
        old
          ? {
              ...old,
              agents: [...old.agents.filter((a) => a.id !== agent.id), agent],
            }
          : old,
      );
      setSelection(({ ids }) => ({
        ids: [...ids.filter((id) => id !== agent.id).slice(-3), agent.id],
        initialized: true,
      }));
      setFilter("all");
      setEditor(null);
      setStarter(blank());
      await refresh();
    } catch (e) {
      setCreateError(e instanceof Error ? e.message : String(e));
    } finally {
      setCreating(false);
    }
  };
  const environmentAction = async (
    kind: "prepare" | "login" | "start",
    chosen = cli,
  ) => {
    if (environmentBusy) return;
    setEnvironmentBusy(true);
    setCreateError("");
    try {
      if (kind === "start") {
        if (vm?.state === "paused") await resumeVm(vmId);
        else await startVm(vmId);
      } else if (kind === "prepare") {
        const preparation = await agentRequest<
          NonNullable<AgentStatus["preparation"]>
        >("/prepare", { cli: chosen }, status?.controllerId, vmId);
        setStatus((old) => (old ? { ...old, preparation } : old));
      } else {
        const agent = await agentRequest<AgentSession>(
          "/login",
          { id: crypto.randomUUID(), cli: chosen },
          status?.controllerId,
          vmId,
        );
        setSelection({ ids: [agent.id], initialized: true });
      }
      if (kind !== "start") await refresh();
    } catch (e) {
      setCreateError(e instanceof Error ? e.message : String(e));
    } finally {
      setEnvironmentBusy(false);
    }
  };
  const updateAgent = useCallback((agent: AgentSession, mutation = false) => {
    if (mutation) revision.current++;
    setStatus((old) =>
      old
        ? {
            ...old,
            agents: old.agents.map((a) => (a.id === agent.id ? agent : a)),
          }
        : old,
    );
  }, []);
  const agents = status?.agents || [];
  const visible = agents.filter(
    (a) =>
      (filter === "all"
        ? !a.closed
        : filter === "closed"
          ? a.closed
          : filter === a.state) &&
      [a.name, a.role, a.cwd, a.kind]
        .join(" ")
        .toLowerCase()
        .includes(search.toLowerCase()),
  );
  const toggle = (id: string) =>
    setSelection(({ ids }) => ({
      ids: ids.includes(id)
        ? ids.filter((x) => x !== id)
        : [...ids.slice(-3), id],
      initialized: true,
    }));
  const selectedAgents = selected
    .map((id) => agents.find((a) => a.id === id))
    .filter((a): a is AgentSession => !!a)
    .slice(0, 4);
  return (
    <Screen
      view="ai-sandbox"
      actions={
        <>
          {agents.length > 0 && (
            <button
              className="btn primary"
              onClick={openEditor}
              disabled={!canLaunch || creating}
            >
              <Plus aria-hidden="true" />
              {t("New task", "새 작업")}
            </button>
          )}
        </>
      }
    >
      <div
        className={`agent-manager${status?.project?.state !== "ready" || !cliReady ? " am-setting-up" : ""}`}
      >
        <AgentEnvironment
          vms={vms}
          vmId={vmId}
          cli={cli}
          status={status}
          busy={creating || environmentBusy}
          canControl={canControl}
          onVm={onVm}
          onCli={(value) => {
            localStorage.setItem(cliKey, value);
            setCli(value);
            setStarter((old) => ({ ...old, cli: value, model: "" }));
            setEditor((old) => (old ? { ...old, cli: value, model: "" } : old));
          }}
          onCreate={() => setCreatingVm(true)}
          onStart={() => void environmentAction("start")}
          onPrepare={() => void environmentAction("prepare")}
          onLogin={(value) => void environmentAction("login", value)}
          onRefresh={() => void refresh()}
        />
        {vm?.state === "running" && (
          <AgentRepository
            project={status?.project || null}
            github={status?.github}
            connectionUnavailable={stale}
            vmId={vmId}
            controllerId={status?.controllerId || controllerId}
            canControl={canControl && !environmentBusy}
            onSaved={refresh}
            onDirty={setRepositoryDirty}
            onPrepareGitHub={() => void environmentAction("prepare", "github")}
            preparingGitHub={
              status?.preparation?.state === "installing" &&
              status.preparation.cli === "github"
            }
          />
        )}
        {createError &&
          !editor &&
          !creatingVm &&
          (!canLaunch || agents.length > 0) && (
            <p className="am-error" role="alert">
              {createError}
            </p>
          )}

        {error && (
          <div className="am-connection-error" role="alert">
            <div>
              <strong>
                {t(
                  "Could not connect to this MicroVM",
                  "이 MicroVM에 연결할 수 없다",
                )}
              </strong>
              <button className="btn" onClick={() => void refresh()}>
                <RefreshCw aria-hidden="true" />
                {t("Retry connection", "연결 다시 확인")}
              </button>
            </div>
            <details>
              <summary>{t("Error details", "오류 내용")}</summary>
              <code>{error}</code>
            </details>
          </div>
        )}
        {status && host?.platform?.controllerId && !identityMatches && (
          <p className="am-error" role="alert">
            {t(
              "The native controller does not match this host. Reload this host’s dashboard before sending commands.",
              "연결된 호스트와 에이전트 실행 호스트가 다르다. 이 호스트의 대시보드를 다시 열어 확인한다.",
            )}
          </p>
        )}
        {!agents.length ? (
          <div className="am-start">
            {cliReady ? (
              <AgentTaskForm
                inline
                draft={{ ...starter, cli }}
                onChange={setStarter}
                onSubmit={(e) => void create(e, starter)}
                busy={creating}
                canControl={canLaunch}
                drivers={status.drivers}
                error={createError}
              />
            ) : (
              <div className="am-runtime-unavailable">
                <h2>{t("Ready when you are", "작업 준비")}</h2>
                <p>
                  {status
                    ? t(
                        "Choose a coding CLI above to start a task in this MicroVM.",
                        "위에서 코딩 CLI를 선택하고 계정을 연결하면 작업을 시작할 수 있다.",
                      )
                    : t(
                        "Choose a MicroVM and CLI to start.",
                        "MicroVM과 사용할 CLI를 선택한다.",
                      )}
                </p>
              </div>
            )}
          </div>
        ) : (
          <div className="am-shell">
            <aside
              className="am-agent-list"
              aria-label={t("Agents", "에이전트")}
            >
              <div className="am-list-head">
                <h2>
                  {t("Tasks", "작업")}
                  <span>{agents.filter((a) => !a.closed).length}</span>
                </h2>
                <label className="am-filter">
                  <span className="sr-only">
                    {t("Agent status filter", "에이전트 상태 필터")}
                  </span>
                  <select
                    value={filter}
                    onChange={(e) => setFilter(e.target.value)}
                  >
                    {["all", "working", "blocked", "idle", "closed"]
                      .filter(
                        (state) =>
                          state === "all" ||
                          state === filter ||
                          agents.some((a) =>
                            state === "closed"
                              ? a.closed
                              : !a.closed && a.state === state,
                          ),
                      )
                      .map((state) => (
                        <option key={state} value={state}>
                          {state === "all"
                            ? t("Active tasks", "진행 중인 작업")
                            : t(...labels[state])}
                        </option>
                      ))}
                  </select>
                  <ChevronDown aria-hidden="true" />
                </label>
              </div>
              {agents.length > 4 && (
                <label className="am-search">
                  <span className="sr-only">
                    {t("Find an agent", "에이전트 검색")}
                  </span>
                  <input
                    value={search}
                    onChange={(e) => setSearch(e.target.value)}
                    placeholder={t("Find a task", "작업 검색")}
                  />
                </label>
              )}
              {visible.map((agent) => (
                <button
                  key={agent.id}
                  className={`am-agent ${selected.includes(agent.id) ? "selected" : ""}`}
                  aria-pressed={selected.includes(agent.id)}
                  onClick={() => toggle(agent.id)}
                >
                  <AgentBrandIcon kind={agent.kind} />
                  <span className="am-agent-copy">
                    <strong title={agent.name}>{agent.name}</strong>
                    <small className="am-agent-provider">
                      {agentName(agent.kind)}
                    </small>
                  </span>
                  <span className="am-selection-mark" aria-hidden="true">
                    {selected.includes(agent.id) && <Check />}
                  </span>
                  <span className="am-agent-state">
                    <span
                      className={`am-dot ${agent.state}`}
                      aria-hidden="true"
                    />
                    {t(...(labels[agent.state] || labels.unknown))}
                  </span>
                </button>
              ))}
              {!visible.length && (
                <p className="am-list-hint">
                  {t(
                    "No agents match this filter.",
                    "조건에 맞는 에이전트가 없다.",
                  )}
                </p>
              )}
            </aside>
            <section
              className="am-sessions"
              aria-label={t("Agent sessions", "에이전트 세션")}
            >
              <div
                className={`am-panels ${selectedAgents.length > 1 ? "split" : ""}`}
              >
                {selectedAgents.map((agent) => (
                  <AgentPane
                    key={agent.id}
                    agent={agent}
                    vm={vm}
                    controllerId={status?.controllerId || ""}
                    vmId={vmId}
                    canControl={canControl}
                    onUpdate={updateAgent}
                  />
                ))}
                {!selectedAgents.length && (
                  <div className="am-no-selection">
                    <PanelLeftClose aria-hidden="true" />
                    {t(
                      "Select an agent to open its session.",
                      "에이전트를 선택하면 세션을 확인할 수 있다.",
                    )}
                  </div>
                )}
              </div>
            </section>
          </div>
        )}
        {creatingVm && (
          <AppDialog
            label={t("New Sandbox MicroVM", "작업용 MicroVM 만들기")}
            onClose={() => setCreatingVm(false)}
            busy={environmentBusy}
          >
            <header className="am-vm-create-head">
              <h2>{t("New Sandbox MicroVM", "작업용 MicroVM 만들기")}</h2>
              <button
                type="button"
                className="icon-btn"
                aria-label={t("Close", "닫기")}
                title={t("Close", "닫기")}
                disabled={environmentBusy}
                onClick={() => setCreatingVm(false)}
              >
                <X aria-hidden="true" />
              </button>
            </header>
            {createError && (
              <p className="am-error" role="alert">
                {createError}
              </p>
            )}
            <CreateVm
              initialResources={{ cpu: 1, ram: 1024, diskGb: 3 }}
              onCancel={() => setCreatingVm(false)}
              onError={setCreateError}
              onCreated={(value) => {
                onCreated(value);
                setCreatingVm(false);
              }}
            />
          </AppDialog>
        )}
        {editor && (
          <AppDialog
            label={t("New task", "새 작업")}
            busy={creating}
            onClose={() => setEditor(null)}
          >
            <AgentTaskForm
              draft={{ ...editor, cli }}
              onChange={setEditor}
              onSubmit={(e) => void create(e, editor)}
              onCancel={() => setEditor(null)}
              busy={creating}
              canControl={canLaunch}
              drivers={status?.drivers || []}
              error={createError}
            />
          </AppDialog>
        )}
      </div>
    </Screen>
  );
}

function AgentPane({
  agent,
  vm,
  vmId,
  controllerId,
  canControl,
  onUpdate,
}: {
  agent: AgentSession;
  vm: VmResponse | undefined;
  vmId: string;
  controllerId: string;
  canControl: boolean;
  onUpdate: (agent: AgentSession, mutation?: boolean) => void;
}) {
  const { t } = useI18n();
  const [automation, setAutomation] = useState(false);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const action = async (body: object, path = "input") => {
    if (busy || !canControl) return;
    setBusy(true);
    setError("");
    try {
      const value = await agentRequest<AgentTerminal | AgentSession>(
        `/${encodeURIComponent(agent.id)}/${path}`,
        body,
        controllerId,
        vmId,
      );
      if ("agent" in value) {
        onUpdate(value.agent, true);
      } else onUpdate(value, true);
    } catch (e) {
      setError(
        `${e instanceof Error ? e.message : String(e)} ${t("Inspect the terminal before resending.", "다시 보내기 전에 터미널을 확인한다.")}`,
      );
    } finally {
      setBusy(false);
    }
  };
  const close = () => {
    if (
      window.confirm(
        t(
          `Close ${agent.name}? Its running CLI will stop.`,
          `‘${agent.name}’ 세션을 종료할까? 실행 중인 CLI가 중지된다.`,
        ),
      )
    )
      void action({}, "close");
  };
  return (
    <section className="am-pane" aria-label={agent.name}>
      {automation && (
        <AgentAutomation
          agent={agent}
          vm={vm}
          controllerId={controllerId}
          canControl={canControl}
          onClose={() => setAutomation(false)}
        />
      )}
      <header>
        <div className="am-pane-heading">
          <AgentBrandIcon kind={agent.kind} />
          <div className="am-pane-heading-copy">
            <h3 title={agent.name}>{agent.name}</h3>
            <span>
              {agentName(agent.kind)}
              {agent.model ? ` · ${agent.model}` : ""}
            </span>
          </div>
        </div>
        <div className="am-pane-header-actions">
          <span className={`am-session-state ${agent.state}`}>
            <span className={`am-dot ${agent.state}`} aria-hidden="true" />
            {t(...(labels[agent.state] || labels.unknown))}
          </span>
          <ActionMenu
            label={`${agent.name} ${t("session actions", "세션 메뉴")}`}
          >
            <button
              type="button"
              disabled={busy || !canControl || agent.closed}
              onClick={() => void action({ key: "ctrl+c" })}
            >
              <Square aria-hidden="true" />
              {t("Interrupt task", "작업 중단")}
            </button>
            {agent.role !== "authentication" && (
              <button
                type="button"
                disabled={!canControl || busy}
                onClick={() => setAutomation(true)}
              >
                {t("Automation and preview", "자동화·미리보기")}
              </button>
            )}
            <button
              type="button"
              className="danger"
              disabled={busy || !canControl || agent.closed}
              onClick={close}
            >
              {t("End session", "세션 종료")}
            </button>
          </ActionMenu>
        </div>
      </header>
      <div className="am-pane-meta">
        <span title={agent.cwd}>
          <Folder aria-hidden="true" />
          {agent.cwd}
        </span>
        {agent.role !== "authentication" && (
          <a
            className="am-project-editor"
            href={
              canControl
                ? agentEditorUrl(vmId, agent.id, controllerId)
                : undefined
            }
            target="_blank"
            rel="noreferrer noopener"
            aria-disabled={!canControl}
          >
            <CodeXml />
            {t("Web VS Code", "Web VS Code")}
          </a>
        )}
      </div>
      {(error || agent.launchError) && (
        <p className="am-pane-error" role="alert">
          {error || agent.launchError}
        </p>
      )}
      <AgentTerminalView
        id={agent.id}
        name={agent.name}
        vmId={vmId}
        controllerId={controllerId}
        closed={agent.closed}
        canControl={canControl && !busy}
      />
      {agent.hasPendingTask && (
        <div className="am-attention am-pending-task">
          <span>
            {t(
              "Finish CLI startup in the terminal, then send the waiting task.",
              "터미널에서 CLI 시작 절차를 마친 뒤 준비한 작업을 전달한다.",
            )}
          </span>
          <button
            className="btn"
            disabled={
              busy || !canControl || !["idle", "done"].includes(agent.state)
            }
            onClick={() => void action({}, "resume")}
          >
            {t("Send waiting task", "준비한 작업 전달")}
          </button>
        </div>
      )}
      {agent.taskDeliveryUncertain && (
        <p className="am-attention">
          {t(
            "Task delivery could not be confirmed. Inspect the terminal before sending it again.",
            "작업 전달 여부를 확인하지 못했다. 다시 보내기 전에 터미널을 확인한다.",
          )}
        </p>
      )}
    </section>
  );
}

function ActionMenu({
  label,
  children,
}: {
  label: string;
  children: ReactNode;
}) {
  const menu = useRef<HTMLDetailsElement>(null);
  useEffect(() => {
    const close = (e: PointerEvent) => {
      if (menu.current && !menu.current.contains(e.target as Node))
        menu.current.open = false;
    };
    document.addEventListener("pointerdown", close);
    return () => document.removeEventListener("pointerdown", close);
  }, []);
  return (
    <details
      className="am-more"
      ref={menu}
      onKeyDown={(e) => {
        if (e.key === "Escape") {
          e.preventDefault();
          e.stopPropagation();
          if (menu.current) {
            menu.current.open = false;
            menu.current.querySelector("summary")?.focus();
          }
        }
      }}
    >
      <summary className="icon-btn" aria-label={label} title={label}>
        <MoreHorizontal aria-hidden="true" />
      </summary>
      <div
        className="am-more-content"
        onClick={(e) => {
          if ((e.target as Element).closest("button") && menu.current)
            menu.current.open = false;
        }}
      >
        {children}
      </div>
    </details>
  );
}
