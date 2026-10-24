export type ShellResponse = {
  id: string;
  name: string;
  description?: string | null;
  latestVersion: number;
  latestRevisionId?: string | null;
  contentSha256?: string | null;
  createdAtMs: number;
  updatedAtMs: number;
};
