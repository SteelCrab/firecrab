import { useCallback, useEffect, useRef, useState } from "react";
import {
  Check,
  ExternalLink,
  GitBranch,
  Play,
  RefreshCw,
  ShieldCheck,
  Square,
  X,
} from "lucide-react";
import AppDialog from "./ui/AppDialog";
import { useI18n } from "../i18n";
import { agentRequest, type AgentSession } from "../api/agents";
import type { VmResponse } from "../bindings";
import "../styles/agent-automation.css";

type Config = {
  testCommand: string;
  buildCommand: string;
  startCommand: string;
  port: number;
};
type Verification = {
  vmId: string;
  agentId: string;
  config: Config;
  repository: string | null;
  branch: string;
  head: string;
  workflowConfigured: boolean;
  preview: null | {
    running: boolean;
    publicUrl: string | null;
    error: string | null;
  };
  run: null | {
    id: string;
    state: string;
    phase: string;
    exitCode: number | null;
    appReady: boolean;
    appRunning?: boolean;
    config: Config;
    log: string;
  };
};
type GithubRun = {
  databaseId: number;
  name: string;
  headBranch: string;
  headSha: string;
  status: string;
  conclusion: string;
  url: string;
};
type Proof = { url: string; status: number; ok: boolean; latencyMs: number };
const empty: Config = {
  testCommand: "",
  buildCommand: "",
  startCommand: "",
  port: 3000,
};

