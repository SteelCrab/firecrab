import { useEffect, useState } from "react";
import { agentRequest, type AgentPreview } from "../api/agents";

/** CLIs whose transcripts the VM can read. Other CLIs keep a card without previews. */
export const previewKinds = ["codex", "claude"];

/** Poll the latest input and reply of each session while the board is on screen. */
export function useAgentPreviews(vmId: string, active: boolean) {
  const [previews, setPreviews] = useState<Record<string, AgentPreview> | null>(
    null,
  );
  useEffect(() => {
    if (!vmId || !active) return;
    let cancelled = false;
    let timer = 0;
    const poll = async () => {
      if (!document.hidden) {
        try {
          const next = await agentRequest<{ previews: AgentPreview[] }>(
            "/previews",
            undefined,
            undefined,
            vmId,
          );
          if (!cancelled)
            setPreviews(
              Object.fromEntries(next.previews.map((p) => [p.id, p])),
            );
        } catch {
          // Keep the last previews. The status check reports connection problems.
        }
      }
      if (!cancelled) timer = window.setTimeout(() => void poll(), 3000);
    };
    void poll();
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, [vmId, active]);
  return previews;
}
