import { agentRequest } from "./agents";

export type RuleBlock = { start: number; end: number };
export type RulesDocument = {
  state: "ready" | "unavailable";
  reason: string | null;
  exists: boolean;
  text: string;
  digest: string;
  blocks: RuleBlock[];
  /** Set when the rule was written but Claude Code or Gemini CLI wiring could not be updated. */
  warning?: string | null;
};
export type RuleMode = "add" | "edit";
export type RuleProposal = {
  mode: RuleMode;
  block: number | null;
  before: string;
  after: string;
  cli: string;
  name: string;
  model: string;
  digest: string;
};

export const readRules = (vmId: string, controllerId: string) =>
  agentRequest<RulesDocument>("/rules", undefined, controllerId, vmId);

export const refineRule = (
  vmId: string,
  controllerId: string,
  request: {
    mode: RuleMode;
    block?: number;
    input: string;
    cli: string;
    model?: string;
    digest: string;
    previous?: string;
  },
) => agentRequest<RuleProposal>("/rules/refine", request, controllerId, vmId);

export const applyRule = (
  vmId: string,
  controllerId: string,
  request: { mode: RuleMode; block?: number; text: string; digest: string },
) => agentRequest<RulesDocument>("/rules/apply", request, controllerId, vmId);
