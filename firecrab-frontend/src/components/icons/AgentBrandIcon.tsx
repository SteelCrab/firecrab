import { Sparkles, SquareTerminal } from "lucide-react";

/** Original official app-bundle artwork. See /agent-brands/README.md for provenance. */
export default function AgentBrandIcon({ kind }: { kind: string }) {
  return (
    <span
      className={`am-agent-brand ${kind}`}
      data-agent-kind={kind}
      aria-hidden="true"
    >
      {kind === "codex" || kind === "claude" ? (
        <img
          src={`/agent-brands/${kind === "codex" ? "chatgpt" : kind}.png`}
          width="32"
          height="32"
          alt=""
          draggable={false}
        />
      ) : kind === "auto" ? (
        <Sparkles />
      ) : (
        <SquareTerminal />
      )}
    </span>
  );
}
