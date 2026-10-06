import type { VmResponse, VmState } from "../bindings";
import { useI18n } from "../i18n";
import ReconciliationStatus from "./ReconciliationStatus";
import StatusLabel from "./StatusLabel";

const LABELS: Record<VmState, [string, string]> = {
  created: ["Created", "생성됨"],
  starting: ["Starting", "시작 중"],
  running: ["Running", "실행 중"],
  stopping: ["Stopping", "중지 중"],
  stopped: ["Stopped", "중지됨"],
  error: ["Error", "오류"],
};

const DESCRIPTIONS: Record<VmState, [string, string]> = {
  created: ["The VM is created and has not been started yet.", "VM이 생성되었으며 아직 시작되지 않았습니다."],
  starting: ["The VM is starting. Open its details to follow startup progress.", "VM이 시작 중입니다. 상세보기에서 시작 진행 상황을 확인할 수 있습니다."],
  running: ["The VM is running.", "VM이 실행 중입니다."],
  stopping: ["The VM is stopping.", "VM이 종료 중입니다."],
  stopped: ["The VM is stopped.", "VM이 중지되어 있습니다."],
  error: ["The VM is in an error state. Open its details to inspect the logs.", "VM이 오류 상태입니다. 상세보기에서 로그를 확인할 수 있습니다."],
};

export default function VmStateLabels({ vm }: { vm: VmResponse }) {
  const { t } = useI18n();
  const label = `VM-STATUS: ${t(...LABELS[vm.state])}`;
  return (
    <div className="vm-state-labels">
      <StatusLabel
        className={`vm-status-label ${vm.state}`}
        accessibleLabel={label}
        state={vm.state}
        tooltip={(
          <>
            <h3>VM-STATUS</h3>
            <dl>
              <dt>NAME</dt><dd>{vm.name}</dd>
              <dt>ID</dt><dd className="mono">{vm.id}</dd>
              <dt>{t("Status", "상태")}</dt>
              <dd className={`vm-status-label ${vm.state}`}>{t(...LABELS[vm.state])}</dd>
              <dt>{t("Info", "정보")}</dt><dd>{t(...DESCRIPTIONS[vm.state])}</dd>
            </dl>
          </>
        )}
      >
        VM
      </StatusLabel>
      <ReconciliationStatus result={vm.reconciliation} state={vm.state} />
    </div>
  );
}
