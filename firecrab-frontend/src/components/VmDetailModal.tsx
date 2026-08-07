import { useEffect, useRef, useState } from "react";
import type {
  EgressPolicy,
  MicroNetworkResponse,
  StartupStep,
  StartupStepRun,
  StorageRootResponse,
  VmResponse,
} from "../bindings";
import {
  ApiClientError,
  assignVmStorage,
  getVm,
  getVmLog,
  listMicroNetworks,
  listStorageRoots,
  runPackageAction,
  updateVmResources,
} from "../api/client";
import { isEditableState } from "../model";
import { logDownloadFilename } from "../lib/textExport";
import LogExportActions from "./LogExportActions";
import RamStepper from "./RamStepper";
import { useI18n } from "../i18n";

const STARTUP_STEPS: StartupStep[] = [
  "preparingDisk",
  "generatingConfig",
  "startingProcess",
  "configuringNetwork",
];

const STARTUP_STEP_LABEL: Record<StartupStep, string> = {
  preparingDisk: "Preparing disk",
  generatingConfig: "Generating configuration",
  startingProcess: "Starting process",
  configuringNetwork: "Checking network",
};

// Derived client-side from the polled `startupStep` value — no dedicated
// backend log field. See docs/40-tests/vm-detail-modal.md for why.
const STARTUP_STEP_LOG_LINE: Record<StartupStep, string> = {
  preparingDisk: "Preparing disk (copying rootfs template)…",
  generatingConfig: "Disk ready → generating Firecracker configuration…",
  startingProcess: "Configuration ready → starting Firecracker process…",
  configuringNetwork: "Process started → checking guest DHCP/DNS…",
};

const POLL_MILLIS = 750;

interface VmDetailModalProps {
  vmId: string;
  vms: VmResponse[];
  onClose: () => void;
}

/**
 * VM detail modal: pipeline step-by-step progress at the top and a combined
 * log (derived pipeline lines, then the real captured guest console output
 * once Firecracker has produced any) below.
 */
