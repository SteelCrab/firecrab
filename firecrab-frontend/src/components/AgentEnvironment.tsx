import { Plus, Play, RefreshCw, SquareTerminal, Check } from "lucide-react";
import type { VmResponse } from "../bindings";
import type { AgentStatus } from "../api/agents";
import { useI18n } from "../i18n";
import { consolePageUrl } from "../navigation";
import AgentBrandIcon from "./icons/AgentBrandIcon";

type Props = {
  vms: VmResponse[];
  vmId: string;
  cli: string;
  status: AgentStatus | null;
  busy: boolean;
  canControl: boolean;
  onVm: (id: string) => void;
  onCli: (id: string) => void;
  onCreate: () => void;
  onStart: () => void;
  onPrepare: () => void;
  onLogin: (cli: string) => void;
  onRefresh: () => void;
};
export default function AgentEnvironment(p: Props) {
  const { t } = useI18n();
  const vm = p.vms.find((v) => v.id === p.vmId);
  const driver = p.status?.drivers.find((d) => d.id === p.cli);
  const transitioning =
    !!vm && ["starting", "pausing", "resuming", "stopping"].includes(vm.state);
  const installing = p.status?.preparation?.state === "installing";
  const drivers = [
    { id: "codex", name: "Codex" },
    { id: "claude", name: "Claude Code" },
    ...(p.status?.drivers.filter((d) => d.available && d.id === "copilot") ||
      []),
  ];
  const vmState = (state: string) => {
    switch (state) {
      case "running":
        return t("Running", "실행 중");
      case "created":
        return t("Not started", "시작 전");
      case "stopped":
        return t("Stopped", "중지됨");
      case "paused":
        return t("Paused", "일시정지됨");
      case "starting":
      case "resuming":
        return t("Starting", "시작 중");
      case "stopping":
        return t("Stopping", "중지 중");
      case "pausing":
        return t("Pausing", "일시정지 중");
      case "error":
        return t("Error", "오류");
      default:
        return state;
    }
  };
  return (
    <section
      className="am-environment"
      aria-label={t("Execution environment", "실행 환경")}
    >
      <div className="am-vm-row">
        <label htmlFor="am-vm">MicroVM</label>
        <select
          id="am-vm"
          value={p.vmId}
          onChange={(e) => p.onVm(e.target.value)}
          disabled={p.busy}
        >
          <option value="">{t("Choose a MicroVM", "MicroVM 선택")}</option>
          {p.vms.map((v) => (
            <option key={v.id} value={v.id}>
              {v.name} · {vmState(v.state)}
            </option>
          ))}
        </select>
        <button className="btn" onClick={p.onCreate} disabled={p.busy}>
          <Plus />
          {t("New MicroVM", "새 MicroVM")}
        </button>
        {vm && (
          <a
            className="icon-btn am-vm-terminal"
            aria-label={t("VM terminal", "VM 터미널")}
            title={t("VM terminal", "VM 터미널")}
            href={consolePageUrl(vm.id)}
            target="_blank"
            rel="noreferrer"
          >
            <SquareTerminal />
          </a>
        )}
      </div>
      {!vm ? (
        <p className="am-environment-note">
          {t(
            "Choose or create a MicroVM. Agents and repository files run inside that VM.",
            "MicroVM을 선택하거나 만든다. 에이전트와 저장소 파일은 선택한 VM 안에서 실행한다.",
          )}
        </p>
      ) : vm.state !== "running" ? (
        <div className="am-environment-step">
          <p>
            {t(
              "Start this MicroVM to connect its coding workspace.",
              "작업 공간에 연결하려면 이 MicroVM을 시작한다.",
            )}
          </p>
          <button
            className="btn primary"
            onClick={p.onStart}
            disabled={p.busy || transitioning}
          >
            <Play />
            {transitioning
              ? t("Changing VM state…", "VM 상태 변경 중…")
              : vm.state === "paused"
                ? t("Resume MicroVM", "MicroVM 재개")
                : t("Start MicroVM", "MicroVM 시작")}
          </button>
        </div>
      ) : (
        <>
          <div className="am-runtime-row">
            <fieldset className="am-cli-options am-runtime-clis">
              <legend>{t("Coding CLI", "코딩 CLI")}</legend>
              <div className="am-cli-choice-grid">
                {drivers.map((d) => (
                  <label key={d.id} className="am-cli-choice">
                    <input
                      type="radio"
                      name="coding-cli"
                      aria-label={d.name}
                      checked={p.cli === d.id}
                      disabled={p.busy || installing}
                      onChange={() => p.onCli(d.id)}
                    />
                    <AgentBrandIcon kind={d.id} />
                    <span>{d.name}</span>
                    <span className="am-choice-check" aria-hidden>
                      {p.cli === d.id && <Check />}
                    </span>
                  </label>
                ))}
              </div>
            </fieldset>
            <div className="am-runtime-actions">
              {p.cli &&
                p.status &&
                (!p.status.available || !driver?.available) && (
                  <button
                    className="btn primary"
                    disabled={p.busy || installing || !p.canControl}
                    onClick={p.onPrepare}
                  >
                    {installing
                      ? t("Preparing…", "환경 준비 중…")
                      : t("Prepare CLI environment", "CLI 환경 준비")}
                  </button>
                )}
              {p.status?.available &&
                driver?.available &&
                ["codex", "claude"].includes(p.cli) &&
                !driver.authenticated && (
                  <button
                    className="btn primary"
                    disabled={p.busy || !p.canControl}
                    onClick={() => p.onLogin(p.cli)}
                  >
                    {t("Connect CLI account", "CLI 계정 연결")}
                  </button>
                )}
              {driver?.available && driver.authenticated && (
                <span className="am-ready">
                  <Check />
                  {t(
                    "CLI account connected in this VM",
                    "이 VM에서 CLI 계정 연결됨",
                  )}
                </span>
              )}
              {!!p.status && (
                <button
                  className="icon-btn"
                  title={t("Check environment", "환경 다시 확인")}
                  aria-label={t("Check environment", "환경 다시 확인")}
                  onClick={p.onRefresh}
                  disabled={p.busy}
                >
                  <RefreshCw />
                </button>
              )}
            </div>
          </div>
          {installing && (
            <div className="am-prepare-progress" role="status">
              <progress max={4} value={p.status?.preparation?.step || 1} />
              <span>
                {t(
                  p.status?.preparation?.cli === "github"
                    ? "Preparing GitHub tools inside this MicroVM."
                    : "Preparing tools and the selected CLI inside this MicroVM.",
                  p.status?.preparation?.cli === "github"
                    ? "이 MicroVM 안에 GitHub 인증 도구를 준비한다."
                    : "이 MicroVM 안에 기본 도구와 선택한 CLI를 준비한다.",
                )}
              </span>
            </div>
          )}
          {p.status?.preparation?.error && (
            <p className="am-error" role="alert">
              {p.status.preparation.error}
            </p>
          )}
          {!!p.status?.preparation?.log &&
            (installing || !!p.status.preparation.error) && (
              <details className="am-install-log">
                <summary>{t("Environment log", "환경 준비 로그")}</summary>
                <pre>
                  {p.status.preparation.log
                    // eslint-disable-next-line no-control-regex -- remove terminal styling from the readable installation log
                    .replace(/\x1b\[[0-?]*[ -/]*[@-~]/g, "")
                    .replace(/\r/g, "")}
                </pre>
              </details>
            )}
        </>
      )}
    </section>
  );
}
