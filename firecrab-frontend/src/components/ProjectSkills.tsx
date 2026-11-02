import { useState } from "react";
import { Package, RefreshCw, Trash2 } from "lucide-react";
import { useI18n } from "../i18n";
import { canvasController as c } from "../canvas/controller";
import { agentDocument, type Choice } from "../canvas/agent-types";
import { base, type Tab, type Node } from "../canvas/types";
import { request, message } from "../canvas/request";
import type {
  SkillCatalog,
  SkillEntry,
  SkillInstallation,
  SkillSource,
} from "../canvas/skills";
export default function ProjectSkills({
  tab,
  node,
  choice,
  disabled,
}: {
  tab: Tab;
  node: Node;
  choice?: Choice;
  disabled: boolean;
}) {
  "use no memo";
  const { t } = useI18n(),
    d = agentDocument(node, tab.project)!;
  const [open, setOpen] = useState(false),
    [busy, setBusy] = useState(false),
    [error, setError] = useState("");
  const [catalogs, setCatalogs] = useState<SkillCatalog[]>([]),
    [rows, setRows] = useState<SkillInstallation[]>([]);
  const [filter, setFilter] = useState<"all" | SkillSource>("all"),
    [search, setSearch] = useState("");
  const path = base(tab.project) + "/skills",
    selected = d.skills || [],
    locked = busy || disabled;
  async function load() {
    const results = await Promise.allSettled([
      request<SkillCatalog[]>("GET", path + "/catalog"),
      request<SkillInstallation[]>("GET", path + "/installations"),
    ]);
    if (results[0].status === "fulfilled") setCatalogs(results[0].value);
    if (results[1].status === "fulfilled") setRows(results[1].value);
    const failures = results.filter((r) => r.status === "rejected");
    if (failures.length)
      throw new Error(
        failures
          .map((r) => message((r as PromiseRejectedResult).reason))
          .join("; "),
      );
  }
  async function action(work: () => Promise<unknown>) {
    setBusy(true);
    setError("");
    try {
      await work();
    } catch (e) {
      setError(message(e));
    } finally {
      setBusy(false);
    }
  }
  async function install(entry: SkillEntry) {
    await c.agents.installSkill(tab, node, {
      source: entry.source,
      kind: entry.kind,
      path: entry.path,
      revision: entry.revision,
    });
    await load();
  }
  return (
    <section
      className="cw-skills"
      aria-label={t("Project skills", "프로젝트 스킬")}
    >
      <button
        type="button"
        aria-expanded={open}
        onClick={() => {
          setOpen(!open);
          if (!open) void action(load);
        }}
      >
        <Package size={14} />
        {t("Official skills", "공식 스킬")} · {selected.length}
      </button>
      {!!selected.length && (
        <ul className="cw-skill-selections">
          {selected.map((skill) => {
            const installed = rows.find((r) => r.id === skill.id),
              inactive =
                skill.source !== choice?.source ||
                (installed && installed.status !== "installed");
            return (
              <li key={skill.id}>
                {skill.name}
                <small>
                  {inactive ? t("Inactive", "비활성") : t("Selected", "선택됨")}
                </small>
                <button
                  type="button"
                  disabled={disabled || !!d.pending}
                  aria-label={t("Deselect ", "선택 해제 ") + skill.name}
                  onClick={() =>
                    c.agents.change(tab, node, {
                      skills: selected.filter((s) => s.id !== skill.id),
                    })
                  }
                >
                  ×
                </button>
              </li>
            );
          })}
        </ul>
      )}
      {open && (
        <div className="cw-skill-store">
          <p>
            {t(
              "Install skill files in this project and select compatible revisions for each request. Plugin apps, connectors and hooks are not activated.",
              "스킬 파일을 프로젝트에 설치하고 요청별로 호환되는 버전을 선택한다. 플러그인의 앱·커넥터·훅은 활성화하지 않는다.",
            )}
          </p>
          <div className="cw-skill-filters">
            <label>
              {t("Source", "소스")}
              <select
                aria-label={t("Skill source", "스킬 소스")}
                value={filter}
                onChange={(e) => setFilter(e.target.value as typeof filter)}
              >
                <option value="all">{t("All sources", "전체 소스")}</option>
                <option value="openai">OpenAI Plugins</option>
                <option value="anthropic">Anthropic Skills</option>
              </select>
            </label>
            <input
              aria-label={t("Search official skills", "공식 스킬 검색")}
              placeholder={t("Search skills", "스킬 검색")}
              value={search}
              onChange={(e) => setSearch(e.target.value)}
            />
          </div>
          {(["openai", "anthropic"] as const).map((source) => (
            <div className="cw-skill-source" key={source}>
              <a
                href={
                  source === "openai"
                    ? "https://github.com/openai/plugins"
                    : "https://github.com/anthropics/skills"
                }
                target="_blank"
                rel="noreferrer"
              >
                {source === "openai" ? "OpenAI Plugins" : "Anthropic Skills"}
              </a>
              <button
                type="button"
                disabled={locked}
                aria-label={t("Refresh ", "새로고침 ") + source}
                onClick={() =>
                  void action(async () => {
                    await request("POST", path + "/catalog/" + source);
                    await load();
                  })
                }
              >
                <RefreshCw size={12} />
                {t("Refresh catalog", "목록 새로고침")}
              </button>
            </div>
          ))}
          {d.skillInstall && (
            <button
              type="button"
              disabled={locked}
              onClick={() =>
                void action(async () => {
                  await c.agents.installSkill(tab, node);
                  await load();
                })
              }
            >
              {t("Retry saved installation", "저장된 설치 재시도")}
            </button>
          )}
          <h4>{t("Installed in project", "프로젝트 설치 목록")}</h4>
          {!rows.length && (
            <p>
              {t(
                "No installed skills. Refresh an official catalog to begin.",
                "설치된 스킬이 없다. 공식 목록을 새로고침해 시작한다.",
              )}
            </p>
          )}
          {rows.map((row) => {
            const chosen = selected.some((s) => s.id === row.id),
              compatible =
                row.entry.source === choice?.source &&
                row.status === "installed";
            return (
              <article key={row.id} className="cw-skill-item">
                <label>
                  <input
                    type="checkbox"
                    aria-label={
                      t("Select skill ", "스킬 선택 ") + row.entry.name
                    }
                    checked={chosen}
                    disabled={
                      locked ||
                      !!d.pending ||
                      (!chosen && !compatible) ||
                      (!chosen && selected.length >= 8)
                    }
                    onChange={(e) =>
                      c.agents.change(tab, node, {
                        skills: e.target.checked
                          ? [
                              ...selected,
                              {
                                id: row.id,
                                source: row.entry.source,
                                name: row.entry.name,
                              },
                            ]
                          : selected.filter((s) => s.id !== row.id),
                      })
                    }
                  />
                  <strong>{row.entry.name}</strong>
                </label>
                <small>
                  {row.entry.source} · {row.entry.revision.slice(0, 8)} ·{" "}
                  {row.status}
                  {!compatible ? " · " + t("Inactive", "비활성") : ""}
                </small>
                <code>/workspace/shared/{row.root}</code>
                {row.status === "pending" && (
                  <button
                    type="button"
                    disabled={locked}
                    onClick={() =>
                      void action(async () => {
                        const {
                          source,
                          path: sourcePath,
                          revision,
                          kind,
                        } = row.entry;
                        await request(
                          "PUT",
                          path + "/installations/" + row.id,
                          { source, path: sourcePath, revision, kind },
                        );
                        await load();
                      })
                    }
                  >
                    {t("Resume installation", "설치 이어서 하기")}
                  </button>
                )}
                {row.status !== "removed" && (
                  <button
                    type="button"
                    disabled={locked}
                    aria-label={
                      t("Remove skill ", "스킬 제거 ") + row.entry.name
                    }
                    onClick={() =>
                      void action(async () => {
                        await request(
                          "DELETE",
                          path + "/installations/" + row.id,
                        );
                        await load();
                      })
                    }
                  >
                    <Trash2 size={12} />
                    {t("Remove", "제거")}
                  </button>
                )}
              </article>
            );
          })}
          <h4>{t("Official catalog", "공식 목록")}</h4>
          {catalogs
            .flatMap((s) => s.entries)
            .filter(
              (e) =>
                (filter === "all" || e.source === filter) &&
                (e.name + " " + e.path)
                  .toLowerCase()
                  .includes(search.toLowerCase()),
            )
            .map((entry) => {
              const existing = rows.some(
                (r) =>
                  r.status !== "removed" &&
                  r.entry.source === entry.source &&
                  r.entry.path === entry.path &&
                  r.entry.revision === entry.revision,
              );
              return (
                <article
                  key={entry.source + entry.path}
                  className="cw-skill-item"
                >
                  <strong>{entry.name}</strong>
                  <small>
                    {entry.source} ·{" "}
                    {entry.kind === "skill_bundle"
                      ? t("Plugin skill bundle", "플러그인 스킬 묶음")
                      : t("Individual skill", "개별 스킬")}{" "}
                    · {entry.revision.slice(0, 8)}
                  </small>
                  <button
                    type="button"
                    disabled={
                      locked ||
                      !!d.skillInstall ||
                      existing ||
                      tab.workspace?.status !== "ready"
                    }
                    onClick={() => void action(() => install(entry))}
                  >
                    {existing
                      ? t("Already added", "이미 추가됨")
                      : t("Install in project", "프로젝트에 설치")}
                  </button>
                </article>
              );
            })}
          <small>
            {t(
              "Removal disables new requests. Existing task snapshots, files and history remain retained.",
              "제거하면 새 요청에서 사용하지 않는다. 기존 작업의 스냅샷·파일·기록은 보존한다.",
            )}
          </small>
          {error && <p role="alert">{error}</p>}
        </div>
      )}
    </section>
  );
}