export default function AgentAutomation({
  agent,
  vm,
  controllerId,
  canControl,
  onClose,
}: {
  agent: AgentSession;
  vm: VmResponse | undefined;
  controllerId: string;
  canControl: boolean;
  onClose: () => void;
}) {
  const { t } = useI18n();
  const [status, setStatus] = useState<Verification | null>(null);
  const [config, setConfig] = useState<Config>(empty);
  const [busy, setBusy] = useState("");
  const [error, setError] = useState("");
  const [loadError, setLoadError] = useState("");
  const [message, setMessage] = useState("");
  const [proof, setProof] = useState<Proof | null>(null);
  const [runs, setRuns] = useState<GithubRun[] | null>(null);
  const [loaded, setLoaded] = useState(false);
  const alive = useRef(true),
    initialized = useRef(false),
    mutating = useRef(false);
  const vmId = vm?.id || "";
  const endpoint = `/${encodeURIComponent(agent.id)}/automation`;
  const refresh = useCallback(async () => {
    try {
      const next = await agentRequest<Verification>(
        endpoint,
        undefined,
        undefined,
        vmId,
      );
      if (next.vmId !== vmId || next.agentId !== agent.id)
        throw Error(
          t(
            "Workspace identity changed. Reload this page.",
            "작업 공간이 변경되었습니다. 페이지를 새로고침하세요.",
          ),
        );
      if (!alive.current || mutating.current) return;
      setStatus(next);
      if (!initialized.current) {
        setConfig(next.config);
        initialized.current = true;
      }
      setLoaded(true);
      setLoadError("");
    } catch (e) {
      if (alive.current) {
        setLoaded(true);
        setLoadError((e as Error).message);
      }
    }
  }, [endpoint, vmId, agent.id, t]);
  useEffect(() => {
    alive.current = true;
    const initial = window.setTimeout(() => void refresh(), 0);
    const timer = setInterval(() => {
      if (!document.hidden && !mutating.current) void refresh();
    }, 4000);
    return () => {
      alive.current = false;
      clearTimeout(initial);
      clearInterval(timer);
    };
  }, [refresh]);
  const running = status?.run?.state === "running";
  const disabled =
    !canControl ||
    !status ||
    !!busy ||
    running ||
    agent.state === "working" ||
    !!agent.hasPendingTask;
  const dirty =
    status && JSON.stringify(status.config) !== JSON.stringify(config);
  const close = () => {
    if (
      dirty &&
      !window.confirm(
        t(
          "Leave the edited automation settings?",
          "자동화 설정을 저장하지 않고 닫을까요?",
        ),
      )
    )
      return;
    onClose();
  };
  async function action(kind: string) {
    if (mutating.current || !canControl) return;
    mutating.current = true;
    setBusy(kind);
    setError("");
    setMessage("");
    try {
      const next = await agentRequest<
        Verification & { runs?: GithubRun[] } & Proof
      >(
        endpoint,
        { action: kind, id: crypto.randomUUID(), config },
        controllerId,
        vmId,
      );
      if (!alive.current) return;
      if (kind === "github") setRuns(next.runs || []);
      else if (kind === "check_preview") setProof(next);
      else {
        setStatus(next);
        setConfig(next.config);
        if (kind === "configure_ci")
          setMessage(
            t(
              "Created .github/workflows/firecrab-checks.yml in this VM. Inspect the changes before committing and pushing.",
              "VM에 .github/workflows/firecrab-checks.yml 파일을 만들었습니다. 변경 내용을 확인한 뒤 커밋하고 푸시하세요.",
            ),
          );
      }
    } catch (e) {
      if (alive.current) setError((e as Error).message);
    } finally {
      mutating.current = false;
      if (alive.current) setBusy("");
    }
  }
  const run = status?.run;
  const stateLabel = (state: string) =>
    ({
      running: t("Running", "실행 중"),
      passed: t("Passed", "통과"),
      failed: t("Failed", "실패"),
      interrupted: t("Interrupted", "중단됨"),
      unknown: t("Check required", "확인 필요"),
    })[state] || state;
  const phaseLabel = (phase: string) =>
    ({
      starting: t("Preparing", "준비"),
      tests: t("Tests", "테스트"),
      build: t("Build", "빌드"),
      preview: t("Start test app", "테스트 앱 시작"),
      complete: t("Complete", "완료"),
    })[phase] || phase;
  const stages = run
    ? [
        { id: "tests", enabled: true },
        { id: "build", enabled: !!run.config.buildCommand.trim() },
        { id: "preview", enabled: !!run.config.startCommand.trim() },
      ]
    : [];
  const phaseIndex =
    (
      {
        starting: -1,
        tests: 0,
        build: 1,
        preview: 2,
        complete: 3,
      } as Record<string, number>
    )[run?.phase || "starting"] ?? -1;
  const tunnel = status?.preview;
  const url = tunnel?.publicUrl;
  const verified = proof && proof.url === url ? proof : null;
  return (
    <AppDialog
      label={t("Automation and preview", "자동화·미리보기")}
      busy={!!busy}
      onClose={close}
    >
      <div className="aa-dialog">
        <header className="aa-head">
          <div>
            <span>
              {agent.name} · {vm?.name}
            </span>
            <h2>{t("Automation and preview", "자동화·미리보기")}</h2>
          </div>
          <button
            className="icon-btn"
            aria-label={t("Close", "닫기")}
            onClick={close}
            disabled={!!busy}
          >
            <X />
          </button>
        </header>
        <p className="aa-intro">
          {t(
            "Test → build → test app. Every command runs in this task’s MicroVM workspace.",
            "테스트 → 빌드 → 테스트 앱 순서로 실행합니다. 모든 명령은 이 작업의 MicroVM 안에서 실행됩니다.",
          )}
        </p>
        {(error || loadError) && (
          <p className="aa-error" role="alert">
            {error || loadError}
          </p>
        )}
        {message && (
          <p className="aa-message" role="status">
            {message}
          </p>
        )}
        {!loaded ? (
          <p>{t("Loading workspace…", "작업 공간 확인 중…")}</p>
        ) : (
          status && (
            <>
              <section className="aa-section">
                <div className="aa-section-title">
                  <ShieldCheck />
                  <h3>{t("Verification pipeline", "자동 검증")}</h3>
                </div>
                {run && (
                  <div className={`aa-run ${run.state}`} role="status">
                    <strong>{stateLabel(run.state)}</strong>
                    <span>{phaseLabel(run.phase)}</span>
                    {run.exitCode !== null && (
                      <span>
                        {t("Exit code", "종료 코드")} {run.exitCode}
                      </span>
                    )}
                  </div>
                )}
                {run && (
                  <ol
                    className="aa-stages"
                    aria-label={t("Verification stages", "검증 단계")}
                  >
                    {stages.map((stage, index) => {
                      const state = !stage.enabled
                        ? "skipped"
                        : run.state === "passed" || index < phaseIndex
                          ? "done"
                          : index === phaseIndex
                            ? run.state === "running"
                              ? "running"
                              : "failed"
                            : "waiting";
                      const names = {
                        done: t("Passed", "통과"),
                        skipped: t("Skipped", "생략"),
                        running: t("Running", "진행 중"),
                        failed: t("Check required", "확인 필요"),
                        waiting: t("Waiting", "대기"),
                      };
                      return (
                        <li key={stage.id} className={state}>
                          <span>{phaseLabel(stage.id)}</span>
                          <small>{names[state]}</small>
                        </li>
                      );
                    })}
                  </ol>
                )}
                {(agent.state === "working" || agent.hasPendingTask) && (
                  <p>
                    {t(
                      "Verification becomes available after the coding task and CLI setup finish.",
                      "코딩 작업과 CLI 초기 설정을 마치면 검증을 실행할 수 있습니다.",
                    )}
                  </p>
                )}
                <details open={!config.testCommand} className="aa-settings">
                  <summary>{t("Execution settings", "실행 설정")}</summary>
                  <div className="aa-fields">
                    <label>
                      {t("Test command", "테스트 명령")}
                      <input
                        value={config.testCommand}
                        onChange={(e) =>
                          setConfig({ ...config, testCommand: e.target.value })
                        }
                        placeholder="npm ci && npm test"
                        disabled={disabled}
                      />
                    </label>
                    <label>
                      {t("Build command (optional)", "빌드 명령 (선택)")}
                      <input
                        value={config.buildCommand}
                        onChange={(e) =>
                          setConfig({ ...config, buildCommand: e.target.value })
                        }
                        disabled={disabled}
                      />
                    </label>
                    <label>
                      {t(
                        "Test app command (optional)",
                        "테스트 앱 실행 명령 (선택)",
                      )}
                      <input
                        value={config.startCommand}
                        onChange={(e) =>
                          setConfig({ ...config, startCommand: e.target.value })
                        }
                        placeholder="npm run dev -- --host 0.0.0.0 --port 3000"
                        disabled={disabled}
                      />
                    </label>
                    <label>
                      {t("App port", "앱 포트")}
                      <input
                        type="number"
                        min="1"
                        max="65535"
                        value={config.port}
                        onChange={(e) =>
                          setConfig({ ...config, port: Number(e.target.value) })
                        }
                        disabled={disabled}
                      />
                    </label>
                    <small>
                      {t(
                        "Missing project tools must be installed in the VM. Run the test app in the foreground on the selected port.",
                        "프로젝트 실행에 필요한 도구를 VM에 미리 설치하세요. 테스트 앱은 선택한 포트에서 포그라운드로 실행됩니다.",
                      )}
                    </small>
                  </div>
                </details>
                <div className="aa-actions">
                  <button
                    className="btn primary"
                    disabled={
                      disabled ||
                      !config.testCommand.trim() ||
                      !!run?.appRunning
                    }
                    onClick={() => void action("run")}
                  >
                    <Play />
                    {busy === "run"
                      ? t("Starting…", "시작 중…")
                      : t("Run verification", "검증 실행")}
                  </button>
                  {run?.appRunning && (
                    <button
                      className="btn"
                      disabled={!canControl || !!busy || !!tunnel?.running}
                      onClick={() => void action("stop_app")}
                    >
                      <Square />
                      {t("Stop test app", "테스트 앱 중지")}
                    </button>
                  )}
                  <button
                    className="icon-btn"
                    title={t("Refresh verification", "검증 새로고침")}
                    onClick={() => void refresh()}
                    disabled={!!busy}
                  >
                    <RefreshCw />
                  </button>
                </div>
                {run?.log && (
                  <details className="aa-log">
                    <summary>{t("Verification log", "검증 로그")}</summary>
                    <pre>{run.log}</pre>
                  </details>
                )}
              </section>
              <section className="aa-section">
                <div className="aa-section-title">
                  <GitBranch />
                  <h3>GitHub Actions</h3>
                </div>
                <p>
                  {status.workflowConfigured
                    ? t(
                        "Workflow file is present. Push the reviewed changes to run it on GitHub.",
                        "워크플로우 파일이 있습니다. 변경 내용을 확인한 뒤 푸시하면 GitHub에서 실행됩니다.",
                      )
                    : t(
                        "Create a workflow that tests and builds on pushes and pull requests.",
                        "커밋을 푸시하거나 PR을 만들 때 테스트·빌드를 실행할 워크플로우 파일을 만듭니다.",
                      )}
                </p>
                <div className="aa-actions">
                  <button
                    className="btn"
                    disabled={
                      disabled ||
                      status.workflowConfigured ||
                      !config.testCommand.trim()
                    }
                    onClick={() => void action("configure_ci")}
                  >
                    {t("Create workflow", "워크플로우 생성")}
                  </button>
                  <button
                    className="btn"
                    disabled={!canControl || !!busy || !status.repository}
                    onClick={() => void action("github")}
                  >
                    {t("Load workflow runs", "실행 내역 확인")}
                  </button>
                </div>
                {runs && (
                  <ul className="aa-github-runs">
                    {runs.length ? (
                      runs.map((r) => (
                        <li key={r.databaseId}>
                          <a href={r.url} target="_blank" rel="noreferrer">
                            {r.name}
                            <ExternalLink />
                          </a>
                          <span>
                            {r.headBranch} · {r.headSha.slice(0, 7)} ·{" "}
                            {r.conclusion || r.status}
                          </span>
                        </li>
                      ))
                    ) : (
                      <li>
                        {t("No workflow runs yet.", "실행 내역이 없습니다.")}
                      </li>
                    )}
                  </ul>
                )}
              </section>
              <section className="aa-section">
                <div className="aa-section-title">
                  <ExternalLink />
                  <h3>{t("Tunnel preview", "터널 미리보기")}</h3>
                </div>
                <p>
                  {t(
                    "Create a temporary public HTTPS address from inside this MicroVM. Stopping the VM ends the connection.",
                    "이 VM의 테스트 앱을 임시 HTTPS 주소로 공개합니다. VM을 끄면 연결도 종료됩니다.",
                  )}
                </p>
                <div className="aa-actions">
                  <button
                    className="btn"
                    disabled={
                      !canControl ||
                      !!busy ||
                      !run?.appReady ||
                      run.state !== "passed" ||
                      !!url
                    }
                    onClick={() => void action("start_preview")}
                  >
                    {t("Create test URL", "테스트 URL 만들기")}
                  </button>
                  {tunnel && (
                    <button
                      className="btn"
                      disabled={!canControl || !!busy || !tunnel.running}
                      onClick={() => void action("stop_preview")}
                    >
                      <Square />
                      {t("Stop tunnel", "터널 중지")}
                    </button>
                  )}
                </div>
                {tunnel?.error && <p className="aa-error">{tunnel.error}</p>}
                {tunnel && !url && tunnel.running && (
                  <p role="status">
                    {t("Issuing HTTPS address…", "HTTPS 주소 발급 중…")}
                  </p>
                )}
                {url && (
                  <div className="aa-preview">
                    <a href={url} target="_blank" rel="noreferrer">
                      {url}
                      <ExternalLink />
                    </a>
                    <button
                      className="btn"
                      disabled={!canControl || !!busy}
                      onClick={() => void action("check_preview")}
                    >
                      {t("Check HTTPS", "HTTPS 확인")}
                    </button>
                  </div>
                )}
                {verified && (
                  <p
                    className={verified.ok ? "aa-proof" : "aa-error"}
                    role="status"
                  >
                    {verified.ok && <Check />}HTTPS {verified.status} ·{" "}
                    {verified.latencyMs} ms
                  </p>
                )}
                {!run?.appReady && (
                  <small>
                    {t(
                      "Enter a test app command and pass verification before publishing.",
                      "테스트 앱 실행 명령을 입력하고 검증을 통과하면 공개 주소를 만들 수 있습니다.",
                    )}
                  </small>
                )}
              </section>
            </>
          )
        )}
      </div>
    </AppDialog>
  );
}
