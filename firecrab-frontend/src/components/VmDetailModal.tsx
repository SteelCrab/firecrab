import { useEffect, useRef, useState } from "react";
import type {
  EgressPolicy,
  ImageResponse,
  MicroNetworkResponse,
  PortForward,
  PortProtocol,
  ShellResponse,
  StartupStep,
  StartupStepRun,
  StorageRootResponse,
  VmResponse,
} from "../bindings";
import {
  ApiClientError,
  assignVmStorage,
  downloadSshKey,
  fetchSshKeyPem,
  getVm,
  getVmLog,
  listImages,
  listMicroNetworks,
  listShells,
  listStorageRoots,
  updateVmPortForwards,
  updateVmResources,
  updateVmShells,
} from "../api/client";
import { isEditableState, isEnvEditableState, isPortEditableState } from "../model";
import { isValidPort } from "../lib/portForward";
import { copyText, logDownloadFilename } from "../lib/textExport";
import ConsoleSshTab from "./ConsoleSshTab";
import LogExportActions from "./LogExportActions";
import RamStepper from "./RamStepper";
import ShellCheckboxList from "./ShellCheckboxList";
import UsageCharts from "./UsageCharts";
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
// backend log field. See public-docs/dashboard.md for why.
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
  const [editShellIds, setEditShellIds] = useState<string[]>([]);
  const [editPortForwards, setEditPortForwards] = useState<PortForward[]>([]);
  const [editEnvRows, setEditEnvRows] = useState<{ key: string; value: string }[]>([]);
  const [catalogShells, setCatalogShells] = useState<ShellResponse[]>([]);
  const [images, setImages] = useState<ImageResponse[]>([]);
  const [saving, setSaving] = useState(false);
  const [saveError, setSaveError] = useState<ApiClientError | null>(null);
  const [sshKeyBusy, setSshKeyBusy] = useState(false);
  /** SSH commands are long; they stay folded until asked for. */
  const [sshOpen, setSshOpen] = useState(false);
  const [sshKeyCopied, setSshKeyCopied] = useState(false);
  const [sshKeyError, setSshKeyError] = useState<string | null>(null);
  const [microNetworks, setMicroNetworks] = useState<MicroNetworkResponse[]>([]);
  const [storageRoots, setStorageRoots] = useState<StorageRootResponse[]>([]);

  // Only to resolve the VM's microNetworkId into a readable name/subnet; a
  // failed load just falls back to showing the raw id.
  useEffect(() => {
    listMicroNetworks().then(setMicroNetworks).catch(() => setMicroNetworks([]));
    listStorageRoots().then(setStorageRoots).catch(() => setStorageRoots([]));
    listShells().then(setCatalogShells).catch(() => setCatalogShells([]));
    listImages().then(setImages).catch(() => setImages([]));
  }, []);

  const addEditPortForward = () => {
    setEditPortForwards((current) => [...current, { hostPort: 8080, guestPort: 80, protocol: "tcp" }]);
  };

  const removeEditPortForward = (index: number) => {
    setEditPortForwards((current) => current.filter((_, i) => i !== index));
  };

  const updateEditPortForward = (index: number, field: keyof PortForward, value: any) => {
    setEditPortForwards((current) => {
      const next = [...current];
      next[index] = { ...next[index], [field]: value };
      return next;
    });
  };

  const addEditEnvRow = () => {
    setEditEnvRows((current) => [...current, { key: "", value: "" }]);
  };

  const removeEditEnvRow = (index: number) => {
    setEditEnvRows((current) => current.filter((_, i) => i !== index));
  };

  const updateEditEnvRow = (
    index: number,
    field: "key" | "value",
    value: string,
  ) => {
    setEditEnvRows((current) => {
      const next = [...current];
      next[index] = { ...next[index], [field]: value };
      return next;
    });
  };

  const startEditing = () => {
    if (!vm) return;
    setEditCpu(String(vm.cpu));
    setEditRam(String(vm.ram));
    setEditDisk(String(vm.diskGb));
    setEditEgressPolicy(vm.egressPolicy);
    setEditStorageRoot(vm.storageRoot || "default");
    setEditShellIds((vm.shellRefs ?? []).map((ref) => ref.shellId));
    setEditPortForwards((vm.portForwards ?? []).map((pf) => ({ ...pf })));
    setEditEnvRows(
      Object.entries(vm.env ?? {}).map(([key, value]) => ({ key, value })),
    );
    setSaveError(null);
    setEditing(true);
    // Refresh catalog so latest versions show on the checkboxes.
    listShells().then(setCatalogShells).catch(() => setCatalogShells([]));
  };

  const cancelEditing = () => {
    setEditing(false);
    setSaveError(null);
  };

  const handleSave = async () => {
    if (!vm) return;
    // Incomplete rows must block the save, not be silently dropped — the
    // row stays in the editor so the user can fix or explicitly remove it.
    const incompleteIndex = editPortForwards.findIndex(
      (pf) => !isValidPort(pf.hostPort) || !isValidPort(pf.guestPort),
    );
    if (incompleteIndex !== -1) {
      setSaveError(
        ApiClientError.api(400, {
          code: "validation",
          message: "Every port forward needs a host and guest port (1-65535).",
          fields: { portForwards: "host and guest port must be 1-65535" },
          requestId: "",
        }),
      );
      return;
    }
    const env: { [key: string]: string } = {};
    for (const row of editEnvRows) {
      const key = row.key.trim();
      if (!key) {
        setSaveError(
          ApiClientError.api(400, {
            code: "validation",
            message: "Every environment variable needs a key.",
            fields: { env: "key must not be empty" },
            requestId: "",
          }),
        );
        return;
      }
      if (key in env) {
        setSaveError(
          ApiClientError.api(400, {
            code: "validation",
            message: "Environment keys must be unique.",
            fields: { env: "duplicate key" },
            requestId: "",
          }),
        );
        return;
      }
      env[key] = row.value;
    }
    setSaving(true);
    setSaveError(null);
    try {
      let updated = await updateVmResources(vm.id, {
        cpu: parseInt(editCpu, 10) || 0,
        ram: parseInt(editRam, 10) || 0,
        diskGb: parseInt(editDisk, 10) || 0,
        egressPolicy: editEgressPolicy,
        env,
      });
      // Shells and storage cannot change while Firecracker is live.
      if (isEditableState(vm.state)) {
        if (editStorageRoot && editStorageRoot !== vm.storageRoot) {
          updated = await assignVmStorage(vm.id, { storageRoot: editStorageRoot });
        }
        updated = await updateVmShells(vm.id, { shellIds: editShellIds });
      }
      // Port forwards re-apply nft while running; do not fold this into the
      // stopped-only block above or a live add is persisted nowhere.
      if (isPortEditableState(vm.state)) {
        updated = await updateVmPortForwards(vm.id, { portForwards: editPortForwards });
      }

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

  const vmImage = images.find((image) => image.alias === vm?.template);
  const envIgnored = Boolean(vmImage && !vmImage.hasGuestService);
  const envEntries = Object.entries(vm?.env ?? {});

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
                {editing && isEditableState(vm.state) ? (
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
                {editing && isEditableState(vm.state) ? (
                  <RamStepper id="vm-edit-ram" value={editRam} onChange={setEditRam} />
                ) : (
                  `${vm.ram} MiB`
                )}
              </dd>
              <dt>disk</dt>
              <dd>
                {editing && isEditableState(vm.state) ? (
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
                {editing && isEditableState(vm.state) && storageRoots.length > 0 ? (
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
                {editing && isEditableState(vm.state) ? (
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
              <dt>{t("Shells", "Shell")}</dt>
              <dd>
                {editing && isEditableState(vm.state) ? (
                  <div className="detail-shell-check">
                    <ShellCheckboxList
                      shells={catalogShells}
                      selectedIds={editShellIds}
                      onChange={setEditShellIds}
                      disabled={saving}
                      idPrefix="vm-detail-shell"
                      emptyLabel={t(
                        "No shells in catalog — create under Shells first.",
                        "등록된 Shell이 없습니다. 먼저 Shell 메뉴에서 만드세요.",
                      )}
                    />
                    {saveError?.fieldError("shellIds") && (
                      <span className="field-error">{saveError.fieldError("shellIds")}</span>
                    )}
                  </div>
                ) : (vm.shellRefs ?? []).length === 0 ? (
                  "—"
                ) : (
                  (vm.shellRefs ?? [])
                    .map((ref) => `${ref.name}@v${ref.version}`)
                    .join(", ")
                )}
              </dd>
              <dt>ports</dt>
              <dd>
                {editing ? (
                  <div style={{ display: "flex", flexDirection: "column", gap: "0.5rem" }}>
                    {editPortForwards.map((pf, idx) => (
                      <div key={idx} style={{ display: "flex", gap: "0.5rem", alignItems: "center" }}>
                        <input
                          className="detail-edit-input"
                          type="number"
                          placeholder="80"
                          value={pf.guestPort || ""}
                          onChange={(e) => updateEditPortForward(idx, "guestPort", Number(e.target.value))}
                          style={{
                            width: "5rem",
                            borderColor: isValidPort(pf.guestPort) ? undefined : "var(--danger, #e5484d)",
                          }}
                        />
                        <span style={{ color: "var(--shell)" }}>:</span>
                        <input
                          className="detail-edit-input"
                          type="number"
                          placeholder="8080"
                          value={pf.hostPort || ""}
                          onChange={(e) => updateEditPortForward(idx, "hostPort", Number(e.target.value))}
                          style={{
                            width: "5rem",
                            borderColor: isValidPort(pf.hostPort) ? undefined : "var(--danger, #e5484d)",
                          }}
                        />
                        <select
                          className="detail-edit-input"
                          value={pf.protocol}
                          onChange={(e) => updateEditPortForward(idx, "protocol", e.target.value as PortProtocol)}
                          style={{ width: "4.5rem" }}
                        >
                          <option value="tcp">tcp</option>
                          <option value="udp">udp</option>
                        </select>
                        <button
                          type="button"
                          className="btn small danger"
                          onClick={() => removeEditPortForward(idx)}
                          style={{ padding: "0.2rem 0.5rem", fontSize: "0.75rem" }}
                        >
                          ✕
                        </button>
                      </div>
                    ))}
                    <button
                      type="button"
                      className="btn small secondary"
                      onClick={addEditPortForward}
                      style={{ width: "fit-content", marginTop: "0.2rem" }}
                    >
                      + {t("Add Rule", "규칙 추가")}
                    </button>
                    {saveError?.fieldError("portForwards") && (
                      <span className="field-error">{saveError.fieldError("portForwards")}</span>
                    )}
                    {vm.state === "running" && (
                      <div style={{ fontSize: "0.75rem", color: "var(--muted, #888)", marginTop: "0.2rem" }}>
                        {t(
                          "Saving applies host NAT immediately.",
                          "저장하면 호스트 NAT가 바로 적용됩니다.",
                        )}
                      </div>
                    )}
                  </div>
                ) : (vm.portForwards ?? []).length === 0 ? (
                  "—"
                ) : (
                  (vm.portForwards ?? [])
                    .map((pf) => `${pf.guestPort}:${pf.hostPort}/${pf.protocol}`)
                    .join(", ")
                )}
              </dd>
              <dt>env</dt>
              <dd>
                {editing ? (
                  <div style={{ display: "flex", flexDirection: "column", gap: "0.5rem" }}>
                    {editEnvRows.map((row, idx) => (
                      <div key={idx} style={{ display: "flex", gap: "0.5rem", alignItems: "center" }}>
                        <input
                          id={`vm-edit-env-key-${idx}`}
                          className="detail-edit-input"
                          type="text"
                          placeholder="APP_NAME"
                          value={row.key}
                          maxLength={256}
                          autoComplete="off"
                          spellCheck={false}
                          onChange={(event) => updateEditEnvRow(idx, "key", event.target.value)}
                          style={{ width: "8rem" }}
                        />
                        <span style={{ color: "var(--shell)" }}>=</span>
                        <input
                          id={`vm-edit-env-value-${idx}`}
                          className="detail-edit-input"
                          type="text"
                          placeholder="web"
                          value={row.value}
                          maxLength={4096}
                          autoComplete="off"
                          spellCheck={false}
                          onChange={(event) => updateEditEnvRow(idx, "value", event.target.value)}
                          style={{ width: "12rem" }}
                        />
                        <button
                          type="button"
                          className="btn small danger"
                          onClick={() => removeEditEnvRow(idx)}
                          style={{ padding: "0.2rem 0.5rem", fontSize: "0.75rem" }}
                        >
                          ✕
                        </button>
                      </div>
                    ))}
                    <button
                      type="button"
                      id="vm-edit-env-add"
                      className="btn small secondary"
                      onClick={addEditEnvRow}
                      disabled={editEnvRows.length >= 64}
                      style={{ width: "fit-content", marginTop: "0.2rem" }}
                    >
                      + {t("Add variable", "변수 추가")}
                    </button>
                    {saveError?.fieldError("env") && (
                      <span className="field-error">{saveError.fieldError("env")}</span>
                    )}
                  </div>
                ) : envEntries.length === 0 ? (
                  "—"
                ) : (
                  envEntries.map(([key, value]) => `${key}=${value}`).join(", ")
                )}
                {envIgnored && (
                  <div style={{ fontSize: "0.75rem", color: "var(--muted, #888)", marginTop: "0.35rem" }}>
                    {t(
                      "Runtime env is ignored: this image has no guest service.",
                      "게스트 서비스가 없는 이미지라 런타임 환경 변수는 적용되지 않습니다.",
                    )}
                  </div>
                )}
                {vm?.state === "running" && (
                  <div style={{ fontSize: "0.75rem", color: "var(--muted, #888)", marginTop: "0.35rem" }}>
                    {t(
                      "Saving restarts the guest service so the new env takes effect.",
                      "저장하면 게스트 서비스를 재시작해 새 환경 변수를 적용합니다.",
                    )}
                  </div>
                )}
              </dd>
              <dt>ip</dt>
              <dd>{vm.ipv4 ?? "—"}</dd>
              <dt>ipv6</dt>
              <dd>{vm.ipv6 ?? "—"}</dd>
              <dt>ssh</dt>
              <dd>
                <button
                  type="button"
                  className="btn small secondary"
                  disabled={sshKeyBusy}
                  onClick={() => {
                    setSshKeyError(null);
                    setSshKeyBusy(true);
                    downloadSshKey(vm.id, vm.name)
                      .catch((error: unknown) => {
                        setSshKeyError(error instanceof Error ? error.message : String(error));
                      })
                      .finally(() => setSshKeyBusy(false));
                  }}
                >
                  {sshKeyBusy
                    ? t("Downloading…", "받는 중…")
                    : t("Download key", "키 다운로드")}
                </button>
                <button
                  type="button"
                  className="btn small secondary"
                  disabled={sshKeyBusy}
                  title={t("Copy the private key text", "개인 키 본문을 클립보드로 복사")}
                  onClick={() => {
                    setSshKeyError(null);
                    setSshKeyBusy(true);
                    fetchSshKeyPem(vm.id)
                      .then(copyText)
                      .then((copied) => {
                        setSshKeyCopied(copied);
                        setTimeout(() => setSshKeyCopied(false), 2_000);
                      })
                      .catch((error: unknown) => {
                        setSshKeyError(error instanceof Error ? error.message : String(error));
                      })
                      .finally(() => setSshKeyBusy(false));
                  }}
                >
                  {sshKeyCopied ? t("Key copied", "키 복사됨") : t("Copy key", "키 복사")}
                </button>
                <button
                  type="button"
                  className="btn small secondary"
                  aria-expanded={sshOpen}
                  onClick={() => setSshOpen((open) => !open)}
                >
                  {sshOpen ? t("Hide SSH", "SSH 접기") : t("SSH", "SSH")}
                </button>
                {vm.sshHostFingerprint ? (
                  <div className="mono" style={{ marginTop: "0.35rem", fontSize: "0.75rem" }}>
                    {vm.sshHostFingerprint}
                  </div>
                ) : (
                  <div style={{ fontSize: "0.75rem", color: "var(--muted, #888)", marginTop: "0.35rem" }}>
                    {t("Host fingerprint after first start", "호스트 지문은 첫 시작 후")}
                  </div>
                )}
                {sshKeyError ? <div className="field-error">{sshKeyError}</div> : null}
              </dd>
              <dt>mac</dt>
              <dd>{vm.mac ?? "—"}</dd>
              <dt>hostname</dt>
              <dd>{vm.hostname}</dd>
              <dt>id</dt>
              <dd title={vm.id}>{vm.id}</dd>
            </dl>
            {sshOpen && (
              <section className="panel detail-ssh-panel" aria-label="SSH">
                <ConsoleSshTab vm={vm} />
              </section>
            )}
            {isEnvEditableState(vm.state) && (
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
            {vm.state === "running" && (
              <section className="panel detail-usage-panel" aria-label={t("Resource usage", "리소스 관측")}>
                <h2 className="panel-title">{t("Resource usage", "리소스 관측")}</h2>
                {(vm.usageHistory?.length ?? 0) > 0 ? (
                  <UsageCharts
                    history={vm.usageHistory ?? []}
                    ramMib={vm.ram}
                    size="large"
                  />
                ) : (
                  <p className="usage-charts-empty is-large">
                    {t(
                      "Waiting for Guest Agent… (restart the VM if this persists)",
                      "게스트 에이전트 대기 중… (계속되면 VM을 재시작해 주세요)",
                    )}
                  </p>
                )}
              </section>
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
 * (`public-docs/api.md`). Durations come from the
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
