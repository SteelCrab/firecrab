// Mirrors firecrab_api_types::MicroRegistryImageResponse (camelCase wire shape).

import type { PackageOrigin } from "./PackageOrigin";

export type MicroRegistryImageResponse = {
  alias: string;
  version: string;
  /** Registry-relative package object key. */
  package: string;
  sha256: string;
  minDiskGb: number;
  publishedAt: string;
  installed: boolean;
  packageStaged: boolean;
  packageOrigin?: PackageOrigin;
  /** Whether this Firecrab version can download and validate the alias. */
  downloadable: boolean;
};
