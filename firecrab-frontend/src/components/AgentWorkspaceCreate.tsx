import { useEffect, useRef, useState, type FormEvent } from "react";
import {
  Check,
  ChevronRight,
  CircleDot,
  CodeXml,
  Download,
  Network,
  Play,
  Terminal,
} from "lucide-react";
import {
  ApiClientError,
  getVm,
  listMicroNetworks,
  listStorageRoots,
} from "../api/client";
import { agentRequest, type AgentStatus } from "../api/agents";
import {
  createWorkspace,
  getWorkspace,
  workspaceDefaults,
  type WorkspaceRequest,
  type WorkspaceSetup,
} from "../api/workspaces";
import type {
  MicroNetworkResponse,
  StorageRootResponse,
  VmResponse,
} from "../bindings";
import { useI18n } from "../i18n";

type Saved = { request: WorkspaceRequest; cli: string };
function restore(key: string): Saved | null {
  try {
    const saved = JSON.parse(localStorage.getItem(key) || "null");
    return saved?.request?.id && ["codex", "claude"].includes(saved.cli)
      ? saved
      : null;
  } catch {
    return null;
  }
}
export default function AgentWorkspaceCreate({
  controllerId,
  settings,
  onCreated,
  onProgress,
  onDirty,
}: {
  controllerId: string;
  settings: boolean;
  onCreated: (vm: VmResponse, cli: string) => void;
  onProgress: (job: WorkspaceSetup | null) => void;
  onDirty: (dirty: boolean) => void;
}) {
  const { t } = useI18n();
  const key = `firecrab.workspaceSetup.${controllerId}`;
  const [saved, setSaved] = useState(() => restore(key));
  const [draft, setDraft] = useState(
    () => saved?.request ?? { ...workspaceDefaults(), id: crypto.randomUUID() },
  );
  const [cli, setCli] = useState(saved?.cli || "codex");
  const [job, setJob] = useState<WorkspaceSetup | null>(null);
  const [preparedVm, setPreparedVm] = useState<VmResponse | null>(null);
  const [busy, setBusy] = useState(false),
    [error, setError] = useState("");
  const [preparingCli, setPreparingCli] = useState(false),
    [log, setLog] = useState("");
  const [networks, setNetworks] = useState<MicroNetworkResponse[]>([]),
    [roots, setRoots] = useState<StorageRootResponse[]>([]);
  const active = useRef(false),
    alive = useRef(true),
    generation = useRef(0);
  const onReady = useRef(onCreated);
  onReady.current = onCreated;
  const report = useRef(onProgress);
  report.current = onProgress;
  useEffect(() => {
    onDirty(
      !saved &&
        (cli !== "codex" ||
          JSON.stringify({ ...draft, id: undefined }) !==
            JSON.stringify(workspaceDefaults())),
    );
    return () => onDirty(false);
  }, [draft, cli, saved, onDirty]);
  useEffect(() => {
    if (!settings) return;
    let cancelled = false;
    Promise.allSettled([listMicroNetworks(), listStorageRoots()]).then(
      ([networkResult, storageResult]) => {
        if (cancelled) return;
        if (networkResult.status === "fulfilled")
          setNetworks(networkResult.value.filter((n) => n.internetEnabled));
        if (storageResult.status === "fulfilled") setRoots(storageResult.value);
      },
    );
    return () => {
      cancelled = true;
    };
  }, [settings]);
  const delay = () => new Promise<void>((resolve) => setTimeout(resolve, 1200));
  async function follow(
    initial: WorkspaceSetup,
    chosen: string,
    revision: number,
  ) {
    let next = initial;
    const current = () => alive.current && generation.current === revision;
    while (current()) {
      setJob(next);
      report.current(next);
      if (next.state === "failed")
        throw Error(
          next.error ||
            t(
              "Workspace preparation failed.",
              "작업 공간을 준비하지 못했어요.",
            ),
        );
      if (next.state === "ready") break;
      await delay();
      if (!current()) return;
      next = await getWorkspace(next.id);
    }
    if (!current()) return;
    const vm = await getVm(next.vmId);
    setPreparedVm(vm);
    if (vm.state !== "running")
      throw Error(
        t(
          "Start the prepared VM before continuing.",
          "준비된 VM을 먼저 실행해 주세요.",
        ),
      );
    setPreparingCli(true);
    let prepared = false;
    let failures = 0;
    const deadline = Date.now() + 15 * 60_000;
    while (current()) {
      let status: AgentStatus;
      try {
        status = await agentRequest<AgentStatus>(
          "",
          undefined,
          undefined,
          vm.id,
        );
        failures = 0;
      } catch (failure) {
        if (++failures > 10 || Date.now() > deadline) throw failure;
        await delay();
        continue;
      }
      if (!current()) return;
      if (status.controllerId !== controllerId || status.vmId !== vm.id)
        throw Error(
          t(
            "Host identity changed. Reload this host's dashboard.",
            "연결된 호스트가 바뀌었어요. 대시보드를 다시 열어 주세요.",
          ),
        );
      const driver = status.drivers.find((driver) => driver.id === chosen);
      if (status.available && driver?.available) break;
      setLog(status.preparation?.log || "");
      if (status.preparation?.state === "failed" && prepared)
        throw Error(
          status.preparation.error ||
            t("CLI preparation failed.", "AI 도구를 설치하지 못했어요."),
        );
      if (status.preparation?.state !== "installing" && !prepared) {
        await agentRequest("/prepare", { cli: chosen }, controllerId, vm.id);
        prepared = true;
      }
      if (Date.now() > deadline)
        throw Error(
          t(
            "CLI setup is still running. Retry to check its progress.",
            "AI 도구를 설치하고 있어요. 다시 확인하면 기존 작업을 이어서 볼 수 있어요.",
          ),
        );
      await delay();
    }
    if (!current()) return;
    localStorage.removeItem(key);
    onReady.current(vm, chosen);
  }
  async function submit(event?: FormEvent) {
    event?.preventDefault();
    if (active.current || !controllerId) return;
    active.current = true;
    setBusy(true);
    setError("");
    const revision = ++generation.current;
    const pending = saved || {
      request: { ...draft, name: draft.name.trim() },
      cli,
    };
    // Save the request before the POST; a lost response must reuse its operation ID.
    localStorage.setItem(key, JSON.stringify(pending));
    setSaved(pending);
    try {
      await follow(
        await createWorkspace(pending.request),
        pending.cli,
        revision,
      );
    } catch (failure) {
      if (alive.current && revision === generation.current) {
        setError(failure instanceof Error ? failure.message : String(failure));
        if (
          failure instanceof ApiClientError &&
          failure.status === 400 &&
          !job
        ) {
          localStorage.removeItem(key);
          setSaved(null);
        }
      }
    } finally {
      active.current = false;
      if (alive.current) {
        setBusy(false);
        setPreparingCli(false);
      }
    }
  }
  useEffect(() => {
    alive.current = true;
    if (saved) {
      active.current = true;
      setBusy(true);
      const revision = ++generation.current;
      getWorkspace(saved.request.id)
        .then((job) => follow(job, saved.cli, revision))
        .catch((failure) => {
          if (alive.current)
            setError(
              failure instanceof Error ? failure.message : String(failure),
            );
        })
        .finally(() => {
          active.current = false;
          if (alive.current) {
            setBusy(false);
            setPreparingCli(false);
          }
        });
    }
    return () => {
      alive.current = false;
      generation.current++;
    };
    // Resume only on mount; saving a new request must not launch a second follower.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
  const stages = [
    ["image", Download, t("Ubuntu 26.04 LTS", "Ubuntu 26.04 LTS 설치")],
    ["network", Network, t("Network and storage", "네트워크와 저장 공간 준비")],
    ["create", CodeXml, t("Create workspace", "작업 공간 만들기")],
    ["start", Play, t("Start MicroVM", "VM 실행")],
    ["complete", Terminal, t("Install coding tools", "AI 도구 설치")],
  ] as const;
  const stage = stages.findIndex(([phase]) => phase === job?.phase);
  const frozen = !!saved || busy;
  return (
    <section
      className="awc"
      aria-label={t("Workspace setup", "작업 공간 준비")}
    >
      <header>
        <small>AI Sandbox</small>
        <h2>
          {settings
            ? t("Environment settings", "환경 설정")
            : job
              ? t("Preparing your workspace", "작업 공간을 준비하고 있어요")
              : t("New workspace", "새 작업 공간")}
        </h2>
        <p>
          {t(
            "Ubuntu and coding tools are prepared automatically.",
            "Ubuntu와 코딩 도구를 자동으로 설치해요.",
          )}
        </p>
      </header>
      <form className="awc-form" onSubmit={submit}>
        {error && (
          <p className="awc-error" role="alert">
            {error}
          </p>
        )}
        {error && preparedVm && !busy && (
          <button
            type="button"
            className="btn"
            onClick={() => {
              localStorage.removeItem(key);
              onReady.current(preparedVm, cli);
            }}
          >
            {t("Open the prepared MicroVM", "준비된 VM으로 이동")}
          </button>
        )}
        {error &&
          !preparedVm &&
          (job?.state === "failed" || job?.state === "ready") &&
          !busy && (
            <button
              type="button"
              className="btn"
              onClick={() => {
                localStorage.removeItem(key);
                setSaved(null);
                setJob(null);
                report.current(null);
                setDraft((old) => ({ ...old, id: crypto.randomUUID() }));
                setError("");
              }}
            >
              {t(
                "Keep the failed record and change settings",
                "이전 준비 기록을 남기고 설정 바꾸기",
              )}
            </button>
          )}
        {settings ? (
          <>
            <div className="awc-fields">
              <label>
                vCPU
                <select
                  value={draft.cpu}
                  disabled={frozen}
                  onChange={(e) => setDraft({ ...draft, cpu: +e.target.value })}
                >
                  {[1, 2, 4, 8].map((value) => (
                    <option key={value} value={value}>
                      {value} vCPU
                    </option>
                  ))}
                </select>
              </label>
              <label>
                {t("Memory", "메모리")}
                <select
                  value={draft.ram}
                  disabled={frozen}
                  onChange={(e) => setDraft({ ...draft, ram: +e.target.value })}
                >
                  {[1024, 2048, 4096, 8192, 16384, 32768].map((value) => (
                    <option key={value} value={value}>
                      {value / 1024} GiB
                    </option>
                  ))}
                </select>
              </label>
              <label>
                {t("Disk", "디스크")}
                <input
                  type="number"
                  min={8}
                  max={500}
                  value={draft.diskGb}
                  disabled={frozen}
                  onChange={(e) =>
                    setDraft({ ...draft, diskGb: +e.target.value })
                  }
                />
              </label>
            </div>
            <label>
              MicroNetwork
              <select
                value={draft.microNetworkId || ""}
                disabled={frozen}
                onChange={(e) =>
                  setDraft({ ...draft, microNetworkId: e.target.value || null })
                }
              >
                <option value="">{t("Automatic", "자동 구성")}</option>
                {networks.map((network) => (
                  <option key={network.id} value={network.id}>
                    {network.name}
                  </option>
                ))}
              </select>
            </label>
            <label>
              {t("Storage", "저장 위치")}
              <select
                value={draft.storageRoot || ""}
                disabled={frozen}
                onChange={(e) =>
                  setDraft({ ...draft, storageRoot: e.target.value || null })
                }
              >
                <option value="">{t("Automatic", "자동 선택")}</option>
                {roots.map((root) => (
                  <option key={root.id} value={root.id}>
                    {root.name}
                  </option>
                ))}
              </select>
            </label>
            <p>
              {t(
                "Changes apply when this workspace is created.",
                "선택한 사양으로 작업 공간을 만들어요.",
              )}
            </p>
          </>
        ) : (
          <>
            <label>
              {t("Workspace name", "작업 공간 이름")}
              <input
                value={draft.name}
                disabled={frozen}
                maxLength={64}
                onChange={(e) => setDraft({ ...draft, name: e.target.value })}
                placeholder="my-workspace"
              />
            </label>
            <div className="awc-image">
              <CircleDot />
              <div>
                <strong>Ubuntu 26.04 LTS</strong>
                <small>
                  {t(
                    "Default image · automatic installation",
                    "기본 이미지 · 자동 설치",
                  )}
                </small>
              </div>
              <span>{t("Recommended", "권장")}</span>
            </div>
            <div className="awc-resources">
              <span>
                CPU<strong>{draft.cpu} vCPU</strong>
              </span>
              <span>
                {t("Memory", "메모리")}
                <strong>{draft.ram / 1024} GiB</strong>
              </span>
              <span>
                {t("Disk", "디스크")}
                <strong>{draft.diskGb} GiB</strong>
              </span>
            </div>
            <fieldset disabled={frozen} className="awc-cli">
              <legend>{t("Coding agent", "사용할 AI")}</legend>
              {["codex", "claude"].map((value) => (
                <label key={value}>
                  <input
                    type="radio"
                    name="workspace-cli"
                    value={value}
                    checked={cli === value}
                    onChange={() => setCli(value)}
                  />
                  {value === "codex" ? "Codex" : "Claude Code"}
                </label>
              ))}
            </fieldset>
            {job && (
              <ol className="awc-progress" aria-live="polite">
                {stages.map(([id, Icon, label], index) => (
                  <li
                    key={id}
                    data-state={
                      index < stage
                        ? "done"
                        : index === stage
                          ? "current"
                          : "waiting"
                    }
                  >
                    {index < stage ? <Check /> : <Icon />}
                    <span>{label}</span>
                    <small>
                      {index < stage
                        ? t("Done", "완료")
                        : index === stage
                          ? preparingCli || busy
                            ? t("Working…", "진행 중…")
                            : t("Check required", "확인 필요")
                          : t("Waiting", "대기")}
                    </small>
                  </li>
                ))}
              </ol>
            )}
            {log && (
              <details>
                <summary>{t("Installation log", "설치 로그")}</summary>
                <pre>{log}</pre>
              </details>
            )}
            <button
              type="submit"
              className="btn primary awc-submit"
              disabled={busy || !controllerId}
            >
              {busy
                ? t("Preparing…", "준비 중…")
                : saved
                  ? t("Continue preparation", "이어서 준비하기")
                  : t("Create workspace", "작업 공간 만들기")}
              <ChevronRight />
            </button>
            <p>
              {t(
                "Connect your accounts after installation.",
                "설치가 끝나면 계정을 연결해 주세요.",
              )}
            </p>
          </>
        )}
      </form>
    </section>
  );
}
