import { useEffect, useState, type FormEvent } from "react";
import {
  Check,
  ExternalLink,
  RefreshCw,
  Search,
  ShieldAlert,
  Trash2,
} from "lucide-react";
import { useI18n } from "../i18n";
import {
  SKILLS_CLI,
  changeSkills,
  searchSkills,
  vmSkills,
  type RegistrySkill,
  type SkillAudit,
  type SkillChange,
  type SkillSearch,
  type VmSkills,
} from "../api/skills";
import "../styles/agent-skills.css";

const message = (failure: unknown) =>
  failure instanceof Error ? failure.message : String(failure);
const EXAMPLES = ["react", "playwright", "pdf"];

/** This MicroVM's skills first, then skills.sh results that install from their own row. */
export default function AgentSkillStore({
  vmId,
  controllerId,
}: {
  vmId: string;
  controllerId: string;
}) {
  const { t, locale } = useI18n();
  const [query, setQuery] = useState("");
  const [search, setSearch] = useState<SkillSearch | null>(null);
  const [installed, setInstalled] = useState<VmSkills | null>(null);
  const [audit, setAudit] = useState<{
    name: string;
    security: SkillAudit;
  } | null>(null);
  const [busy, setBusy] = useState<"" | "search" | "change">("");
  const [error, setError] = useState("");

  useEffect(() => {
    if (vmId)
      vmSkills(vmId, controllerId).then(setInstalled, (failure) =>
        setError(message(failure)),
      );
  }, [vmId, controllerId]);

  const number = (value: number) =>
    value.toLocaleString(locale === "ko" ? "ko-KR" : "en-US");

  const run = async (kind: "search" | "change", work: () => Promise<void>) => {
    setBusy(kind);
    setError("");
    try {
      await work();
    } catch (failure) {
      setError(message(failure));
    } finally {
      setBusy("");
    }
  };
  const find = (text: string) => {
    const value = text.trim();
    if (value.length < 2 || busy) return;
    setQuery(value);
    void run("search", async () => setSearch(await searchSkills(value)));
  };
  const submit = (event: FormEvent) => {
    event.preventDefault();
    find(query);
  };
  const change = (next: SkillChange) =>
    run("change", async () => {
      const response = await changeSkills(vmId, controllerId, next);
      setInstalled(response.installed);
      const first = response.result?.[0];
      setAudit(
        first?.security && next.action === "install"
          ? { name: next.skill, security: first.security }
          : null,
      );
    });
  const isInstalled = (skill: RegistrySkill) =>
    !!installed?.skills.some(
      (s) => s.name === skill.skill && (!s.source || s.source === skill.source),
    );

  return (
    <section className="ask" aria-label={t("Skill store", "스킬 스토어")}>
      <header className="am-quick-heading ask-heading">
        <small>{t("Skill store", "스킬 스토어")}</small>
        <h2>{t("Search skills on skills.sh", "skills.sh에서 스킬 찾기")}</h2>
        <p>
          {t(
            "Install skills for the coding tools in this MicroVM with",
            "이 VM의 AI 도구에 스킬을 설치해요. 설치 도구:",
          )}{" "}
          <code>{SKILLS_CLI}</code>
        </p>
      </header>

      <form className="ask-search" role="search" onSubmit={submit}>
        <Search aria-hidden="true" />
        <input
          type="search"
          aria-label={t("Search skills.sh", "skills.sh 검색")}
          value={query}
          maxLength={100}
          placeholder={t("react, playwright, pdf…", "react, playwright, pdf…")}
          onChange={(e) => setQuery(e.target.value)}
        />
        <button
          type="submit"
          className="btn primary"
          disabled={!!busy || query.trim().length < 2}
        >
          {t("Search", "검색")}
        </button>
      </form>
      <p className="ask-caution">
        <ShieldAlert aria-hidden="true" />
        {t(
          "Anyone can publish to skills.sh. Check the source before you install.",
          "skills.sh에는 누구나 스킬을 올릴 수 있어요. 설치 전에 출처를 확인하세요.",
        )}
      </p>
      {error && (
        <p className="ask-error" role="alert">
          {error}
        </p>
      )}
      {!vmId && (
        <p className="ask-note">
          {t(
            "Select a MicroVM to install skills",
            "스킬을 설치하려면 MicroVM을 선택하세요",
          )}
        </p>
      )}
      {busy === "change" && (
        <p className="ask-note" role="status">
          {t(
            "Working… The first install also sets up Node.js and can take 1–2 minutes.",
            "작업 중이에요… 첫 설치는 Node.js도 함께 설치해서 1~2분 걸릴 수 있어요.",
          )}
        </p>
      )}

      {vmId && (
        <section
          className="ask-installed"
          aria-label={t("Installed skills", "설치된 스킬")}
        >
          <div className="ask-section-head">
            <h3>
              {t("Installed in this MicroVM", "이 VM에 설치됨")}
              {installed && <span>{installed.skills.length}</span>}
            </h3>
            {!!installed?.skills.length && (
              <button
                type="button"
                className="btn"
                disabled={!!busy}
                onClick={() => void change({ action: "update" })}
              >
                <RefreshCw aria-hidden="true" />
                {t("Update all", "모두 업데이트")}
              </button>
            )}
          </div>
          {installed?.node === "missing" ? (
            <p className="ask-note">
              {t(
                "No skills yet. The first install also sets up Node.js in this MicroVM.",
                "아직 설치된 스킬이 없어요. 첫 설치 때 Node.js도 함께 설치해요.",
              )}
            </p>
          ) : installed && installed.skills.length === 0 ? (
            <p className="ask-note">
              {t("No skills installed yet.", "아직 설치된 스킬이 없어요.")}
            </p>
          ) : (
            <ul className="ask-chips">
              {installed?.skills.map((skill) => (
                <li key={skill.name}>
                  <span>
                    <strong>{skill.name}</strong>
                    <small>
                      {skill.agents.length > 0
                        ? skill.agents.join(", ")
                        : skill.source || t("Unknown source", "출처 모름")}
                    </small>
                  </span>
                  <button
                    type="button"
                    className="icon-btn"
                    aria-label={t(`Remove ${skill.name}`, `${skill.name} 삭제`)}
                    title={t("Remove", "삭제")}
                    disabled={!!busy}
                    onClick={() =>
                      void change({ action: "remove", skill: skill.name })
                    }
                  >
                    <Trash2 aria-hidden="true" />
                  </button>
                </li>
              ))}
            </ul>
          )}
        </section>
      )}

      <section
        className="ask-results"
        aria-label={t("Search results", "검색 결과")}
      >
        {search ? (
          <p className="ask-count">
            {t(`${search.count} results`, `검색 결과 ${search.count}개`)} ·
            skills.sh
          </p>
        ) : (
          <div className="ask-examples">
            <span>{t("Try", "예시")}</span>
            {EXAMPLES.map((example) => (
              <button
                key={example}
                type="button"
                className="ask-example"
                disabled={!!busy}
                onClick={() => find(example)}
              >
                {example}
              </button>
            ))}
          </div>
        )}
        {!!search?.skills.length && (
          <ul className="ask-rows">
            {search.skills.map((item) => (
              <li key={`${item.source}/${item.skill}`} className="ask-row">
                <span className="ask-row-main">
                  <strong className="ask-name">{item.name}</strong>
                  <small>{item.source}</small>
                </span>
                <span className="ask-installs">
                  {t(
                    `${number(item.installs)} installs`,
                    `${number(item.installs)}회 설치`,
                  )}
                </span>
                <span className="ask-links">
                  <a
                    href={`https://skills.sh/${item.source}`}
                    target="_blank"
                    rel="noreferrer"
                  >
                    skills.sh <ExternalLink aria-hidden="true" />
                  </a>
                  <a
                    href={`https://github.com/${item.source}`}
                    target="_blank"
                    rel="noreferrer"
                  >
                    GitHub <ExternalLink aria-hidden="true" />
                  </a>
                </span>
                {isInstalled(item) ? (
                  <span className="ask-done">
                    <Check aria-hidden="true" />
                    {t("Installed", "설치됨")}
                  </span>
                ) : (
                  <button
                    type="button"
                    className="btn"
                    aria-label={t(`Install ${item.name}`, `${item.name} 설치`)}
                    disabled={!vmId || !!busy}
                    onClick={() =>
                      void change({
                        action: "install",
                        source: item.source,
                        skill: item.skill,
                      })
                    }
                  >
                    {t("Install", "설치")}
                  </button>
                )}
                {audit && audit.name === item.skill && (
                  <p className="ask-audit">
                    {t("Security audit", "보안 검사")}:{" "}
                    {[
                      audit.security.gen && `Gen ${audit.security.gen}`,
                      audit.security.socket &&
                        `Socket ${audit.security.socket}`,
                      audit.security.snyk && `Snyk ${audit.security.snyk}`,
                    ]
                      .filter(Boolean)
                      .join(" · ")}
                  </p>
                )}
              </li>
            ))}
          </ul>
        )}
        {search && search.skills.length === 0 && (
          <p className="ask-note">
            {t(
              "No skills match. Try a shorter or different word.",
              "맞는 스킬이 없어요. 더 짧거나 다른 단어로 검색해 보세요.",
            )}
          </p>
        )}
      </section>
    </section>
  );
}
