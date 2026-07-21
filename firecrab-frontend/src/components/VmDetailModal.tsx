import { useEffect, useRef, useState } from "react";
import type { StartupStep, VmResponse } from "../bindings";
import { ApiClientError, getVm, getVmLog, updateVmResources } from "../api/client";
import { isEditableState } from "../model";
import RamStepper from "./RamStepper";

const STARTUP_STEPS: StartupStep[] = ["preparingDisk", "generatingConfig", "startingProcess"];

const STARTUP_STEP_LABEL: Record<StartupStep, string> = {
  preparingDisk: "디스크 준비",
  generatingConfig: "설정 생성",
  startingProcess: "프로세스 시작",
};

// Derived client-side from the polled `startupStep` value — no dedicated
// backend log field. See docs/tests/vm-detail-modal.md for why.
const STARTUP_STEP_LOG_LINE: Record<StartupStep, string> = {
  preparingDisk: "디스크 준비 중 (rootfs 템플릿 복사)...",
  generatingConfig: "디스크 준비 완료 → Firecracker 설정 생성 중...",
  startingProcess: "설정 생성 완료 → Firecracker 프로세스 시작 중...",
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
  const [saving, setSaving] = useState(false);
  const [saveError, setSaveError] = useState<ApiClientError | null>(null);

  const startEditing = () => {
    if (!vm) return;
    setEditCpu(String(vm.cpu));
    setEditRam(String(vm.ram));
    setEditDisk(String(vm.diskGb));
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
      const updated = await updateVmResources(vm.id, {
        cpu: parseInt(editCpu, 10) || 0,
        ram: parseInt(editRam, 10) || 0,
        diskGb: parseInt(editDisk, 10) || 0,
      });
      setVm(updated);
      setEditing(false);
    } catch (error) {
      setSaveError(error as ApiClientError);
    } finally {
      setSaving(false);
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
          lines = [...lines, `[${timestamp()}] 준비 완료 — VM이 시작되었습니다.`];
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

  const logText = [...pipelineLines, consoleLog].filter(Boolean).join("\n") || "아직 출력이 없습니다.";

  return (
    <div className="console-overlay">
      <div className="console-panel">
        <div className="console-bar">
          <span className="console-title">{`VM 상세 — ${vm?.name ?? vmId}`}</span>
          {vm && <span className={`state-badge ${vm.state}`}>{vm.state}</span>}
          <button className="btn console-close" onClick={onClose}>
            ✕
          </button>
        </div>
        {vm ? (
          <div className="detail-body">
            <dl className="detail-fields mono">
              <dt>template</dt>
              <dd>{vm.templateVersion}</dd>
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
              <dt>id</dt>
              <dd title={vm.id}>{vm.id}</dd>
            </dl>
            {isEditableState(vm.state) && (
              <div className="detail-edit-actions">
                {editing ? (
                  <>
                    <button className="btn primary" onClick={handleSave} disabled={saving}>
                      {saving ? "저장 중…" : "저장"}
                    </button>
                    <button className="btn" onClick={cancelEditing} disabled={saving}>
                      취소
                    </button>
                    {saveError && <span className="field-error">{saveError.message}</span>}
                  </>
                ) : (
                  <button className="btn" onClick={startEditing}>
                    수정
                  </button>
                )}
              </div>
            )}
            <PipelineStepper currentIndex={currentIndex} />
            <pre className="detail-log" ref={logRef}>
              {logText}
            </pre>
          </div>
        ) : (
          <div className="empty">불러오는 중…</div>
        )}
      </div>
    </div>
  );
}

function PipelineStepper({ currentIndex }: { currentIndex: number }) {
  return (
    <ol className="pipeline-stepper">
      {STARTUP_STEPS.map((step, index) => (
        <li key={step} className={index < currentIndex ? "done" : index === currentIndex ? "active" : "pending"}>
          <span className="step-box">{index < currentIndex ? "✓" : index + 1}</span>
          <span className="step-label">{STARTUP_STEP_LABEL[step]}</span>
        </li>
      ))}
    </ol>
  );
}

function timestamp(): string {
  // ISO 8601 with a 9-digit fractional suffix (matches the console log's
  // timestamp shape); the browser only has millisecond precision, so the
  // trailing 6 digits are zero-padded rather than fabricated.
  return `${new Date().toISOString().slice(0, -1)}000000`;
}
