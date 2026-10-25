import { useEffect, useRef, useState } from "react";
import { Check, Download, FileDiff, GitMerge, LoaderCircle, RefreshCw, RotateCcw, Square, Terminal, X } from "lucide-react";
import { activeTask, applyTask, cancelTask, getTask, reconcileTask, type SandboxTask } from "../../api/aiSandbox";
import { navigateTabs } from "./keyboard";
import { useI18n } from "../../i18n";

interface Props { taskId: string; onRetry: () => Promise<void>; onClose: () => void; onChanged: (task: SandboxTask) => void }
export default function TaskReview({ taskId, onRetry, onClose, onChanged }: Props) {
  const { t } = useI18n();
  const [task, setTask] = useState<SandboxTask | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    let done = false; let timer: ReturnType<typeof setTimeout>; let controller: AbortController;
    async function load() {
      controller = new AbortController();
      try { const value = await getTask(taskId, controller.signal); if (!done) { setTask(value); setError(null); if (activeTask(value)) timer = setTimeout(() => void load(), 1500); } }
      catch (e) { if (!done) { setError((e as Error).message); timer = setTimeout(() => void load(), 3000); } }
    }
    void load(); return () => { done = true; controller?.abort(); clearTimeout(timer); };
  }, [taskId]);
  if (!task) return <div className="sb-banner" role={error ? "alert" : "status"}>{error ?? t("Loading task…", "작업을 불러오는 중…")}<button className="btn" onClick={onClose}>{t("Close", "닫기")}</button></div>;
  return <TaskReviewContent loadError={error} task={task} onRetry={onRetry} onClose={onClose} onChanged={value => { setTask(value); onChanged(value); }} />;
}
interface ContentProps { loadError: string | null; task: SandboxTask; onRetry: () => Promise<void>; onClose: () => void; onChanged: (task: SandboxTask) => void }
function TaskReviewContent({ loadError, task, onRetry, onClose, onChanged }: ContentProps) {
  const { t } = useI18n();
  const dialog = useRef<HTMLDialogElement>(null);
  const [tab, setTab] = useState<"changes" | "activity">(task.changes.length ? "changes" : "activity");
  const [selected, setSelected] = useState<string[]>(task.changes.filter(c => !c.applied).map(c => c.path));
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => { const opener = document.activeElement; dialog.current?.showModal(); return () => { if (opener instanceof HTMLElement && opener.isConnected) opener.focus(); }; }, []);
  const paths = selected.filter(path => task.changes.some(c => c.path === path && !c.applied));
  async function apply() {
    setBusy(true); setError(null);
    try { onChanged(await applyTask(task.id, paths)); } catch (e) { setError((e as Error).message); } finally { setBusy(false); }
  }
  async function act(action: "stop" | "reconcile" | "retry") {
    setBusy(true); setError(null);
    try { if (action === "retry") { await onRetry(); onClose(); } else onChanged(await (action === "stop" ? cancelTask(task.id) : reconcileTask(task.id))); }
    catch (e) { setError((e as Error).message); } finally { setBusy(false); }
  }
  function download() {
    const url = URL.createObjectURL(new Blob([JSON.stringify(task, null, 2)], { type: "application/json" }));
    const a = document.createElement("a"); a.href = url; a.download = `sandbox-${task.id}.json`; a.click(); setTimeout(() => URL.revokeObjectURL(url), 1000);
  }
  return <dialog ref={dialog} className="sb-dialog sb-review" aria-labelledby="sb-review-title" onCancel={e => { if (busy) e.preventDefault(); }} onClose={onClose}>
    <header className="sb-dialog-header"><h2 id="sb-review-title"><FileDiff aria-hidden="true" />{t("Task result", "작업 결과")}</h2><div className="sb-row"><button className="icon-btn" onClick={download} aria-label={t("Export task result", "작업 결과 내보내기")}><Download /></button><button className="icon-btn" disabled={busy} onClick={() => dialog.current?.close()} aria-label={t("Close", "닫기")}><X /></button></div></header>
    <div className="sb-review-summary"><p>{task.request.input}</p><span>{task.selection.model} · {task.attemptId.slice(0, 8)}</span>{task.result && <p className="sb-message-text">{task.result}</p>}</div>
    <div className="sb-tabs" role="tablist" onKeyDown={navigateTabs} aria-label={t("Task details", "작업 상세")}><button role="tab" id="sb-review-changes-tab" aria-controls="sb-review-panel" tabIndex={tab === "changes" ? 0 : -1} aria-selected={tab === "changes"} onClick={() => setTab("changes")}><FileDiff />{t("Changes", "변경 내용")} <span>{task.changes.length}</span></button><button role="tab" id="sb-review-activity-tab" aria-controls="sb-review-panel" tabIndex={tab === "activity" ? 0 : -1} aria-selected={tab === "activity"} onClick={() => setTab("activity")}><Terminal />{t("Activity", "실행 기록")} <span>{task.events.length}</span></button></div>
    <div className="sb-review-body" id="sb-review-panel" role="tabpanel" aria-labelledby={`sb-review-${tab}-tab`}>
      {(error || loadError) && <div className="banner error" role="alert">{error || loadError}</div>}
      {task.error && <div className="banner error">{task.error}</div>}
      {tab === "changes" && <>
        {!task.changes.length && <p className="sb-empty-note">{activeTask(task) ? t("Changes become available when the attempt stops.", "실행이 끝나면 변경 내용을 확인할 수 있다.") : t("This attempt did not change any project files.", "이 실행에서 변경한 프로젝트 파일이 없다.")}</p>}
        {task.changes.map(change => <details className="sb-change" key={change.path} open={task.changes.length === 1}><summary><label onClick={e => e.stopPropagation()}><input type="checkbox" disabled={change.applied || busy || activeTask(task)} checked={change.applied || paths.includes(change.path)} onChange={e => setSelected(current => e.target.checked ? [...current, change.path] : current.filter(p => p !== change.path))} aria-label={t(`Apply ${change.path}`, `${change.path} 반영`)} />{change.path}</label>{change.applied ? <span className="sb-verified"><Check />{t("Applied", "반영됨")}</span> : <span>{change.before === null ? t("Added", "추가") : change.after === null ? t("Deleted", "삭제") : t("Modified", "수정")}</span>}</summary><div className="sb-diff-grid"><div><h4>{t("Before", "변경 전")} {change.beforeMode?.toString(8)}</h4><pre className="sb-diff-before">{change.before ?? t("New file", "새 파일")}</pre></div><div><h4>{t("After", "변경 후")} {change.afterMode?.toString(8)}</h4><pre className="sb-diff-after">{change.after ?? t("File deleted", "파일 삭제")}</pre></div></div></details>)}
      </>}
      {tab === "activity" && <>
        {!task.events.length && <p className="sb-empty-note">{t("No tool calls in this attempt.", "이 실행에는 도구 호출 기록이 없다.")}</p>}
        {task.events.map(event => <details className="sb-event" key={event.sequence} open={task.events.length < 4}><summary><Terminal aria-hidden="true" /><strong>{event.name}</strong>{event.exitCode !== null && <span className={event.exitCode === 0 ? "sb-exit ok" : "sb-exit error"}>{t("exit", "종료 코드")} {event.exitCode}</span>}</summary><pre className="sb-event-input">{event.input}</pre>{event.output && <><h4>stdout</h4><pre>{event.output}</pre></>}{event.stderr && <><h4>stderr</h4><pre className="sb-error-text">{event.stderr}</pre></>}</details>)}
      </>}
    </div>
    <footer className="sb-dialog-footer"><p className="sb-hint">{t("Only selected files are applied. A changed source file is reported as a conflict.", "선택한 파일만 원본에 반영한다. 원본이 바뀌었으면 충돌을 알린다.")}</p>{activeTask(task) && <button className="btn" disabled={busy} onClick={() => void act("stop")}><Square />{t("Stop task", "작업 중지")}</button>}{task.state === "interrupted" && <button className="btn" disabled={busy} onClick={() => void act("reconcile")}><RefreshCw />{t("Reconcile attempt", "실행 상태 확인")}</button>}{["failed", "cancelled"].includes(task.state) && <button className="btn" disabled={busy} onClick={() => void act("retry")}><RotateCcw />{t("New attempt", "새 실행")}</button>}<button className="btn primary" disabled={busy || !!loadError || !paths.length || activeTask(task) || task.state === "interrupted"} onClick={() => void apply()}>{busy ? <LoaderCircle className="sb-spin" /> : <GitMerge />}{t("Apply selected files", "선택한 파일 반영")}</button></footer>
  </dialog>;
}
