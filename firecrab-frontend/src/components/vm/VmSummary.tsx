import { Activity, Cpu, HardDrive, MemoryStick } from "lucide-react";
import type { LucideIcon } from "lucide-react";
import type { VmResponse } from "../../bindings";
import { useHostStatus } from "../../hostStatusContext";
import { useI18n } from "../../i18n";

interface SummaryItem {
  key: string;
  icon: LucideIcon;
  label: string;
  value: number;
  /** Text after the value: ` / 4`, ` / 64 GiB`, ` GiB`. */
  suffix: string;
  /** Share of the total for the bar; `null` draws no bar. */
  percent: number | null;
}

const gib = (mib: number) => Number((mib / 1024).toFixed(1));

/**
 * Allocation strip above the MicroVM list (spec §8.1). The API reports no
 * host core count, so vCPU has no total; memory and disk show one only while
 * the host status answers.
 */
export default function VmSummary({ vms }: { vms: VmResponse[] }) {
  const { t } = useI18n();
  const host = useHostStatus();
  const running = vms.filter((vm) => vm.state === "running").length;
  const cpu = vms.reduce((sum, vm) => sum + vm.cpu, 0);
  const memory = gib(vms.reduce((sum, vm) => sum + vm.ram, 0));
  const disk = vms.reduce((sum, vm) => sum + vm.diskGb, 0);
  const hostMemory = host ? gib(host.memoryTotalMib) : null;
  const hostDisk = host ? Math.round(host.diskTotalGib) : null;

  const items: SummaryItem[] = [
    { key: "running", icon: Activity, label: t("Running", "실행 중"), value: running, suffix: ` / ${vms.length}`, percent: vms.length ? (running / vms.length) * 100 : 0 },
    { key: "cpu", icon: Cpu, label: t("vCPU allocated", "vCPU 할당"), value: cpu, suffix: "", percent: null },
    {
      key: "memory",
      icon: MemoryStick,
      label: t("Memory allocated", "메모리 할당"),
      value: memory,
      suffix: hostMemory ? ` / ${hostMemory} GiB` : " GiB",
      percent: hostMemory ? (memory / hostMemory) * 100 : null,
    },
    {
      key: "disk",
      icon: HardDrive,
      label: t("Disk allocated", "디스크 할당"),
      value: disk,
      suffix: hostDisk ? ` / ${hostDisk} GB` : " GB",
      percent: hostDisk ? (disk / hostDisk) * 100 : null,
    },
  ];

  return (
    <dl className="vm-summary">
      {items.map(({ key, icon: Icon, label, value, suffix, percent }) => (
        <div key={key} className="vm-summary-item">
          <dt><Icon aria-hidden="true" />{label}</dt>
          <dd>
            <span className="vm-summary-value">{value}<small>{suffix}</small></span>
            {percent !== null && <span className="vm-summary-bar"><i style={{ width: `${Math.min(100, percent)}%` }} /></span>}
          </dd>
        </div>
      ))}
    </dl>
  );
}
