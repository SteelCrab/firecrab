import { useEffect, useState } from "react";
import {
  Bot,
  Code2,
  ExternalLink,
  Folder,
  GitBranch,
  PlugZap,
  RefreshCw,
  Search,
  Server,
} from "lucide-react";
import {
  AgentRequestError,
  agentEditorUrl,
  agentRequest,
  type AgentSession,
  type AgentStatus,
  type VmRepository,
} from "../api/agents";
import { managerEndpoint } from "../api/manager";
import type { HostOs, VmResponse } from "../bindings";
import { useHostStatus } from "../hostStatusContext";
import { useI18n } from "../i18n";
import { agentName, agentStateLabel } from "../lib/agentLabels";
import { viewHash } from "../navigation";
import AgentBrandIcon from "./icons/AgentBrandIcon";
import Screen from "./layout/Screen";
import "../styles/code.css";

/** The installed main server's dashboard; every host OS serves it here. */
const MAIN_DASHBOARD = "http://127.0.0.1:5523/";

type Project = {
  vmId: string;
  project: VmRepository & { repository: string };
  agents: AgentSession[];
};
type Scan = {
  projects: Project[];
  /** Running MicroVMs whose agent status could not be read. */
  unchecked: number;
  /** No running MicroVM answered because the host bridge itself is down. */
  unreachable: boolean;
};

/** `https://github.com/team/app.git` → `team/app`; other remotes stay as given. */
function repositoryLabel(remote: string): string {
  const match = remote.match(
    /^(?:https:\/\/github\.com\/|git@github\.com:)([^/]+\/[^/]+?)(?:\.git)?\/?$/,
  );
  return match?.[1] ?? remote;
}

/** Sessions a person can open in VS Code: open work, not sign-in helpers. */
function editorAgents(status: AgentStatus): AgentSession[] {
  return status.agents.filter((a) => !a.closed && a.role !== "authentication");
}

function isRegular(vm: VmResponse): boolean {
  return !vm.purpose || vm.purpose === "instance";
}

async function scan(vmIds: string[]): Promise<Scan> {
  const results = await Promise.allSettled(
    vmIds.map((vmId) =>
      agentRequest<AgentStatus>("", undefined, undefined, vmId).then(
        (status) => ({ vmId, status }),
      ),
    ),
  );
  const projects: Project[] = [];
  let unchecked = 0,
    unreachable = results.length > 0;
  for (const result of results) {
    if (result.status === "rejected") {
      unchecked++;
      // An HTTP answer means the bridge is up and only that VM failed.
      if (result.reason instanceof AgentRequestError) unreachable = false;
      continue;
    }
    unreachable = false;
    const { vmId, status } = result.value;
    const project = status.project;
    if (!project?.repository) continue;
    projects.push({
      vmId,
      project: { ...project, repository: project.repository },
      agents: editorAgents(status),
    });
  }
  return { projects, unchecked, unreachable };
}

