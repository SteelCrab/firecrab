import type { VmState } from "./bindings";

export type VmAction = "start" | "stop" | "delete";

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
