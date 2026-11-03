import { useId } from "react";
import { useI18n } from "../i18n";
import type { TaskSuggestions } from "./useTaskSuggestions";

/** The button beside the Task label, with the usage note or the reason it is off. */
export function TaskSuggestButton({
  suggestions,
  blocked,
}: {
  suggestions: TaskSuggestions;
  blocked: string | null;
}) {
  const { t } = useI18n();
  const note = useId();
  const { run, start } = suggestions;
  const running = run.phase === "running";
  return (
    <div className="am-task-suggest">
      <small id={note}>
        {blocked || t("Uses AI provider usage.", "AI 사용량이 들어요.")}
      </small>
      <button
        type="button"
        className="btn ghost sm"
        disabled={!!blocked || running}
        aria-describedby={note}
        onClick={start}
      >
        <span aria-hidden="true">✨</span>{" "}
        {running
          ? t("Getting suggestions…", "제안 받는 중…")
          : run.phase === "ready"
            ? t("Get new suggestions", "새 제안 받기")
            : t("Get task suggestions", "작업 제안 받기")}
      </button>
    </div>
  );
}

/** Progress, the retry message, or the suggestions to pick from. */
export function TaskSuggestionList({
  suggestions,
  onPick,
}: {
  suggestions: TaskSuggestions;
  onPick: (task: string) => void;
}) {
  const { t } = useI18n();
  const { run, elapsed } = suggestions;
  if (run.phase === "idle") return null;
  if (run.phase === "running")
    return (
      <p className="am-task-suggest-status" role="status">
        {t(
          `${run.name} is looking at the repository… ${elapsed ?? 0}s`,
          `${run.name}가 저장소를 살펴보고 있어요… ${elapsed ?? 0}초`,
        )}
      </p>
    );
  if (run.phase === "error")
    return (
      <p className="am-error" role="alert">
        {run.message}
      </p>
    );
  return (
    <ul
      className="am-task-suggestions"
      aria-label={t("Task suggestions", "작업 제안")}
    >
      {run.items.map((item, index) => (
        <li key={index}>
          <button
            type="button"
            className="am-task-suggestion"
            onClick={() => onPick(item.task)}
          >
            <strong>{item.title}</strong>
            <span>{item.reason}</span>
            <small>{item.task}</small>
          </button>
        </li>
      ))}
    </ul>
  );
}
