// Mirrors firecrab_api_types::ImageResponse (camelCase wire shape).

import type { PackageOrigin } from "./PackageOrigin";

export type ImageResponse = {
  alias: string;
  version: string;
  kernelVersion?: string;
  kernelImage: string;
  kernelSha256: string;
  rootfsSha256: string;
  initrdSha256?: string;
  minDiskGb: number;
  /** On-disk rootfs length in bytes (0 / omitted when not installed). */
  rootfsSizeBytes?: number;
  installed: boolean;
  /**
   * Package download URL when `FIRECRAB_IMAGE_BASE_URL` is set
   * (`{base}/{distro}/{version}/{alias}.tar.zst` for built-ins).
   * Omitted when remote install is off.
   */
  packageUrl?: string;
  /**
   * A package archive for this alias is already staged locally, so
   * `POST /api/images/{alias}/install` can extract it with no download.
   * Independent of `packageUrl`: a web bootstrap stages one on hosts that
   * have no remote base URL configured at all.
   */
  packageStaged?: boolean;
  /** Producer of the staged package, when known. */
  packageOrigin?: PackageOrigin;
  description: string;
  /**
   * True when the installed rootfs has `/etc/firecrab/services.d/app`.
   * Uninstalled and catalog-only rows are false.
   */
  hasGuestService?: boolean;
};
