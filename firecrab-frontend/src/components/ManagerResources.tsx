import { useEffect, useRef, useState } from "react";
import type { ManagerNetworkMode, ManagerResources as Allocation, ManagerResourceStatus } from "../bindings";
import { applyManagerResources, getManagerResources, managerEndpoint, ManagerRequestError, saveManagerResources } from "../api/manager";
import { useI18n } from "../i18n";

export default function ManagerResources({ controllerId }: { controllerId?: string }) {
  const { t } = useI18n();
  const [status, setStatus] = useState<ManagerResourceStatus | null>(null);
  const [draft, setDraft] = useState<Allocation | null>(null);
  const [error, setError] = useState("");
  const [connectionError, setConnectionError] = useState("");
  const [message, setMessage] = useState("");
  const [saving, setSaving] = useState(false);
  const [applying, setApplying] = useState(false);
  const operationId = useRef<string | null>(null);
  const applyRequest = useRef<{ previousId: string | null; startedAt: number } | null>(null);

  useEffect(() => {
    if (!managerEndpoint()) return;
    let cancelled = false;
    let inFlight = false;
    const expire = () => {
      if (!applyRequest.current || Date.now() - applyRequest.current.startedAt < 600000) return false;
      applyRequest.current = null; operationId.current = null; setApplying(false);
      setError(t("Apply could not be verified. Inspect firecrab service debug --logs before retrying.", "적용 결과를 확인하지 못했다. 다시 적용하기 전에 firecrab service debug --logs로 상태를 확인한다."));
      return true;
    };
    const tick = async () => {
      if (inFlight) return;
      inFlight = true;
      try {
        const next = await getManagerResources();
        if (cancelled) return;
        if (!controllerId || next.controllerId !== controllerId) {
          setConnectionError(t("This controller belongs to a different host, or the guest API needs an update. Update the native service and guest API, then open this host's localhost dashboard.", "제어 서버의 호스트가 다르거나 게스트 API가 이전 버전이다. 호스트 서비스와 게스트 API를 업데이트한 뒤 해당 호스트의 localhost 화면을 연다."));
          setStatus(null); setDraft(null);
          return;
        }
        setStatus(next);
        setDraft(current => current ?? next.settings);
        setConnectionError("");
        if (applyRequest.current && next.operation && next.operation.id !== applyRequest.current.previousId) operationId.current = next.operation.id;
        if (next.operation?.state === "applying") {
          setApplying(true);
          operationId.current = next.operation.id;
        }
        if (operationId.current && next.operation?.id === operationId.current && next.operation.state !== "applying") {
          setApplying(false);
          operationId.current = null;
          applyRequest.current = null;
          if (next.operation.state === "failed") setError(next.operation.error ?? t("Apply failed", "적용 실패"));
          else setMessage(t("Resources applied. The management VM is ready.", "자원 설정을 적용했다. 관리 VM이 준비되었다."));
        }
        expire();
      } catch (cause) {
        if (!cancelled && !expire() && !operationId.current && !applyRequest.current) setConnectionError(cause instanceof Error ? cause.message : String(cause));
      } finally { inFlight = false; }
    };
    void tick();
    const interval = setInterval(() => void tick(), 3000);
    return () => { cancelled = true; clearInterval(interval); };
  }, [t, controllerId]);

  async function save() {
    if (!draft || !status || status.controllerId !== controllerId) return;
    setSaving(true); setError(""); setMessage("");
    try {
      const next = await saveManagerResources(draft, status.controllerId);
      setStatus(next); setDraft(next.settings);
      setMessage(t("Saved. Apply to restart with these resources.", "설정 저장 완료. 적용하면 새 설정으로 재시작한다."));
    } catch (cause) { setError(cause instanceof Error ? cause.message : String(cause)); }
    finally { setSaving(false); }
  }

  async function apply() {
    if (!status || status.controllerId !== controllerId) return;
    setApplying(true); setError(""); setMessage("");
    applyRequest.current = { previousId: status.operation?.id ?? null, startedAt: Date.now() };
    try {
      const operation = await applyManagerResources(status.controllerId);
      operationId.current = operation.id;
    } catch (cause) {
      // A restart can close the controller before its 202 reaches the browser.
      // The persisted operation ID decides whether that request was accepted.
      if (cause instanceof ManagerRequestError) {
        applyRequest.current = null; setApplying(false);
        setError(cause.message);
      }
    }
  }

  const dirty = draft && status && JSON.stringify(draft) !== JSON.stringify(status.settings);
  const busy = saving || applying;
  function edit<K extends keyof Allocation>(key: K, value: Allocation[K]) {
    setDraft(current => current ? { ...current, [key]: value } : current);
    setMessage("");
  }

  return (
    <div className="manager-resources">
      <div className="manager-resources-heading">
        <h3>{t("microManager allocation", "microManager 자원 설정")}</h3>
        {status && <span className="mono manager-resource-state">{applying ? t("applying", "적용 중") : status.pending ? t("restart pending", "재시작 대기") : t("applied", "적용 완료")}</span>}
      </div>
      {!managerEndpoint() ? <p className="manager-resource-note">{t("Open this dashboard on the host's localhost to change its management VM resources.", "관리 VM 자원을 변경하려면 해당 호스트의 localhost에서 이 화면을 연다.")}</p> : status && draft ? <>
        <p className="manager-resource-note">{t("Physical host", "물리 호스트")}: {status.host.cpu} CPU · {(status.host.memoryMib / 1024).toFixed(1)} GiB RAM · {status.host.diskAvailableGib} GiB {t("disk available", "디스크 여유 공간")}</p>
        {status.current && <p className="manager-resource-note">{t("Current allocation", "현재 할당")}: {status.current.cpu} vCPU · {status.current.memoryMib} MiB RAM · {status.current.diskGib} GiB · {status.current.networkMode}{status.current.networkInterface && ` (${status.current.networkInterface})`}</p>}
        <form className="manager-resource-grid" onSubmit={event => { event.preventDefault(); void save(); }}>
          <div className="field"><label htmlFor="manager-cpu">CPU (vCPU)</label><input id="manager-cpu" type="number" min={1} max={status.host.cpu} required value={draft.cpu} disabled={busy} onChange={event => edit("cpu", Number(event.target.value))} /></div>
          <div className="field"><label htmlFor="manager-ram">RAM (MiB)</label><input id="manager-ram" type="number" min={512} max={status.host.memoryMib} required value={draft.memoryMib} disabled={busy} onChange={event => edit("memoryMib", Number(event.target.value))} /></div>
          <div className="field"><label htmlFor="manager-disk">{t("Data disk", "데이터 디스크")} (GiB)</label><input id="manager-disk" type="number" min={status.current?.diskGib ?? 1} max={65536} required value={draft.diskGib} disabled={busy} onChange={event => edit("diskGib", Number(event.target.value))} /></div>
          <div className="field"><label htmlFor="manager-network">{t("Network", "네트워크")}</label><select id="manager-network" value={draft.networkMode} disabled={busy} onChange={event => setDraft(current => current ? { ...current, networkMode: event.target.value as ManagerNetworkMode, networkInterface: event.target.value === "bridged" ? status.host.networkInterfaces[0] ?? null : null } : current)}><option value="nat">NAT</option>{status.os === "macos" ? <option value="bridged" disabled={!status.host.networkModes.includes("bridged")}>{t("Bridged", "브리지")}</option> : <option value="mirrored">{t("Mirrored", "미러링")}</option>}</select></div>
          {draft.networkMode === "bridged" && <div className="field"><label htmlFor="manager-interface">{t("Physical interface", "물리 인터페이스")}</label><select id="manager-interface" required disabled={busy} value={draft.networkInterface ?? ""} onChange={event => edit("networkInterface", event.target.value)}><option value="">{t("Select interface", "인터페이스 선택")}</option>{status.host.networkInterfaces.map(name => <option key={name} value={name}>{name}</option>)}</select></div>}
          <div className="manager-resource-actions"><button className="btn primary" type="submit" disabled={busy || !dirty}>{saving ? t("Saving…", "저장 중…") : t("Save settings", "설정 저장")}</button><button className="btn danger" type="button" disabled={busy || Boolean(dirty) || !status.pending} onClick={() => void apply()}>{applying ? t("Applying…", "적용 중…") : t("Apply and restart", "적용 및 재시작")}</button></div>
        </form>
        {status.os === "macos" && !status.host.networkModes.includes("bridged") && <p className="manager-resource-note">{t("Bridged mode requires a helper with Apple-approved networking permission. This helper supports NAT.", "브리지 모드는 Apple이 승인한 네트워크 권한이 필요하다. 현재 헬퍼는 NAT를 지원한다.")}</p>}
        <p className="manager-resource-note">{t("Disk growth only. Applying restarts the management VM and stops its running MicroVMs.", "디스크는 확장만 가능하다. 적용하면 관리 VM을 재시작하고 실행 중인 MicroVM을 중지한다.")}</p>
        {status.sharedWsl && <p className="manager-resource-note manager-resource-scope">{t("CPU, RAM, and networking affect every WSL2 distribution. Stop other WSL distributions before applying.", "CPU·RAM·네트워크는 WSL2 전체에 적용된다. 적용 전에 다른 WSL 배포판을 중지한다.")}</p>}
      </> : <p className="manager-resource-note">{t("Connecting to the resource controller…", "자원 제어 서버에 연결 중…")}</p>}
      {error && <div className="field-error" role="alert">{error}</div>}
      {connectionError && <div className="field-error" role="alert">{connectionError}<p>{t("Start the native controller with firecrab service start.", "firecrab service start로 호스트 제어 서버를 시작한다.")}</p></div>}
      {message && <p className="manager-resource-note" role="status">{message}</p>}
      {applying && <p className="manager-resource-note" role="status">{t("Waiting for the management VM to restart. The dashboard reconnects automatically.", "관리 VM 재시작을 기다린다. 대시보드는 자동으로 다시 연결한다.")}</p>}
    </div>
  );
}
