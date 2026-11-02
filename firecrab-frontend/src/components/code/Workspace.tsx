import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
} from "react";
import { fetchJson } from "../../api/client";
import {
  CODE,
  compatible,
  editorMatches,
  equal,
  ready,
  write,
} from "../../api/code";
import type { Editor, Runtime, Workspace } from "../../api/code";
import { useI18n } from "../../i18n";
import { current, type useAccounts } from "./hooks";
import RuntimeSettings, { useRuntime } from "./Runtime";
import Binding from "./Binding";
import CodeTask from "./Task";

export default function CodeWorkspace({
  workspace: w,
  runtime: r,
  runtimeLoaded,
  editing,
  auth,
  refresh,
  manage,
  managing,
  onDirty,
}: {
  workspace: Workspace;
  runtime: Runtime | null;
  runtimeLoaded: boolean;
  editing: boolean;
  auth: ReturnType<typeof useAccounts>;
  refresh: () => void;
  manage: () => void;
  managing: boolean;
  onDirty: (v: boolean) => void;
}) {
  const { t } = useI18n();
  const [binding, setBinding] = useState(false),
    [bindingDirty, setBindingDirty] = useState(false),
    [runtimeDirty, setRuntimeDirty] = useState(false),
    [taskDirty, setTaskDirty] = useState(false);
  const control = useRuntime(w, r, refresh, setRuntimeDirty);
  const [editor, setEditor] = useState<Editor | null>(null),
    [editorBusy, setEditorBusy] = useState(false),
    [editorEpoch, editorUpdate] = useState(0),
    [editorError, setEditorError] = useState(false);
  const [checking, setChecking] = useState(false),
    [report, setReport] = useState<{ label: string; value: unknown } | null>(
      null,
    );
  const editorLock = useRef(false),
    alive = useRef(true),
    settings = useRef<HTMLDetailsElement>(null);
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);
  useEffect(() => {
    onDirty(
      bindingDirty || runtimeDirty || taskDirty || editorBusy || checking,
    );
    return () => onDirty(false);
  }, [bindingDirty, runtimeDirty, taskDirty, editorBusy, checking, onDirty]);
  useEffect(() => {
    setReport(null);
  }, [w.revision, r?.runtimeId, r?.operationId, auth.accounts, managing]);
  const runtimeKey = r
    ? `${r.vmId}/${r.runtimeId}/${r.operationId}/${r.phase}`
    : "";
  const verificationContext = useRef(0);
  useLayoutEffect(() => {
    verificationContext.current++;
  }, [w.revision, runtimeKey, auth.accounts, managing]);
  function openManager() {
    verificationContext.current++;
    setReport(null);
    manage();
  }
  useEffect(() => {
    const controller = new AbortController();
    let timer: ReturnType<typeof setTimeout>;
    if (!editing || !ready(r) || control.pending) {
      setEditor(null);
      return () => controller.abort();
    }
    async function poll() {
      try {
        const next = await fetchJson<Editor | null>(`${CODE}/${w.id}/editor`, {
          signal: controller.signal,
        });
        if (
          next &&
          !(
            next.workspaceId === w.id &&
            next.revision === w.revision &&
            next.vmId === r!.vmId &&
            next.runtimeId === r!.runtimeId &&
            next.operationId === r!.operationId &&
            ["preparing", "ready", "failed"].includes(next.phase)
          )
        )
          throw new Error("invalid");
        if (!controller.signal.aborted) {
          setEditor((old) => (equal(old, next) ? old : next));
          setEditorError(false);
        }
      } catch {
        if (!controller.signal.aborted) {
          setEditor(null);
          setEditorError(true);
        }
      } finally {
        if (!controller.signal.aborted)
          timer = setTimeout(() => void poll(), 3000);
      }
    }
    void poll();
    return () => {
      controller.abort();
      clearTimeout(timer);
    };
  }, [w.id, w.revision, runtimeKey, editing, control.pending, editorEpoch]);
  async function prepare() {
    if (
      editorLock.current ||
      !ready(r) ||
      control.pending ||
      bindingDirty ||
      taskDirty
    )
      return;
    editorLock.current = true;
    setEditorBusy(true);
    setEditorError(false);
    try {
      await current(w);
      await write(`${CODE}/${w.id}/editor`, {
        expectedRevision: w.revision,
        vmId: r.vmId,
        runtimeId: r.runtimeId,
        operationId: r.operationId,
      });
    } catch {
      if (alive.current) setEditorError(true);
    } finally {
      editorLock.current = false;
      if (alive.current) {
        setEditorBusy(false);
        editorUpdate((n) => n + 1);
      }
    }
  }
  async function verify(agent = false) {
    if (checking || bindingDirty || runtimeDirty || taskDirty) return;
    setChecking(true);
    setReport(null);
    const revision = w.revision,
      context = verificationContext.current;
    try {
      await current(w);
      const result = await write<{ id: string; revision: number }>(
        `${CODE}/${w.id}/${agent ? "agent-auth/verify" : "verify"}`,
        { expectedRevision: revision },
      );
      if (result.id !== w.id || result.revision !== revision)
        throw new Error("invalid");
      if (alive.current && context === verificationContext.current)
        setReport({
          label: agent
            ? t("CLI configuration check", "CLI 설정 확인")
            : t("Provider check", "공급자 확인"),
          value: result,
        });
    } catch {
      if (alive.current)
        setReport({ label: t("Check unavailable", "확인 불가"), value: null });
    } finally {
      if (alive.current) setChecking(false);
    }
  }
  const openSettings = useCallback(() => {
    if (settings.current) {
      settings.current.open = true;
      settings.current.scrollIntoView({ block: "nearest" });
    }
  }, []);
  const account = auth.accounts.find(
    (a) => a.id === w.spec.agent?.connectionId,
  );
  const supported =
    !!account &&
    !!w.spec.agent &&
    compatible(w.spec.agent.agent, account) &&
    auth.adapters.some(
      (a) =>
        a.agent === w.spec.agent?.agent &&
        a.provider === account.provider &&
        a.authentication === account.authentication,
    );
  const canTools =
    !control.pending && !control.busy && editorMatches(editor, w, r);
  const canStart =
    runtimeLoaded &&
    !control.busy &&
    !control.pending &&
    !control.moving &&
    !bindingDirty &&
    !taskDirty &&
    !editorBusy &&
    !checking &&
    (!r
      ? control.valid && w.connections.github === "present_unverified"
      : r.phase === "stopped" ||
        (r.phase === "failed" && r.vmState === "stopped"));
  const advanced = (
    <>
      <RuntimeSettings control={control} r={r} />
      <Binding
        w={w}
        accounts={auth.accounts}
        editing={binding}
        setEditing={setBinding}
        manage={openManager}
        refresh={refresh}
        onDirty={setBindingDirty}
        blocked={runtimeDirty || taskDirty || editorBusy || checking}
      />
      <section className="code-section">
        <h3>{t("Configuration", "설정")}</h3>
        <p>
          {w.spec.runtime} · {w.spec.resources.cpu} CPU · {w.spec.resources.ram}{" "}
          MiB · {w.spec.resources.diskGb} GiB
        </p>
        <p>GitHub · {w.connections.github}</p>
        <div className="code-actions">
          <button
            className="btn"
            disabled={checking || bindingDirty || runtimeDirty || taskDirty}
            onClick={() => void verify()}
          >
            {t("Check connections", "연결 확인")}
          </button>
          <button
            className="btn"
            disabled={
              checking ||
              bindingDirty ||
              runtimeDirty ||
              taskDirty ||
              !ready(r) ||
              !supported
            }
            onClick={() => void verify(true)}
          >
            {t("Verify agent configuration", "에이전트 설정 검증")}
          </button>
        </div>
        {report && (
          <div role="status">
            <p>{report.label}</p>
            <pre>{JSON.stringify(report.value, null, 2)}</pre>
          </div>
        )}
      </section>
    </>
  );
  return (
    <article id={editing ? "code-editor" : "code-details"}>
      <header className="code-detail-head">
        {editing && <a href="#/code">{t("Workspaces", "워크스페이스")}</a>}
        <h2>{w.spec.name}</h2>
        <p>
          {w.spec.github.owner}/{w.spec.github.repository} ·{" "}
          {w.spec.github.branch}
        </p>
      </header>
      {!editing ? (
        <>
          <p>
            <a className="btn primary" href={`#/code/${w.id}/editor`}>
              {t("Open workspace", "워크스페이스 열기")}
            </a>
          </p>
          <details
            ref={settings}
            id="code-overview-settings"
            open={binding || !!control.pending ? true : undefined}
          >
            <summary>{t("Workspace settings", "워크스페이스 설정")}</summary>
            {advanced}
          </details>
        </>
      ) : (
        <>
          <div className="code-tools">
            <p role="status">
              {ready(r)
                ? t("Workspace ready", "워크스페이스 준비됨")
                : control.moving
                  ? t("Preparing workspace…", "워크스페이스 준비 중…")
                  : t(
                      "Start this workspace to run your task.",
                      "작업을 실행하려면 워크스페이스를 시작합니다.",
                    )}
            </p>
            {control.pending ? (
              <button className="btn" onClick={openSettings}>
                {t("Review request", "요청 검토")}
              </button>
            ) : (
              !ready(r) && (
                <button
                  className="btn primary"
                  disabled={!canStart}
                  onClick={() => void control.act(r ? "start" : "create")}
                >
                  {t("Start workspace", "워크스페이스 시작")}
                </button>
              )
            )}
            {canTools ? (
              <>
                <a
                  className="btn"
                  href={`${CODE}/${w.id}/editor/open`}
                  target="_blank"
                  rel="noreferrer"
                >
                  {t("Open Web VS Code", "Web VS Code 열기")}
                </a>
                <a
                  className="btn"
                  href={`${CODE}/${w.id}/terminal/open`}
                  target="_blank"
                  rel="noreferrer"
                >
                  {t("Open terminal", "터미널 열기")}
                </a>
              </>
            ) : (
              ready(r) && (
                <button
                  className="btn"
                  disabled={
                    editorBusy ||
                    editor?.phase === "preparing" ||
                    bindingDirty ||
                    runtimeDirty ||
                    taskDirty
                  }
                  onClick={() => void prepare()}
                >
                  {editorBusy || editor?.phase === "preparing"
                    ? t("Preparing editor…", "편집기 준비 중…")
                    : t("Prepare editor tools", "편집 도구 준비")}
                </button>
              )
            )}
            {editorError && (
              <p role="status">
                {t(
                  "Editor status is unavailable. Retrying automatically.",
                  "편집기 상태를 확인하지 못했습니다. 자동으로 다시 확인합니다.",
                )}
              </p>
            )}
          </div>
          <CodeTask
            w={w}
            r={r}
            account={account}
            supported={supported && !auth.error}
            blocked={bindingDirty || runtimeDirty || editorBusy || checking}
            change={() => {
              setBinding(true);
              openSettings();
            }}
            onDirty={setTaskDirty}
          />
          <details
            ref={settings}
            id="code-workspace-settings"
            open={binding || !!control.pending ? true : undefined}
          >
            <summary>{t("Workspace settings", "워크스페이스 설정")}</summary>
            {advanced}
          </details>
        </>
      )}
    </article>
  );
}
