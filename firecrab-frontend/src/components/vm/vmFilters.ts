import type { VmResponse } from "../../bindings";

export type VmFilter = "all" | "running" | "paused" | "stopped" | "error";

/** State tabs above the MicroVM list. Starting, stopping and created VMs show only under "All". */
export const VM_FILTERS: { id: VmFilter; labels: [string, string]; matches: (vm: VmResponse) => boolean }[] = [
  { id: "all", labels: ["All", "전체"], matches: () => true },
  { id: "running", labels: ["Running", "실행 중"], matches: (vm) => vm.state === "running" },
  { id: "paused", labels: ["Paused", "일시정지됨"], matches: (vm) => vm.state === "paused" },
  { id: "stopped", labels: ["Stopped", "중지됨"], matches: (vm) => vm.state === "stopped" },
  { id: "error", labels: ["Error", "오류"], matches: (vm) => vm.state === "error" },
];

/** VMs in the `filter` tab whose name, IPv4 or image contains `query`, ignoring case. */
export function filterVms(vms: VmResponse[], filter: VmFilter, query: string): VmResponse[] {
  const tab = VM_FILTERS.find((entry) => entry.id === filter) ?? VM_FILTERS[0];
  const needle = query.trim().toLowerCase();
  return vms.filter((vm) =>
    tab.matches(vm) &&
    (!needle || [vm.name, vm.ipv4, vm.template].some((value) => value?.toLowerCase().includes(needle))),
  );
}
