import { useEffect } from "react";
import { Play, RefreshCw, Square, FileText, ChevronUp } from "lucide-react";
import { useI18n } from "../i18n";
import { canvasController as c } from "../canvas/controller";
import { agentDocument, activeTask, selectionKey } from "../canvas/agent-types";
import type { Tab, Node } from "../canvas/types";
import "../styles/canvas-agent.css";
export default function CanvasAgent({ tab, node }: { tab: Tab; node: Node }) {
  "use no memo";
  const { t } = useI18n(),
    manager = c.agents,
    s = manager.get(tab, node),
    d = agentDocument(node, tab.project);
  useEffect(() => {
    if (!agentDocument(node, tab.project)) return;
    let alive = true;
    void manager.models(tab, node).then(() => {
      if (alive) void manager.refresh(tab, node);
    });
    const timer = setInterval(() => {
      if (document.visibilityState === "visible")
        void manager.refresh(tab, node);
    }, 3000);
    return () => {
      alive = false;
      clearInterval(timer);
    };
  }, [tab, node, manager]);
  if (!d)
    return (
      <p role="alert">
        {t(
          "Invalid saved agent data. No task will run.",
          "저장된 에이전트 정보를 확인할 수 없다. 작업을 실행하지 않는다.",
        )}
      </p>
    );
  const stateLabel = (state: string) =>
    ({
      waiting: t("Queued", "대기 중"),
      accepted: t("Accepted", "접수됨"),
      in_progress: t("Working", "작업 중"),
      result: t("Completed", "완료"),
      merged: t("Applied", "반영됨"),
      failed: t("Failed", "실패"),
      cancelled: t("Cancelled", "취소됨"),
      interrupted: t("Interrupted", "중단됨"),
    })[state] || state;
  const selected = s.models.find(
    (m) => selectionKey(m.selection) === selectionKey(d.selection),
  );
  const disabled = s.busy || s.refreshing;
  const sendDisabled =
    disabled ||
    (!d.pending &&
      (!s.loaded ||
        !selected?.verified ||
        tab.workspace?.status !== "ready" ||
        !d.draft.trim() ||
        activeTask(s.task) ||
        s.rows.some(activeTask)));
  return (
    <section
      className="cw-agent-panel"
      aria-label={t("Folder agent session", "폴더 에이전트 세션")}
    >
      <div className="cw-agent-context">
        <code>/workspace/shared/{d.directory}</code>
        <span>
          {d.shell
            ? t("Files + shell", "파일 + 셸")
            : t("File edits", "파일 작업")}
        </span>
      </div>
      <div className="cw-agent-model">
        <label>
          {t("Model", "모델")}
          <select
            aria-label={t("Agent model", "에이전트 모델")}
            value={selectionKey(d.selection)}
            disabled={disabled || !!d.pending}
            onChange={(e) => {
              const choice = s.models.find(
                (m) => selectionKey(m.selection) === e.target.value,
              );
              if (choice)
                manager.change(tab, node, { selection: choice.selection });
            }}
          >
            <option value="">
              {t("Choose ChatGPT / Claude", "ChatGPT / Claude 선택")}
            </option>
            {d.selection && !selected && (
              <option value={selectionKey(d.selection)} disabled>
                {t("Unavailable", "사용 불가")} · {d.selection.model}
              </option>
            )}
            {s.models.map((m) => (
              <option
                key={selectionKey(m.selection)}
                value={selectionKey(m.selection)}
              >
                {m.label}
                {m.verified ? "" : t(" · verify tools", " · 도구 확인 필요")}
              </option>
            ))}
          </select>
        </label>
        <button
          type="button"
          disabled={disabled}
          onClick={() => void manager.models(tab, node, true)}
          aria-label={t("Refresh agent models", "에이전트 모델 새로고침")}
        >
          <RefreshCw size={14} />
        </button>
      </div>
      {!!selected && !selected.verified && (
        <button
          type="button"
          disabled={disabled || !!d.pending}
          onClick={() => void manager.verify(tab, node)}
        >
          {t(
            "Verify file tools · sends a model probe",
            "파일 도구 확인 · 모델 검증 요청 전송",
          )}
        </button>
      )}
      {!s.models.length && (
        <p className="cw-agent-hint">
          {t(
            "Connect an account in Pipe → AI, then refresh the model list.",
            "Pipe → AI에서 계정을 연결한 뒤 모델 목록을 새로고침한다.",
          )}{" "}
          <a href="#/pipe/ai">Pipe → AI</a>
        </p>
      )}
      <label className="cw-agent-shell">
        <input
          type="checkbox"
          checked={!!d.shell}
          disabled={disabled || !!d.pending}
          onChange={(e) =>
            manager.change(tab, node, { shell: e.target.checked })
          }
        />
        {t("Enable shell for new requests", "새 요청에 셸 실행 사용")}
      </label>
      <div className="cw-agent-rules">
        <FileText size={13} />
        <button
          type="button"
          onClick={() =>
            void c.editor(
              tab,
              (d.directory ? d.directory + "/" : "") + "AGENTS.md",
            )
          }
        >
          AGENTS.md
        </button>
        <small>
          {s.context
            ? s.context.files
                .map((f) => f.path + " #" + f.version)
                .join(" · ") ||
              t("No saved rule files", "저장된 지침 파일 없음")
            : t(
                "Saved rules captured per request",
                "요청마다 저장된 지침 적용",
              )}
        </small>
      </div>
      <div className="cw-agent-thread">
        {s.more && (
          <button
            type="button"
            disabled={disabled}
            onClick={() => void manager.refresh(tab, node, s.rows[0]?.id)}
          >
            <ChevronUp size={13} />
            {t("Earlier tasks", "이전 작업")}
          </button>
        )}
        {!s.rows.length && (
          <div className="cw-agent-empty">
            <strong>{t("Work in this folder", "이 폴더에서 작업한다")}</strong>
            <p>
              {t(
                "Ask the agent to inspect or edit saved files.",
                "저장된 파일의 확인과 수정을 요청한다.",
              )}
            </p>
          </div>
        )}
        {s.rows.map((row) => (
          <article className="cw-agent-message" key={row.id}>
            <div className="cw-agent-user">{row.request.input}</div>
            <div className="cw-agent-answer">
              <small data-state={row.state}>
                {stateLabel(row.state)} · {row.selection.model}
              </small>
              <p>
                {row.result ||
                  row.error ||
                  t("Waiting for a result…", "결과를 기다린다…")}
              </p>
              <button
                type="button"
                disabled={disabled || !!d.pending}
                onClick={() => void manager.inspect(tab, node, row.id)}
              >
                {t("Task details", "작업 상세")}
              </button>
            </div>
          </article>
        ))}
      </div>
      {s.task && (
        <details className="cw-agent-details">
          <summary>
            {t("Selected task", "선택한 작업")} · {stateLabel(s.task.state)}
          </summary>
          <small>{s.task.id}</small>
          <p>{s.task.error}</p>
          {s.task.folderMutations?.map((m) => (
            <button
              key={m.operationId}
              type="button"
              onClick={() => {
                c.add(tab, "history");
                void c.history(tab, m.path);
              }}
            >
              {m.path} · #{m.beforeVersion} → #{m.afterVersion}
            </button>
          ))}
          {s.task.events?.map((event, i) => (
            <details key={i}>
              <summary>{event.name}</summary>
              <pre>
                {event.output}
                {event.stderr}
              </pre>
            </details>
          ))}
        </details>
      )}
      {s.error && (
        <p className="cw-agent-error" role="alert">
          {s.error}
        </p>
      )}
      {d.pending && (
        <p className="cw-agent-hint">
          {t(
            "Request confirmation is pending. Retry keeps the original task ID and saved rules.",
            "요청 확인이 필요하다. 재시도는 같은 작업 ID와 지침을 사용한다.",
          )}
        </p>
      )}
      <form
        className="cw-agent-compose"
        onSubmit={(e) => {
          e.preventDefault();
          void manager.submit(tab, node);
        }}
      >
        <textarea
          aria-label={t("Agent request", "에이전트 요청")}
          value={d.draft}
          maxLength={4096}
          placeholder={t("Describe a file task…", "파일 작업을 요청한다…")}
          onChange={(e) => manager.change(tab, node, { draft: e.target.value })}
        />
        <div>
          <small>
            {d.shell
              ? t(
                  "Commands run in this project's VM. Changes are shared immediately.",
                  "명령은 이 프로젝트의 VM에서 실행한다. 변경사항은 즉시 공유한다.",
                )
              : t(
                  "Changes are saved to the shared folder immediately. Shell commands are unavailable.",
                  "변경사항은 공용 폴더에 즉시 저장한다. 셸 명령은 지원하지 않는다.",
                )}
          </small>
          <button type="submit" disabled={sendDisabled}>
            <Play size={13} />
            {d.pending
              ? t("Retry saved request", "저장된 요청 재시도")
              : s.busy
                ? t("Confirming…", "확인 중…")
                : d.shell
                  ? t("Start shell task", "셸 작업 시작")
                  : t("Start file task", "파일 작업 시작")}
          </button>
        </div>
      </form>
      <footer>
        <button
          type="button"
          disabled={disabled}
          onClick={() => void manager.refresh(tab, node)}
        >
          <RefreshCw size={13} />
          {t("Refresh tasks", "작업 새로고침")}
        </button>
        {activeTask(s.task) && (
          <button
            type="button"
            disabled={disabled}
            onClick={() => void manager.control(tab, node, "cancel")}
          >
            <Square size={12} />
            {t("Cancel task", "작업 취소")}
          </button>
        )}
        {s.task?.state === "interrupted" && (
          <button
            type="button"
            disabled={disabled}
            onClick={() => void manager.control(tab, node, "reconcile")}
          >
            {t("Check saved receipts", "저장 확인 기록 조회")}
          </button>
        )}
      </footer>
    </section>
  );
}
