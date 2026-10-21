import type { VmReconciliation, VmReconciliationOutcome } from "../bindings";
import { useI18n } from "../i18n";

const LABELS: Record<VmReconciliationOutcome, [string, string]> = {
  reconnected: ["Reconnected", "재연결 성공"],
  gone: ["VM not found", "VM 없음"],
  mismatched: ["Connection mismatch", "연결 불일치"],
  networkFailed: ["Network recovery failed", "네트워크 복구 실패"],
  interrupted: ["Startup interrupted", "시작 중단"],
  exited: ["Exited while API offline", "API 중단 중 종료"],
};

const DESCRIPTIONS: Record<VmReconciliationOutcome, [string, string]> = {
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

/**
 * What the API recorded about a VM at its last startup or network retry. Only
 * the VM's detail shows it: the list stays to the VM's own state.
 */
export default function ReconciliationStatus({ result }: { result: VmReconciliation }) {
  const { t } = useI18n();
  const label = t(...LABELS[result.outcome]);
  const checked = new Date(result.checkedAtMs);
  const time = checked.toLocaleString("sv-SE", {
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
    hourCycle: "h23",
    timeZoneName: "longOffset",
  }).replace("GMT", "UTC");
  const heading = t("API-STATUS · Firecrab API · VM reconciliation", "API-STATUS · Firecrab API · VM 재조정");

  return (
    <div className="reconciliation-detail">
      <dl className="api-status-fields">
        <dt>API</dt>
        <dd className="api-status-service">Firecrab API <code>firecrab-api</code></dd>
        <dt>{t("Status", "상태")}</dt>
        <dd>
          <span className={`reconciliation-badge ${result.outcome}`} aria-label={`${heading}: ${label}`}>
            {label}
          </span>
        </dd>
        <dt>{t("Info", "정보")}</dt>
        <dd>
          <p>{t(...DESCRIPTIONS[result.outcome])}</p>
          <p className="reconciliation-note">
            {t(
              "Latest API VM lifecycle check, including operator network recovery.",
              "최근 API 시작 또는 운영자의 네트워크 복구 시 기록한 VM 상태 확인 결과입니다.",
            )}
          </p>
          {result.detail && <p className="reconciliation-diagnostic">{result.detail}</p>}
        </dd>
        <dt>{t("Checked at", "확인 시각")}</dt>
        <dd><time dateTime={checked.toISOString()}>{time}</time></dd>
      </dl>
    </div>
  );
}
