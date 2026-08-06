// Mirrors firecrab_api_types::PackageUpdateStatus (camelCase wire shape, tagged on "state").

export type PackageUpdateStatus =
  | { state: "running" }
  | { state: "succeeded", outputTail: string }
  | { state: "failed", reason: string, outputTail: string };
