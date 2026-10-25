import { useEffect, useRef, useState, type FormEvent } from "react";
import { Check, FolderGit2, LoaderCircle, RefreshCw, ShieldCheck, X } from "lucide-react";
import type { AiConnection } from "../../api/pipeAi";
import { listModels, saveProject, verifyModel, type ModelCatalog, type ModelSelection, type Project, type ProjectSpec } from "../../api/aiSandbox";
import type { VmResponse } from "../../bindings";
import { useI18n } from "../../i18n";

interface Props { project: Project | null; accounts: AiConnection[]; vms: VmResponse[]; onClose: () => void; onSaved: (project: Project) => void }

export default function ProjectDialog({ project, accounts, vms, onClose, onSaved }: Props) {
  const { t } = useI18n();
  const dialog = useRef<HTMLDialogElement>(null);
  const projectKey = useRef(project?.id ?? crypto.randomUUID());
  const [spec, setSpec] = useState<ProjectSpec>(project?.spec ?? { name: "", sourcePath: null, vmId: null, selection: null, instructions: "" });
  const [connection, setConnection] = useState(project?.spec.selection?.connectionId ?? accounts[0]?.id ?? "");
  const [model, setModel] = useState(project?.spec.selection?.model ?? "");
  const [catalog, setCatalog] = useState<ModelCatalog | null>(null);
  const [tools, setTools] = useState(true);
  const [loading, setLoading] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const revision = useRef(0);
  useEffect(() => { const opener = document.activeElement; dialog.current?.showModal(); return () => { if (opener instanceof HTMLElement && opener.isConnected) opener.focus(); }; }, []);
  useEffect(() => {
    if (!connection) return;
    const controller = new AbortController(); const current = ++revision.current;
    void listModels(connection, false, controller.signal).then(value => { if (current === revision.current) setCatalog(value); }).catch((e: Error) => { if (!controller.signal.aborted) setError(e.message); });
    return () => controller.abort();
  }, [connection]);
  async function discover() {
    setLoading(true); setError(null);
    try { setCatalog(await listModels(connection, true)); } catch (e) { setError((e as Error).message); } finally { setLoading(false); }
  }
  async function verify() {
    setBusy(true); setError(null);
    try {
      const verified = await verifyModel(connection, model.trim(), tools);
      setCatalog(current => ({ discoveryAvailable: current?.discoveryAvailable ?? false, models: [...(current?.models ?? []).filter(m => m.id !== verified.id), verified] }));
      setSpec(current => ({ ...current, selection: { connectionId: connection, model: verified.id } }));
    } catch (e) { setError((e as Error).message); } finally { setBusy(false); }
  }
  function chooseModel(value: string) {
    setModel(value);
    const verified = catalog?.models.find(m => m.id === value && m.verified);
    setSpec(current => ({ ...current, selection: verified ? { connectionId: connection, model: value } : null }));
  }
  async function submit(event: FormEvent) {
    event.preventDefault(); setBusy(true); setError(null);
    try { onSaved(await saveProject(projectKey.current, spec)); } catch (e) { setError((e as Error).message); } finally { setBusy(false); }
  }
  const selected = catalog?.models.find(m => m.id === model && m.verified);
  const selection: ModelSelection | null = spec.selection;
  return <dialog ref={dialog} className="sb-dialog" aria-labelledby="sb-project-title" onCancel={event => { if (busy) event.preventDefault(); }} onClose={onClose}>
    <header className="sb-dialog-header"><h2 id="sb-project-title"><FolderGit2 aria-hidden="true" />{project ? t("Project settings", "프로젝트 설정") : t("New project", "새 프로젝트")}</h2><button className="icon-btn" aria-label={t("Close", "닫기")} disabled={busy} onClick={() => dialog.current?.close()}><X /></button></header>
    <form onSubmit={submit} className="sb-project-form">
      {error && <div className="banner error" role="alert">{error}</div>}
      <label>{t("Project name", "프로젝트 이름")}<input disabled={busy} autoFocus required maxLength={200} value={spec.name} onChange={e => setSpec({ ...spec, name: e.target.value })} /></label>
      <fieldset className="sb-fieldset" disabled={busy}><legend>{t("Model", "모델")}</legend>
        {!accounts.length ? <p className="muted">{t("Connect an account in Pipe AI to use models.", "Pipe AI에서 계정을 연결한 뒤 모델을 사용한다.")} <a href="#/pipe/ai">Pipe AI ↗</a></p> : <>
          <label>{t("Connected account", "연결한 계정")}<select value={connection} disabled={busy || loading} onChange={e => { setConnection(e.target.value); setModel(""); setCatalog(null); setSpec({ ...spec, selection: null }); setError(null); }}>{accounts.map(a => <option key={a.id} value={a.id}>{a.name} · {a.authentication === "chatgpt" ? "ChatGPT" : a.provider === "anthropic" ? "Anthropic" : "OpenAI"}</option>)}</select></label>
          <div className="sb-model-entry"><label>{t("Model ID", "모델 ID")}<input list="sb-models" value={model} disabled={busy} placeholder={t("Choose or enter a model ID", "모델을 고르거나 ID 입력")} onChange={e => chooseModel(e.target.value)} /></label><button type="button" className="btn" onClick={() => void discover()} disabled={!connection || loading || busy} aria-label={t("Discover models", "모델 목록 조회")}>{loading ? <LoaderCircle className="sb-spin" /> : <RefreshCw />}</button></div>
          <datalist id="sb-models">{catalog?.models.map(m => <option key={m.id} value={m.id}>{m.name}{m.verified ? " ✓" : ""}</option>)}</datalist>
          {accounts.find(a => a.id === connection)?.authentication === "chatgpt" && <p className="sb-hint">{t("Refresh your ChatGPT model list, or enter a model ID, then verify access.", "ChatGPT 모델 목록을 조회하거나 ID를 입력한 뒤 접근 권한을 검증한다.")}</p>}
          <label className="sb-check"><input type="checkbox" checked={tools} disabled={busy} onChange={e => setTools(e.target.checked)} />{t("Verify tool support for code tasks", "코드 작업에 필요한 도구 호출도 검증")}</label>
          <div className="sb-verify"><button type="button" className="btn" disabled={!connection || !model.trim() || busy || loading} onClick={() => void verify()}>{busy ? <LoaderCircle className="sb-spin" /> : <ShieldCheck />}{t("Verify access", "접근 권한 검증")}</button>{selected && selection?.model === model && <span className="sb-verified"><Check />{selected.toolsVerified ? t("Conversation + code tasks", "대화 + 코드 작업") : t("Conversation verified", "대화 검증 완료")}</span>}</div>
          <p className="sb-hint">{t("Verification sends small provider requests. Model usage may be billed by your provider.", "검증 시 공급자에 작은 요청을 보낸다. 공급자 정책에 따라 사용 요금이 발생할 수 있다.")}</p>
        </>}
      </fieldset>
      <fieldset className="sb-fieldset" disabled={busy}><legend>{t("Code workspace", "코드 작업 공간")}</legend>
        <label>{t("Project directory on API host", "API 호스트의 프로젝트 경로")}<input value={spec.sourcePath ?? ""} placeholder="/home/me/my-project" onChange={e => setSpec({ ...spec, sourcePath: e.target.value || null })} /></label>
        <label>{t("Execution MicroVM", "실행 MicroVM")}<select value={spec.vmId ?? ""} onChange={e => setSpec({ ...spec, vmId: e.target.value || null })}><option value="">{t("Conversation only", "대화만 사용")}</option>{vms.map(vm => <option key={vm.id} value={vm.id}>{vm.name} · {vm.state}</option>)}</select></label>
        <p className="sb-hint">{t("Files are edited in an isolated task branch. Commands run as a dedicated user inside this MicroVM. Your source changes only when you apply reviewed files.", "파일은 별도 작업 브랜치에서 수정한다. 명령은 이 MicroVM의 전용 사용자로 실행한다. 검토한 파일을 반영할 때 원본이 변경된다.")}</p>
      </fieldset>
      <label>{t("Project instructions", "프로젝트 지침")}<textarea disabled={busy} rows={3} maxLength={16384} value={spec.instructions} placeholder={t("Conventions, test commands, and context…", "코드 규칙, 테스트 명령, 참고 내용…")} onChange={e => setSpec({ ...spec, instructions: e.target.value })} /></label>
      <footer className="sb-dialog-footer"><button type="button" className="btn" disabled={busy} onClick={() => dialog.current?.close()}>{t("Cancel", "취소")}</button><button className="btn primary" disabled={busy || !spec.name.trim()}>{busy && <LoaderCircle className="sb-spin" />}{t("Save project", "프로젝트 저장")}</button></footer>
    </form>
  </dialog>;
}
