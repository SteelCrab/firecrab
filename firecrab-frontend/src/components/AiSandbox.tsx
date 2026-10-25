import { useCallback, useEffect, useRef, useState, type FormEvent } from "react";
import { ArrowUpRight, Bot, CircleCheck, CircleX, FileDiff, FolderGit2, Kanban, LoaderCircle, MessageSquare, Plus, RefreshCw, RotateCcw, Send, Settings2, ShieldCheck, Square, Terminal, Trash2 } from "lucide-react";
import { listAiConnections, type AiConnection } from "../api/pipeAi";
import { activeTask, cancelTask, createTask, deleteProject, listModels, listProjects, listTasks, reconcileTask, selectionFromKey, selectionKey, summarizeTask, type ModelCatalog, type Project, type SandboxTask, type TaskSummary, type TaskState } from "../api/aiSandbox";
import type { VmResponse } from "../bindings";
import { useI18n } from "../i18n";
import Screen from "./layout/Screen";
import ProjectDialog from "./ai-sandbox/ProjectDialog";
import { navigateTabs } from "./ai-sandbox/keyboard";
import TaskReview from "./ai-sandbox/TaskReview";
import "../styles/ai-sandbox.css";

interface Props { vms: VmResponse[]; onUnsavedChange: (dirty: boolean) => void }
export default function AiSandbox({ vms, onUnsavedChange }: Props) {
  const { t } = useI18n();
  const [projects, setProjects] = useState<Project[] | null>(null);
  const [accounts, setAccounts] = useState<AiConnection[]>([]);
  const [catalogs, setCatalogs] = useState<Record<string, ModelCatalog>>({});
  const [projectId, setProjectId] = useState(() => new URLSearchParams(window.location.hash.split("?")[1]).get("project") ?? "");
  const [taskSnapshot, setTaskSnapshot] = useState<{ project: string; items: TaskSummary[] } | null>(null);
  const [tab, setTab] = useState<"chat" | "tasks">("chat");
  const [kind, setKind] = useState<"chat" | "code">("chat");
  const [input, setInput] = useState("");
  const [override, setOverride] = useState("");
  const [editing, setEditing] = useState<Project | "new" | null>(null);
  const [reviewId, setReviewId] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [stopping, setStopping] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [taskFailure, setTaskFailure] = useState<{ project: string; message: string } | null>(null);
  const [refresh, setRefresh] = useState(0);
  const [olderEnd, setOlderEnd] = useState<string | null>(null);
  const pollRevision = useRef(0);
  const pending = useRef<{ key: string; id: string } | null>(null);
  const messageEnd = useRef<HTMLDivElement>(null);
  const project = projects?.find(p => p.id === projectId) ?? projects?.[0] ?? null;
  const selectedId = project?.id;
  const tasks = taskSnapshot && taskSnapshot.project === selectedId ? taskSnapshot.items : null;
  const taskError = taskFailure && taskFailure.project === selectedId ? taskFailure.message : null;
  const taskCount = tasks?.length;
  const lastTaskState = tasks?.at(-1)?.state;
  useEffect(() => {
    const controller = new AbortController();
    void Promise.all([listProjects(controller.signal), listAiConnections(controller.signal)]).then(async ([projects, accounts]) => {
      if (controller.signal.aborted) return;
      setProjects(projects); setAccounts(accounts); setError(null);
      const values = await Promise.allSettled(accounts.map(async a => [a.id, await listModels(a.id, false, controller.signal)] as const));
      if (!controller.signal.aborted) setCatalogs(Object.fromEntries(values.flatMap(r => r.status === "fulfilled" ? [r.value] : [])));
    }).catch((e: Error) => { if (!controller.signal.aborted) { setProjects(null); setError(e.message); } });
    return () => controller.abort();
  }, [refresh]);
  useEffect(() => {
    if (!selectedId) return;
    let since: number | undefined; let cancelled = false; let timer: ReturnType<typeof setTimeout>; let controller: AbortController;
    async function poll() {
      controller = new AbortController(); const revision = pollRevision.current;
      try {
        const result = await listTasks(selectedId!, controller.signal, since);
        if (!cancelled && revision === pollRevision.current) { setTaskSnapshot(current => {
          const items = since !== undefined && current && current.project === selectedId ? current.items : [];
          const merged = [...items];
          for (const task of result) { const index = merged.findIndex(t => t.id === task.id); if (index < 0) merged.push(task); else if (task.updatedAt >= merged[index].updatedAt) merged[index] = task; }
          return { project: selectedId!, items: merged.sort((a,b) => a.createdAt - b.createdAt) };
        }); since = Math.max(since ?? 0, ...result.map(t => t.updatedAt)); setTaskFailure(null); if (result.length && !result.some(activeTask)) setStopping(null); }
      } catch (e) { if (!cancelled) { setTaskFailure({ project: selectedId!, message: (e as Error).message }); } }
      finally { if (!cancelled) timer = setTimeout(() => void poll(), 1500); }
    }
    void poll();
    return () => { cancelled = true; controller?.abort(); clearTimeout(timer); };
  }, [selectedId, refresh]);
  useEffect(() => {
    onUnsavedChange(!!input.trim() || editing !== null);
    const beforeUnload = (event: BeforeUnloadEvent) => { if (input.trim() || editing !== null) { event.preventDefault(); event.returnValue = ""; } };
    window.addEventListener("beforeunload", beforeUnload);
    return () => { onUnsavedChange(false); window.removeEventListener("beforeunload", beforeUnload); };
  }, [input, editing, onUnsavedChange]);
  useEffect(() => { messageEnd.current?.scrollIntoView({ block: "nearest" }); }, [taskCount, lastTaskState]);
  const updateTask = useCallback((full: SandboxTask) => {
    const next = summarizeTask(full);
    pollRevision.current++;
    setTaskSnapshot(current => { const items = current?.project === next.projectId ? current.items : []; return { project: next.projectId, items: items.some(t => t.id === next.id) ? items.map(t => t.id === next.id && next.updatedAt >= t.updatedAt ? next : t) : [...items, next] }; });
  }, []);
  function chooseProject(id: string) {
    if (busy) return;
    if (input.trim() && !window.confirm(t("Discard the unsent message and switch projects?", "보내지 않은 내용을 지우고 프로젝트를 바꿀까요?"))) return;
    setInput(""); setOverride(""); setProjectId(id); setReviewId(null); pending.current = null;
    window.history.replaceState(window.history.state, "", `#/ai-sandbox?project=${id}`);
  }
  async function send(event?: FormEvent, retry?: TaskSummary) {
    event?.preventDefault(); if (!project || busy) return;
    const request = retry ? { ...retry.request, selection: retry.selection, retryOf: retry.id } : { kind, input: input.trim(), selection: selectionFromKey(override), retryOf: null };
    if (!request.input) return;
    const key = JSON.stringify({ project: project.id, request });
    const id = pending.current?.key === key ? pending.current.id : crypto.randomUUID(); pending.current = { key, id };
    setBusy(true); setError(null);
    try {
      const task = await createTask(project.id, id, request); updateTask(task); pending.current = null; setInput("");
      if (request.kind === "code") setTab("tasks");
    } catch (e) { setError((e as Error).message); } finally { setBusy(false); }
  }
  async function stop(task: TaskSummary) {
    setStopping(task.id); setError(null);
    try { await cancelTask(task.id); } catch (e) { setStopping(null); setError((e as Error).message); }
  }
  async function reconcile(task: TaskSummary) {
    setBusy(true); setError(null);
    try { updateTask(await reconcileTask(task.id)); } catch (e) { setError((e as Error).message); } finally { setBusy(false); }
  }
  async function loadOlder() {
    if (!selectedId || !tasks?.length) return; setBusy(true); setError(null);
    try {
      const older = await listTasks(selectedId, undefined, undefined, tasks[0].id);
      if (older.length < 50) setOlderEnd(selectedId);
      setTaskSnapshot(current => { if (!current || current.project !== selectedId) return current; const ids = new Set(current.items.map(t => t.id)); return { ...current, items: [...older.filter(t => !ids.has(t.id)), ...current.items] }; });
    } catch (e) { setError((e as Error).message); } finally { setBusy(false); }
  }
  async function removeProject() {
    if (!project || !window.confirm(t("Delete this project's conversation and retained task copies? Source files and the MicroVM are preserved.", "대화와 보관된 작업 복사본을 삭제할까요? 원본 파일과 MicroVM은 유지한다."))) return;
    setBusy(true); setError(null);
    try { await deleteProject(project.id); setProjects(current => current?.filter(p => p.id !== project.id) ?? []); setProjectId(""); setInput(""); } catch (e) { setError((e as Error).message); } finally { setBusy(false); }
  }
  function stateLabel(state: TaskState) {
    return ({ waiting: t("Waiting", "대기"), accepted: t("Preparing", "준비 중"), in_progress: t("In progress", "진행 중"), result: t("Result", "결과"), merged: t("Applied", "반영됨"), failed: t("Failed", "실패"), cancelled: t("Cancelled", "취소됨"), interrupted: t("Needs reconciliation", "상태 확인 필요") })[state];
  }
  const running = tasks?.find(activeTask);
  const effective = selectionFromKey(override) ?? project?.spec.selection;
  const effectiveModel = effective ? catalogs[effective.connectionId]?.models.find(m => m.id === effective.model && m.verified) : null;
  const canRunCode = !!project?.spec.sourcePath && !!project?.spec.vmId && effectiveModel?.toolsVerified && vms.some(vm => vm.id === project.spec.vmId && vm.state === "running");
  const canSend = !!project && !!input.trim() && !!effectiveModel && !busy && !running && !taskError && tasks !== null && (kind === "chat" || canRunCode);
  const review = tasks?.find(task => task.id === reviewId);
  const codeTasks = tasks?.filter(task => task.request.kind === "code") ?? [];
  const taskCard = (task: TaskSummary) => <button key={task.id} className="sb-task-card" onClick={() => setReviewId(task.id)}><span className={`sb-task-state ${task.state}`}>{activeTask(task) ? <LoaderCircle className="sb-spin" /> : task.state === "failed" || task.state === "interrupted" ? <CircleX /> : <CircleCheck />}{stateLabel(task.state)}</span><strong>{task.request.input}</strong><span className="sb-card-meta">{task.selection.model}<span>{task.changeCount > 0 ? t(`${task.changeCount} files`, `파일 ${task.changeCount}개`) : task.attemptId.slice(0, 8)}</span></span>{task.error && <span className="sb-error-text">{task.error}</span>}</button>;
  return <Screen view="ai-sandbox" flush>
    <div className="sb-layout">
      <aside className="sb-projects" aria-label={t("Sandbox projects", "Sandbox 프로젝트")}><div className="sb-projects-title"><span>{t("Projects", "프로젝트")}</span><button className="icon-btn" onClick={() => setEditing("new")} aria-label={t("New project", "새 프로젝트")}><Plus /></button></div><div className="sb-project-list">{projects?.map(item => <button key={item.id} aria-current={item.id === project?.id ? "page" : undefined} className="sb-project" onClick={() => chooseProject(item.id)}><FolderGit2 /><span>{item.spec.name}</span></button>)}{projects?.length === 0 && <p className="sb-hint">{t("A project keeps your context, model and tasks together.", "프로젝트에 지침, 모델, 작업을 함께 저장한다.")}</p>}</div><a href="#/pipe/ai" className="sb-account-link"><ShieldCheck /><span>Pipe AI</span><ArrowUpRight /></a></aside>
      <section className="sb-workspace" aria-label="AI Sandbox">
        {error && <div className="sb-banner banner error" role="alert">{error}<button className="icon-btn" onClick={() => setRefresh(v => v + 1)} aria-label={t("Retry", "다시 시도")}><RefreshCw /></button></div>}
        {projects === null && !error && <div className="sb-welcome"><LoaderCircle className="sb-spin" /><p>{t("Loading projects…", "프로젝트를 불러오는 중…")}</p></div>}
        {projects !== null && !project && <div className="sb-welcome"><div className="sb-welcome-icon"><Bot /></div><h2>{t("A place to think. A place to build.", "대화에서 시작해, 실행까지.")}</h2><p>{t("Choose a connected AI account, bring a project, and work in a MicroVM.", "연결한 AI 계정으로 대화하고, 프로젝트를 MicroVM에서 실행한다.")}</p><button className="btn primary" onClick={() => setEditing("new")}><Plus />{t("Create a project", "프로젝트 만들기")}</button>{!accounts.length && <a href="#/pipe/ai">{t("Connect an AI account first", "먼저 AI 계정 연결")} ↗</a>}</div>}
        {project && <>
          <header className="sb-workspace-header"><div className="sb-project-heading"><FolderGit2 /><div><h2>{project.spec.name}</h2><span>{project.spec.vmId ? vms.find(vm => vm.id === project.spec.vmId)?.name ?? t("MicroVM unavailable", "MicroVM 확인 불가") : t("Conversation workspace", "대화 작업 공간")}</span></div></div><div className="sb-row"><button className="icon-btn" disabled={busy || !!running} onClick={() => setEditing(project)} aria-label={t("Project settings", "프로젝트 설정")}><Settings2 /></button><button className="icon-btn" disabled={busy || !!running} onClick={() => void removeProject()} aria-label={t("Delete project", "프로젝트 삭제")}><Trash2 /></button></div></header>
          <div className="sb-tabs" role="tablist" onKeyDown={navigateTabs} aria-label={t("Sandbox view", "Sandbox 보기")}><button role="tab" id="sb-chat-tab" aria-controls="sb-panel" tabIndex={tab === "chat" ? 0 : -1} aria-selected={tab === "chat"} onClick={() => setTab("chat")}><MessageSquare />{t("Conversation", "대화")}</button><button role="tab" id="sb-tasks-tab" aria-controls="sb-panel" tabIndex={tab === "tasks" ? 0 : -1} aria-selected={tab === "tasks"} onClick={() => setTab("tasks")}><Kanban />{t("Tasks", "작업")}<span>{codeTasks.length}</span></button></div>
          {taskError && <div className="banner error sb-banner" role="alert">{taskError}</div>}
          <div className="sb-content" id="sb-panel" role="tabpanel" aria-labelledby={`sb-${tab}-tab`}>
            {tasks === null && !taskError && <div className="sb-welcome"><LoaderCircle className="sb-spin" /></div>}
            {tab === "chat" && tasks !== null && <div className="sb-messages" aria-label={t("Conversation messages", "대화 메시지")}>
              {!tasks.length && <div className="sb-conversation-empty"><Bot /><h3>{t("What would you like to work on?", "어떤 작업을 시작할까?")}</h3><p>{t("Ask a question, or switch to Code task to edit and test your project.", "질문을 보내거나 코드 작업으로 전환해 프로젝트를 수정하고 테스트한다.")}</p><div className="sb-suggestions">{[t("Help me plan my next feature", "다음 기능의 구현 계획을 세워줘"), t("Find a bug and add a regression test", "버그를 찾고 회귀 테스트를 추가해줘")].map((text, index) => <button key={text} onClick={() => { setInput(text); setKind(index === 0 ? "chat" : "code"); }}><span>{text}</span><ArrowUpRight /></button>)}</div></div>}
              {tasks.length >= 50 && olderEnd !== selectedId && <button className="btn quiet" disabled={busy} onClick={() => void loadOlder()}>{t("Load older messages", "이전 대화 불러오기")}</button>}
              {tasks.map(task => <article key={task.id} className="sb-exchange"><div className="sb-user-message"><span>{t("You", "사용자")}</span><p>{task.request.input}</p></div><div className="sb-assistant-message"><div className="sb-message-avatar"><Bot /></div><div className="sb-message-body"><header><strong>{task.selection.model}</strong><span className={`sb-task-state ${task.state}`}>{stateLabel(task.state)}</span></header>{task.result && <div className="sb-message-text">{task.result}</div>}{activeTask(task) && <div className="sb-progress" role="status"><LoaderCircle className="sb-spin" />{task.lastTool ?? t("Preparing response…", "응답을 준비하는 중…")}</div>}{task.error && <p className="sb-error-text">{task.error}</p>}<div className="sb-message-actions">{task.request.kind === "code" && <button className="btn quiet" onClick={() => setReviewId(task.id)}><FileDiff />{t("Review changes & activity", "변경·실행 기록 검토")}</button>}{["failed", "cancelled"].includes(task.state) && <button className="btn quiet" disabled={busy || !!running} onClick={() => void send(undefined, task)}><RotateCcw />{t("Retry as a new attempt", "새 실행으로 다시 시도")}</button>}{task.state === "interrupted" && <button className="btn" disabled={busy} onClick={() => void reconcile(task)}><RefreshCw />{t("Reconcile attempt", "실행 상태 확인")}</button>}</div></div></div></article>)}<div ref={messageEnd} /></div>}
            {tab === "tasks" && <div className="sb-board" aria-label={t("Task board", "작업 보드")}>{[{ title: t("Waiting", "대기"), states: ["waiting", "accepted"] }, { title: t("In progress", "진행 중"), states: ["in_progress"] }, { title: t("Results", "결과"), states: ["result", "merged", "failed", "cancelled", "interrupted"] }].map(column => <section key={column.title} className="sb-board-column"><h3>{column.title}<span>{codeTasks.filter(task => column.states.includes(task.state)).length}</span></h3>{codeTasks.filter(task => column.states.includes(task.state)).map(taskCard)}{!codeTasks.some(task => column.states.includes(task.state)) && <p className="sb-column-empty">{t("No tasks", "작업 없음")}</p>}</section>)}</div>}
          </div>
          <form className="sb-composer" onSubmit={event => void send(event)}>
            <div className="sb-composer-box"><label className="sr-only" htmlFor="sb-input">{t("Message or task", "대화 또는 작업 내용")}</label><textarea id="sb-input" rows={3} maxLength={32768} value={input} onChange={e => setInput(e.target.value)} onKeyDown={e => { if ((e.metaKey || e.ctrlKey) && e.key === "Enter" && !e.nativeEvent.isComposing && canSend) { e.preventDefault(); void send(); } }} placeholder={kind === "code" ? t("Describe a change and how to test it…", "수정할 내용과 테스트 방법을 설명한다…") : t("Ask a question or plan your next step…", "질문이나 다음 작업을 입력한다…")} /><div className="sb-composer-tools"><div className="sb-row"><select className="sb-kind" aria-label={t("Execution mode", "실행 모드")} value={kind} onChange={e => setKind(e.target.value as "chat" | "code")}><option value="chat">{t("Conversation", "대화")}</option><option value="code">{t("Code task", "코드 작업")}</option></select><select className="sb-model-select" aria-label={t("Task model", "작업 모델")} value={override} onChange={e => setOverride(e.target.value)}><option value="">{project.spec.selection?.model ?? t("Choose a default model", "기본 모델 선택")}</option>{accounts.map(account => <optgroup key={account.id} label={account.name}>{catalogs[account.id]?.models.filter(m => m.verified).map(model => <option key={model.id} value={selectionKey({ connectionId: account.id, model: model.id })}>{model.name !== model.id ? `${model.name} (${model.id})` : model.id}{model.toolsVerified ? " · tools" : ""}</option>)}</optgroup>)}</select></div>{running ? <button type="button" className="sb-send stopping" disabled={stopping === running.id} onClick={() => void stop(running)} aria-label={t("Stop task", "작업 중지")}>{stopping === running.id ? <LoaderCircle className="sb-spin" /> : <Square />}</button> : <button className="sb-send" disabled={!canSend} aria-label={kind === "code" ? t("Run code task", "코드 작업 실행") : t("Send message", "메시지 전송")}>{busy ? <LoaderCircle className="sb-spin" /> : <Send />}</button>}</div></div>
            <div className="sb-composer-note"><span>{kind === "code" ? <><Terminal />{canRunCode ? t("Runs inside the selected MicroVM · source changes require review", "선택한 MicroVM에서 실행 · 원본 반영 전 검토") : t("Set a project path, running MicroVM and tool-verified model in settings", "설정에서 프로젝트 경로, 실행 중인 MicroVM, 도구 검증 모델을 선택한다")}</> : <><ShieldCheck />{effectiveModel ? t("Uses your connected account", "연결한 계정으로 요청") : t("Verify and select a model in project settings", "프로젝트 설정에서 모델을 검증하고 선택한다")}</>}</span><kbd>⌘ / Ctrl ↵</kbd></div>
          </form>
        </>}
      </section>
    </div>
    {editing && <ProjectDialog key={editing === "new" ? "new" : editing.id} project={editing === "new" ? null : editing} accounts={accounts} vms={vms} onClose={() => setEditing(null)} onSaved={saved => { setProjects(current => current?.some(p => p.id === saved.id) ? current.map(p => p.id === saved.id ? saved : p) : [...(current ?? []), saved]); setEditing(null); chooseProject(saved.id); setRefresh(v => v + 1); }} />}
    {review && <TaskReview key={review.id} taskId={review.id} onRetry={() => send(undefined, review)} onClose={() => setReviewId(null)} onChanged={updateTask} />}
  </Screen>;
}
