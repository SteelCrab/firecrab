import type { EgressPolicy, PortForward, UpdateVmResourcesRequest, VmResponse, VmState } from "../../../bindings";
import { ApiClientError, assignVmStorage, updateVmPortForwards, updateVmResources, updateVmShells } from "../../../api/client";
import { isValidPort } from "../../../lib/portForward";
import { isEditableState, isEnvEditableState, isPortEditableState } from "../../../model";

export type VmSection = "spec" | "egress" | "env" | "storage" | "shells" | "ports";

export interface EnvRow {
  key: string;
  value: string;
}

/** The inputs of one section while it is being edited. */
export type VmSectionDraft =
  | { section: "spec"; cpu: string; ram: string; diskGb: string }
  | { section: "egress"; egressPolicy: EgressPolicy }
  | { section: "env"; rows: EnvRow[] }
  | { section: "storage"; storageRoot: string }
  | { section: "shells"; shellIds: string[] }
  | { section: "ports"; portForwards: PortForward[] };

/** Same rules as the API: cpu/ram/disk, storage, shells and egress only while no Firecracker process is live. */
export function isSectionEditable(state: VmState, section: VmSection, purpose?: VmResponse["purpose"]): boolean {
  switch (section) {
    case "env":
      return isEnvEditableState(state, purpose);
    case "ports":
      return isPortEditableState(state, purpose);
    default:
      return isEditableState(state, purpose);
  }
}

/** A draft holding the VM's current values for `section`. */
export function draftFor(vm: VmResponse, section: VmSection): VmSectionDraft {
  switch (section) {
    case "spec":
      return { section, cpu: String(vm.cpu), ram: String(vm.ram), diskGb: String(vm.diskGb) };
    case "egress":
      return { section, egressPolicy: vm.egressPolicy };
    case "env":
      return { section, rows: Object.entries(vm.env ?? {}).map(([key, value]) => ({ key, value })) };
    case "storage":
      return { section, storageRoot: vm.storageRoot || "default" };
    case "shells":
      return { section, shellIds: (vm.shellRefs ?? []).map((ref) => ref.shellId) };
    case "ports":
      return { section, portForwards: (vm.portForwards ?? []).map((forward) => ({ ...forward })) };
  }
}

function validation(message: string, field: string, detail: string): ApiClientError {
  return ApiClientError.api(400, { code: "validation", message, fields: { [field]: detail }, requestId: "" });
}

/** Rejects rows the API would reject, so the user fixes them instead of losing them. */
function envMap(rows: EnvRow[]): Record<string, string> {
  const env: Record<string, string> = {};
  for (const row of rows) {
    const key = row.key.trim();
    if (!key) throw validation("Every environment variable needs a key.", "env", "key must not be empty");
    if (key in env) throw validation("Environment keys must be unique.", "env", "duplicate key");
    env[key] = row.value;
  }
  return env;
}

/**
 * Saves one section with the API call that owns it (spec §8.3).
 * `PUT /api/vms/<id>` needs cpu, ram, disk and egress every time, so the
 * fields this section does not edit go out with the VM's current values; env
 * goes out only from the env section (left out, the server keeps the stored map).
 */
export async function saveVmSection(vm: VmResponse, draft: VmSectionDraft): Promise<VmResponse> {
  const resources: UpdateVmResourcesRequest = { cpu: vm.cpu, ram: vm.ram, diskGb: vm.diskGb, egressPolicy: vm.egressPolicy };
  switch (draft.section) {
    case "spec":
      return updateVmResources(vm.id, {
        ...resources,
        cpu: parseInt(draft.cpu, 10) || 0,
        ram: parseInt(draft.ram, 10) || 0,
        diskGb: parseInt(draft.diskGb, 10) || 0,
      });
    case "egress":
      return updateVmResources(vm.id, { ...resources, egressPolicy: draft.egressPolicy });
    case "env":
      return updateVmResources(vm.id, { ...resources, env: envMap(draft.rows) });
    case "storage":
      return draft.storageRoot === vm.storageRoot ? vm : assignVmStorage(vm.id, { storageRoot: draft.storageRoot });
    case "shells":
      return updateVmShells(vm.id, { shellIds: draft.shellIds });
    case "ports":
      if (draft.portForwards.some((forward) => !isValidPort(forward.hostPort) || !isValidPort(forward.guestPort))) {
        throw validation("Every port forward needs a host and guest port (1-65535).", "portForwards", "host and guest port must be 1-65535");
      }
      return updateVmPortForwards(vm.id, { portForwards: draft.portForwards });
  }
}
