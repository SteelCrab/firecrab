import Observation from "./Observation";
import { useCallback, useEffect, useRef, useState } from "react";
import { fetchJson } from "../../api/client";
import {
  CODE,
  active,
  agentName,
  equal,
  ready,
  remember,
  restore,
  uuid,
  write,
} from "../../api/code";
import type { Account, Run, RunBody, Runtime, Workspace } from "../../api/code";
import { current } from "./hooks";
import { useI18n } from "../../i18n";
type Pending = {
  workspace: string;
  id: string;
  account: string;
  agent: string;
  body: RunBody;
};
export default function CodeTask({
  w,
  r,
  account,
  supported,
  blocked,
  change,
  onDirty,
}: {
  w: Workspace;
  r: Runtime | null;
  account: Account | undefined;
  supported: boolean;
  blocked: boolean;
  change: () => void;
  onDirty: (v: boolean) => void;
}) {
  const { t } = useI18n(),
    key = `firecrab.code.run.react.${w.id}`;
  const [pending, setPending] = useState<Pending | null>(() => {
    const p = restore<Pending>(key);
    return p?.workspace === w.id &&
      uuid(p.id) &&
      uuid(p.account) &&
      p.body &&
      uuid(p.body.vmId) &&
      uuid(p.body.runtimeId) &&
      uuid(p.body.operationId) &&
      Number.isSafeInteger(p.body.expectedRevision) &&
      typeof p.body.prompt === "string" &&
      p.body.prompt.length <= 16384
      ? p
      : null;
  });
  const [prompt, setPrompt] = useState(pending?.body.prompt ?? ""),
    [model, setModel] = useState(pending?.body.model ?? "");
  const [rows, setRows] = useState<Run[]>([]),
    [loaded, setLoaded] = useState(false),
    [error, setError] = useState(false),
    [busy, setBusy] = useState(false),
    [storageError, setStorageError] = useState(false),
    [epoch, update] = useState(0);
  const lock = useRef(false),
    alive = useRef(true),
    journal = useRef(pending);
  useEffect(() => {
    journal.current = pending;
  }, [pending]);
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);
  useEffect(() => {
    onDirty(!!pending || busy);
    return () => onDirty(false);
  }, [pending, busy, onDirty]);
  const matches = useCallback(
    (run: Run, p: Pending) =>
      run.id === p.id &&
      run.workspaceId === w.id &&
      run.revision === p.body.expectedRevision &&
      run.connectionId === p.account &&
      run.agent === p.agent &&
      run.vmId === p.body.vmId &&
      run.runtimeId === p.body.runtimeId &&
      run.operationId === p.body.operationId,
    [w.id],
  );
  useEffect(() => {
    if (busy) return;
    const controller = new AbortController();
    let timer: ReturnType<typeof setTimeout>;
    async function poll() {
      try {
        const history = await fetchJson<Run[]>(`${CODE}/${w.id}/agent-runs`, {
          signal: controller.signal,
        });
        if (
          !Array.isArray(history) ||
          history.some(
            (v) =>
              !uuid(v.id) ||
              v.workspaceId !== w.id ||
              !uuid(v.connectionId) ||
              typeof v.output !== "string" ||
              typeof v.phase !== "string",
          )
        )
          throw new Error("invalid");
        const p = journal.current;
        if (p) {
          const row = history.find((v) => v.id === p.id);
          if (row) {
            if (!matches(row, p)) throw new Error("invalid");
            if (!controller.signal.aborted) {
              sessionStorage.removeItem(key);
              journal.current = null;
              setPending(null);
            }
          }
        }
        if (!controller.signal.aborted) {
          setRows((old) => (equal(old, history) ? old : history));
          setLoaded(true);
          setError(false);
        }
      } catch {
        if (!controller.signal.aborted) {
          setLoaded(false);
          setError(true);
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
  }, [w.id, epoch, busy, key, matches]);
  const running = rows.find((v) => active(v.phase)),
    locked = busy || !!pending || !!running;
  const eligible =
    !blocked &&
    supported &&
    !!account &&
    w.connections.ai === "present_unverified" &&
    ready(r) &&
    loaded &&
    !error;
  const valid =
    eligible &&
    !!prompt.trim() &&
    new TextEncoder().encode(prompt).length <= 16384 &&
    model.length <= 200;
  async function run(retry = false) {
    if (lock.current || running || !valid || (!retry && pending)) return;
    lock.current = true;
    setBusy(true);
    setStorageError(false);
    const p = pending ?? {
      workspace: w.id,
      id: crypto.randomUUID(),
      account: w.spec.agent!.connectionId,
      agent: w.spec.agent!.agent,
      body: {
        expectedRevision: w.revision,
        vmId: r!.vmId,
        runtimeId: r!.runtimeId!,
        operationId: r!.operationId,
        model,
        prompt,
      },
    };
    try {
      await current(w);
      if (
        p.account !== w.spec.agent?.connectionId ||
        p.agent !== w.spec.agent.agent ||
        p.body.expectedRevision !== w.revision ||
        p.body.vmId !== r?.vmId ||
        p.body.runtimeId !== r.runtimeId ||
        p.body.operationId !== r.operationId
      )
        throw new Error("changed");
      try {
        remember(key, p);
      } catch {
        setStorageError(true);
        return;
      }
      journal.current = p;
      setPending(p);
      const row = await write<Run>(
        `${CODE}/${w.id}/agent-runs/${p.id}`,
        p.body,
      );
      if (!matches(row, p)) throw new Error("invalid");
      sessionStorage.removeItem(key);
      journal.current = null;
      if (alive.current) {
        setPending(null);
        setRows((old) => [row, ...old.filter((v) => v.id !== row.id)]);
        setError(false);
      }
    } catch {
      if (alive.current) setError(true);
    } finally {
      lock.current = false;
      if (alive.current) {
        setBusy(false);
        update((n) => n + 1);
      }
    }
  }
  async function cancel() {
    if (lock.current || (!pending && !running)) return;
    lock.current = true;
    setBusy(true);
    try {
      await write(
        `${CODE}/${w.id}/agent-runs/${pending?.id ?? running!.id}/cancel`,
        {},
      );
      sessionStorage.removeItem(key);
      journal.current = null;
      if (alive.current) {
        setPending(null);
        update((n) => n + 1);
      }
    } catch {
      if (alive.current) setError(true);
    } finally {
      lock.current = false;
      if (alive.current) setBusy(false);
    }
  }
  return (
    <section
      id="code-agent-execution"
      className="code-section"
      aria-label={t("Agent execution", "에이전트 실행")}
    >
      <div className="code-task-head">
        <h3>{t("Work with AI", "AI와 작업")}</h3>
        <span>
          {agentName(w.spec.agent?.agent ?? "AI")} ·{" "}
          {account?.name ?? t("Choose AI AUTH", "AI AUTH 선택")}
        </span>
        <button
          className="code-text-link"
          disabled={locked || blocked}
          onClick={change}
        >
          {t("Change", "변경")}
        </button>
      </div>
      <label htmlFor="code-agent-prompt">{t("Task", "작업 지시")}</label>
      <textarea
        id="code-agent-prompt"
        rows={5}
        maxLength={16384}
        disabled={locked}
        value={prompt}
        placeholder={t("Describe what to change…", "수정할 내용을 입력하세요…")}
        onChange={(e) => setPrompt(e.target.value)}
      />
      <details id="code-agent-options">
        <summary>{t("Options", "옵션")}</summary>
        <label>
          {t("Model ID (optional)", "모델 ID (선택)")}
          <input
            id="code-agent-model"
            maxLength={200}
            disabled={locked}
            value={model}
            placeholder={t("Agent default", "에이전트 기본 모델")}
            onChange={(e) => setModel(e.target.value)}
          />
        </label>
      </details>
      <div className="code-actions">
        <button
          className="btn primary"
          disabled={!valid || locked}
          onClick={() => void run()}
        >
          {t("Run agent", "에이전트 실행")}
        </button>
        {pending && (
          <button
            className="btn"
            disabled={busy || !valid}
            onClick={() => void run(true)}
          >
            {t("Retry same request", "같은 요청 재시도")}
          </button>
        )}
        {(pending || running) && (
          <button
            className="btn"
            disabled={busy || running?.phase === "cancelling"}
            onClick={() => void cancel()}
          >
            {t("Cancel run", "실행 취소")}
          </button>
        )}
      </div>
      <p className="code-hint">
        {t(
          "AI can edit files and run commands. Usage is billed to this AI account.",
          "AI가 파일을 수정하고 명령을 실행합니다. 사용료는 이 AI 계정에 청구됩니다.",
        )}
      </p>
      {!supported && (
        <p>
          {t(
            "Select a supported agent and AI AUTH connection to run a task.",
            "지원되는 에이전트와 AI AUTH 연결을 선택해야 작업을 실행할 수 있습니다.",
          )}
        </p>
      )}
      {pending && (
        <p role="status">
          {t(
            "Start was not confirmed. Retry the same request or cancel it.",
            "시작을 확인하지 못했습니다. 같은 요청을 재시도하거나 취소해 주세요.",
          )}
        </p>
      )}
      {(error || storageError) && (
        <p role="alert">
          {storageError
            ? t(
                "Could not save the request in session storage. No request was sent.",
                "세션 저장소에 요청을 저장하지 못했습니다. 요청을 보내지 않았습니다.",
              )
            : t(
                "Could not confirm execution. Check the account and workspace.",
                "실행을 확인하지 못했습니다. 계정과 워크스페이스를 확인해 주세요.",
              )}
        </p>
      )}
      {rows[0] && <Observation row={rows[0]} />}
      {rows.length > 1 && (
        <details>
          <summary>{t("Earlier runs", "이전 실행")}</summary>
          {rows.slice(1).map((row) => (
            <Observation key={row.id} row={row} />
          ))}
        </details>
      )}
    </section>
  );
}
