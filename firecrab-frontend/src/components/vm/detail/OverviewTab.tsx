import { useEffect, useState } from "react";
import { Check, CircleAlert, Loader2, Minus, SquareTerminal, X } from "lucide-react";
import type { StartupStep, StartupStepRun, VmResponse } from "../../../bindings";
import { downloadSshKey, fetchSshKeyPem } from "../../../api/client";
import { useI18n } from "../../../i18n";
import { copyText } from "../../../lib/textExport";
import ConsoleSshTab from "../../ConsoleSshTab";
import RamStepper from "../../RamStepper";
import ReconciliationStatus from "../../ReconciliationStatus";
import ShellCheckboxList from "../../ShellCheckboxList";
import Sparkline from "../../ui/Sparkline";
import { STARTUP_STEPS, clockTime, duration, startupFailure, startupFailureDetail, startupStepLabel } from "../startup";
import { HIGH_USAGE_PERCENT, formatMib, formatPercent, usageSeries, vmSpec } from "../vmFormat";
import { DetailProp, DetailSection } from "../../ui/DetailSection";
import { EditActions, EditButton, EditableProp, FieldError } from "./parts";
import type { SectionEdit } from "./useSectionEdit";
import type { VmDetailData } from "./useVmDetail";

interface OverviewTabProps {
  vm: VmResponse;
  detail: VmDetailData;
  edit: SectionEdit;
  sshOpen: boolean;
  onToggleSsh: () => void;
  onShowLog: () => void;
}

