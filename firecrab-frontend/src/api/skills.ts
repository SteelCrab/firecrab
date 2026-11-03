import { managerEndpoint } from "./manager";
import { agentRequest, AgentRequestError } from "./agents";

export type SkillLicense = "apache2" | "proprietary" | "unknown";
export type SkillReason =
  | null
  | "proprietary"
  | "unknown-license"
  | "unsupported-files"
  | "too-large";
export type StoreSkill = {
  name: string;
  description: string;
  license: SkillLicense;
  importable: boolean;
  reason: SkillReason;
  files: string[];
  preview: string;
  imported: { commit: string; digest: string } | null;
  updateAvailable: boolean;
};
export type SkillCatalog = {
  repository: string;
  commit: string | null;
  error: string | null;
  skills: StoreSkill[];
};
export type VmSkills = {
  enabled: string[];
  state: "applied" | "pending" | "failed";
  reason: string | null;
  clis: { id: string; name: string; installed: boolean }[];
  conflicts: string[];
};

/** The store is shared by every MicroVM, so it lives beside, not under, the VM routes. */
async function storeRequest<T>(path = "", controllerId?: string): Promise<T> {
  const resource = managerEndpoint();
  if (!resource)
    throw Error(
      "The skill store requires this host’s localhost dashboard. Start firecrab service dev on this host.",
    );
  const post = controllerId !== undefined;
  const response = await fetch(
    resource.replace(/\/resources$/, "/agents/skills") + path,
    {
      method: post ? "POST" : "GET",
      headers: post
        ? {
            "Content-Type": "application/json",
            "X-Firecrab-Controller-Id": controllerId,
          }
        : undefined,
      body: post ? "{}" : undefined,
      signal: AbortSignal.timeout(post ? 160000 : 60000),
    },
  );
  const value = await response.json().catch(() => null);
  if (!response.ok)
    throw new AgentRequestError(
      typeof value?.error === "string"
        ? value.error
        : `Skill store returned HTTP ${response.status}`,
      response.status,
    );
  return value as T;
}

export const skillCatalog = () => storeRequest<SkillCatalog>();

export const importSkill = (name: string, controllerId: string) =>
  storeRequest<SkillCatalog>(
    `/${encodeURIComponent(name)}/import`,
    controllerId,
  );

export const vmSkills = (vmId: string, controllerId: string) =>
  agentRequest<VmSkills>("/skills", undefined, controllerId, vmId);

export const setVmSkill = (
  vmId: string,
  controllerId: string,
  name: string,
  enabled: boolean,
) => agentRequest<VmSkills>("/skills", { name, enabled }, controllerId, vmId);
