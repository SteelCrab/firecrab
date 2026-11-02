import { useEffect, useRef, useState } from "react";
import { ApiClientError, fetchJson } from "../../api/client";
import {
  CODE,
  agentName,
  compatible,
  equal,
  remember,
  restore,
  write,
} from "../../api/code";
import type { Account, Agent, Spec, Workspace } from "../../api/code";
import { useI18n } from "../../i18n";
import { current } from "./hooks";
type Pending = {
  workspace: string;
  revision: number;
  original: Spec;
  spec: Spec;
};
export default function Binding({
  w,
  accounts,
  editing,
  setEditing,
  manage,
  refresh,
  onDirty,
  blocked,
}: {
  w: Workspace;
  accounts: Account[];
  editing: boolean;
  setEditing: (v: boolean) => void;
  manage: () => void;
  refresh: () => void;
  onDirty: (v: boolean) => void;
  blocked: boolean;
}) {
  const { t } = useI18n(),
    key = `firecrab.code.binding.react.${w.id}`;
  const [pending, setPending] = useState<Pending | null>(() => {
    const p = restore<Pending>(key);
    return p?.workspace === w.id && p.revision > 0 && p.original && p.spec
      ? p
      : null;
  });
  const [draft, setDraft] = useState<Agent | null>(
      pending?.spec.agent ?? w.spec.agent,
    ),
    [busy, setBusy] = useState(false),
    [error, setError] = useState("");
  const [original, setOriginal] = useState(() => ({
    revision: pending?.revision ?? w.revision,
    spec: structuredClone(pending?.original ?? w.spec),
  }));
  const changed =
    original.revision !== w.revision || !equal(original.spec, w.spec);
  const lock = useRef(false),
    alive = useRef(true),
    wasEditing = useRef(false);
  useEffect(() => {
    if (editing && !wasEditing.current && !pending) {
      setOriginal({ revision: w.revision, spec: structuredClone(w.spec) });
      setDraft(w.spec.agent);
      setError("");
    }
    wasEditing.current = editing;
  }, [editing, w, pending]);
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);
  useEffect(() => {
    if (pending) setEditing(true);
  }, [pending, setEditing]);
  useEffect(() => {
    onDirty(editing || !!pending || busy);
    return () => onDirty(false);
  }, [editing, pending, busy, onDirty]);
  function clear() {
    sessionStorage.removeItem(key);
    setPending(null);
  }
  async function save() {
    if (lock.current || blocked || (!pending && changed)) return;
    lock.current = true;
    setBusy(true);
    setError("");
    const p = pending ?? {
      workspace: w.id,
      revision: original.revision,
      original: structuredClone(original.spec),
      spec: { ...structuredClone(original.spec), agent: draft },
    };
    try {
      const latest = await fetchJson<Workspace>(`${CODE}/${w.id}`);
      if (latest.id !== w.id) throw new Error("changed");
      if (
        pending &&
        latest.revision === p.revision + 1 &&
        equal(latest.spec, p.spec)
      ) {
        clear();
        setEditing(false);
        refresh();
        return;
      }
      if (latest.revision !== p.revision || !equal(latest.spec, p.original)) {
        setError("changed");
        refresh();
        return;
      }
      if (
        p.spec.agent &&
        !accounts.some(
          (a) =>
            a.id === p.spec.agent!.connectionId &&
            compatible(p.spec.agent!.agent, a),
        )
      )
        throw new Error("account");
      remember(key, p);
      setPending(p);
      const saved = await write<Workspace>(
        `${CODE}/${w.id}`,
        { expectedRevision: p.revision, spec: p.spec },
        "PUT",
      );
      if (
        saved.id !== w.id ||
        saved.revision !== p.revision + 1 ||
        !equal(saved.spec, p.spec)
      )
        throw new Error("unknown");
      clear();
      if (alive.current) setEditing(false);
      refresh();
    } catch (e) {
      if (alive.current) setError("unknown");
      if (e instanceof ApiClientError && e.status && e.status < 500) clear();
    } finally {
      lock.current = false;
      if (alive.current) setBusy(false);
    }
  }
  async function open() {
    try {
      await current(w);
      setOriginal({ revision: w.revision, spec: structuredClone(w.spec) });
      setDraft(w.spec.agent);
      setError("");
      setEditing(true);
    } catch {
      setError("changed");
      refresh();
    }
  }
  const valid =
    !draft ||
    accounts.some(
      (a) => a.id === draft.connectionId && compatible(draft.agent, a),
    );
  return (
    <section className="code-section">
      <h3>AI AUTH</h3>
      <p>
        {w.connections.ai === "missing"
          ? t(
              "Account missing or disconnected",
              "계정이 없거나 연결이 해제되었습니다.",
            )
          : w.connections.ai === "incompatible"
            ? t(
                "Account is incompatible with the selected agent",
                "선택한 에이전트와 호환되지 않는 계정입니다.",
              )
            : w.spec.agent
              ? `${agentName(w.spec.agent.agent)} · ${accounts.find((a) => a.id === w.spec.agent?.connectionId)?.name ?? t("Account unavailable", "계정 확인 불가")}`
              : t("Workspace only", "워크스페이스만 사용")}
      </p>
      {!editing ? (
        <button className="btn" disabled={blocked} onClick={() => void open()}>
          {t("Choose AI AUTH", "AI AUTH 선택")}
        </button>
      ) : (
        <fieldset
          disabled={busy || !!pending || blocked}
          className="code-fields"
        >
          <label>
            {t("Agent", "에이전트")}
            <select
              value={draft?.agent ?? "none"}
              onChange={(e) => {
                const agent = e.target.value,
                  candidates = accounts.filter((a) => compatible(agent, a));
                setDraft(
                  agent === "none"
                    ? null
                    : {
                        agent,
                        connectionId: accounts.some(
                          (a) =>
                            a.id === draft?.connectionId &&
                            compatible(agent, a),
                        )
                          ? draft!.connectionId
                          : candidates.length === 1
                            ? candidates[0].id
                            : "",
                      },
                );
              }}
            >
              <option value="none">
                {t("Workspace only", "워크스페이스만 사용")}
              </option>
              {["codex", "claude_code", "opencode"].map((a) => (
                <option key={a} value={a}>
                  {agentName(a)}
                </option>
              ))}
            </select>
          </label>
          {draft && (
            <label>
              {t("AI account", "AI 계정")}
              <select
                value={draft.connectionId}
                onChange={(e) =>
                  setDraft({ ...draft, connectionId: e.target.value })
                }
              >
                <option value="">
                  {t("Select a compatible account", "호환 계정 선택")}
                </option>
                {accounts
                  .filter((a) => compatible(draft.agent, a))
                  .map((a) => (
                    <option key={a.id} value={a.id}>
                      {a.name}
                    </option>
                  ))}
              </select>
            </label>
          )}
        </fieldset>
      )}
      {editing && (
        <div className="code-actions">
          <button
            className="btn primary"
            disabled={
              busy ||
              blocked ||
              error === "changed" ||
              (!pending && (!valid || changed))
            }
            onClick={() => void save()}
          >
            {pending
              ? t("Check saved assignment", "저장한 할당 확인")
              : t("Save assignment", "할당 저장")}
          </button>
          {!pending && (
            <button
              className="btn"
              disabled={busy}
              onClick={() => setEditing(false)}
            >
              {t("Cancel", "취소")}
            </button>
          )}
        </div>
      )}
      <button
        className="btn"
        disabled={busy || !!pending || blocked}
        onClick={manage}
      >
        {t("Manage AI AUTH", "AI AUTH 관리")}
      </button>
      {(error || (editing && !pending && changed)) && (
        <p role="alert">
          {error === "changed" || (!pending && changed)
            ? t(
                "The workspace changed. Review its latest configuration.",
                "워크스페이스가 변경되었습니다. 최신 설정을 확인해 주세요.",
              )
            : t(
                "Save could not be confirmed. Check the original assignment before retrying.",
                "저장을 확인하지 못했습니다. 다시 시도하기 전에 이전 할당을 확인해 주세요.",
              )}
        </p>
      )}
    </section>
  );
}