export default function OverviewTab({ vm, detail, edit, sshOpen, onToggleSsh, onShowLog }: OverviewTabProps) {
  const { t } = useI18n();
  const failure = vm.state === "error" ? startupFailure(vm) : null;
  const draft = edit.draft;
  const network = detail.networks.find((entry) => entry.id === vm.microNetworkId);
  const storage = detail.storageRoots.find((root) => root.id === vm.storageRoot);
  const currentIndex = vm.state === "running"
    ? STARTUP_STEPS.length
    : vm.state === "starting" || vm.state === "error"
      ? detail.highestStepSeen
      : -1;

  return (
    <>
      {vm.state === "error" && (
        <div className="alert error vm-failure" role="alert">
          <CircleAlert aria-hidden="true" />
          <div>
            <b>{t("Start failed", "시작 실패")}</b>
            <span>{failure ?? t("The VM stopped with an error.", "VM이 오류로 멈췄습니다.")}</span>
            <button type="button" className="link-button" onClick={onShowLog}>{t("View log", "로그 보기")}</button>
          </div>
        </div>
      )}

      {vm.reconciliation && (
        <section className="detail-sec vm-api-status" aria-label="API-STATUS">
          <div className="detail-sec-head"><h3>API-STATUS</h3></div>
          <ReconciliationStatus result={vm.reconciliation} />
        </section>
      )}

      <DetailSection title={t("Resource usage", "리소스 사용량")}>
        <Usage vm={vm} />
      </DetailSection>

      <DetailSection title={t("Properties", "속성")}>
        <dl className="props">
          <DetailProp label="ID" copy={vm.id}><span className="mono">{vm.id}</span></DetailProp>
          <DetailProp label={t("Image", "이미지")}>
            {vm.template} <span className="muted">· {vm.templateVersion}</span>
          </DetailProp>
          <EditableProp
            vm={vm}
            edit={edit}
            section="spec"
            label={t("Spec", "사양")}
            editLabel={t("Edit spec", "사양 편집")}
            lockedNote={t("Stop to change", "중지 후 변경")}
            editor={draft?.section === "spec" && (
              <div className="prop-fields">
                <div className="field">
                  <label htmlFor="vm-edit-cpu">vCPU</label>
                  <input id="vm-edit-cpu" type="number" min={1} max={32} value={draft.cpu} onChange={(event) => edit.update({ ...draft, cpu: event.target.value })} />
                </div>
                <div className="field">
                  <label htmlFor="vm-edit-ram">RAM (MiB)</label>
                  <RamStepper id="vm-edit-ram" value={draft.ram} onChange={(ram) => edit.update({ ...draft, ram })} />
                </div>
                <div className="field">
                  <label htmlFor="vm-edit-disk">{t("Disk (GiB)", "디스크 (GiB)")}</label>
                  <input id="vm-edit-disk" type="number" min={vm.diskGb} max={500} value={draft.diskGb} onChange={(event) => edit.update({ ...draft, diskGb: event.target.value })} />
                </div>
              </div>
            )}
          >
            {vmSpec(vm)}
          </EditableProp>
          <DetailProp label="MicroNetwork">
            {!vm.microNetworkId ? t("Default network", "기본 네트워크") : network ? `${network.name} (${network.subnetCidr})` : vm.microNetworkId}
          </DetailProp>
          <EditableProp
            vm={vm}
            edit={edit}
            section="storage"
            label="MicroStorage"
            editLabel={t("Edit storage", "스토리지 편집")}
            editor={draft?.section === "storage" && (
              <div className="field">
                <label htmlFor="vm-edit-storage" className="sr-only">MicroStorage</label>
                <select id="vm-edit-storage" value={draft.storageRoot} onChange={(event) => edit.update({ ...draft, storageRoot: event.target.value })}>
                  {detail.storageRoots.map((root) => (
                    <option key={root.id} value={root.id}>
                      {root.name} ({root.path}){root.availableGib > 0 ? ` · ${root.availableGib} GiB free` : ""}
                    </option>
                  ))}
                </select>
              </div>
            )}
          >
            {storage ? `${storage.name} (${storage.path})` : (vm.storageRoot || "default")}
          </EditableProp>
          <DetailProp label={t("Egress", "외부 통신")}>
            {vm.egressPolicy === "internet" ? t("Internet access", "인터넷 허용") : t("Isolated (gateway only)", "격리(게이트웨이만 허용)")}
          </DetailProp>
          <DetailProp label={t("Hostname", "호스트명")}><span className="mono">{vm.hostname}</span></DetailProp>
          <DetailProp label="MAC"><span className="mono">{vm.mac ?? "—"}</span></DetailProp>
        </dl>
      </DetailSection>

      <DetailSection title="Shell" aside={<EditButton vm={vm} section="shells" edit={edit} label={t("Edit shells", "Shell 편집")} />}>
        {draft?.section === "shells" ? (
          <>
            <ShellCheckboxList
              shells={detail.shells}
              selectedIds={draft.shellIds}
              onChange={(shellIds) => edit.update({ ...draft, shellIds })}
              disabled={edit.saving}
              idPrefix="vm-detail-shell"
              emptyLabel={t("No shells in catalog — create under Shells first.", "등록된 Shell이 없습니다. 먼저 Shell 메뉴에서 만드세요.")}
            />
            <FieldError edit={edit} field="shellIds" />
            <EditActions edit={edit} />
          </>
        ) : (vm.shellRefs ?? []).length === 0 ? (
          <p className="detail-note">{t("No shells pinned.", "고정된 Shell이 없습니다.")}</p>
        ) : (
          <div className="chips">
            {(vm.shellRefs ?? []).map((ref) => (
              <span key={ref.shellId} className="chip"><SquareTerminal aria-hidden="true" />{ref.name}<span className="muted">v{ref.version}</span></span>
            ))}
          </div>
        )}
      </DetailSection>

      <DetailSection title={t("Startup", "시작 과정")}>
        <Timeline currentIndex={currentIndex} timeline={vm.startupTimeline} />
      </DetailSection>

      <Ssh vm={vm} open={sshOpen} onToggle={onToggleSsh} />
    </>
  );
}

function Usage({ vm }: { vm: VmResponse }) {
  const { t } = useI18n();
  if (vm.state !== "running") {
    return <p className="detail-note">{t("CPU and memory use show while the VM runs.", "VM이 실행 중일 때 CPU와 메모리 사용량이 보입니다.")}</p>;
  }
  const cpu = usageSeries(vm.usageHistory, (sample) => sample.cpuUsagePercent);
  const memory = usageSeries(vm.usageHistory, (sample) => sample.memoryUsedPercent);
  if (cpu.length === 0 && vm.cpuUsagePercent == null) {
    return <p className="detail-note">{t("Waiting for Guest Agent… (restart the VM if this persists)", "게스트 에이전트 대기 중… (계속되면 VM을 재시작해 주세요)")}</p>;
  }
  const cpuHot = (vm.cpuUsagePercent ?? 0) >= HIGH_USAGE_PERCENT;
  const memoryHot = (vm.memoryUsedPercent ?? 0) >= HIGH_USAGE_PERCENT;
  return (
    <div className="vm-usage-cards">
      <div className="vm-usage-card">
        <div className="vm-usage-card-label">CPU</div>
        <div className="vm-usage-card-value">{vm.cpuUsagePercent == null ? "—" : formatPercent(vm.cpuUsagePercent)}</div>
        <Sparkline values={cpu} width={160} height={36} area tone={cpuHot ? "warn" : "accent"} />
      </div>
      <div className="vm-usage-card">
        <div className="vm-usage-card-label">{t("Memory", "메모리")}</div>
        <div className="vm-usage-card-value">
          {vm.memoryUsedMib == null ? "—" : formatMib(vm.memoryUsedMib)}
          {vm.memoryUsedMib != null && <small> / {formatMib(vm.memoryTotalMib ?? vm.ram)}</small>}
        </div>
        <Sparkline values={memory} width={160} height={36} area tone={memoryHot ? "warn" : "neutral"} />
      </div>
    </div>
  );
}