export default function Code({ vms }: { vms: VmResponse[] }) {
  const { t } = useI18n();
  const host = useHostStatus();
  const os: HostOs | undefined = host?.platform?.os;
  const controllerId = host?.platform?.controllerId || "";
  const bridge = managerEndpoint() !== null;
  const regular = vms.filter(isRegular);
  const running = regular.filter((v) => v.state === "running");
  const stopped = regular.length - running.length;
  const runningKey = running.map((v) => v.id).join(",");
  const canScan = !!os && os !== "linux" && bridge;

  const [epoch, setEpoch] = useState(0),
    [query, setQuery] = useState(""),
    [result, setResult] = useState<(Scan & { key: string }) | null>(null);
  // A scan belongs to one set of running MicroVMs and one refresh request.
  const requestKey = `${runningKey}#${epoch}`;
  const busy = canScan && result?.key !== requestKey;

  useEffect(() => {
    if (!canScan) return;
    let alive = true;
    let timer: ReturnType<typeof setTimeout> | undefined;
    void scan(runningKey ? runningKey.split(",") : []).then((next) => {
      if (!alive) return;
      setResult({ ...next, key: `${runningKey}#${epoch}` });
      // Keep checking while a repository is still being cloned.
      if (next.projects.some((p) => p.project.state === "preparing"))
        timer = setTimeout(() => setEpoch((n) => n + 1), 5000);
    });
    return () => {
      alive = false;
      clearTimeout(timer);
    };
  }, [canScan, runningKey, epoch]);

  const vmName = (id: string) => vms.find((v) => v.id === id)?.name ?? id;
  const openSandbox = (vmId: string) => {
    try {
      localStorage.setItem(`firecrab.agentVm.${controllerId}`, vmId);
    } catch {
      /* AI Sandbox falls back to its own selection. */
    }
    window.location.assign(viewHash("ai-sandbox"));
  };

  const term = query.trim().toLowerCase();
  const visible = (result?.projects ?? []).filter((p) =>
    [repositoryLabel(p.project.repository), p.project.branch, vmName(p.vmId)]
      .join(" ")
      .toLowerCase()
      .includes(term),
  );
  const agentCount = (result?.projects ?? []).reduce(
    (n, p) => n + p.agents.length,
    0,
  );

  let body;
  if (!os)
    body = (
      <p role="status">
        {t("Checking this host…", "이 호스트를 확인하는 중…")}
      </p>
    );
  else if (os === "linux") body = <LinuxNotice />;
  else if (!bridge || result?.unreachable)
    body = <BridgeNotice os={os} unreachable={!!result?.unreachable} />;
  else if (!result)
    body = (
      <p role="status">{t("Checking projects…", "프로젝트를 확인하는 중…")}</p>
    );
  else if (!result.projects.length)
    body = (
      <div className="code-empty">
        <span className="code-empty-icon">
          <Code2 aria-hidden="true" />
        </span>
        <h2>
          {t(
            "No AI Sandbox has a linked project",
            "프로젝트가 연동된 AI Sandbox가 없습니다",
          )}
        </h2>
        <p>
          {t(
            "Link a GitHub repository in AI Sandbox. Its VS Code links then appear here.",
            "AI Sandbox에서 GitHub 저장소를 연동하면 여기에 VS Code 링크가 나타납니다.",
          )}
        </p>
        <a className="btn primary" href={viewHash("ai-sandbox")}>
          <Bot aria-hidden="true" />
          {t("Open AI Sandbox", "AI Sandbox 열기")}
        </a>
        <Footnote stopped={stopped} unchecked={result.unchecked} />
      </div>
    );
  else
    body = (
      <>
        <p className="code-lead">
          {t(
            "Open VS Code for projects linked in AI Sandbox. Link projects and manage tasks in AI Sandbox.",
            "AI Sandbox에 연동된 프로젝트의 VS Code를 엽니다. 프로젝트 연동과 작업 관리는 AI Sandbox에서 합니다.",
          )}
        </p>
        <div className="code-toolbar">
          <label className="code-search">
            <Search aria-hidden="true" />
            <input
              id="code-project-search"
              type="search"
              aria-label={t("Search projects", "프로젝트 검색")}
              placeholder={t(
                "Search repository, branch or MicroVM",
                "저장소, 브랜치, MicroVM 검색",
              )}
              value={query}
              onChange={(e) => setQuery(e.target.value)}
            />
          </label>
          <span className="code-count">
            {t(
              `${result.projects.length} projects · ${agentCount} agents`,
              `프로젝트 ${result.projects.length}개 · 에이전트 ${agentCount}개`,
            )}
          </span>
        </div>
        {visible.map((p) => (
          <ProjectCard
            key={p.vmId}
            item={p}
            vmName={vmName(p.vmId)}
            controllerId={controllerId}
            onOpenSandbox={() => openSandbox(p.vmId)}
          />
        ))}
        {!visible.length && (
          <p role="status" className="code-footnote">
            {t(
              "No project matches this search.",
              "검색과 맞는 프로젝트가 없습니다.",
            )}
          </p>
        )}
        <Footnote stopped={stopped} unchecked={result.unchecked} />
      </>
    );

  return (
    <Screen
      view="code"
      actions={
        <>
          {canScan && (
            <button
              type="button"
              className="btn"
              disabled={busy}
              onClick={() => setEpoch((n) => n + 1)}
            >
              <RefreshCw aria-hidden="true" />
              {t("Refresh", "새로고침")}
            </button>
          )}
          <a className="btn primary" href={viewHash("ai-sandbox")}>
            <Bot aria-hidden="true" />
            {t("Open AI Sandbox", "AI Sandbox 열기")}
          </a>
        </>
      }
    >
      <div className="code-view">{body}</div>
    </Screen>
  );
}

