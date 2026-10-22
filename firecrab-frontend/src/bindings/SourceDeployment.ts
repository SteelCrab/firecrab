// This file mirrors firecrab_api_types::SourceDeployment.

export type SourceDeployment = {
  repository: string;
  revision?: string;
  buildCommand: string;
} & (
  | { runtime: "native"; runCommand: string }
  | { runtime: "wasm"; artifactPath: string; args?: Array<string> }
);
