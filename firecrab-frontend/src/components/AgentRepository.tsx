import { useCallback, useEffect, useState } from "react";
import {
  Folder,
  GitBranch,
  Check,
  Copy,
  ExternalLink,
  RefreshCw,
  ChevronRight,
  ArrowRight,
} from "lucide-react";
import {
  agentRequest,
  type GitHubAuth,
  type VmRepository,
  type GitHubRepository as Repository,
} from "../api/agents";
import { useI18n } from "../i18n";
import AgentRepositoryCreate from "./AgentRepositoryCreate";

type Props = {
  project: VmRepository | null;
  github?: GitHubAuth;
  vmId: string;
  controllerId: string;
  canControl: boolean;
  onSaved: () => Promise<void>;
  onDirty: (value: boolean) => void;
  onPrepareGitHub: () => void;
  preparingGitHub: boolean;
  connectionUnavailable?: boolean;
  /** Summary row only; sign-in belongs to the Connect accounts page. */
  compact?: boolean;
};

function GitHubMark() {
  return (
    <img
      className="am-github-mark"
      src="/agent-brands/github.svg"
      alt=""
      width="18"
      height="18"
      aria-hidden="true"
    />
  );
}

export default function AgentRepository(p: Props) {
  const { t } = useI18n();
  const { onDirty } = p;
  const [repository, setRepository] = useState(
    () =>
      p.project?.repository
        ?.replace(/^https:\/\/github.com\//, "")
        .replace(/\.git$/, "") || "",
  );
  const [branch, setBranch] = useState(() => p.project?.branch || "");
  const [busy, setBusy] = useState(false);
  const [authBusy, setAuthBusy] = useState(false);
  const [error, setError] = useState("");
  const [authError, setAuthError] = useState("");
  const [repositories, setRepositories] = useState<Repository[]>([]);
  const [loading, setLoading] = useState(true);
  const [listError, setListError] = useState("");
  const [copiedCode, setCopiedCode] = useState("");
  const [manual, setManual] = useState(false);
  const [createRepository, setCreateRepository] = useState(false);
  const [creationDirty, setCreationDirty] = useState(false);
  const ready = p.project?.state === "ready";
  const preparing = busy || p.project?.state === "preparing";
  const connected = p.github?.state === "connected";
  const pending = p.github?.state === "pending";
  const code = p.github?.userCode;
  const loadRepositories = useCallback(
    async (signal?: AbortSignal) => {
      try {
        const result = await agentRequest<{ repositories: Repository[] }>(
          "/github/repositories",
          undefined,
          undefined,
          p.vmId,
        );
        if (!signal?.aborted) setRepositories(result.repositories);
      } catch (e) {
        if (!signal?.aborted)
          setListError(e instanceof Error ? e.message : String(e));
      } finally {
        if (!signal?.aborted) setLoading(false);
      }
    },
    [p.vmId],
  );
  useEffect(() => {
    if (!connected || ready) return;
    const cancel = new AbortController();
    // eslint-disable-next-line react/set-state-in-effect -- results are set only after the VM network request settles
    void loadRepositories(cancel.signal);
    return () => cancel.abort();
  }, [connected, ready, loadRepositories]);
  useEffect(() => {
    onDirty(
      creationDirty || (!ready && !!(repository.trim() || branch.trim())),
    );
    return () => onDirty(false);
  }, [creationDirty, ready, repository, branch, onDirty]);
  const connect = async () => {
    if (!p.canControl || authBusy || pending) return;
    setAuthBusy(true);
    setAuthError("");
    try {
      await agentRequest<GitHubAuth>("/github", {}, p.controllerId, p.vmId);
      await p.onSaved();
    } catch (e) {
      setAuthError(e instanceof Error ? e.message : String(e));
    } finally {
      setAuthBusy(false);
    }
  };
  const setup = async (empty = false) => {
    if (!p.canControl || preparing) return;
    setBusy(true);
    setError("");
    try {
      await agentRequest<VmRepository>(
        "/project",
        {
          repository: empty ? "" : repository.trim(),
          branch: empty ? "" : branch.trim(),
        },
        p.controllerId,
        p.vmId,
      );
      await p.onSaved();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      await p.onSaved();
    } finally {
      setBusy(false);
    }
  };
  const name = p.project?.repository
    ?.replace(/^https:\/\/github.com\//, "")
    .replace(/\.git$/, "");
  const copyCode = () => {
    if (!code) return;
    void navigator.clipboard
      .writeText(code)
      .then(() => setCopiedCode(code))
      .catch(() =>
        setAuthError(
          t("Select and copy the code.", "코드를 선택해서 복사해 주세요."),
        ),
      );
  };
  const repositoryField = (
    <label className="am-repository-field">
      <span className="sr-only">{t("Repository address", "저장소 주소")}</span>
      <input
        required
        placeholder="owner/repository"
        value={repository}
        disabled={!p.canControl || preparing}
        onChange={(e) => setRepository(e.target.value)}
      />
    </label>
  );
  const connectRepository = (
    <button
      className="btn primary"
      type="submit"
      aria-label={t("Connect repository", "저장소 연결")}
      disabled={!p.canControl || !repository.trim() || preparing}
    >
      {t("Connect", "연결")}
      <ArrowRight aria-hidden="true" />
    </button>
  );
  return (
    <section
      className={`am-vm-repository${ready && (connected || p.compact) ? " is-ready" : " is-setup"}`}
      aria-label={t("MicroVM repository", "MicroVM 저장소")}
    >
      <header className="am-connection-overview">
        <span className="am-connection-mark">
          {ready && !name ? <Folder aria-hidden="true" /> : <GitHubMark />}
        </span>
        <div className="am-connection-copy">
          <strong>
            {ready
              ? name || t("VM workspace", "VM 작업 폴더")
              : t("Connect a repository", "작업할 저장소 연결")}
          </strong>
        </div>
        <span
          className={`am-connection-status${p.connectionUnavailable ? " unavailable" : connected ? " connected" : pending ? " pending" : ""}`}
        >
          {connected ? (
            <Check aria-hidden="true" />
          ) : (
            <span aria-hidden="true" className="am-connection-dot" />
          )}
          {p.connectionUnavailable
            ? t("VM connection unavailable", "VM 연결 확인 필요")
            : connected
              ? t("Connected in this VM", "GitHub 인증됨")
              : pending
                ? t("Waiting for authorization", "인증 대기 중")
                : !p.github
                  ? t("Checking connection", "상태 확인 중")
                  : t("Not connected", "연결 전")}
        </span>
        {ready && connected && !!p.project?.branch && (
          <span
            className="am-repository-current-branch"
            title={t("Initial branch", "처음 사용할 브랜치")}
          >
            <GitBranch aria-hidden="true" />
            {p.project.branch}
          </span>
        )}
      </header>
      {!connected && !p.compact && (
        <div
          className="am-connection-step am-github-auth"
          aria-label="GitHub AUTH"
        >
          <span className="am-step-number" aria-hidden="true">
            1
          </span>
          <div className="am-step-content">
            <div className="am-step-heading">
              <strong>{t("GitHub account", "GitHub 계정 인증")}</strong>
              {!pending && (
                <button
                  className="btn primary"
                  aria-label={
                    p.github?.state === "unavailable"
                      ? t("Prepare GitHub AUTH", "GitHub 인증 준비")
                      : t("Connect with GitHub AUTH", "GitHub AUTH로 연결")
                  }
                  disabled={
                    !p.canControl || authBusy || p.preparingGitHub || !p.github
                  }
                  onClick={
                    p.github?.state === "unavailable"
                      ? p.onPrepareGitHub
                      : () => void connect()
                  }
                >
                  {p.preparingGitHub
                    ? t("Preparing…", "준비 중…")
                    : authBusy
                      ? t("Connecting…", "연결 중…")
                      : t("Connect GitHub", "GitHub로 연결")}
                  <ArrowRight aria-hidden="true" />
                </button>
              )}
            </div>
            {pending ? (
              <div className="am-device-auth" role="status">
                {code ? (
                  <>
                    <p>
                      {t(
                        "Copy the code and enter it on GitHub.",
                        "코드를 복사한 뒤 GitHub에 입력해 주세요.",
                      )}
                    </p>
                    <div className="am-device-actions">
                      <div className="am-device-code">
                        <code>{code}</code>
                        <button
                          className="icon-btn"
                          aria-label={t(
                            "Copy authentication code",
                            "인증 코드 복사",
                          )}
                          title={
                            copiedCode === code
                              ? t("Copied", "복사됨")
                              : t("Copy code", "코드 복사")
                          }
                          onClick={copyCode}
                        >
                          {copiedCode === code ? <Check /> : <Copy />}
                        </button>
                      </div>
                      <a
                        className="btn"
                        href="https://github.com/login/device"
                        aria-label={t("Authorize on GitHub", "GitHub에서 인증")}
                        target="_blank"
                        rel="noopener noreferrer"
                      >
                        <ExternalLink aria-hidden="true" />
                        {t("Authorize", "인증하기")}
                      </a>
                    </div>
                    <span className="am-auth-waiting">
                      {t(
                        "This screen updates automatically after authorization.",
                        "로그인이 끝나면 자동으로 연결돼요.",
                      )}
                    </span>
                  </>
                ) : (
                  <p>
                    {t(
                      "Requesting an authentication code…",
                      "로그인 코드를 받고 있어요…",
                    )}
                  </p>
                )}
              </div>
            ) : (
              <p>
                {t(
                  "Authorize once. All tasks in this VM use the same GitHub account.",
                  "한 번 로그인하면 이 VM의 모든 작업에서 사용할 수 있어요.",
                )}
              </p>
            )}
            {(authError || p.github?.error) && (
              <p className="am-error" role="alert">
                {authError || p.github?.error}
              </p>
            )}
          </div>
        </div>
      )}
      {!ready && (
        <form
          className="am-connection-step am-repository-setup"
          aria-busy={preparing}
          onSubmit={(e) => {
            e.preventDefault();
            void setup();
          }}
        >
          <span
            className={`am-step-number${connected ? "" : " is-muted"}`}
            aria-hidden="true"
          >
            2
          </span>
          <div className="am-step-content">
            <div className="am-step-heading">
              <strong>{t("Repository", "저장소 선택")}</strong>
              {connected && (
                <div className="am-repository-options">
                  <button
                    className="am-text-button"
                    type="button"
                    disabled={!p.canControl || preparing}
                    onClick={() => setCreateRepository(true)}
                  >
                    {t("New repository", "새 저장소 만들기")}
                  </button>
                  <button
                    className="am-text-button"
                    type="button"
                    disabled={!p.canControl || preparing}
                    onClick={() => setManual((value) => !value)}
                  >
                    {manual
                      ? t("Choose from list", "목록에서 선택")
                      : t("Enter an address", "주소로 연결")}
                  </button>
                </div>
              )}
            </div>
            {preparing ? (
              <div className="am-repository-progress" role="status">
                <progress
                  aria-label={t("Preparing repository", "저장소 준비")}
                />
                <p>
                  {t(
                    "Preparing the repository in this MicroVM…",
                    "이 VM에 저장소를 준비하고 있어요…",
                  )}
                </p>
              </div>
            ) : connected ? (
              <>
                <div className="am-repository-fields">
                  {manual ? (
                    repositoryField
                  ) : (
                    <label className="am-repository-field">
                      <span className="sr-only">
                        {t("Your GitHub repositories", "GitHub 저장소 선택")}
                      </span>
                      <select
                        disabled={!p.canControl || loading}
                        value={
                          repositories.some((r) => r.name === repository)
                            ? repository
                            : ""
                        }
                        onChange={(e) => setRepository(e.target.value)}
                      >
                        <option value="">
                          {loading
                            ? t("Loading repositories…", "저장소 불러오는 중…")
                            : t("Choose a repository", "작업할 저장소 선택")}
                        </option>
                        {repositories.map((r) => (
                          <option
                            key={r.name}
                            value={r.name}
                            disabled={r.archived}
                          >
                            {r.name}
                            {r.private ? t(" · Private", " · 비공개") : ""}
                            {r.archived ? t(" · Archived", " · 보관됨") : ""}
                          </option>
                        ))}
                      </select>
                    </label>
                  )}
                  {connectRepository}
                  {!manual && (
                    <button
                      className="icon-btn"
                      type="button"
                      disabled={!p.canControl || loading}
                      aria-label={t(
                        "Reload repositories",
                        "저장소 다시 불러오기",
                      )}
                      onClick={() => {
                        setLoading(true);
                        setListError("");
                        void loadRepositories();
                      }}
                    >
                      <RefreshCw />
                    </button>
                  )}
                </div>
                {listError && (
                  <p className="am-error" role="alert">
                    {listError}
                  </p>
                )}
                {!loading && !listError && repositories.length === 0 && (
                  <p>
                    {t(
                      "No repositories found. Enter an address instead.",
                      "저장소가 없어요. 주소를 직접 입력해서 연결할 수도 있어요.",
                    )}
                  </p>
                )}
                <details className="am-repository-branch">
                  <summary>{t("Initial branch", "처음 사용할 브랜치")}</summary>
                  <input
                    aria-label={t("Initial branch", "처음 사용할 브랜치")}
                    placeholder={t(
                      "Repository default branch",
                      "저장소 기본 브랜치",
                    )}
                    value={branch}
                    disabled={!p.canControl}
                    onChange={(e) => setBranch(e.target.value)}
                  />
                </details>
              </>
            ) : (
              <p className="am-repository-locked">
                {t(
                  "Your repositories appear after GitHub authorization.",
                  "GitHub에 로그인하면 저장소 목록을 불러옵니다.",
                )}
              </p>
            )}
            {!preparing && (
              <details className="am-repository-alternative">
                <summary>
                  <ChevronRight aria-hidden="true" />
                  {t("Continue without GitHub", "GitHub 없이 시작")}
                </summary>
                <div>
                  {!connected && (
                    <div className="am-repository-fields">
                      {repositoryField}
                      {connectRepository}
                    </div>
                  )}
                  {!p.project && (
                    <button
                      type="button"
                      className="am-text-button"
                      disabled={!p.canControl}
                      onClick={() => void setup(true)}
                    >
                      {t(
                        "Start with an empty VM folder",
                        "빈 VM 폴더에서 시작",
                      )}
                      <ArrowRight aria-hidden="true" />
                    </button>
                  )}
                </div>
              </details>
            )}
            {(error || p.project?.error) && (
              <p className="am-error" role="alert">
                {error || p.project?.error}
              </p>
            )}
          </div>
        </form>
      )}
      {createRepository && connected && (
        <AgentRepositoryCreate
          controllerId={p.controllerId}
          vmId={p.vmId}
          canControl={p.canControl}
          onClose={() => setCreateRepository(false)}
          onDirty={setCreationDirty}
          onCreated={async (value) => {
            setRepository(value.name);
            setBranch("");
            setRepositories((old) => [
              value,
              ...old.filter((row) => row.name !== value.name),
            ]);
            try {
              await agentRequest<VmRepository>(
                "/project",
                { repository: value.name, branch: "" },
                p.controllerId,
                p.vmId,
              );
            } catch (failure) {
              void p.onSaved().catch(() => {});
              throw failure;
            }
            await p.onSaved();
          }}
        />
      )}
    </section>
  );
}
