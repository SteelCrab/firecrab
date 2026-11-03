import {
  ArrowUpRight,
  CircleDashed,
  CircleCheck,
  CircleHelp,
  Activity,
} from "lucide-react";
import { useI18n } from "../i18n";
import { agentModelLabel, type AgentSession } from "../api/agents";
import AgentBrandIcon from "./icons/AgentBrandIcon";
import AgentCardPreview from "./AgentCardPreview";
import { previewKinds, useAgentPreviews } from "./useAgentPreviews";
import "../styles/agent-board.css";

type Stage = "queued" | "working" | "attention" | "done";
const stages = ["queued", "working", "attention", "done"] as const;
const names: Record<Stage, [string, string]> = {
  queued: ["Waiting", "대기"],
  working: ["Working", "실행 중"],
  attention: ["Needs attention", "입력·확인 필요"],
  done: ["Done", "완료"],
};
const icons = {
  queued: CircleDashed,
  working: Activity,
  attention: CircleHelp,
  done: CircleCheck,
};
const providers: Record<string, string> = {
  codex: "Codex",
  claude: "Claude Code",
  copilot: "GitHub Copilot",
};

function stageOf(agent: AgentSession): Stage {
  if (agent.closed || agent.state === "closed") return "done";
  if (agent.taskDeliveryUncertain) return "attention";
  if (agent.hasPendingTask) return "queued";
  if (agent.state === "working") return "working";
  if (agent.state === "idle") return "queued";
  if (agent.state === "done") return "done";
  return "attention";
}

export default function AgentTaskBoard({
  agents,
  hidden,
  stale,
  known,
  onOpen,
  vmId,
  pageOpen,
}: {
  agents: AgentSession[];
  hidden: boolean;
  stale: boolean;
  known: boolean;
  onOpen: (id: string) => void;
  vmId: string;
  pageOpen: boolean;
}) {
  const { t } = useI18n();
  const previewed = (agent: AgentSession) =>
    !agent.closed && previewKinds.includes(agent.kind);
  const previews = useAgentPreviews(
    vmId,
    pageOpen && !hidden && agents.some(previewed),
  );
  const stateLabel = (agent: AgentSession) => {
    if (stale) return t("Last known state", "최근 확인한 상태");
    if (agent.closed || agent.state === "closed") return t("Closed", "종료됨");
    if (agent.taskDeliveryUncertain)
      return t("Check task delivery", "작업 전달 확인 필요");
    if (agent.hasPendingTask)
      return t("Waiting for CLI setup", "CLI 시작 절차 대기");
    if (agent.state === "blocked") return t("Needs input", "입력 필요");
    if (agent.state === "disconnected") return t("Disconnected", "연결 끊김");
    if (agent.state === "failed") return t("Launch failed", "시작 실패");
    if (agent.state === "unknown") return t("Unknown", "상태 미확인");
    return t(...names[stageOf(agent)]);
  };
  return (
    <section
      className="am-task-board"
      aria-label={t("Task board", "작업 보드")}
      hidden={hidden}
      data-stale={stale || !known}
    >
      {stages.map((stage) => {
        const rows = agents.filter((agent) => stageOf(agent) === stage);
        const Icon = icons[stage];
        return (
          <section
            key={stage}
            className={`am-board-column ${stage}`}
            aria-label={t(...names[stage])}
          >
            <header>
              <Icon aria-hidden="true" />
              <h3>{t(...names[stage])}</h3>
              <span
                className="am-board-count"
                aria-label={t("Task count", "작업 수")}
              >
                {known ? rows.length : "—"}
              </span>
            </header>
            <div className="am-board-cards">
              {rows.map((agent) => (
                <button
                  type="button"
                  key={agent.id}
                  className="am-board-card"
                  data-task-id={agent.id}
                  onClick={() => onOpen(agent.id)}
                >
                  <span className="am-board-card-provider">
                    <AgentBrandIcon kind={agent.kind} />
                    <span>{providers[agent.kind] || agent.kind}</span>
                    <ArrowUpRight aria-hidden="true" />
                  </span>
                  <strong>{agent.name}</strong>
                  <span className="am-board-card-model">
                    {agentModelLabel(agent, t("default", "기본값"))}
                  </span>
                  <span
                    className={`am-board-card-state ${agent.closed ? "closed" : stage}`}
                  >
                    <span
                      className={`am-dot ${stale ? "unknown" : agent.state}`}
                      aria-hidden="true"
                    />
                    {stateLabel(agent)}
                  </span>
                  {previews && previewed(agent) && (
                    <AgentCardPreview preview={previews[agent.id]} />
                  )}
                </button>
              ))}
              {!rows.length && (
                <span className="am-board-empty" aria-hidden="true">
                  —
                </span>
              )}
            </div>
          </section>
        );
      })}
    </section>
  );
}
