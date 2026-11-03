import { useCallback, useEffect, useState } from "react";
import { Check, RefreshCw, Search } from "lucide-react";
import { useI18n } from "../i18n";
import {
  importSkill,
  setVmSkill,
  skillCatalog,
  vmSkills,
  type SkillCatalog,
  type SkillReason,
  type StoreSkill,
  type VmSkills,
} from "../api/skills";
import "../styles/agent-skills.css";

const message = (failure: unknown) =>
  failure instanceof Error ? failure.message : String(failure);

/** Official skill store: list on the left, the selected skill on the right. */
export default function AgentSkillStore({
  vmId,
  controllerId,
}: {
  vmId: string;
  controllerId: string;
}) {
  const { t } = useI18n();
  const [catalog, setCatalog] = useState<SkillCatalog | null>(null);
  const [vm, setVm] = useState<VmSkills | null>(null);
  const [selected, setSelected] = useState("");
  const [query, setQuery] = useState("");
  const [busy, setBusy] = useState(false);
  // Show the requested switch position until the server answers; a failure falls back.
  const [requested, setRequested] = useState<boolean | null>(null);
  const [error, setError] = useState("");

  const loadCatalog = useCallback(async () => {
    try {
      const next = await skillCatalog();
      setCatalog(next);
      setError("");
    } catch (failure) {
      setError(message(failure));
    }
  }, []);
  const loadVm = useCallback(async () => {
    if (!vmId) return;
    try {
      setVm(await vmSkills(vmId, controllerId));
    } catch (failure) {
      setError(message(failure));
    }
  }, [vmId, controllerId]);
  useEffect(() => {
    skillCatalog().then(setCatalog, (failure) => setError(message(failure)));
  }, []);
  useEffect(() => {
    if (vmId)
      vmSkills(vmId, controllerId).then(setVm, (failure) =>
        setError(message(failure)),
      );
  }, [vmId, controllerId]);

  const reasons: Record<Exclude<SkillReason, null>, string> = {
    proprietary: t(
      "Proprietary license. Firecrab cannot copy this skill.",
      "독점 라이선스라서 가져올 수 없어요.",
    ),
    "unknown-license": t(
      "The license could not be confirmed.",
      "라이선스를 확인할 수 없어 가져올 수 없어요.",
    ),
    "unsupported-files": t(
      "This skill contains unsupported files.",
      "지원하지 않는 파일이 있어 가져올 수 없어요.",
    ),
    "too-large": t(
      "This skill is too large.",
      "스킬이 너무 커서 가져올 수 없어요.",
    ),
  };
  const license = (skill: StoreSkill) =>
    skill.license === "apache2"
      ? "Apache 2.0"
      : skill.license === "proprietary"
        ? t("Proprietary", "독점")
        : t("Unknown", "확인 불가");

  const skills = catalog?.skills || [];
  const visible = skills.filter((s) =>
    s.name.toLowerCase().includes(query.trim().toLowerCase()),
  );
  const skill =
    skills.find((s) => s.name === selected) || visible[0] || skills[0] || null;
  const enabled = !!skill && !!vm?.enabled.includes(skill.name);

  const run = async (work: () => Promise<void>) => {
    setBusy(true);
    setError("");
    try {
      await work();
    } catch (failure) {
      setError(message(failure));
    } finally {
      setBusy(false);
    }
  };
  const importSelected = () =>
    skill &&
    run(async () => setCatalog(await importSkill(skill.name, controllerId)));
  const toggle = async (next: boolean) => {
    if (!skill) return;
    setRequested(next);
    await run(async () =>
      setVm(await setVmSkill(vmId, controllerId, skill.name, next)),
    );
    setRequested(null);
  };

  return (
    <section className="ask" aria-label={t("Skill store", "스킬 스토어")}>
      <header className="am-quick-heading ask-heading">
        <small>{t("Skill store", "스킬 스토어")}</small>
        <h2>{t("Official skills", "공식 스킬")}</h2>
        {catalog && (
          <p className="ask-source">
            {t("Official", "공식")} · {catalog.repository}
            {catalog.commit ? ` · ${catalog.commit.slice(0, 7)}` : ""}
          </p>
        )}
      </header>
      {(catalog?.error || error) && (
        <div className="ask-error" role="alert">
          <span>{catalog?.error || error}</span>
          <button
            type="button"
            className="btn"
            onClick={() => void loadCatalog()}
          >
            <RefreshCw aria-hidden="true" />
            {t("Retry", "다시 시도")}
          </button>
        </div>
      )}
      <div className="ask-layout">
        <nav className="ask-list" aria-label={t("Skills", "스킬 목록")}>
          <label className="ask-search">
            <Search aria-hidden="true" />
            <span className="sr-only">{t("Search skills", "스킬 검색")}</span>
            <input
              type="search"
              value={query}
              placeholder={t("Search skills…", "스킬 검색…")}
              onChange={(e) => setQuery(e.target.value)}
            />
          </label>
          <ul>
            {visible.map((item) => (
              <li key={item.name}>
                <button
                  type="button"
                  aria-label={item.name}
                  aria-current={item.name === skill?.name ? "true" : undefined}
                  onClick={() => setSelected(item.name)}
                >
                  <span>{item.name}</span>
                  {item.imported && <Check aria-hidden="true" />}
                </button>
              </li>
            ))}
          </ul>
        </nav>
        {skill && (
          <article className="ask-detail">
            <div className="ask-title">
              <h3>{skill.name}</h3>
              {skill.imported && !skill.updateAvailable ? (
                <span className="ask-imported">
                  {t("✓ Imported", "✓ 가져옴")}
                </span>
              ) : (
                <button
                  type="button"
                  className="btn primary"
                  disabled={busy || !skill.importable}
                  onClick={() => void importSelected()}
                >
                  {skill.updateAvailable
                    ? t("Update", "업데이트")
                    : t("Import", "가져오기")}
                </button>
              )}
            </div>
            {skill.updateAvailable && (
              <p className="ask-note">
                {t("Update available", "업데이트 있음")}
              </p>
            )}
            {skill.reason && (
              <p className="ask-reason">{reasons[skill.reason]}</p>
            )}
            <p className="ask-description">{skill.description}</p>
            <dl className="ask-facts">
              <div>
                <dt>{t("Files", "파일")}</dt>
                <dd>{skill.files.join(" · ")}</dd>
              </div>
              <div>
                <dt>{t("License", "라이선스")}</dt>
                <dd>{license(skill)}</dd>
              </div>
              <div>
                <dt>{t("Version", "버전")}</dt>
                <dd>
                  {(skill.imported?.commit || catalog?.commit || "").slice(
                    0,
                    7,
                  )}{" "}
                  {skill.imported ? t("(pinned)", "(고정)") : ""}
                </dd>
              </div>
            </dl>
            {skill.imported && (
              <div className="ask-workspace">
                {!vmId ? (
                  <p>
                    {t(
                      "Select a MicroVM to use skills",
                      "스킬을 쓰려면 MicroVM을 선택하세요",
                    )}
                  </p>
                ) : (
                  <>
                    <label className="ask-switch">
                      <input
                        type="checkbox"
                        role="switch"
                        checked={requested ?? enabled}
                        disabled={busy || !vm}
                        onChange={(e) => void toggle(e.target.checked)}
                      />
                      <span>
                        {t("Use in this workspace", "이 작업 공간에서 사용")}
                      </span>
                    </label>
                    {enabled && vm && (
                      <VmState vm={vm} onRetry={() => void loadVm()} />
                    )}
                  </>
                )}
              </div>
            )}
            <pre className="ask-preview">{skill.preview}</pre>
          </article>
        )}
      </div>
    </section>
  );
}

