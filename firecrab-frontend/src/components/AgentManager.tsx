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
  Pencil,
  Square,
  RefreshCw,
  MoreHorizontal,
  ChevronDown,
  Folder,
  CodeXml,
  Columns3,
  SquareTerminal,
  Server,
  Link,
  ListTodo,
  Play,
  Globe,
  Eye,
  Settings,
  ChevronRight,
  Package,
  FileText,
} from "lucide-react";
import { useI18n } from "../i18n";
import { AGENT_STATE_LABELS as labels, agentName } from "../lib/agentLabels";
import { useHostStatus } from "../hostStatusContext";
import {
  agentRequest,
  agentEditorUrl,
  AgentRequestError,
  agentModelLabel,
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
import AgentTaskBoard from "./AgentTaskBoard";
import AgentEnvironment from "./AgentEnvironment";
import AgentRepository from "./AgentRepository";
import AgentFileTree from "./AgentFileTree";
import AgentAutomation from "./AgentAutomation";
import AgentWorkspaceCreate from "./AgentWorkspaceCreate";
import AgentWorkspaceName from "./AgentWorkspaceName";
import AgentSkillStore from "./AgentSkillStore";
import AgentRules from "./AgentRules";
import AgentWorkspaceApp, {
  type WorkspaceAppProgress,
} from "./AgentWorkspaceApp";
import type { WorkspaceSetup } from "../api/workspaces";
import { startVm, resumeVm } from "../api/client";
import { vmDetailsHash } from "../navigation";
import type { VmResponse } from "../bindings";
import "../styles/agent-manager.css";
import "../styles/agent-quick-start.css";

type WorkspacePage =
  | "workspace"
  | "connections"
  | "tasks"
  | "verify"
  | "address"
  | "preview"
  | "rules"
  | "skills"
  | "settings";

type Props = {
  vms: VmResponse[];
  onUnsavedChange: (dirty: boolean) => void;
};
const blank = (): AgentTask => ({
  id: crypto.randomUUID(),
  input: "",
  cli: "",
  model: "",
  effort: "",
});

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
  // A rename shows at once. It applies only while the VM list still has the
  // old name, so the next refresh (or a later rename elsewhere) wins.
  const [renamed, setRenamed] = useState<
    Record<string, { from: string; to: string }>
  >({});
  const unsaved = useRef(false);
  const vms = [
    ...props.vms,
    ...created.filter((v) => !props.vms.some((p) => p.id === v.id)),
  ]
    .filter((v) => !v.purpose || v.purpose === "instance")
    .map((v) =>
      renamed[v.id]?.from === v.name ? { ...v, name: renamed[v.id].to } : v,
    );
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
        t("Leave the unsent draft?", "입력한 내용을 보내지 않고 이동할까요?"),
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
      onRenamed={(id, name) => {
        const from = props.vms.find((v) => v.id === id)?.name;
        if (from !== undefined)
          setRenamed((old) => ({ ...old, [id]: { from, to: name } }));
      }}
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
  onRenamed,
  onUnsavedChange,
}: Props & {
  controllerId: string;
  vmId: string;
  onVm: (id: string) => void;
  onCreated: (vm: VmResponse) => void;
  onRenamed: (vmId: string, name: string) => void;
}) {
  const { t } = useI18n();
  const host = useHostStatus();
  const vm = vms.find((v) => v.id === vmId);
  const cliKey = `firecrab.agentCli.${controllerId}.${vmId}`;
  const viewKey = `firecrab.agentView.${controllerId}.${vmId}`;
  const [view, setView] = useState<"board" | "terminals">(() =>
    localStorage.getItem(viewKey) === "terminals" ? "terminals" : "board",
  );
  const changeView = (next: "board" | "terminals") => {
    localStorage.setItem(viewKey, next);
    setView(next);
  };

  // Written by the earlier one-CLI flow; new workspaces save no choice.
  const [savedCli] = useState(() => {
    const saved = localStorage.getItem(cliKey) || "";
    return ["codex", "claude", "grok", "copilot"].includes(saved) ? saved : "";
  });
  const pageKey = `firecrab.workspacePage.${controllerId}.${vmId}`;
  const [page, setPage] = useState<WorkspacePage>(() => {
    const saved = localStorage.getItem(pageKey) as WorkspacePage;
    return [
      "workspace",
      "connections",
      "tasks",
      "verify",
      "address",
      "preview",
      "rules",
      "skills",
      "settings",
    ].includes(saved)
      ? saved
      : vm
        ? localStorage.getItem(cliKey)
          ? "tasks"
          : "connections"
        : "workspace";
  });
  const [appVisited, setAppVisited] = useState(
    ["verify", "address", "preview"].includes(page),
  );
  const [appProgress, setAppProgress] = useState<WorkspaceAppProgress>({
    verified: false,
    fixedVerified: false,
    previewVerified: false,
  });
  useEffect(() => {
    localStorage.setItem(pageKey, page);
  }, [pageKey, page]);
  const navigate = (next: WorkspacePage) => {
    if (["verify", "address", "preview"].includes(next)) setAppVisited(true);
    setPage(next);
    localStorage.setItem(pageKey, next);
  };
  const [setupJob, setSetupJob] = useState<WorkspaceSetup | null>(null);
  const [setupDirty, setSetupDirty] = useState(false),
    [appDirty, setAppDirty] = useState(false);
  const [environmentBusy, setEnvironmentBusy] = useState(false);
  // Coding CLIs checked for installation. The VM runs one preparation at a
  // time and reports only the latest result, so CLIs install one after
  // another and each failure is kept here before the next install hides it.
  const [installs, setInstalls] = useState({
    queue: [] as string[],
    current: "",
    failures: {} as Record<string, string>,
  });
  const posting = useRef(false);
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
    repositoryDirty ||
    setupDirty ||
    appDirty ||
    !!editor ||
    !!(starter.input.trim() || starter.model || starter.effort);
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
      setInstalls((old) => {
        const done = next.preparation;
        if (!old.current || done?.state === "installing") return old;
        const failed =
          done?.cli === old.current &&
          ["failed", "interrupted"].includes(done.state);
        return {
          ...old,
          current: "",
          failures: failed
            ? { ...old.failures, [old.current]: done.error || "" }
            : old.failures,
        };
      });
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
  // The VM knows which CLIs are installed and signed in. Prefer a saved choice
  // that is signed in, then any signed-in CLI, so tasks are not blocked by a
  // choice this browser cannot change any more. An installed CLI still names
  // the next step (sign in) when none is signed in.
  const codingDrivers =
    status?.drivers.filter((d) =>
      ["codex", "claude", "grok", "copilot"].includes(d.id),
    ) || [];
  const signedIn = codingDrivers.filter((d) => d.available && d.authenticated);
  const cli =
    signedIn.find((d) => d.id === savedCli)?.id ||
    signedIn[0]?.id ||
    savedCli ||
    codingDrivers.find((d) => d.available)?.id ||
    "";
  const driver = status?.drivers.find((d) => d.id === cli);
  const cliReady =
    vm?.state === "running" &&
    status?.controllerId === controllerId &&
    status?.vmId === vmId &&
    !!cli &&
    !!driver?.available &&
    (!["codex", "claude", "grok"].includes(cli) || !!driver.authenticated) &&
    status?.preparation?.state !== "installing";
  const canLaunch =
    canControl && cliReady && status?.project?.state === "ready";
  // The first-task form follows the VM's CLI. A model or effort chosen for
  // another CLI does not carry over when that CLI changes.
  useEffect(() => {
    setStarter((old) =>
      old.cli && old.cli !== cli ? { ...old, cli, model: "", effort: "" } : old,
    );
  }, [cli]);
  // Name the first thing that still blocks a task, not everything that could.
  // The repository comes first, as on the Connect accounts page.
  const blocker = (() => {
    if (canLaunch) return null;
    const name = driver?.name || cli;
    if (status?.project?.state !== "ready")
      return t(
        "Connect a repository in Connect accounts to start coding.",
        "계정 연결에서 저장소를 연결하면 작업을 시작할 수 있어요.",
      );
    if (status?.preparation?.state === "installing")
      return t(
        "Coding tools are still being installed in this VM.",
        "이 VM에 AI 도구를 설치하고 있어요.",
      );
    if (!cli)
      return t(
        "Install a coding CLI in Connect accounts to start coding.",
        "계정 연결에서 코딩 CLI를 설치하면 작업을 시작할 수 있어요.",
      );
    if (!driver?.available)
      return t(
        `Prepare ${name} in Connect accounts to start coding.`,
        `계정 연결에서 ${name}를 준비하면 작업을 시작할 수 있어요.`,
      );
    if (["codex", "claude", "grok"].includes(cli) && !driver.authenticated)
      return t(
        `Sign in to ${name} in Connect accounts to start coding.`,
        `계정 연결에서 ${name}에 로그인하면 작업을 시작할 수 있어요.`,
      );
    return null;
  })();
  const openEditor = () => {
    const next = starter.input.trim() ? { ...starter } : blank();
    next.cli = cli;
    setEditor(next);
    setCreateError("");
  };
  const create = async (e: FormEvent, request: AgentTask) => {
    e.preventDefault();
    const chosenCli = request.cli || cli;
    const chosenDriver = status?.drivers.find(
      (driver) => driver.id === chosenCli,
    );
    if (
      !request.input.trim() ||
      creating ||
      !canLaunch ||
      !chosenDriver?.available ||
      (["codex", "claude", "grok"].includes(chosenCli) &&
        !chosenDriver.authenticated)
    )
      return;
    setCreating(true);
    setCreateError("");
    try {
      const agent = await agentRequest<AgentSession>(
        "/tasks",
        {
          id: request.id,
          input: request.input,
          cli: chosenCli,
          model: request.model,
          effort: request.effort,
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
        changeView("terminals");
        navigate("tasks");
      }
      if (kind !== "start") await refresh();
    } catch (e) {
      setCreateError(e instanceof Error ? e.message : String(e));
    } finally {
      setEnvironmentBusy(false);
    }
  };
  const preparation = status?.preparation;
  const controller = status?.controllerId;
  useEffect(() => {
    const next = installs.queue[0];
    if (
      !next ||
      installs.current ||
      preparation?.state === "installing" ||
      !canControl ||
      posting.current
    )
      return;
    posting.current = true;
    void (async () => {
      try {
        const started = await agentRequest<
          NonNullable<AgentStatus["preparation"]>
        >("/prepare", { cli: next }, controller, vmId);
        posting.current = false;
        if (!alive.current) return;
        // A poll sent before this install began may still report the old state.
        revision.current++;
        setStatus((old) => (old ? { ...old, preparation: started } : old));
        setInstalls((old) => ({
          ...old,
          queue: old.queue.filter((id) => id !== next),
          current: next,
        }));
      } catch (e) {
        posting.current = false;
        if (!alive.current) return;
        // Busy means another preparation is running; this CLI keeps its turn.
        if (!(e instanceof AgentRequestError && e.status === 409))
          setInstalls((old) => ({
            ...old,
            queue: old.queue.filter((id) => id !== next),
            failures: {
              ...old.failures,
              [next]: e instanceof Error ? e.message : String(e),
            },
          }));
      }
      await refresh();
    })();
  }, [installs, preparation, canControl, controller, vmId, refresh]);
  const install = (clis: string[]) =>
    setInstalls((old) => ({
      ...old,
      queue: [
        ...old.queue,
        ...clis.filter((id) => !old.queue.includes(id) && id !== old.current),
      ],
      failures: Object.fromEntries(
        Object.entries(old.failures).filter(([id]) => !clis.includes(id)),
      ),
    }));
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
  const tasks = agents.filter((agent) => agent.role !== "authentication");
  const working = tasks.filter(
    (agent) =>
      !agent.closed &&
      agent.state === "working" &&
      !agent.hasPendingTask &&
      !agent.taskDeliveryUncertain,
  ).length;
  const openTask = (id: string) => {
    setSelection({ ids: [id], initialized: true });
    changeView("terminals");
  };
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
          {page === "tasks" && (
            <button
              type="button"
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
      <div className="am-quick-layout">
        <main
          className={`am-quick-main ${page === "tasks" ? "am-task-page" : ""}`}
        >
          {!vm && (
            <div hidden={page !== "workspace" && page !== "settings"}>
              <AgentWorkspaceCreate
                controllerId={controllerId}
                settings={page === "settings"}
                onProgress={setSetupJob}
                onDirty={setSetupDirty}
                onCreated={(value) => {
                  localStorage.setItem(
                    `firecrab.workspacePage.${controllerId}.${value.id}`,
                    "connections",
                  );
                  onCreated(value);
                }}
              />
            </div>
          )}
          {vm && (page === "workspace" || page === "settings") && (
            <section className="awc">
              <small>{t("Workspace", "작업 공간")}</small>
              <AgentWorkspaceName
                vm={vm}
                onRenamed={(name) => onRenamed(vm.id, name)}
              />
              <p>
                {t(
                  "Continue with accounts, AI tasks and your running app.",
                  "계정을 연결한 뒤 AI에게 작업을 맡겨 보세요. 완성한 앱도 여기서 실행할 수 있어요.",
                )}
              </p>
              <dl className="awc-specs">
                <div>
                  <dt>CPU</dt>
                  <dd>{vm.cpu} vCPU</dd>
                </div>
                <div>
                  <dt>RAM</dt>
                  <dd>{vm.ram / 1024} GiB</dd>
                </div>
                <div>
                  <dt>{t("State", "상태")}</dt>
                  <dd>{vm.state}</dd>
                </div>
              </dl>
              {page === "settings" && (
                <p>
                  <a className="btn" href={vmDetailsHash(vm.id)}>
                    {t("Open VM settings", "VM 설정 열기")}
                    <ChevronRight />
                  </a>
                </p>
              )}
              <button
                className="btn primary"
                onClick={() => navigate("connections")}
              >
                {t("Connect accounts", "계정 연결하기")}
                <ChevronRight />
              </button>
            </section>
          )}
          {vm && page === "rules" && (
            <AgentRules
              vmId={vm.id}
              controllerId={controllerId}
              drivers={status?.drivers || []}
              onNavigate={navigate}
            />
          )}
          {page === "skills" && (
            <AgentSkillStore vmId={vm?.id || ""} controllerId={controllerId} />
          )}
          {vm && page === "tasks" && status?.project?.state === "ready" && (
            <AgentFileTree
              key={`${vmId}:${status.project.repository}:${status.project.path}`}
              vmId={vmId}
              project={status.project}
            />
          )}
          <div
            hidden={!vm || !["connections", "tasks"].includes(page)}
            className={`agent-manager${status?.project?.state !== "ready" || !cliReady ? " am-setting-up" : ""}`}
          >
            <header
              hidden={page !== "connections"}
              className="am-connections-page am-quick-heading"
            >
              <small>{t("Connect accounts", "계정 연결")}</small>
              <h2>
                {t(
                  "Connect a repository, then choose your AI",
                  "저장소를 연결하고 사용할 AI를 골라 주세요",
                )}
              </h2>
              <p>
                {t(
                  "Sign in inside this VM. You can inspect each login command in its terminal.",
                  "이 VM 안에서 로그인해요. 로그인 명령과 결과는 터미널에서 확인할 수 있어요.",
                )}
              </p>
            </header>
            <div
              hidden={
                page !== "connections" && status?.project?.state !== "ready"
              }
            >
              {vm?.state === "running" && (
                <AgentRepository
                  project={status?.project || null}
                  github={status?.github}
                  compact={page !== "connections"}
                  connectionUnavailable={stale}
                  vmId={vmId}
                  controllerId={status?.controllerId || controllerId}
                  canControl={canControl && !environmentBusy}
                  onSaved={refresh}
                  onDirty={setRepositoryDirty}
                  onPrepareGitHub={() =>
                    void environmentAction("prepare", "github")
                  }
                  preparingGitHub={
                    status?.preparation?.state === "installing" &&
                    status.preparation.cli === "github"
                  }
                />
              )}
            </div>
            <div hidden={page !== "connections"} className="am-connections-cli">
              <AgentEnvironment
                compact
                vms={vms}
                vmId={vmId}
                status={status}
                busy={creating || environmentBusy}
                canControl={canControl}
                queue={installs.queue}
                failures={installs.failures}
                onVm={onVm}
                onCreate={() => onVm("")}
                onStart={() => void environmentAction("start")}
                onInstall={install}
                onLogin={(value) => void environmentAction("login", value)}
                onRefresh={() => void refresh()}
              />
            </div>
            {page === "tasks" && blocker && (
              <div className="am-quick-notice">
                <span>{blocker}</span>
                <button className="btn" onClick={() => navigate("connections")}>
                  {t("Connect accounts", "계정 연결")}
                </button>
              </div>
            )}
            {createError && !editor && (!canLaunch || agents.length > 0) && (
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
                      "이 VM에 연결할 수 없어요",
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
                  "다른 호스트의 실행 도구에 연결됐어요. 현재 호스트의 대시보드를 다시 열어 주세요.",
                )}
              </p>
            )}
            <div
              hidden={page !== "tasks"}
              className={`am-workflow ${!agents.length ? "am-empty" : ""}`}
            >
              <header className="am-workflow-head">
                <div className="am-workflow-heading">
                  <h2>
                    {t("Multi-agent workspace", "멀티 에이전트 작업 공간")}
                  </h2>
                  <p>
                    {t(
                      "Tasks call agents on demand. Follow their work on the board.",
                      "AI에게 할 일을 적어 주세요. 진행 상황은 보드와 터미널에서 확인할 수 있어요.",
                    )}
                  </p>
                </div>
                <div className="am-workflow-actions">
                  <button
                    type="button"
                    className="icon-btn"
                    aria-label={t("Check environment", "환경 다시 확인")}
                    disabled={environmentBusy}
                    onClick={() => void refresh()}
                  >
                    <RefreshCw />
                  </button>
                  {status && (
                    <span className="am-worker-count">
                      <span
                        className={`am-dot ${stale || !identityMatches || !working ? "idle" : "working"}`}
                        aria-hidden="true"
                      />
                      {stale || !identityMatches
                        ? t("Checking connection", "연결 확인 중")
                        : t(
                            `${working} ${working === 1 ? "agent" : "agents"} working`,
                            `에이전트 ${working}개 실행 중`,
                          )}
                    </span>
                  )}
                  {agents.length > 0 && (
                    <label className="am-filter">
                      <span className="sr-only">
                        {t("Agent status filter", "에이전트 상태 필터")}
                      </span>
                      <select
                        value={filter}
                        onChange={(e) => setFilter(e.target.value)}
                      >
                        {[
                          "all",
                          "working",
                          "blocked",
                          "idle",
                          "done",
                          "failed",
                          "disconnected",
                          "unknown",
                          "closed",
                        ]
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
                                ? t("Current tasks", "현재 작업")
                                : t(...labels[state])}
                            </option>
                          ))}
                      </select>
                      <ChevronDown aria-hidden="true" />
                    </label>
                  )}
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
                  <nav
                    className="am-workflow-views"
                    aria-label={t("Workspace view", "작업 화면")}
                  >
                    <button
                      type="button"
                      aria-pressed={view === "board"}
                      onClick={() => changeView("board")}
                    >
                      <Columns3 aria-hidden="true" />
                      {t("Board", "보드")}
                    </button>
                    <button
                      type="button"
                      aria-pressed={view === "terminals"}
                      onClick={() => changeView("terminals")}
                    >
                      <SquareTerminal aria-hidden="true" />
                      {t("Terminals", "터미널")}
                    </button>
                  </nav>
                </div>
              </header>
              {view === "board" &&
                agents.some(
                  (agent) => agent.role === "authentication" && !agent.closed,
                ) && (
                  <div className="am-auth-sessions">
                    <span>
                      {t("CLI account sessions", "CLI 계정 연결 세션")}
                    </span>
                    {agents
                      .filter(
                        (agent) =>
                          agent.role === "authentication" && !agent.closed,
                      )
                      .map((agent) => (
                        <button
                          type="button"
                          key={agent.id}
                          onClick={() => openTask(agent.id)}
                        >
                          <AgentBrandIcon kind={agent.kind} />
                          {agent.name}
                        </button>
                      ))}
                  </div>
                )}
              <AgentTaskBoard
                agents={visible.filter(
                  (agent) => agent.role !== "authentication",
                )}
                hidden={view !== "board"}
                stale={stale || (!!status && !identityMatches)}
                known={!!status}
                onOpen={openTask}
                vmId={vmId}
                pageOpen={page === "tasks"}
              />
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
                      vmId={vmId}
                      controllerId={status.controllerId}
                    />
                  ) : (
                    <div className="am-runtime-unavailable">
                      <h2>{t("Ready when you are", "작업 준비")}</h2>
                      <p>
                        {status
                          ? t(
                              "Choose a coding CLI above to start a task in this MicroVM.",
                              "‘계정 연결’에서 AI를 선택하고 로그인해 주세요.",
                            )
                          : t(
                              "Choose a MicroVM and CLI to start.",
                              "작업 공간을 만들거나 기존 VM을 선택해 주세요.",
                            )}
                      </p>
                    </div>
                  )}
                </div>
              ) : (
                <div className="am-shell" hidden={view !== "terminals"}>
                  <aside
                    className="am-agent-list"
                    aria-label={t("Agents", "에이전트")}
                  >
                    <div className="am-list-head">
                      <h2>
                        {t("Sessions", "세션")}
                        <span>{agents.filter((a) => !a.closed).length}</span>
                      </h2>
                    </div>
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
                          {agent.role !== "authentication" && (
                            <small className="am-agent-model">
                              {agentModelLabel(agent, t("default", "기본값"))}
                            </small>
                          )}
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
                          "조건에 맞는 작업이 없어요.",
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
                          githubNeedsWorkflow={
                            !!status?.github?.missingScopes?.includes(
                              "workflow",
                            )
                          }
                          onUpdate={updateAgent}
                        />
                      ))}
                      {!selectedAgents.length && (
                        <div className="am-no-selection">
                          <PanelLeftClose aria-hidden="true" />
                          {t(
                            "Select an agent to open its session.",
                            "작업을 선택하면 터미널을 열 수 있어요.",
                          )}
                        </div>
                      )}
                    </div>
                  </section>
                </div>
              )}
            </div>
            {editor && (
              <AppDialog
                label={t("New task", "새 작업")}
                busy={creating}
                onClose={() => setEditor(null)}
              >
                <AgentTaskForm
                  draft={editor}
                  onChange={setEditor}
                  onSubmit={(e) => void create(e, editor)}
                  onCancel={() => setEditor(null)}
                  busy={creating}
                  canControl={canLaunch}
                  drivers={status?.drivers || []}
                  error={createError}
                  vmId={vmId}
                  controllerId={status?.controllerId || ""}
                />
              </AppDialog>
            )}
          </div>
          {vm && appVisited && (
            <div hidden={!["verify", "address", "preview"].includes(page)}>
              <AgentWorkspaceApp
                onAddress={() => navigate("address")}
                onPreview={() => navigate("preview")}
                onProgress={setAppProgress}
                vm={vm}
                controllerId={status?.controllerId || controllerId}
                canControl={canControl}
                agents={agents}
                mode={
                  page === "address"
                    ? "address"
                    : page === "preview"
                      ? "preview"
                      : "verify"
                }
                onTask={() => {
                  navigate("tasks");
                  if (agents.some((agent) => agent.role !== "authentication"))
                    openEditor();
                }}
                onDirty={setAppDirty}
              />
            </div>
          )}
        </main>
        <aside
          className="am-quick-sidebar"
          aria-label={t("Quick start", "빠른 시작")}
        >
          <header>
            <small>AI SANDBOX</small>
            <h2>{t("Quick start", "빠른 시작")}</h2>
          </header>
          <label className="am-quick-vm-label" htmlFor="am-quick-vm">
            MicroVM
          </label>
          <select
            id="am-quick-vm"
            value={vmId}
            onChange={(e) => onVm(e.target.value)}
          >
            <option value="">{t("New workspace", "새 작업 공간")}</option>
            {vms.map((value) => (
              <option key={value.id} value={value.id}>
                {value.name} · {value.state}
              </option>
            ))}
          </select>
          <button
            className="btn am-quick-new"
            onClick={() => {
              if (vm) onVm("");
              else navigate("workspace");
            }}
          >
            <Plus />
            {t("New MicroVM", "새 MicroVM")}
          </button>
          <nav>
            {(
              [
                [
                  "workspace",
                  Server,
                  t("Workspace", "작업 공간"),
                  !!vm || setupJob?.state === "ready",
                ],
                [
                  "connections",
                  Link,
                  t("Connect accounts", "계정 연결"),
                  !!canLaunch,
                ],
                [
                  "tasks",
                  ListTodo,
                  t("AI tasks", "AI 작업"),
                  agents.some((agent) => agent.role !== "authentication"),
                ],
                ["verify", Play, t("Run app", "앱 실행"), appProgress.verified],
                [
                  "address",
                  Globe,
                  t("App address", "앱 주소 연결"),
                  appProgress.fixedVerified,
                ],
                [
                  "preview",
                  Eye,
                  t("Preview", "앱 미리보기"),
                  appProgress.previewVerified,
                ],
                [
                  "rules",
                  FileText,
                  t("AGENTS.md rules", "AGENTS.md 규칙"),
                  false,
                ],
                ["skills", Package, t("Skill store", "스킬 스토어"), false],
                ["settings", Settings, t("Settings", "설정"), false],
              ] as const
            ).map(([id, Icon, label, done], index) => (
              <button
                key={id}
                aria-current={page === id ? "step" : undefined}
                disabled={
                  !vm && !["workspace", "skills", "settings"].includes(id)
                }
                onClick={() => navigate(id)}
              >
                <Icon />
                <span>{label}</span>
                {done ? (
                  <Check className="am-step-check" />
                ) : (
                  <small aria-hidden="true">
                    {String(index + 1).padStart(2, "0")}
                  </small>
                )}
              </button>
            ))}
          </nav>
          <p>
            {t(
              "Your code, files and agents run inside this VM.",
              "코드와 파일, AI 에이전트는 선택한 VM 안에서 실행돼요.",
            )}
          </p>
        </aside>
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
  githubNeedsWorkflow,
  onUpdate,
}: {
  agent: AgentSession;
  vm: VmResponse | undefined;
  vmId: string;
  controllerId: string;
  canControl: boolean;
  githubNeedsWorkflow: boolean;
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
        `${e instanceof Error ? e.message : String(e)} ${t("Inspect the terminal before resending.", "다시 보내기 전에 터미널을 확인해 주세요.")}`,
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
          `‘${agent.name}’ 세션을 종료할까요? 실행 중인 CLI도 중지됩니다.`,
        ),
      )
    )
      void action({}, "close");
  };
  const [renaming, setRenaming] = useState(false);
  const rename = async (value: string) => {
    const name = value.trim();
    setRenaming(false);
    if (!name || name === agent.name || !canControl) return;
    setError("");
    try {
      onUpdate(
        await agentRequest<AgentSession>(
          `/${encodeURIComponent(agent.id)}/name`,
          { name },
          controllerId,
          vmId,
        ),
        true,
      );
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  };
  return (
    <section className="am-pane" aria-label={agent.name}>
      {automation && (
        <AgentAutomation
          agent={agent}
          vm={vm}
          controllerId={controllerId}
          canControl={canControl}
          githubNeedsWorkflow={githubNeedsWorkflow}
          onClose={() => setAutomation(false)}
        />
      )}
      <header>
        <div className="am-pane-heading">
          <AgentBrandIcon kind={agent.kind} />
          <div className="am-pane-heading-copy">
            {renaming ? (
              <input
                className="am-pane-rename"
                aria-label={t("Session name", "세션 이름")}
                defaultValue={agent.name}
                maxLength={128}
                autoFocus
                onFocus={(e) => e.currentTarget.select()}
                // Leaving the field is the one save path; Escape marks it cancelled first.
                onBlur={(e) =>
                  e.currentTarget.dataset.cancelled
                    ? setRenaming(false)
                    : void rename(e.currentTarget.value)
                }
                onKeyDown={(e) => {
                  if (e.key === "Escape") {
                    e.stopPropagation();
                    e.currentTarget.dataset.cancelled = "true";
                  }
                  if (e.key === "Enter" || e.key === "Escape")
                    e.currentTarget.blur();
                }}
              />
            ) : (
              <h3 title={agent.name}>{agent.name}</h3>
            )}
            <span>
              {agentName(agent.kind)}
              {agent.role !== "authentication" &&
                ` · ${agentModelLabel(agent, t("default", "기본값"))}`}
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
            <button
              type="button"
              disabled={busy || !canControl}
              onClick={() => setRenaming(true)}
            >
              <Pencil aria-hidden="true" />
              {t("Rename session", "이름 변경")}
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
              "터미널에서 AI의 시작 안내를 마친 뒤 준비한 작업을 보내 주세요.",
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
            "작업이 전달됐는지 확인하지 못했어요. 다시 보내기 전에 터미널을 확인해 주세요.",
          )}
        </p>
      )}
      {!agent.closed && agent.recovery === "pending" && (
        <p className="am-attention">
          {t(
            "Starting the CLI again after the VM restart…",
            "VM 재시작 후 CLI를 다시 시작하는 중이에요…",
          )}
        </p>
      )}
      {!agent.closed && agent.recovery === "fresh" && (
        <p className="am-attention">
          {t(
            "The CLI started a new conversation after the VM restart. The earlier one could not be continued.",
            "VM 재시작 후 CLI가 새 대화로 시작했어요. 이전 대화는 이어 갈 수 없었어요.",
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
