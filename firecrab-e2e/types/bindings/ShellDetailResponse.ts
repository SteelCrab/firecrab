import type { ShellRevisionSummary } from "./ShellRevisionSummary";

export type ShellDetailResponse = {
  id: string;
  name: string;
  description?: string | null;
  createdAtMs: number;
  updatedAtMs: number;
  revisions: Array<ShellRevisionSummary>;
  latestContent?: string | null;
};
