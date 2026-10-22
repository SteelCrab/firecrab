import type { VmResponse, VmUsageSample } from "../../bindings";

/** `512 MiB`, `1 GiB`, `1.5 GiB`. */
export function formatMib(mib: number): string {
  return mib >= 1024 ? `${Number((mib / 1024).toFixed(1))} GiB` : `${mib} MiB`;
}

/** `4 vCPU · 4 GiB · 40 GiB` — allocated cpu, ram, disk. */
export function vmSpec(vm: VmResponse): string {
  return `${vm.cpu} vCPU · ${formatMib(vm.ram)} · ${vm.diskGb} GiB`;
}

/** One decimal under 10%, whole numbers above: `4.2%`, `41%`. */
export function formatPercent(value: number): string {
  return `${value >= 10 ? Math.round(value) : Math.round(value * 10) / 10}%`;
}

/** Usage samples that carry a value, oldest first. */
export function usageSeries(history: VmUsageSample[], pick: (sample: VmUsageSample) => number | null): number[] {
  return history.map(pick).filter((value): value is number => value != null);
}

/** At or above this share a usage reading is drawn as a warning. */
export const HIGH_USAGE_PERCENT = 80;