function ProjectCard({
  item,
  vmName,
  controllerId,
  onOpenSandbox,
}: {
  item: Project;
  vmName: string;
  controllerId: string;
  onOpenSandbox: () => void;
}) {
  const { t } = useI18n();
  const { project, agents } = item;
  const repo = repositoryLabel(project.repository);
  const ready = project.state === "ready";
  const stateLabel: Record<VmRepository["state"], [string, string]> = {
    ready: ["Ready", "준비됨"],
    preparing: ["Preparing", "준비 중"],
    failed: ["Failed", "실패"],
    interrupted: ["Interrupted", "중단됨"],
  };
  return (
    <article className="code-project" aria-label={repo}>
      <header className="code-project-head">
        <div className="code-project-id">
          <div className="code-repo">
            <strong title={project.repository}>{repo}</strong>
            <span className="code-chip">
              <GitBranch aria-hidden="true" />
              {project.branch}
            </span>
            <span className={`code-pill ${project.state}`}>
              {t(...stateLabel[project.state])}
            </span>
          </div>
          <div className="code-meta">
            <span>
              <Server aria-hidden="true" />
              {vmName}
            </span>
            {ready && project.path && (
              <span>
                <Folder aria-hidden="true" />
                <code>{project.path}</code>
              </span>
            )}
          </div>
        </div>
        <button type="button" className="btn ghost sm" onClick={onOpenSandbox}>
          {t("Open in AI Sandbox", "AI Sandbox에서 보기")}
        </button>
      </header>
      {!ready ? (
        <p className="code-hint">
          {project.state === "preparing"
            ? t(
                "Fetching the repository. VS Code opens after it is ready.",
                "저장소를 가져오는 중입니다. 준비가 끝나면 VS Code를 열 수 있습니다.",
              )
            : project.error ||
              t(
                "The project is not ready. Check it in AI Sandbox.",
                "프로젝트가 준비되지 않았습니다. AI Sandbox에서 확인하세요.",
              )}
        </p>
      ) : !agents.length ? (
        <p className="code-hint">
          {t(
            "No agent is open yet. Start a task in AI Sandbox to get a VS Code link.",
            "아직 열린 에이전트가 없습니다. AI Sandbox에서 작업을 시작하면 VS Code 링크가 생깁니다.",
          )}
        </p>
      ) : (
        <ul className="code-agents">
          {agents.map((agent) => (
            <li key={agent.id} className="code-agent" aria-label={agent.name}>
              <div className="code-agent-name">
                <AgentBrandIcon kind={agent.kind} />
                <b>{agentName(agent.kind)}</b>
                <span>{agent.name}</span>
              </div>
              <span className={`code-pill ${agent.state}`}>
                {t(...agentStateLabel(agent.state))}
              </span>
              <a
                className="btn sm"
                href={
                  controllerId
                    ? agentEditorUrl(item.vmId, agent.id, controllerId)
                    : undefined
                }
                target="_blank"
                rel="noreferrer noopener"
                aria-disabled={!controllerId || undefined}
              >
                <Code2 aria-hidden="true" />
                {t("Open VS Code", "VS Code 열기")}
                <ExternalLink aria-hidden="true" />
              </a>
            </li>
          ))}
        </ul>
      )}
    </article>
  );
}

