import { useEffect, useId, useRef, useState, type FormEvent } from "react";
import { useI18n } from "../i18n";
import {
  agentEfforts,
  agentRequest,
  claudeModels,
  type AgentModel,
  type AgentTask,
  type AgentStatus,
} from "../api/agents";
import AgentBrandIcon from "./icons/AgentBrandIcon";
import { TaskSuggestButton, TaskSuggestionList } from "./AgentTaskSuggestions";
import { useTaskSuggestions } from "./useTaskSuggestions";

// The select's value while a typed name is in use. Never a model name:
// catalog names carry no control characters.
const OTHER = "\u0000other";

/** The VM's own Codex catalog: null until it answers, empty when it cannot. */
function useCodexModels(vmId: string, wanted: boolean) {
  const [catalog, setCatalog] = useState<{
    vmId: string;
    models: AgentModel[];
  }>();
  useEffect(() => {
    if (!wanted || !vmId) return;
    let live = true;
    void agentRequest<{ models: AgentModel[] }>(
      "/codex/models",
      undefined,
      undefined,
      vmId,
    ).then(
      (result) => live && setCatalog({ vmId, models: result.models }),
      () => live && setCatalog({ vmId, models: [] }),
    );
    return () => {
      live = false;
    };
  }, [vmId, wanted]);
  return catalog?.vmId === vmId ? catalog.models : null;
}

/** Keyed by CLI, so a new CLI starts again from its defaults. `models` is null while loading. */
function ModelEffortChoice({
  draft,
  models,
  onChange,
}: {
  draft: AgentTask;
  models: AgentModel[] | null;
  onChange: (value: AgentTask) => void;
}) {
  const { t } = useI18n();
  const note = useId();
  const [typing, setTyping] = useState(false);
  // Grok Build takes a model name but documents no effort levels.
  const modelOk = ["codex", "claude", "grok"].includes(draft.cli);
  const effortOk = ["codex", "claude"].includes(draft.cli);
  const custom =
    typing || (!!draft.model && !models?.some((m) => m.id === draft.model));
  return (
    <div className="am-task-options">
      <label>
        {t("Model", "모델")}
        <select
          disabled={!modelOk}
          aria-describedby={modelOk ? undefined : note}
          value={custom ? OTHER : draft.model}
          onChange={(e) => {
            const other = e.target.value === OTHER;
            setTyping(other);
            onChange({ ...draft, model: other ? "" : e.target.value });
          }}
        >
          <option value="">{t("CLI default", "CLI 기본값")}</option>
          {models?.map((model) => (
            <option key={model.id} value={model.id}>
              {model.name}
            </option>
          ))}
          {!models && (
            <option disabled>
              {t("Loading models…", "모델 목록을 불러오는 중…")}
            </option>
          )}
          <option value={OTHER}>{t("Other…", "직접 입력")}</option>
        </select>
      </label>
      <label>
        {t("Effort", "추론 강도")}
        <select
          disabled={!effortOk}
          aria-describedby={effortOk ? undefined : note}
          value={draft.effort}
          onChange={(e) => onChange({ ...draft, effort: e.target.value })}
        >
          <option value="">{t("CLI default", "CLI 기본값")}</option>
          {agentEfforts.map((effort) => (
            <option key={effort} value={effort}>
              {effort}
            </option>
          ))}
        </select>
      </label>
      {custom && (
        <label className="am-task-model-name">
          {t("Model name", "모델 이름")}
          <input
            autoFocus={typing}
            autoCapitalize="none"
            autoCorrect="off"
            spellCheck={false}
            maxLength={128}
            value={draft.model}
            onChange={(e) => onChange({ ...draft, model: e.target.value })}
          />
        </label>
      )}
      {!modelOk ? (
        <small id={note}>
          {t(
            "Model choices work with Codex, Claude Code and Grok Build. Effort choices work only with Codex and Claude Code.",
            "모델은 Codex, Claude Code, Grok Build에서 고를 수 있어요. 추론 강도는 Codex와 Claude Code에서만 고를 수 있어요.",
          )}
        </small>
      ) : (
        !effortOk && (
          <small id={note}>
            {t(
              "Grok Build uses its own effort setting. Choose Other… to type a model name.",
              "Grok Build는 자체 추론 강도 설정을 써요. 모델은 직접 입력에서 이름을 적어 주세요.",
            )}
          </small>
        )
      )}
    </div>
  );
}