export default function VmDetailModal({ vmId, vms, onClose }: VmDetailModalProps) {
  const { t } = useI18n();
  const [vm, setVm] = useState<VmResponse | null>(
    () => vms.find((candidate) => candidate.id === vmId) ?? null,
  );
  const [consoleLog, setConsoleLog] = useState("");
  const [pipelineLines, setPipelineLines] = useState<string[]>([]);
  const [highestStepSeen, setHighestStepSeen] = useState(-1);
  const logRef = useRef<HTMLPreElement>(null);

  const [editing, setEditing] = useState(false);
  const [editCpu, setEditCpu] = useState("1");
  const [editRam, setEditRam] = useState("512");
  const [editDisk, setEditDisk] = useState("2");
  const [editEgressPolicy, setEditEgressPolicy] = useState<EgressPolicy>("internet");
  const [editStorageRoot, setEditStorageRoot] = useState("default");
  const [saving, setSaving] = useState(false);
  const [saveError, setSaveError] = useState<ApiClientError | null>(null);
  const [microNetworks, setMicroNetworks] = useState<MicroNetworkResponse[]>([]);
  const [storageRoots, setStorageRoots] = useState<StorageRootResponse[]>([]);
  const [installInput, setInstallInput] = useState("");
  const [removeInput, setRemoveInput] = useState("");
  const [packageBusy, setPackageBusy] = useState<"install" | "remove" | "update" | null>(null);
  const [packageError, setPackageError] = useState<string | null>(null);

  // Only to resolve the VM's microNetworkId into a readable name/subnet; a
  // failed load just falls back to showing the raw id.
  useEffect(() => {
    listMicroNetworks().then(setMicroNetworks).catch(() => setMicroNetworks([]));
    listStorageRoots().then(setStorageRoots).catch(() => setStorageRoots([]));
  }, []);

  const startEditing = () => {
    if (!vm) return;
    setEditCpu(String(vm.cpu));
    setEditRam(String(vm.ram));
    setEditDisk(String(vm.diskGb));
    setEditEgressPolicy(vm.egressPolicy);
    setEditStorageRoot(vm.storageRoot || "default");
    setSaveError(null);
    setEditing(true);
  };

  const cancelEditing = () => {
    setEditing(false);
    setSaveError(null);
  };

  const handleSave = async () => {
    if (!vm) return;
    setSaving(true);
    setSaveError(null);
    try {
      let updated = await updateVmResources(vm.id, {
        cpu: parseInt(editCpu, 10) || 0,
        ram: parseInt(editRam, 10) || 0,
        diskGb: parseInt(editDisk, 10) || 0,
        egressPolicy: editEgressPolicy,
      });
      // Storage reassignment is a separate endpoint: only when inactive and
      // no rootfs exists yet (server returns 409 otherwise).
      if (editStorageRoot && editStorageRoot !== vm.storageRoot) {
        updated = await assignVmStorage(vm.id, { storageRoot: editStorageRoot });
      }
      setVm(updated);
      setEditing(false);
    } catch (error) {
      setSaveError(error as ApiClientError);
    } finally {
      setSaving(false);
    }
  };

  const runPackages = async (action: "install" | "remove" | "update", input?: string) => {
    if (!vm) return;
    const packages = input ? input.split(/\s+/).filter(Boolean) : [];
    if (action !== "update" && packages.length === 0) {
      setPackageError(t("Enter at least one package name.", "패키지 이름을 입력하세요."));
      return;
    }
    setPackageBusy(action);
    setPackageError(null);
    try {
      const updated = await runPackageAction(vm.id, action, packages);
      setVm(updated);
      if (action === "install") setInstallInput("");
      if (action === "remove") setRemoveInput("");
    } catch (error) {
      setPackageError((error as Error).message);
    } finally {
      setPackageBusy(null);
    }
  };

  useEffect(() => {
    let cancelled = false;
    // `startup_step` resets to `null` on every transition out of Starting,
    // including the successful one — so "how far did this attempt get" has
    // to be remembered here, not read back off the server after the fact.
    let wasStarting = false;
    let seen = -1;
    let lines: string[] = [];

    const tick = async () => {
      try {
        const [nextVm, log] = await Promise.all([getVm(vmId), getVmLog(vmId)]);
        if (cancelled) return;

        // A fresh start (including a restart while this modal happens to
        // still be open) gets a fresh pipeline log.
        if (nextVm.state === "starting" && !wasStarting) {
          seen = -1;
          lines = [];
        }
        wasStarting = nextVm.state === "starting";

        if (nextVm.startupStep) {
          const index = STARTUP_STEPS.indexOf(nextVm.startupStep);
          for (let i = seen + 1; i <= index; i++) {
            lines = [...lines, `[${timestamp()}] ${STARTUP_STEP_LOG_LINE[STARTUP_STEPS[i]]}`];
          }
          seen = Math.max(seen, index);
        } else if (nextVm.state === "running" && seen < STARTUP_STEPS.length - 1) {
          seen = STARTUP_STEPS.length - 1;
          lines = [...lines, `[${timestamp()}] Ready — VM started.`];
        }

        setVm(nextVm);
        setConsoleLog(log.consoleLog);
        setPipelineLines(lines);
        setHighestStepSeen(seen);
      } catch {
        // Transient poll miss — keep the last known state, try again next
        // tick (same philosophy as the main dashboard's own polling).
      }
    };

    tick();
    const interval = setInterval(tick, POLL_MILLIS);
    return () => {
      cancelled = true;
      clearInterval(interval);
    };
  }, [vmId]);

  useEffect(() => {
    const el = logRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [pipelineLines, consoleLog]);

  const currentIndex =
    vm?.state === "running"
      ? STARTUP_STEPS.length
      : vm?.state === "starting" || vm?.state === "error"
        ? highestStepSeen
        : -1;

  const emptyLog = t("No output yet.", "아직 출력이 없습니다.");
  const logText = [...pipelineLines, consoleLog].filter(Boolean).join("\n") || emptyLog;

  const microNetwork = microNetworks.find((network) => network.id === vm?.microNetworkId);
  const microNetworkLabel = !vm?.microNetworkId
    ? t("Default network", "기본 네트워크")
    : microNetwork
      ? `${microNetwork.name} (${microNetwork.subnetCidr})`
      : vm.microNetworkId;

  const storageRootMeta = storageRoots.find((root) => root.id === vm?.storageRoot);
  const storageRootLabel = storageRootMeta
    ? `${storageRootMeta.name} (${storageRootMeta.path})`
    : (vm?.storageRoot ?? "default");

  return (
    <div className="console-overlay">
      <div className="console-panel">
        <div className="console-bar">
          <span className="console-title">{t(`VM details — ${vm?.name ?? vmId}`, `VM 상세 — ${vm?.name ?? vmId}`)}</span>
          {vm && <span className={`state-badge ${vm.state}`}>{vm.state}</span>}
          <button className="btn console-close" onClick={onClose}>
            ✕
          </button>
        </div>
        {vm ? (
          <div className="detail-body">
            <dl className="detail-fields mono">
              <dt>image</dt>
              <dd>{vm.template}</dd>
              <dt>cpu</dt>
              <dd>
                {editing ? (
                  <input
                    className="detail-edit-input"
                    type="number"
                    min={1}
                    max={32}
                    value={editCpu}
                    onChange={(event) => setEditCpu(event.target.value)}
                  />
                ) : (
                  vm.cpu
                )}
              </dd>
              <dt>ram</dt>
              <dd>
                {editing ? (
                  <RamStepper id="vm-edit-ram" value={editRam} onChange={setEditRam} />
                ) : (
                  `${vm.ram} MiB`
                )}
              </dd>
              <dt>disk</dt>
              <dd>
                {editing ? (
                  <input
                    className="detail-edit-input"
                    type="number"
                    min={vm.diskGb}
                    max={500}
                    value={editDisk}
                    onChange={(event) => setEditDisk(event.target.value)}
                  />
                ) : (
                  `${vm.diskGb} GiB`
                )}
              </dd>
              <dt>MicroNetwork</dt>
              <dd>{microNetworkLabel}</dd>
              <dt>MicroStorage</dt>
              <dd>
                {editing && storageRoots.length > 0 ? (
                  <select
                    className="detail-edit-input"
                    value={editStorageRoot}
                    onChange={(event) => setEditStorageRoot(event.target.value)}
                  >
                    {storageRoots.map((root) => (
                      <option key={root.id} value={root.id}>
                        {root.name} ({root.path})
                        {root.availableGib > 0 ? ` · ${root.availableGib} GiB free` : ""}
                      </option>
                    ))}
                  </select>
                ) : (
                  storageRootLabel
                )}
              </dd>
              <dt>{t("Egress", "외부 통신")}</dt>
              <dd>
                {editing ? (
                  <select
                    className="detail-edit-input"
                    value={editEgressPolicy}
                    onChange={(event) => setEditEgressPolicy(event.target.value as EgressPolicy)}
                  >
                    {(["internet", "isolated"] as EgressPolicy[]).map((policy) => (
                      <option key={policy} value={policy}>
                        {policy === "internet" ? t("Internet access", "인터넷 허용") : t("Isolated (gateway only)", "격리(게이트웨이만 허용)")}
                      </option>
                    ))}
                  </select>
                ) : (
                  vm.egressPolicy === "internet" ? t("Internet access", "인터넷 허용") : t("Isolated (gateway only)", "격리(게이트웨이만 허용)")
                )}
              </dd>
              <dt>ip</dt>
              <dd>{vm.ipv4 ?? "—"}</dd>
              <dt>mac</dt>
              <dd>{vm.mac ?? "—"}</dd>
              <dt>hostname</dt>
              <dd>{vm.hostname}</dd>
              <dt>id</dt>
              <dd title={vm.id}>{vm.id}</dd>
            </dl>
            {isEditableState(vm.state) && (
              <div className="detail-edit-actions">
                {editing ? (
                  <>
                    <button className="btn primary" onClick={handleSave} disabled={saving}>
                      {saving ? t("Saving…", "저장 중…") : t("Save", "저장")}
                    </button>
                    <button className="btn" onClick={cancelEditing} disabled={saving}>
                      {t("Cancel", "취소")}
                    </button>
                    {saveError && <span className="field-error">{saveError.message}</span>}
                  </>
                ) : (
                  <button className="btn" onClick={startEditing}>
                    {t("Edit", "수정")}
                  </button>
                )}
              </div>
            )}
            <PipelineStepper currentIndex={currentIndex} timeline={vm?.startupTimeline ?? []} />
            <div className="log-export-bar">
              <span className="log-export-bar-label">{t("Startup · console log", "시작 · 콘솔 로그")}</span>
              <LogExportActions
                text={logText === emptyLog ? "" : logText}
                filename={logDownloadFilename("vm-log", vm?.name ?? vmId)}
                buttonClassName="btn console-bar-btn"
              />
            </div>
            <pre className="detail-log" ref={logRef}>
              {logText}
            </pre>
            {vm && vm.state === "running" && (
              <section className="panel">
                <h2 className="panel-title">{t("Packages", "패키지")}</h2>
                {packageError && <div className="field-error">{packageError}</div>}
                <div className="package-row">
                  <input
                    type="text"
                    placeholder={t("Packages to install (space-separated)", "설치할 패키지 (공백으로 구분)")}
                    value={installInput}
                    onChange={(event) => setInstallInput(event.target.value)}
                    disabled={packageBusy !== null}
                  />
                  <button
                    type="button"
                    className="btn"
                    disabled={packageBusy !== null || !installInput.trim()}
                    onClick={() => void runPackages("install", installInput)}
                  >
                    {packageBusy === "install" ? t("Installing…", "설치 중…") : t("Install", "설치")}
                  </button>
                </div>
                <div className="package-row">
                  <input
                    type="text"
                    placeholder={t("Packages to remove (space-separated)", "삭제할 패키지 (공백으로 구분)")}
                    value={removeInput}
                    onChange={(event) => setRemoveInput(event.target.value)}
                    disabled={packageBusy !== null}
                  />
                  <button
                    type="button"
                    className="btn danger"
                    disabled={packageBusy !== null || !removeInput.trim()}
                    onClick={() => void runPackages("remove", removeInput)}
                  >
                    {packageBusy === "remove" ? t("Removing…", "삭제 중…") : t("Remove", "삭제")}
                  </button>
                </div>
                <div className="package-row">
                  <button
                    type="button"
                    className="btn"
                    disabled={packageBusy !== null}
                    onClick={() => void runPackages("update")}
                  >
                    {packageBusy === "update" ? t("Updating…", "업데이트 중…") : t("Update all packages", "전체 패키지 업데이트")}
                  </button>
                </div>
                {vm.packageUpdate && vm.packageUpdate.state !== "running" && (
                  <pre className="detail-log">
                    {vm.packageUpdate.state === "succeeded"
                      ? vm.packageUpdate.outputTail
                      : `${vm.packageUpdate.reason}\n${vm.packageUpdate.outputTail}`}
                  </pre>
                )}
              </section>
            )}
          </div>
        ) : (
          <div className="empty">{t("Loading…", "불러오는 중…")}</div>
        )}
      </div>
    </div>
  );
}

/** Formats a span the way a build log does: `820ms`, `3s`, `1m 32s`. */
function duration(millis: number): string {
  if (millis < 1000) return `${millis}ms`;
  const seconds = Math.round(millis / 1000);
  if (seconds < 60) return `${seconds}s`;
  return `${Math.floor(seconds / 60)}m ${seconds % 60}s`;
}

/** Wall-clock `HH:MM:SS.mmm`, to line up with the console log below. */
function clockTime(epochMillis: number): string {
  const at = new Date(epochMillis);
  const pad = (value: number, width = 2) => String(value).padStart(width, "0");
  return (
    `${pad(at.getHours())}:${pad(at.getMinutes())}:${pad(at.getSeconds())}` +
    `.${pad(at.getMilliseconds(), 3)}`
  );
}

/**
 * The start pipeline as a row of timed steps, like a CI build log
 * (`docs/30-tasks/task-vm-startup-timeline.md`). Durations come from the
 * server's own timestamps — polling is far too coarse to time a 2-second
 * disk copy — and only the still-running step ticks locally.
 */
function PipelineStepper({
  currentIndex,
  timeline,
}: {
  currentIndex: number;
  timeline: StartupStepRun[];
}) {
  // Re-renders once a second so the open step's elapsed time keeps moving
  // between polls. Stops as soon as nothing is open.
  const [now, setNow] = useState(() => Date.now());
  const hasOpenStep = timeline.some((run) => run.outcome === "running");
  useEffect(() => {
    if (!hasOpenStep) return;
    const tick = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(tick);
  }, [hasOpenStep]);

  const runFor = (step: StartupStep) => timeline.find((run) => run.step === step);

  return (
    <ol className="pipeline">
      {STARTUP_STEPS.map((step, index) => {
        const run = runFor(step);
        const status = run
          ? run.outcome
          : index < currentIndex
            ? "succeeded"
            : index === currentIndex
              ? "running"
              : "pending";
        const elapsed = run
          ? (run.endedAtMs ?? now) - run.startedAtMs
          : null;

        return (
          <li key={step} className={`pipeline-step ${status}`}>
            <span className="step-label">{STARTUP_STEP_LABEL[step]}</span>
            <span className="step-bar">
              <span className="step-time">{elapsed === null ? "—" : duration(elapsed)}</span>
              <span className="step-mark">
                {status === "succeeded" ? "✓" : status === "failed" ? "✕" : ""}
              </span>
            </span>
            <span className="step-started">{run ? clockTime(run.startedAtMs) : ""}</span>
            {run?.detail && <span className="step-detail">{startupFailureDetail(run.detail)}</span>}
          </li>
        );
      })}
    </ol>
  );
}

/** Gives the operator a safe, action-oriented explanation for an nft IP-map
 * collision instead of exposing the raw ruleset syntax in the VM timeline. */
function startupFailureDetail(detail: string): string {
  const conflict = detail.match(
    /(?:IPv4 lease|vm_egress\s*\{)\s*([0-9]+(?:\.[0-9]+){3})(?:\s+conflicts|\s*:)/,
  );
  if (conflict && detail.includes("File exists")) {
    return `IP ${conflict[1]}은 기존 MicroVM이 사용 중입니다. 기존 VM을 중지하거나 복구한 뒤 다시 시도하세요.`;
  }
  if (detail.includes("conflicts with an existing host firewall policy")) {
    return "기존 MicroVM의 호스트 네트워크 정책과 충돌합니다. 기존 VM을 중지하거나 복구한 뒤 다시 시도하세요.";
  }
  return detail;
}

function timestamp(): string {
  // ISO 8601 with a 9-digit fractional suffix (matches the console log's
  // timestamp shape); the browser only has millisecond precision, so the
  // trailing 6 digits are zero-padded rather than fabricated.
  return `${new Date().toISOString().slice(0, -1)}000000`;
}