function Footnote({
  stopped,
  unchecked,
}: {
  stopped: number;
  unchecked: number;
}) {
  const { t } = useI18n();
  const lines = [
    stopped > 0 &&
      t(
        stopped === 1
          ? "1 stopped MicroVM was not checked."
          : `${stopped} stopped MicroVMs were not checked.`,
        `중지된 MicroVM ${stopped}개는 확인하지 않았습니다.`,
      ),
    unchecked > 0 &&
      t(
        unchecked === 1
          ? "1 running MicroVM could not be checked."
          : `${unchecked} running MicroVMs could not be checked.`,
        `실행 중인 MicroVM ${unchecked}개는 확인하지 못했습니다.`,
      ),
  ].filter(Boolean);
  if (!lines.length) return null;
  return <p className="code-footnote">{lines.join(" ")}</p>;
}

function Commands({ rows }: { rows: [string, string][] }) {
  return (
    <dl className="code-commands">
      {rows.map(([label, command]) => (
        <div key={command}>
          <dt>{label}</dt>
          <dd>
            <code>{command}</code>
          </dd>
        </div>
      ))}
    </dl>
  );
}

/** macOS and Windows reach agents through the microManager bridge on the main server. */
function BridgeNotice({
  os,
  unreachable,
}: {
  os: Exclude<HostOs, "linux">;
  unreachable: boolean;
}) {
  const { t } = useI18n();
  const shell = os === "windows" ? "PowerShell" : t("Terminal", "터미널");
  return (
    <section
      className="code-notice"
      aria-label={t("VS Code connection", "VS Code 연결")}
    >
      <span className="code-empty-icon">
        <PlugZap aria-hidden="true" />
      </span>
      <h2>
        {unreachable
          ? t(
              "The host connection does not answer",
              "호스트 연결이 응답하지 않습니다",
            )
          : t(
              "Open this page from the main server dashboard",
              "메인 서버 대시보드에서 여세요",
            )}
      </h2>
      <p>
        {t(
          "VS Code links use this host's microManager connection. Open the main server dashboard on this host:",
          "VS Code 링크는 이 호스트의 microManager 연결을 씁니다. 이 호스트에서 메인 서버 대시보드를 여세요:",
        )}{" "}
        <code>{MAIN_DASHBOARD}</code>
      </p>
      <p>
        {t(
          `If it does not open, run these in ${shell} on this ${os === "windows" ? "Windows" : "macOS"} host.`,
          `열리지 않으면 이 ${os === "windows" ? "Windows" : "macOS"} 호스트의 ${shell}에서 실행하세요.`,
        )}
      </p>
      <Commands
        rows={[
          [
            t("Check the service", "서비스 상태 확인"),
            "firecrab service status",
          ],
          [t("Start the service", "서비스 시작"), "firecrab service start"],
        ]}
      />
    </section>
  );
}

/** Linux runs the main server natively, without the microManager bridge AI Sandbox needs. */
function LinuxNotice() {
  const { t } = useI18n();
  return (
    <section
      className="code-notice"
      aria-label={t("VS Code connection", "VS Code 연결")}
    >
      <span className="code-empty-icon">
        <PlugZap aria-hidden="true" />
      </span>
      <h2>
        {t(
          "Linux hosts cannot open AI Sandbox VS Code yet",
          "Linux 호스트에서는 아직 AI Sandbox VS Code를 열 수 없습니다",
        )}
      </h2>
      <p>
        {t(
          "AI Sandbox editors open through microManager. microManager runs only on macOS and Windows hosts.",
          "AI Sandbox 편집기는 microManager를 거쳐 열립니다. microManager는 macOS와 Windows 호스트에서만 동작합니다.",
        )}
      </p>
      <p>
        {t(
          "The Linux main server itself runs at",
          "Linux 메인 서버 자체는 다음 주소에서 동작합니다:",
        )}{" "}
        <code>{MAIN_DASHBOARD}</code>
      </p>
      <Commands
        rows={[
          [
            t("Check the main server", "메인 서버 상태 확인"),
            "systemctl status firecrab-helper firecrab-api",
          ],
        ]}
      />
    </section>
  );
}