export default function AgentTaskForm({
  draft,
  onChange,
  onSubmit,
  onCancel,
  busy,
  canControl,
  drivers,
  error,
  vmId,
  controllerId,
  inline = false,
}: {
  draft: AgentTask;
  onChange: (value: AgentTask) => void;
  onSubmit: (event: FormEvent) => void;
  onCancel?: () => void;
  busy: boolean;
  canControl: boolean;
  drivers: AgentStatus["drivers"];
  error: string;
  vmId: string;
  controllerId: string;
  inline?: boolean;
}) {
  const { t } = useI18n();
  const taskId = useId();
  const taskInput = useRef<HTMLTextAreaElement>(null);
  const driver = drivers.find((d) => d.id === draft.cli);
  const cliName = driver?.name || draft.cli;
  const suggestions = useTaskSuggestions({
    vmId,
    controllerId,
    cli: draft.cli,
    name: cliName,
    model: draft.model,
    effort: draft.effort,
  });
  // Name the first thing that keeps suggestions off. Copilot is not supported yet.
  const suggestBlocked = !["codex", "claude"].includes(draft.cli)
    ? t(
        "Suggestions work with Codex and Claude Code.",
        "작업 제안은 Codex와 Claude Code에서만 쓸 수 있어요.",
      )
    : !driver?.available || !driver.authenticated
      ? t(`Sign in to ${cliName} first.`, `먼저 ${cliName}에 로그인해 주세요.`)
      : !canControl || !controllerId
        ? t(
            "Available when a task can start in this VM.",
            "이 VM에서 작업을 시작할 수 있을 때 쓸 수 있어요.",
          )
        : null;
  const pick = (task: string) => {
    if (
      draft.input.trim() &&
      draft.input !== task &&
      !window.confirm(
        t(
          "Replace the task text with this suggestion?",
          "작성한 작업 내용을 이 제안으로 바꿀까요?",
        ),
      )
    )
      return;
    onChange({ ...draft, input: task });
    taskInput.current?.focus();
  };
  const codexModels = useCodexModels(vmId, draft.cli === "codex");
  const models =
    draft.cli === "codex"
      ? codexModels
      : draft.cli === "claude"
        ? claudeModels
        : [];
  return (
    <form
      className={`am-create am-task-create${inline ? " am-inline-task" : ""}`}
      onSubmit={onSubmit}
      aria-busy={busy}
    >
      <fieldset className="am-task-fields" disabled={busy}>
        <header>
          {!inline && <span>AI Sandbox</span>}
          {inline ? (
            <h2>{t("What shall we work on?", "어떤 작업을 할까요?")}</h2>
          ) : (
            <h3>{t("New task", "새 작업")}</h3>
          )}
          <p>
            {t(
              "Run this task in this MicroVM's connected workspace.",
              "이 VM의 작업 폴더에서 AI가 코드를 작성해요.",
            )}
          </p>
        </header>
        {error && (
          <p className="am-error" role="alert">
            {error}
          </p>
        )}
        <div className="am-task-input">
          <div className="am-task-label-row">
            <label htmlFor={taskId}>{t("Task", "작업")}</label>
            <TaskSuggestButton
              suggestions={suggestions}
              blocked={suggestBlocked}
            />
          </div>
          <textarea
            id={taskId}
            ref={taskInput}
            autoFocus={!inline}
            required
            rows={5}
            placeholder={t(
              "Describe what needs to be done…",
              "예: 할 일 앱을 만들어 줘. 추가·완료·삭제 기능과 테스트를 작성해 줘. 앱은 Firecrab에서 실행할 수 있도록 시작 명령만 준비해 줘.",
            )}
            value={draft.input}
            onChange={(e) => onChange({ ...draft, input: e.target.value })}
          />
          <TaskSuggestionList suggestions={suggestions} onPick={pick} />
        </div>
        {!inline && (
          <fieldset className="am-task-drivers">
            <legend>{t("Agent", "담당 에이전트")}</legend>
            <div className="am-cli-choice-grid">
              {drivers
                .filter((driver) =>
                  ["codex", "claude", "grok", "copilot"].includes(driver.id),
                )
                .map((driver) => (
                  <label key={driver.id} className="am-cli-choice">
                    <input
                      type="radio"
                      name="task-cli"
                      aria-label={t(
                        `${driver.name} for this task`,
                        `${driver.name}로 이 작업 실행`,
                      )}
                      checked={draft.cli === driver.id}
                      disabled={
                        !driver.available || driver.authenticated === false
                      }
                      onChange={() =>
                        onChange({
                          ...draft,
                          cli: driver.id,
                          model: "",
                          effort: "",
                        })
                      }
                    />
                    <AgentBrandIcon kind={driver.id} />
                    <span>{driver.name}</span>
                  </label>
                ))}
            </div>
          </fieldset>
        )}
        <ModelEffortChoice
          key={draft.cli}
          draft={draft}
          models={models}
          onChange={onChange}
        />
        <p className="am-host-note">
          {t(
            "The CLI uses the account connected inside the selected MicroVM. Provider usage may incur charges.",
            "이 VM에 연결한 AI 계정을 사용해요. AI 서비스 요금이 발생할 수 있어요.",
          )}
        </p>
        <footer>
          {onCancel && (
            <button
              type="button"
              className="btn"
              disabled={busy}
              onClick={onCancel}
            >
              {t("Cancel", "취소")}
            </button>
          )}
          <button
            className="btn primary"
            disabled={
              busy ||
              !canControl ||
              !draft.input.trim() ||
              !drivers.some(
                (d) =>
                  d.available &&
                  d.authenticated !== false &&
                  (!draft.cli || d.id === draft.cli),
              )
            }
          >
            {busy
              ? t("Starting task…", "작업 시작 중…")
              : t("Start task", "작업 시작")}
          </button>
        </footer>
      </fieldset>
    </form>
  );
}
