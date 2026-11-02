import {
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type FormEvent,
} from "react";
import { ApiClientError, fetchJson } from "../../api/client";
import {
  CODE,
  agentName,
  codeError,
  compatible,
  equal,
  preferred,
  remember,
  restore,
  uuid,
  write,
} from "../../api/code";
import type { Repository, Spec, Workspace } from "../../api/code";
import type { useAccounts } from "./hooks";
import { useI18n } from "../../i18n";

const KEY = "firecrab.code.setup.react";
type Pending = { id: string; spec: Spec };
const empty = (): Spec => ({
  name: "",
  github: { account: "", owner: "", repository: "", branch: "" },
  runtime: "node_js",
  resources: { cpu: 2, ram: 4096, diskGb: 16 },
  agent: null,
});
export default function CodeSetup({
  auth,
  manage,
  onDirty,
}: {
  auth: ReturnType<typeof useAccounts>;
  manage: () => void;
  onDirty: (value: boolean) => void;
}) {
  const { t } = useI18n();
  const [initial] = useState(() => {
    const p = restore<Pending>(KEY);
    return p && uuid(p.id) && p.spec?.github ? p : null;
  });
  const [spec, set] = useState<Spec>(initial?.spec ?? empty),
    [pending, setPending] = useState<Pending | null>(initial);
  const [login, setLogin] = useState(""),
    [repos, setRepos] = useState<Repository[]>([]),
    [branches, setBranches] = useState<string[]>([]);
  const [repoNext, setRepoNext] = useState<number | null>(null),
    [branchNext, setBranchNext] = useState<number | null>(null);
  const [repoPage, repoMore] = useState(1),
    [branchPage, branchMore] = useState(1),
    [metadataEpoch, reload] = useState(0);
  const [loading, setLoading] = useState(true),
    [branchLoading, setBranchLoading] = useState(false),
    [busy, setBusy] = useState(false),
    [error, setError] = useState("");
  const [explicit, setExplicit] = useState(!!initial),
    [retry, setRetry] = useState(false);
  const manual = useRef({ name: !!initial, runtime: !!initial }),
    observedLogin = useRef(initial?.spec.github.account ?? ""),
    pendingSave = useRef(pending),
    lock = useRef(false),
    alive = useRef(true),
    committed = useRef(false);
  useLayoutEffect(() => {
    pendingSave.current = pending;
  }, [pending]);
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);
  useEffect(() => {
    onDirty(
      !committed.current &&
        (!!spec.github.repository ||
          !!pending ||
          manual.current.name ||
          manual.current.runtime),
    );
    return () => onDirty(false);
  }, [spec, pending, onDirty]);
  useEffect(() => {
    const controller = new AbortController();
    setLoading(true);
    void (async () => {
      try {
        const rows = await fetchJson<{ provider: string; account: string }[]>(
          "/api/pipe/auth",
          { signal: controller.signal },
        );
        const github = rows.find((a) => a.provider === "github")?.account ?? "";
        if (controller.signal.aborted) return;
        if (github !== observedLogin.current) {
          if (observedLogin.current && !pendingSave.current) {
            set((old) => ({
              ...old,
              github: {
                account: github,
                owner: "",
                repository: "",
                branch: "",
              },
            }));
            setBranches([]);
            branchMore(1);
          }
          observedLogin.current = github;
          if (repoPage !== 1) {
            repoMore(1);
            return;
          }
        }
        if (!github) {
          if (!controller.signal.aborted) {
            setLogin("");
            setRepos([]);
          }
          return;
        }
        const result = await fetchJson<{
          account: string;
          repositories: Repository[];
          nextPage?: number;
        }>(
          `/api/code/github/repositories?account=${encodeURIComponent(github)}&page=${repoPage}`,
          { signal: controller.signal },
        );
        if (result.account !== github || !Array.isArray(result.repositories))
          throw new Error("invalid");
        if (!controller.signal.aborted) {
          setLogin(github);
          setRepos((old) =>
            repoPage === 1
              ? result.repositories
              : [...old, ...result.repositories],
          );
          setRepoNext(result.nextPage ?? null);
          setError("");
        }
      } catch {
        if (!controller.signal.aborted) setError("catalog_unavailable");
      } finally {
        if (!controller.signal.aborted) setLoading(false);
      }
    })();
    return () => controller.abort();
  }, [repoPage, metadataEpoch]);
  useEffect(() => {
    if (!spec.github.repository || !login) return;
    const controller = new AbortController();
    setBranchLoading(true);
    void (async () => {
      try {
        const q = new URLSearchParams({
          account: login,
          owner: spec.github.owner,
          repository: spec.github.repository,
          page: String(branchPage),
        });
        const result = await fetchJson<{
          account: string;
          owner: string;
          repository: string;
          branches: string[];
          nextPage?: number;
        }>(`/api/code/github/branches?${q}`, { signal: controller.signal });
        if (
          result.account !== login ||
          result.owner !== spec.github.owner ||
          result.repository !== spec.github.repository ||
          !Array.isArray(result.branches)
        )
          throw new Error("invalid");
        if (!controller.signal.aborted) {
          setBranches((old) =>
            branchPage === 1 ? result.branches : [...old, ...result.branches],
          );
          setBranchNext(result.nextPage ?? null);
          const def = repos.find(
            (r) =>
              r.owner === spec.github.owner &&
              r.repository === spec.github.repository,
          )?.defaultBranch;
          if (!pending)
            set((old) => ({
              ...old,
              github: {
                ...old.github,
                account: login,
                branch:
                  old.github.branch ||
                  (def && result.branches.includes(def) ? def : ""),
              },
            }));
        }
      } catch {
        if (!controller.signal.aborted) {
          setBranches([]);
          setError("branches_unavailable");
        }
      } finally {
        if (!controller.signal.aborted) setBranchLoading(false);
      }
    })();
    return () => controller.abort();
  }, [
    login,
    spec.github.owner,
    spec.github.repository,
    branchPage,
    metadataEpoch,
    repos,
    pending,
  ]); // Defaults never overwrite a selected branch.
  useEffect(() => {
    const eligible = auth.accounts.filter((a) => preferred(a));
    if (!explicit && !pending && eligible.length === 1)
      set((old) =>
        old.agent
          ? old
          : {
              ...old,
              agent: {
                agent: preferred(eligible[0]),
                connectionId: eligible[0].id,
              },
            },
      );
  }, [auth.accounts, explicit, pending]);
  const eligible = auth.accounts.filter((a) => preferred(a));
  const binding = spec.agent,
    account = auth.accounts.find((a) => a.id === binding?.connectionId);
  const valid =
    !!login &&
    login === spec.github.account &&
    !loading &&
    !branchLoading &&
    !auth.loading &&
    (!auth.error || explicit) &&
    /^[A-Za-z0-9._-]{1,64}$/.test(spec.name) &&
    /[A-Za-z0-9]/.test(spec.name) &&
    repos.some(
      (r) =>
        r.owner === spec.github.owner &&
        r.repository === spec.github.repository,
    ) &&
    branches.includes(spec.github.branch) &&
    (!eligible.length || !!binding || explicit) &&
    (!binding || (!!account && compatible(binding.agent, account))) &&
    spec.resources.cpu >= 1 &&
    spec.resources.cpu <= 32 &&
    [128, 256, 512, 1024, 2048, 4096, 8192, 16384, 32768].includes(
      spec.resources.ram,
    ) &&
    spec.resources.diskGb >= 1 &&
    spec.resources.diskGb <= 500;
  function repository(key: string) {
    const r = repos.find((v) => `${v.owner}/${v.repository}` === key);
    if (!r) return;
    setBranches([]);
    branchMore(1);
    set((old) => ({
      ...old,
      name: manual.current.name
        ? old.name
        : r.repository.replace(/[^A-Za-z0-9._-]/g, "").slice(0, 64) ||
          "workspace",
      runtime: manual.current.runtime
        ? old.runtime
        : r.language === "Python"
          ? "python"
          : r.language === "Rust"
            ? "rust"
            : "node_js",
      github: {
        account: login,
        owner: r.owner,
        repository: r.repository,
        branch: "",
      },
    }));
  }
  function selectAgent(agent: string) {
    setExplicit(!!agent);
    set((old) => {
      const candidates = auth.accounts.filter((a) => compatible(agent, a));
      return {
        ...old,
        agent:
          !agent || agent === "none"
            ? null
            : {
                agent,
                connectionId:
                  account && compatible(agent, account)
                    ? account.id
                    : candidates.length === 1
                      ? candidates[0].id
                      : "",
              },
      };
    });
  }
  function complete(id: string) {
    committed.current = true;
    sessionStorage.removeItem(KEY);
    setPending(null);
    onDirty(false);
    location.hash = `#/code/${id}/editor`;
  }
  async function save(event: FormEvent) {
    event.preventDefault();
    if (lock.current || (!pending && !valid)) return;
    lock.current = true;
    setBusy(true);
    setError("");
    const p = pending ?? {
      id: crypto.randomUUID(),
      spec: structuredClone(spec),
    };
    try {
      if (pending) {
        try {
          const saved = await fetchJson<Workspace>(`${CODE}/${p.id}`);
          if (
            saved.id === p.id &&
            saved.revision > 0 &&
            equal(saved.spec, p.spec)
          ) {
            complete(p.id);
            return;
          }
          setError("save_conflict");
          return;
        } catch (e) {
          if (
            !(e instanceof ApiClientError) ||
            e.status !== 404 ||
            e.apiError?.code !== "not_found"
          )
            throw e;
        }
        if (!retry) {
          setRetry(true);
          return;
        }
        if (!valid) throw new Error("accounts_changed");
      }
      remember(KEY, p);
      setPending(p);
      setRetry(false);
      const saved = await write<Workspace>(
        `${CODE}/${p.id}`,
        { expectedRevision: 0, spec: p.spec },
        "PUT",
      );
      if (saved.id !== p.id || !equal(saved.spec, p.spec) || saved.revision < 1)
        throw new Error("invalid");
      complete(p.id);
    } catch (e) {
      if (alive.current) {
        setError(codeError(e));
        if (e instanceof ApiClientError && e.status && e.status < 500) {
          sessionStorage.removeItem(KEY);
          setPending(null);
        }
      }
    } finally {
      lock.current = false;
      if (alive.current) setBusy(false);
    }
  }
  return (
    <form id="code-setup" className="code-setup" onSubmit={(e) => void save(e)}>
      <p>
        {t(
          "Choose your repository. Start with the defaults.",
          "저장소를 선택하면 기본 설정을 준비합니다.",
        )}
      </p>
      <fieldset disabled={busy || !!pending}>
        {!login && (
          <a href="#/pipe/auth" target="_blank" rel="noreferrer">
            {t("Connect GitHub", "GitHub 연결")}
          </a>
        )}
        <label>
          {t("GitHub repository", "GitHub 저장소")}
          <select
            id="code-field-repository"
            value={`${spec.github.owner}/${spec.github.repository}`}
            onChange={(e) => repository(e.target.value)}
          >
            <option value="/">{t("Select a repository", "저장소 선택")}</option>
            {repos.map((r) => (
              <option
                key={`${r.owner}/${r.repository}`}
                value={`${r.owner}/${r.repository}`}
              >
                {r.owner}/{r.repository}
              </option>
            ))}
          </select>
        </label>
        {repoNext && (
          <button
            type="button"
            className="btn"
            onClick={() => repoMore(repoNext)}
          >
            {t("Load more repositories", "저장소 더 불러오기")}
          </button>
        )}
        {spec.github.repository && (
          <p className="code-hint">
            {spec.name} · {spec.github.branch} ·{" "}
            {spec.runtime === "node_js"
              ? "Node.js"
              : spec.runtime === "python"
                ? "Python"
                : "Rust"}
          </p>
        )}
        <section className="code-section">
          <h3>AI AUTH</h3>
          {binding && account && (
            <p>
              {agentName(binding.agent)} · {account.name}
            </p>
          )}
          {(eligible.length > 1 || (!!binding && !account)) &&
            !(explicit && !binding) && (
              <label>
                {t("AI account", "AI 계정")}
                <select
                  id="code-field-assignment"
                  value={binding?.connectionId ?? ""}
                  onChange={(e) => {
                    const a = eligible.find((a) => a.id === e.target.value);
                    setExplicit(!!a);
                    set((old) => ({
                      ...old,
                      agent: a
                        ? {
                            agent:
                              old.agent && compatible(old.agent.agent, a)
                                ? old.agent.agent
                                : preferred(a),
                            connectionId: a.id,
                          }
                        : null,
                    }));
                  }}
                >
                  <option value="">
                    {t("Choose an AI account", "AI 계정 선택")}
                  </option>
                  {eligible.map((a) => (
                    <option key={a.id} value={a.id}>
                      {a.name} · {a.provider}
                    </option>
                  ))}
                </select>
              </label>
            )}
          {!eligible.length && (
            <button type="button" className="btn" onClick={manage}>
              {t("Connect AI AUTH", "AI AUTH 연결")}
            </button>
          )}
          {explicit && !binding && (
            <p>{t("Workspace only", "워크스페이스만 사용")}</p>
          )}
        </section>
        <details
          id="code-setup-options"
          open={error === "code_workspace_name_taken" ? true : undefined}
        >
          <summary>{t("Settings", "설정")}</summary>
          <div className="code-fields">
            <label>
              {t("Workspace name", "워크스페이스 이름")}
              <input
                id="code-field-name"
                maxLength={64}
                value={spec.name}
                onChange={(e) => {
                  manual.current.name = true;
                  set({ ...spec, name: e.target.value });
                }}
              />
            </label>
            <label>
              {t("Branch", "브랜치")}
              <select
                value={spec.github.branch}
                onChange={(e) =>
                  set({
                    ...spec,
                    github: { ...spec.github, branch: e.target.value },
                  })
                }
              >
                <option value="">{t("Select a branch", "브랜치 선택")}</option>
                {branches.map((b) => (
                  <option key={b}>{b}</option>
                ))}
              </select>
            </label>
            {branchNext && (
              <button
                type="button"
                className="btn"
                onClick={() => branchMore(branchNext)}
              >
                {t("Load more branches", "브랜치 더 불러오기")}
              </button>
            )}
            <label>
              {t("Runtime", "실행 환경")}
              <select
                value={spec.runtime}
                onChange={(e) => {
                  manual.current.runtime = true;
                  set({ ...spec, runtime: e.target.value });
                }}
              >
                {["node_js", "python", "rust"].map((v) => (
                  <option key={v}>{v}</option>
                ))}
              </select>
            </label>
            {(["cpu", "ram", "diskGb"] as const).map((k) => (
              <label key={k}>
                {
                  {
                    cpu: "CPU",
                    ram: t("Memory (MiB)", "메모리 (MiB)"),
                    diskGb: t("Disk (GiB)", "디스크 (GiB)"),
                  }[k]
                }
                <input
                  type="number"
                  value={spec.resources[k]}
                  onChange={(e) =>
                    set({
                      ...spec,
                      resources: {
                        ...spec.resources,
                        [k]: Number(e.target.value),
                      },
                    })
                  }
                />
              </label>
            ))}
            <label>
              {t("Agent", "에이전트")}
              <select
                value={binding?.agent ?? (explicit ? "none" : "")}
                onChange={(e) => selectAgent(e.target.value)}
              >
                <option value="">
                  {t("Choose an agent", "에이전트 선택")}
                </option>
                <option value="none">
                  {t("Workspace only", "워크스페이스만 사용")}
                </option>
                {["codex", "claude_code", "opencode"].map((v) => (
                  <option key={v} value={v}>
                    {agentName(v)}
                  </option>
                ))}
              </select>
            </label>
            {binding && (
              <label>
                AI AUTH
                <select
                  value={binding.connectionId}
                  onChange={(e) =>
                    set({
                      ...spec,
                      agent: { ...binding, connectionId: e.target.value },
                    })
                  }
                >
                  <option value="">
                    {t("Select a compatible account", "호환 계정 선택")}
                  </option>
                  {auth.accounts
                    .filter((a) => compatible(binding.agent, a))
                    .map((a) => (
                      <option key={a.id} value={a.id}>
                        {a.name}
                      </option>
                    ))}
                </select>
              </label>
            )}
            <button type="button" className="btn" onClick={manage}>
              {t("Manage AI AUTH", "AI AUTH 관리")}
            </button>
            <button
              type="button"
              className="btn"
              onClick={() => reload((n) => n + 1)}
            >
              {t("Reload connections", "연결 다시 불러오기")}
            </button>
          </div>
        </details>
      </fieldset>
      {(error || auth.error) && (
        <p role="alert">
          {t(
            "Could not confirm the settings. Check connections and retry.",
            "설정을 확인하지 못했습니다. 연결을 확인한 뒤 다시 시도해 주세요.",
          )}{" "}
          <code>{error}</code>
        </p>
      )}
      {pending && (
        <p role="status">
          {t(
            "Confirm the original save before continuing.",
            "계속하려면 이전 저장 결과를 확인해 주세요.",
          )}
        </p>
      )}
      <footer>
        <a className="btn" href="#/code">
          {t("Cancel", "취소")}
        </a>
        <button className="btn primary" disabled={busy || (!pending && !valid)}>
          {pending
            ? retry
              ? t("Retry same request", "같은 요청 재시도")
              : t("Check saved workspace", "저장한 워크스페이스 확인")
            : t("Create workspace", "워크스페이스 생성")}
        </button>
      </footer>
    </form>
  );
}
