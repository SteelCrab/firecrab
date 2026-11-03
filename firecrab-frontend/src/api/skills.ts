import { managerEndpoint } from "./manager";
import { agentRequest, AgentRequestError } from "./agents";

/** The pinned Vercel Skills CLI release the MicroVM runs. */
export const SKILLS_CLI = "skills@1.7.6";

export type RegistrySkill = {
  name: string;
  skill: string;
  source: string;
  installs: number;
};
export type SkillSearch = {
  query: string;
  count: number;
  skills: RegistrySkill[];
};
export type InstalledSkill = {
  name: string;
  source: string | null;
  agents: string[];
  status: string;
};
export type VmSkills = {
  node: "ready" | "missing";
  skills: InstalledSkill[];
};
export type SkillChange =
  | { action: "install"; source: string; skill: string }
  | { action: "remove"; skill: string }
  | { action: "update" };
export type SkillAudit = {
  gen?: string;
  socket?: string;
  snyk?: string;
  details?: string;
};
export type SkillChangeResult = {
  result:
    | {
        name?: string;
        status: string;
        agents?: string[];
        security?: SkillAudit | null;
      }[]
    | null;
  installed: VmSkills;
};

/** skills.sh search runs on the host, so it works before any MicroVM is selected. */
export async function searchSkills(query: string): Promise<SkillSearch> {
  const resource = managerEndpoint();
  if (!resource)
    throw Error(
      "Skill search requires this host’s localhost dashboard. Start firecrab service dev on this host.",
    );
  const response = await fetch(
    resource.replace(/\/resources$/, "/agents/skills/search") +
      `?${new URLSearchParams({ q: query })}`,
    { signal: AbortSignal.timeout(30000) },
  );
  const value = await response.json().catch(() => null);
  if (!response.ok)
    throw new AgentRequestError(
      typeof value?.error === "string"
        ? value.error
        : `Skill search returned HTTP ${response.status}`,
      response.status,
    );
  return value as SkillSearch;
}

export const vmSkills = (vmId: string, controllerId: string) =>
  agentRequest<VmSkills>("/skills", undefined, controllerId, vmId);

export const changeSkills = (
  vmId: string,
  controllerId: string,
  change: SkillChange,
) => agentRequest<SkillChangeResult>("/skills", change, controllerId, vmId);
