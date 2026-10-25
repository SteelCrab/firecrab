/** Result of `GET /api/oci/inspect?reference=` (camelCase wire shape). */

export type OciInspectResponse = {
  registry: string;
  repository: string;
  version: string;
  immutable: boolean;
  digest: string;
  architecture: string;
  singlePlatform: boolean;
  /** Template alias this host would register on import. */
  alias: string;
};
