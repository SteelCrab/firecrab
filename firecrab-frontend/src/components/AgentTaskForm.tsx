import type { FormEvent } from "react";
import { useI18n } from "../i18n";
import type { AgentTask, AgentStatus } from "../api/agents";
import AgentBrandIcon from "./icons/AgentBrandIcon";

export default function AgentTaskForm({
  draft,
  onChange,
  onSubmit,
  onCancel,
  busy,
  canControl,
  drivers,
  error,
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
  inline?: boolean;
}) {
  const { t } = useI18n();
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
        <label>
          {t("Task", "작업")}
          <textarea
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
        </label>
        {!inline && (
          <fieldset className="am-task-drivers">
            <legend>{t("Agent", "담당 에이전트")}</legend>
            <div className="am-cli-choice-grid">
              {drivers
                .filter((driver) =>
                  ["codex", "claude", "copilot"].includes(driver.id),
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
                        onChange({ ...draft, cli: driver.id, model: "" })
                      }
                    />
                    <AgentBrandIcon kind={driver.id} />
                    <span>{driver.name}</span>
                  </label>
                ))}
            </div>
          </fieldset>
        )}
        <details className="am-task-settings">
          <summary>{t("Model", "모델")}</summary>
          <label className="am-task-model-field">
            {t("Model", "모델")}
            <input
              disabled={!["codex", "claude"].includes(draft.cli)}
              placeholder={t("CLI default model", "CLI 기본 모델")}
              value={draft.model}
              onChange={(e) => onChange({ ...draft, model: e.target.value })}
            />
          </label>
        </details>
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
              ? t("Starting task…", "작업을 시작하고 있다…")
              : t("Start task", "작업 시작")}
          </button>
        </footer>
      </fieldset>
    </form>
  );
}
