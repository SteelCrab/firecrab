import type { VmReconciliation, VmState } from "../bindings";
import { useI18n } from "../i18n";
import { apiStatus, type ApiStatus } from "../lib/apiStatus";
import StatusLabel from "./StatusLabel";

const LABELS: Record<Exclude<ApiStatus, "unchecked">, [string, string]> = {
  connected: ["Connected", "연결됨"],
  reconnected: ["Reconnected", "재연결 성공"],
  gone: ["VM not found", "VM 없음"],
  mismatched: ["Connection mismatch", "연결 불일치"],
  networkFailed: ["Network recovery failed", "네트워크 복구 실패"],
  interrupted: ["Startup interrupted", "시작 중단"],
  exited: ["Exited while API offline", "API 중단 중 종료"],
};

const DESCRIPTIONS: Record<Exclude<ApiStatus, "unchecked">, [string, string]> = {
  connected: [
    "The running API started this VM and controls it directly, so no startup check was needed.",
    "실행 중인 API가 이 VM을 직접 시작해 제어하고 있어 시작 시 확인이 필요하지 않았습니다.",
  ],
  reconnected: [
    "The API reconnected to the surviving VM. Any pending stop was resumed.",
    "API가 계속 실행 중이던 VM에 다시 연결했습니다. 진행 중이던 종료 작업이 있으면 이어서 처리했습니다.",
  ],
  gone: [
    "No running VM or exit record was found. The VM was marked stopped.",
    "실행 중인 VM이나 종료 기록을 찾지 못해 중지 상태로 변경했습니다.",
  ],
  mismatched: [
    "The API could not control the VM and attempted to stop its service.",
    "API가 VM을 제어할 수 없어 해당 서비스를 중지하려고 시도했습니다.",
  ],
  networkFailed: [
    "The VM was reconnected, but restoring its network configuration failed.",
    "VM에 다시 연결했지만 네트워크 설정 복구에 실패했습니다.",
  ],
  interrupted: [
    "The restart interrupted a VM start. The API attempted to stop the VM and marked it error.",
    "API 재시작으로 VM 시작이 중단되었습니다. VM 종료를 시도하고 오류 상태로 변경했습니다.",
  ],
  exited: [
    "An exit record was found. The VM was marked stopped or error based on its exit status.",
    "종료 기록을 확인해 종료 상태에 따라 중지 또는 오류 상태로 변경했습니다.",
  ],
};

export default function ReconciliationStatus({
  result,
  state,
  details = false,
}: {
  result?: VmReconciliation | null;
  state: VmState;
  details?: boolean;
}) {
  const { t } = useI18n();
  const status = apiStatus(result, state);
  const label = status === "unchecked" ? t("No reconciliation result", "확인 결과 없음") : t(...LABELS[status]);
  const description = status === "unchecked" ? "" : t(...DESCRIPTIONS[status]);
  const checked = result ? new Date(result.checkedAtMs) : null;
  const time = checked?.toLocaleString("sv-SE", {
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
    hourCycle: "h23",
    timeZoneName: "longOffset",
  }).replace("GMT", "UTC") ?? "";
  const heading = t("API-STATUS · Firecrab API · VM reconciliation", "API-STATUS · Firecrab API · VM 재조정");
  const badge = (
    <span
      className={`reconciliation-badge ${status}`}
      aria-label={`${heading}: ${label}`}
    >
      {label}
    </span>
  );

  const content = (
    <div className="reconciliation-detail">
      <dl className="api-status-fields">
        <dt>API</dt>
        <dd className="api-status-service">Firecrab API <code>firecrab-api</code></dd>
        <dt>{t("Status", "상태")}</dt>
        <dd>{badge}</dd>
        <dt>{t("Info", "정보")}</dt>
        <dd>
          <p>{description || t(
            "Results are recorded for VMs left active at API startup and cleared when a new VM start is accepted.",
            "API 시작 시 활성 상태였던 VM의 확인 결과를 기록하며, 새로운 VM 시작이 수락되면 이전 결과를 지웁니다.",
          )}</p>
          {result && (
            <p className="reconciliation-note">
              {t(
                "Latest API VM lifecycle check, including operator network recovery.",
                "최근 API 시작 또는 운영자의 네트워크 복구 시 기록한 VM 상태 확인 결과입니다.",
              )}
            </p>
          )}
          {result?.detail && <p className="reconciliation-diagnostic">{result.detail}</p>}
        </dd>
        <dt>{t("Checked at", "확인 시각")}</dt>
        <dd>{checked ? <time dateTime={checked.toISOString()}>{time}</time> : "—"}</dd>
      </dl>
    </div>
  );

  if (details) return status === "unchecked" ? null : content;
  return (
    <StatusLabel
      className={`api-status-label ${status}`}
      accessibleLabel={`${heading}: ${label}`}
      outcome={status}
      tooltip={<><h3>API-STATUS</h3>{content}</>}
    >
      API
    </StatusLabel>
  );
}
