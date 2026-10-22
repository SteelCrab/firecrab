import type { VmResponse } from "../../bindings";
import { useI18n } from "../../i18n";
import Sparkline from "../ui/Sparkline";
import { HIGH_USAGE_PERCENT, formatMib, formatPercent, usageSeries } from "./vmFormat";

/** CPU and memory cards with recent history; a note while there is nothing to show. */
export default function VmUsage({ vm }: { vm: VmResponse }) {
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
