import type { VmState } from "./bindings";

export type VmAction = "start" | "stop" | "delete";

/** RAM is restricted to powers of two, matching how cloud instance sizes
 * are usually picked (and the server's own validation). */
export const RAM_OPTIONS_MIB = [128, 256, 512, 1024, 2048, 4096, 8192, 16384, 32768] as const;

/** Next valid RAM value in `direction` from `current` — snaps a non-power-of-two
 * legacy value (e.g. a VM created before this constraint existed) to the
 * nearest option in that direction instead of jumping to a list index. */
export function stepRamValue(current: number, direction: 1 | -1): number {
  const options = RAM_OPTIONS_MIB;
  const exact = options.indexOf(current as (typeof options)[number]);
  if (exact !== -1) {
    const next = Math.min(Math.max(exact + direction, 0), options.length - 1);
    return options[next];
  }
  if (direction === 1) {
    return options.find((option) => option > current) ?? options[options.length - 1];
  }
  return [...options].reverse().find((option) => option < current) ?? options[0];
}

/** Lifecycle actions the API accepts for a VM in `state`; everything else 409s. */
export function availableActions(state: VmState): VmAction[] {
  switch (state) {
    case "created":
    case "stopped":
    case "error":
      return ["start", "delete"];
    case "running":
      return ["stop"];
    case "starting":
    case "stopping":
      return [];
  }
}

/** cpu/ram/disk edits only take effect on the next start, so they're only
 * accepted while no Firecracker process is live for this VM. */
export function isEditableState(state: VmState): boolean {
  return state === "created" || state === "stopped" || state === "error";
}