function VmState({ vm, onRetry }: { vm: VmSkills; onRetry: () => void }) {
  const { t } = useI18n();
  if (vm.state !== "applied")
    return (
      <div className="ask-vm" data-state={vm.state}>
        <p>
          {vm.state === "pending"
            ? t("Waiting for this MicroVM", "VM 연결을 기다리는 중")
            : t("Could not apply skills", "스킬을 적용하지 못했어요")}
        </p>
        {vm.reason && <small>{vm.reason}</small>}
        <button type="button" className="btn" onClick={onRetry}>
          {t("Retry", "다시 시도")}
        </button>
      </div>
    );
  return (
    <div className="ask-vm" data-state="applied">
      <ul>
        {vm.clis.map((cli) => (
          <li key={cli.id}>
            {cli.installed
              ? `${cli.name} ✓`
              : `${cli.name} · ${t("Not installed", "미설치")}`}
          </li>
        ))}
      </ul>
      {vm.conflicts.map((path) => (
        <small key={path}>
          {t(
            `Skipped ${path}: a skill with this name already exists.`,
            `${path}에 같은 이름의 스킬이 있어 건너뛰었어요.`,
          )}
        </small>
      ))}
      <small>
        {t("Applies to new agent sessions", "새 에이전트 세션부터 적용돼요")}
      </small>
    </div>
  );
}
