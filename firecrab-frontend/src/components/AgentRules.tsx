import { useEffect, useState, type FormEvent } from "react";
import { ArrowUp, X } from "lucide-react";
import { useI18n } from "../i18n";
import type { AgentStatus } from "../api/agents";
import {
  applyRule,
  readRules,
  refineRule,
  type RuleProposal,
  type RulesDocument,
} from "../api/rules";
import "../styles/agent-rules.css";

const REFINERS = ["codex", "claude"];

const message = (failure: unknown) =>
  failure instanceof Error ? failure.message : String(failure);

/** AGENTS.md as a document; the fixed composer adds a rule, or edits the clicked one. */
export default function AgentRules({
  vmId,
  controllerId,
  drivers,
  onNavigate,
}: {
  vmId: string;
  controllerId: string;
  drivers: AgentStatus["drivers"];
  onNavigate: (page: "workspace" | "connections") => void;
}) {
  const { t } = useI18n();
  const cliKey = `firecrab.rulesCli.${controllerId}.${vmId}`;
  const refiners = drivers.filter(
    (d) => REFINERS.includes(d.id) && d.available && d.authenticated,
  );
  const [chosen, setChosen] = useState(
    () => localStorage.getItem(cliKey) || "",
  );
  const cli = refiners.some((d) => d.id === chosen)
    ? chosen
    : refiners[0]?.id || "";
  const cliName = refiners.find((d) => d.id === cli)?.name || "";
  const [doc, setDoc] = useState<RulesDocument | null>(null);
  const [editing, setEditing] = useState<number | null>(null);
  const [input, setInput] = useState("");
  const [proposal, setProposal] = useState<RuleProposal | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [warning, setWarning] = useState("");

  useEffect(() => {
    readRules(vmId, controllerId).then(setDoc, (failure) =>
      setError(message(failure)),
    );
  }, [vmId, controllerId]);

  const known: Record<string, string> = {
    "Prepare the repository in Workspace first.": t(
      "Prepare the repository in Workspace first.",
      "작업 공간에서 저장소를 먼저 준비해 주세요",
    ),
    "The file changed. Reload it and refine again.": t(
      "The file changed. Reload it and refine again.",
      "파일이 바뀌었습니다. 다시 불러온 뒤 다시 다듬어 주세요.",
    ),
  };
  const say = (text: string) => known[text] || text;

  const work = async (task: () => Promise<void>) => {
    setBusy(true);
    setError("");
    try {
      await task();
    } catch (failure) {
      setError(say(message(failure)));
    } finally {
      setBusy(false);
    }
  };
  const mode = editing === null ? "add" : "edit";
  const refine = (previous?: string) =>
    doc &&
    work(async () =>
      setProposal(
        await refineRule(vmId, controllerId, {
          mode,
          ...(editing === null ? {} : { block: editing }),
          input,
          cli,
          digest: doc.digest,
          ...(previous ? { previous } : {}),
        }),
      ),
    );
  const submit = (event: FormEvent) => {
    event.preventDefault();
    if (input.trim() && cli && !busy) void refine();
  };
  const approve = () =>
    proposal &&
    work(async () => {
      const next = await applyRule(vmId, controllerId, {
        mode: proposal.mode,
        ...(proposal.block === null ? {} : { block: proposal.block }),
        text: proposal.after,
        digest: proposal.digest,
      });
      setDoc(next);
      setWarning(next.warning || "");
      setProposal(null);
      setEditing(null);
      setInput("");
    });

  const lines =
    doc?.text.replace(/\r/g, "").replace(/\n$/, "").split("\n") || [];
  const rendered = (doc?.blocks || []).map((block, index) => ({
    index,
    block,
    text: lines.slice(block.start, block.end).join("\n"),
  }));
  const proposalView = proposal && (
    <div className="arl-proposal">
      {proposal.before && <pre className="arl-del">{proposal.before}</pre>}
      <pre className="arl-ins">{proposal.after}</pre>
      <div className="arl-actions">
        <button
          type="button"
          className="btn primary"
          disabled={busy}
          onClick={() => void approve()}
        >
          {t("Approve", "승인")}
        </button>
        <button
          type="button"
          className="btn"
          disabled={busy}
          onClick={() => void refine(proposal.after)}
        >
          {t("Refine again", "다시 다듬기")}
        </button>
        <button
          type="button"
          className="btn"
          disabled={busy}
          onClick={() => setProposal(null)}
        >
          {t("Discard", "버리기")}
        </button>
        <span className="arl-by">
          <span className="arl-dot" aria-hidden="true" />
          {proposal.name}
        </span>
      </div>
    </div>
  );

  return (
    <section
      className="arl"
      aria-label={t("AGENTS.md rules", "AGENTS.md 규칙")}
    >
      <header className="am-quick-heading">
        <small>{t("AGENTS.md rules", "AGENTS.md 규칙")}</small>
        <h2>{t("Rules for every agent", "모든 에이전트의 규칙")}</h2>
      </header>
      {doc?.state === "unavailable" ? (
        <div className="arl-unavailable">
          <p>{say(doc.reason || "")}</p>
          <button
            type="button"
            className="btn"
            onClick={() => onNavigate("workspace")}
          >
            {t("Workspace", "작업 공간")}
          </button>
        </div>
      ) : (
        <>
          <div className="arl-doc">
            <div className="arl-doc-head">
              <span>AGENTS.md</span>
              <span>{t("Applies to every agent", "모든 에이전트에 적용")}</span>
            </div>
            {doc && !doc.exists && !proposal && (
              <p className="arl-empty">
                {t(
                  "No rules yet. Add the first one below.",
                  "아직 규칙이 없어요. 아래에서 첫 규칙을 추가하세요.",
                )}
              </p>
            )}
            {rendered.map(({ index, text }) =>
              proposal?.mode === "edit" && proposal.block === index ? (
                <div key={index}>{proposalView}</div>
              ) : (
                <button
                  key={index}
                  type="button"
                  className="arl-line"
                  aria-label={text}
                  aria-pressed={editing === index}
                  onClick={() => {
                    setEditing(index);
                    setProposal(null);
                  }}
                >
                  {text}
                </button>
              ),
            )}
            {proposal?.mode === "add" && proposalView}
          </div>
          {warning && <p className="arl-warning">{warning}</p>}
          {error && (
            <p className="arl-error" role="alert">
              {error}
            </p>
          )}
          {refiners.length === 0 ? (
            <div className="arl-unavailable">
              <p>
                {t(
                  "Connect Codex or Claude Code to refine rules.",
                  "규칙을 다듬으려면 Codex나 Claude Code를 연결하세요.",
                )}
              </p>
              <button
                type="button"
                className="btn"
                onClick={() => onNavigate("connections")}
              >
                {t("Connect accounts", "계정 연결")}
              </button>
            </div>
          ) : (
            <form className="arl-composer" onSubmit={submit}>
              <div className="arl-target">
                {editing === null ? (
                  <span>{t("New rule", "새 규칙 추가")}</span>
                ) : (
                  <>
                    <span>
                      {t(
                        `Editing rule ${editing + 1}`,
                        `${editing + 1}번째 규칙 수정 중`,
                      )}
                    </span>
                    <button
                      type="button"
                      className="arl-cancel"
                      aria-label={t("Cancel edit", "수정 취소")}
                      onClick={() => {
                        setEditing(null);
                        setProposal(null);
                      }}
                    >
                      <X aria-hidden="true" />
                    </button>
                  </>
                )}
              </div>
              <textarea
                aria-label={t("Rule", "규칙")}
                value={input}
                maxLength={16384}
                placeholder={t(
                  "Describe the rule in your own words…",
                  "규칙을 편하게 적어 주세요…",
                )}
                onChange={(e) => setInput(e.target.value)}
              />
              <div className="arl-bar">
                <label className="arl-chip">
                  <span className="arl-dot" aria-hidden="true" />
                  <select
                    aria-label={t("Agent", "에이전트")}
                    value={cli}
                    onChange={(e) => {
                      localStorage.setItem(cliKey, e.target.value);
                      setChosen(e.target.value);
                    }}
                  >
                    {refiners.map((d) => (
                      <option key={d.id} value={d.id}>
                        {d.name}
                      </option>
                    ))}
                  </select>
                </label>
                {busy && (
                  <span className="arl-busy">
                    {t(`Refining with ${cliName}…`, `${cliName}가 다듬는 중…`)}
                  </span>
                )}
                <button
                  type="submit"
                  className="arl-send"
                  aria-label={t("Refine", "다듬기")}
                  disabled={busy || !input.trim()}
                >
                  <ArrowUp aria-hidden="true" />
                </button>
              </div>
            </form>
          )}
        </>
      )}
    </section>
  );
}