/**
 * The start pipeline as timed steps, like a CI build log. Durations come from
 * the server's timestamps — polling is far too coarse to time a 2-second disk
 * copy — and only the still-running step ticks locally.
 */
function Timeline({ currentIndex, timeline }: { currentIndex: number; timeline: StartupStepRun[] }) {
  const { t } = useI18n();
  const [now, setNow] = useState(() => Date.now());
  const hasOpenStep = timeline.some((run) => run.outcome === "running");
  useEffect(() => {
    if (!hasOpenStep) return;
    const tick = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(tick);
  }, [hasOpenStep]);

  const runFor = (step: StartupStep) => timeline.find((run) => run.step === step);

  return (
    <ol className="vm-timeline">
      {STARTUP_STEPS.map((step, index) => {
        const run = runFor(step);
        const status = run ? run.outcome : index < currentIndex ? "succeeded" : index === currentIndex ? "running" : "pending";
        const elapsed = run ? (run.endedAtMs ?? now) - run.startedAtMs : null;
        const Icon = status === "succeeded" ? Check : status === "failed" ? X : status === "running" ? Loader2 : Minus;
        return (
          <li key={step} className={`vm-timeline-step ${status}`}>
            <span className="vm-timeline-dot"><Icon aria-hidden="true" /></span>
            <span className="vm-timeline-label">{startupStepLabel(step, t)}</span>
            <span className="vm-timeline-time" title={run ? clockTime(run.startedAtMs) : undefined}>{elapsed === null ? "—" : duration(elapsed)}</span>
            {run?.detail && <span className="vm-timeline-detail">{startupFailureDetail(run.detail)}</span>}
          </li>
        );
      })}
    </ol>
  );
}

function Ssh({ vm, open, onToggle }: { vm: VmResponse; open: boolean; onToggle: () => void }) {
  const { t } = useI18n();
  const [busy, setBusy] = useState(false);
  const [copied, setCopied] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const fail = (caught: unknown) => setError(caught instanceof Error ? caught.message : String(caught));

  return (
    <DetailSection title="SSH">
      <dl className="props">
        <DetailProp label={t("Host key", "호스트 키")} copy={vm.sshHostFingerprint ?? undefined}>
          {vm.sshHostFingerprint
            ? <span className="mono">{vm.sshHostFingerprint}</span>
            : <span className="muted">{t("Host fingerprint after first start", "호스트 지문은 첫 시작 후")}</span>}
        </DetailProp>
      </dl>
      <div className="detail-sec-buttons">
        <button
          type="button"
          className="btn sm"
          disabled={busy}
          onClick={() => {
            setError(null);
            setBusy(true);
            downloadSshKey(vm.id, vm.name).catch(fail).finally(() => setBusy(false));
          }}
        >
          {busy ? t("Downloading…", "받는 중…") : t("Download key", "키 다운로드")}
        </button>
        <button
          type="button"
          className="btn sm"
          disabled={busy}
          title={t("Copy the private key text", "개인 키 본문을 클립보드로 복사")}
          onClick={() => {
            setError(null);
            setBusy(true);
            fetchSshKeyPem(vm.id)
              .then(copyText)
              .then((ok) => {
                setCopied(ok);
                setTimeout(() => setCopied(false), 2_000);
              })
              .catch(fail)
              .finally(() => setBusy(false));
          }}
        >
          {copied ? t("Key copied", "키 복사됨") : t("Copy key", "키 복사")}
        </button>
        <button type="button" className="btn sm ghost" aria-expanded={open} onClick={onToggle}>
          {open ? t("Hide SSH commands", "SSH 명령 접기") : t("SSH commands", "SSH 명령")}
        </button>
      </div>
      {error && <div className="field-error">{error}</div>}
      {open && <div className="detail-ssh-panel"><ConsoleSshTab vm={vm} /></div>}
    </DetailSection>
  );
}
