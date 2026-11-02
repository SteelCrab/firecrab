export type SkillSource = "openai" | "anthropic";
export type SkillEntry = {
  source: SkillSource;
  kind: "skill" | "skill_bundle";
  path: string;
  name: string;
  revision: string;
  instructions: string[];
};
export type SkillInstall = Pick<
  SkillEntry,
  "source" | "kind" | "path" | "revision"
>;
export type SkillRef = { id: string; source: SkillSource; name: string };
export type SkillInstallation = {
  id: string;
  entry: SkillEntry;
  root: string;
  status: "pending" | "installed" | "removed";
  files: { path: string; version: number; operationId: string }[];
  sha256: string;
};
export type SkillCatalog = {
  source: SkillSource;
  url: string;
  revision?: string;
  entries: SkillEntry[];
};
export const validSkillSource = (s: unknown): s is SkillSource =>
  s === "openai" || s === "anthropic";
export const validSkillInstall = (v: SkillInstall) =>
  v &&
  validSkillSource(v.source) &&
  (v.kind === "skill" || v.kind === "skill_bundle") &&
  typeof v.path === "string" &&
  v.path.length <= 1024 &&
  !v.path.split("/").some((p) => !p || p === "." || p === "..") &&
  /^[a-f0-9]{40}$/.test(v.revision);
