export type ShellRevisionResponse = {
  shellId: string;
  revisionId: string;
  version: number;
  contentSha256: string;
  content: string;
  createdAtMs: number;
};
