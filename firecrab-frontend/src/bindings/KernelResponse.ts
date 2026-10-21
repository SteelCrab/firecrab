// Mirrors firecrab_api_types::KernelResponse (camelCase wire shape).

export type KernelResponse = {
  version: string;
  architecture: string;
  image: string;
  imageSha256: string;
  packageSha256: string;
  sizeBytes?: number;
  installed: boolean;
  inUse: boolean;
  packageUrl?: string;
};
