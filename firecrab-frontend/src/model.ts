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
