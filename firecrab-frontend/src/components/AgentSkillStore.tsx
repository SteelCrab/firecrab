import { useEffect, useState, type FormEvent } from "react";
import { ExternalLink, RefreshCw, Search, Trash2 } from "lucide-react";
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

/** skills.sh search on the left, the selected skill and this MicroVM's installed skills on the right. */
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
  const [selected, setSelected] = useState<RegistrySkill | null>(null);
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
  const submit = (event: FormEvent) => {
    event.preventDefault();
    if (query.trim().length < 2 || busy) return;
    void run("search", async () => {
      const found = await searchSkills(query.trim());
      setSearch(found);
      setSelected(found.skills[0] || null);
    });
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
            "Firecrab installs skills with the Vercel Skills CLI",
            "Vercel Skills CLI",
          )}{" "}
          (<code>{SKILLS_CLI}</code>){" "}
          {t(
            "for Claude Code, Codex, Gemini CLI, OpenCode, GitHub Copilot and Cursor in this MicroVM. Anyone can publish to skills.sh, so check the source before you install.",
            "로 이 MicroVM의 Claude Code, Codex, Gemini CLI, OpenCode, GitHub Copilot, Cursor에 설치해요. skills.sh에는 누구나 스킬을 올릴 수 있으니 설치 전에 출처를 확인하세요.",
          )}
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
          className="btn"
          disabled={!!busy || query.trim().length < 2}
        >
          {t("Search", "검색")}
        </button>
      </form>
      {error && (
        <p className="ask-error" role="alert">
          {error}
        </p>
      )}
      <div className="ask-layout">
        <nav className="ask-list" aria-label={t("Search results", "검색 결과")}>
          {search && (
            <p className="ask-count">
              {t(`${search.count} results`, `검색 결과 ${search.count}개`)} ·
              skills.sh
            </p>
          )}
          <ul>
            {search?.skills.map((item) => (
              <li key={`${item.source}/${item.skill}`}>
                <button
                  type="button"
                  aria-current={
                    selected?.source === item.source &&
                    selected.skill === item.skill
                      ? "true"
                      : undefined
                  }
                  onClick={() => setSelected(item)}
                >
                  <span className="ask-name">{item.name}</span>
                  <small>{item.source}</small>
                </button>
              </li>
            ))}
          </ul>
        </nav>
        <div className="ask-side">
          {selected && (
            <article className="ask-detail">
              <h3>{selected.name}</h3>
              <dl className="ask-facts">
                <div>
                  <dt>{t("Source", "출처")}</dt>
                  <dd>{selected.source}</dd>
                </div>
                <div>
                  <dt>{t("Installs", "설치 수")}</dt>
                  <dd>{number(selected.installs)}</dd>
                </div>
              </dl>
              <p className="ask-links">
                <a
                  href={`https://skills.sh/${selected.source}`}
                  target="_blank"
                  rel="noreferrer"
                >
                  skills.sh <ExternalLink aria-hidden="true" />
                </a>
                <a
                  href={`https://github.com/${selected.source}`}
                  target="_blank"
                  rel="noreferrer"
                >
                  GitHub <ExternalLink aria-hidden="true" />
                </a>
              </p>
              {isInstalled(selected) ? (
                <p className="ask-done">{t("✓ Installed", "✓ 설치됨")}</p>
              ) : (
                <button
                  type="button"
                  className="btn primary"
                  disabled={!vmId || !!busy}
                  onClick={() =>
                    void change({
                      action: "install",
                      source: selected.source,
                      skill: selected.skill,
                    })
                  }
                >
                  {t("Install in this MicroVM", "이 MicroVM에 설치")}
                </button>
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
                <p className="ask-note">
                  {t(
                    "Working… The first install also sets up Node.js and can take 1–2 minutes.",
                    "작업 중이에요… 첫 설치는 Node.js도 함께 설치해서 1~2분 걸릴 수 있어요.",
                  )}
                </p>
              )}
              {audit && audit.name === selected.skill && (
                <p className="ask-audit">
                  {t("Security audit", "보안 검사")}:{" "}
                  {[
                    audit.security.gen && `Gen ${audit.security.gen}`,
                    audit.security.socket && `Socket ${audit.security.socket}`,
                    audit.security.snyk && `Snyk ${audit.security.snyk}`,
                  ]
                    .filter(Boolean)
                    .join(" · ")}
                </p>
              )}
            </article>
          )}
          {vmId && (
            <section
              className="ask-installed"
              aria-label={t("Installed skills", "설치된 스킬")}
            >
              <div className="ask-installed-head">
                <h3>
                  {t("Installed in this MicroVM", "이 MicroVM에 설치된 스킬")}{" "}
                  {installed && `(${installed.skills.length})`}
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
                <ul>
                  {installed?.skills.map((skill) => (
                    <li key={skill.name}>
                      <div>
                        <strong>{skill.name}</strong>
                        <small>
                          {skill.source || t("Unknown source", "출처 모름")}
                          {skill.agents.length > 0 &&
                            ` · ${skill.agents.join(", ")}`}
                        </small>
                      </div>
                      <button
                        type="button"
                        className="icon-btn"
                        aria-label={t(
                          `Remove ${skill.name}`,
                          `${skill.name} 삭제`,
                        )}
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
        </div>
      </div>
    </section>
  );
}
