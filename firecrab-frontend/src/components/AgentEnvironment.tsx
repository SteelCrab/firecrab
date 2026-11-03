import { useState } from "react";
import { Plus, Play, RefreshCw, SquareTerminal } from "lucide-react";
import type { VmResponse } from "../bindings";
import type { AgentStatus } from "../api/agents";
import { useI18n } from "../i18n";
import { consolePageUrl } from "../navigation";
import { useElapsedSeconds } from "../lib/useElapsedSeconds";
import AgentBrandIcon from "./icons/AgentBrandIcon";

type Props = {
  compact?: boolean;
  vms: VmResponse[];
  vmId: string;
  status: AgentStatus | null;
  busy: boolean;
  canControl: boolean;
  /** CLIs waiting for their turn to install, in order. */
  queue: string[];
  /** Install errors kept after a later install replaced the VM's record. */
  failures: Record<string, string>;
  onVm: (id: string) => void;
  onCreate: () => void;
  onStart: () => void;
  onInstall: (clis: string[]) => void;
  onLogin: (cli: string) => void;
  onRefresh: () => void;
};
export default function AgentEnvironment(p: Props) {
  const { t } = useI18n();
  const [picked, setPicked] = useState<string[]>([]);
  const vm = p.vms.find((v) => v.id === p.vmId);
  const transitioning =
    !!vm && ["starting", "pausing", "resuming", "stopping"].includes(vm.state);
  const preparation = p.status?.preparation;
  const installing = preparation?.state === "installing";
  const installingCli = installing ? preparation?.cli || null : null;
  const elapsed = useElapsedSeconds(installingCli);
  const drivers = [
    { id: "codex", name: "Codex" },
    { id: "claude", name: "Claude Code" },
    ...(p.status?.drivers.filter((d) => d.available && d.id === "copilot") ||
      []),
  ];
  const installingName =
    drivers.find((d) => d.id === installingCli)?.name || installingCli;
  const failure = (id: string) => {
    if (id in p.failures)
      return p.failures[id] || t("Install failed", "설치 실패");
    if (
      preparation?.cli === id &&
      ["failed", "interrupted"].includes(preparation.state)
    )
      return preparation.error || t("Install failed", "설치 실패");
    return "";
  };
  // Installed and signed in inside this VM, as the guest reports it. A CLI
  // runs only through Herdr, so it counts as installed only with Herdr.
  const cliState = (id: string) => {
    const found = p.status?.drivers.find((d) => d.id === id);
    if (!found) return null;
    if (installingCli === id)
      return {
        id: "installing",
        label: t(`Installing… ${elapsed ?? 0}s`, `설치 중… ${elapsed ?? 0}초`),
      };
    if (p.queue.includes(id))
      return { id: "waiting", label: t("Waiting", "대기") };
    if (found.available && p.status?.available)
      return found.authenticated
        ? { id: "connected", label: t("Connected", "연결됨") }
        : { id: "signed-out", label: t("Not signed in", "로그인 전") };
    if (failure(id))
      return { id: "failed", label: t("Install failed", "설치 실패") };
    return { id: "missing", label: t("Not installed", "설치 전") };
  };
  const installable = (id: string) =>
    ["missing", "failed"].includes(cliState(id)?.id || "");
  const chosen = picked.filter(installable);
  const errors = [
    ...drivers
      .filter((d) => cliState(d.id)?.id === "failed")
      .map((d) => `${d.name}: ${failure(d.id)}`),
    ...(preparation?.error && !drivers.some((d) => d.id === preparation.cli)
      ? [preparation.error]
      : []),
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
        return t("Paused", "일시정지");
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
      <div className="am-vm-row" hidden={p.compact}>
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
            "VM을 선택하거나 새로 만들어 주세요. 코드와 AI는 이 VM 안에서 실행돼요.",
          )}
        </p>
      ) : vm.state !== "running" ? (
        <div className="am-environment-step">
          <p>
            {t(
              "Start this MicroVM to connect its coding workspace.",
              "작업 공간에 연결하려면 VM을 시작해 주세요.",
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
              <p className="am-cli-hint">
                {t(
                  "Check every AI you want to use. Sign in to each one after it installs.",
                  "사용할 AI를 모두 골라 주세요. 설치가 끝나면 AI마다 로그인해요.",
                )}
              </p>
              <ul className="am-cli-list">
                {drivers.map((d) => {
                  const state = cliState(d.id);
                  const open = installable(d.id);
                  return (
                    <li
                      key={d.id}
                      className="am-cli-item"
                      data-state={state?.id}
                    >
                      <label>
                        <input
                          type="checkbox"
                          aria-label={d.name}
                          checked={open ? picked.includes(d.id) : !!state}
                          disabled={!open}
                          onChange={(e) => {
                            const checked = e.target.checked;
                            setPicked((old) =>
                              checked
                                ? [...old, d.id]
                                : old.filter((id) => id !== d.id),
                            );
                          }}
                        />
                        <AgentBrandIcon kind={d.id} />
                        <span>{d.name}</span>
                        {state && (
                          <small className="am-cli-state" data-state={state.id}>
                            {state.label}
                          </small>
                        )}
                      </label>
                      {state?.id === "signed-out" &&
                        ["codex", "claude"].includes(d.id) && (
                          <button
                            className="btn primary"
                            disabled={p.busy || !p.canControl}
                            onClick={() => p.onLogin(d.id)}
                          >
                            {t(
                              `Connect ${d.name} account`,
                              `${d.name} 계정 연결`,
                            )}
                          </button>
                        )}
                    </li>
                  );
                })}
              </ul>
            </fieldset>
            <div className="am-runtime-actions">
              {drivers.some((d) => installable(d.id)) && (
                <button
                  className="btn primary"
                  disabled={!chosen.length || !p.canControl}
                  onClick={() => {
                    p.onInstall(chosen);
                    setPicked([]);
                  }}
                >
                  {t("Install selected CLIs", "선택한 CLI 설치")}
                </button>
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
              <progress max={4} value={preparation?.step || 1} />
              <span>
                {preparation?.cli === "github"
                  ? t(
                      "Preparing GitHub tools inside this MicroVM.",
                      "이 VM에 GitHub 로그인 도구를 설치해요.",
                    )
                  : t(
                      `Installing ${installingName} inside this MicroVM.`,
                      `이 VM에 ${installingName}를 설치하고 있어요.`,
                    )}
              </span>
            </div>
          )}
          {errors.map((error) => (
            <p key={error} className="am-error" role="alert">
              {error}
            </p>
          ))}
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
