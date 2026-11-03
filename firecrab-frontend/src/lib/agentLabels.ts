/** Session states shown for AI Sandbox CLI agents, as `[English, Korean]`. */
export const AGENT_STATE_LABELS: Record<string, [string, string]> = {
  working: ["Working", "실행 중"],
  blocked: ["Needs input", "입력 필요"],
  idle: ["Idle", "대기"],
  done: ["Done", "완료"],
  closed: ["Closed", "종료됨"],
  disconnected: ["Disconnected", "연결 끊김"],
  unknown: ["Unknown", "상태 미확인"],
  failed: ["Launch failed", "시작 실패"],
};

export function agentStateLabel(state: string): [string, string] {
  return AGENT_STATE_LABELS[state] || AGENT_STATE_LABELS.unknown;
}

const AGENT_NAMES: Record<string, string> = {
  codex: "Codex",
  claude: "Claude Code",
  grok: "Grok Build",
  gemini: "Gemini CLI",
  opencode: "OpenCode",
  copilot: "GitHub Copilot",
  cursor: "Cursor Agent",
  github: "GitHub",
};

export function agentName(kind: string): string {
  return AGENT_NAMES[kind] || kind;
}
