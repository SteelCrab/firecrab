import { useEffect, useRef, useState } from "react";
import { useI18n } from "../i18n";
import { agentRequest, AgentRequestError } from "../api/agents";
import { useElapsedSeconds } from "../lib/useElapsedSeconds";

export type TaskSuggestion = { title: string; reason: string; task: string };
type Reply = {
  state: "ready" | "busy" | "timeout" | "failed" | "invalid" | "outdated";
  name: string;
  suggestions: TaskSuggestion[];
};
type Run =
  | { phase: "idle" }
  | { phase: "running"; key: string; name: string }
  | { phase: "ready"; items: TaskSuggestion[] }
  | { phase: "error"; message: string };

/** The backend already validates and trims; this only drops what is not a suggestion. */
function usable(items: unknown): TaskSuggestion[] {
  if (!Array.isArray(items)) return [];
  return items
    .filter(
      (item): item is TaskSuggestion =>
        !!item &&
        typeof item.title === "string" &&
        typeof item.reason === "string" &&
        typeof item.task === "string" &&
        !!item.task.trim(),
    )
    .slice(0, 5);
}

/**
 * Task ideas from the chosen CLI in the VM. They live only while the form is
 * open; a late reply after the form closes is dropped.
 */
export function useTaskSuggestions({
  vmId,
  controllerId,
  cli,
  name,
  model,
  effort,
}: {
  vmId: string;
  controllerId: string;
  cli: string;
  name: string;
  model: string;
  effort: string;
}) {
  const { t, locale } = useI18n();
  const [run, setRun] = useState<Run>({ phase: "idle" });
  const live = useRef(true);
  useEffect(() => {
    live.current = true;
    return () => {
      live.current = false;
    };
  }, []);
  const elapsed = useElapsedSeconds(run.phase === "running" ? run.key : null);
  const start = () => {
    if (run.phase === "running") return;
    const key = `${Date.now()}`;
    setRun({ phase: "running", key, name });
    const fail = (message: string) =>
      live.current && setRun({ phase: "error", message });
    agentRequest<Reply>(
      "/tasks/suggestions",
      { cli, model, effort, language: locale },
      controllerId,
      vmId,
    ).then(
      (reply) => {
        const items = reply?.state === "ready" ? usable(reply.suggestions) : [];
        if (items.length) {
          if (live.current) setRun({ phase: "ready", items });
          return;
        }
        fail(
          reply?.state === "timeout"
            ? t(
                `${name} did not answer within 2 minutes. Try again.`,
                `${name}가 2분 안에 답하지 않았어요. 다시 시도해 주세요.`,
              )
            : reply?.state === "busy"
              ? t(
                  "Suggestions are already being made in this VM. Try again soon.",
                  "이 VM에서 이미 제안을 만들고 있어요. 잠시 후 다시 시도해 주세요.",
                )
              : reply?.state === "outdated"
                ? t(
                    `Update ${name} in this VM to get suggestions.`,
                    `제안을 받으려면 이 VM의 ${name}를 업데이트해 주세요.`,
                  )
                : reply?.state === "invalid" || reply?.state === "ready"
                  ? t(
                      `${name}'s reply could not be read. Try again.`,
                      `${name}의 답을 읽지 못했어요. 다시 시도해 주세요.`,
                    )
                  : t(
                      `${name} could not make suggestions. Try again.`,
                      `${name}가 제안을 만들지 못했어요. 다시 시도해 주세요.`,
                    ),
        );
      },
      (failure: unknown) =>
        fail(
          failure instanceof AgentRequestError && failure.status === 404
            ? t(
                "Update the Firecrab service to get suggestions.",
                "제안을 받으려면 Firecrab 서비스를 업데이트해 주세요.",
              )
            : t(
                "Could not get suggestions. Try again.",
                "제안을 받지 못했어요. 다시 시도해 주세요.",
              ),
        ),
    );
  };
  return { run, elapsed, start };
}

export type TaskSuggestions = ReturnType<typeof useTaskSuggestions>;
