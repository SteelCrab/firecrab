// Mirrors firecrab_api_types::ImageResponse (camelCase wire shape).

export type ImageResponse = {
  alias: string;
  version: string;
  kernelSha256: string;
  rootfsSha256: string;
  initrdSha256?: string;
  minDiskGb: number;
  installed: boolean;
  description: string;
};
