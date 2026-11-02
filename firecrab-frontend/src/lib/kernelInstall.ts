import type { KernelInstallResponse } from "../bindings";

/** Display the server reported by the API, including older source-build APIs. */
export function kernelDownloadSource(address?: string) {
  if (!address) return null;
  try {
    const url = new URL(address);
    if (url.protocol !== "https:" && url.protocol !== "http:") return null;
    return { host: url.host, origin: url.origin, href: url.href };
  } catch {
    return null;
  }
}

export const KERNEL_STEPS = [
  ["Prepare source", "소스 준비"],
  ["Download source", "소스 다운로드"],
  ["Wait for builder", "빌드 대기"],
  ["Build kernel", "커널 빌드"],
  ["Verify and install", "검증 및 설치"],
] as const;

export const REGISTRY_STEPS = [
  ["Resolve package", "패키지 확인"],
  ["Download", "다운로드"],
  ["Extract", "압축 해제"],
  ["Verify", "검증"],
  ["Install", "설치"],
] as const;

export function registryInstall(job: KernelInstallResponse): boolean {
  return /kernel package/.test(job.log);
}
export function kernelSteps(job: KernelInstallResponse, registry = false) {
  return registry || registryInstall(job) ? REGISTRY_STEPS : KERNEL_STEPS;
}

/** Use explicit backend milestones; resolving metadata is not a download. */
export function kernelStage(job: KernelInstallResponse): number {
  if (job.status === "succeeded") return KERNEL_STEPS.length;
  if (registryInstall(job)) {
    if (/installing kernel package|verified and cached/.test(job.log)) return 4;
    if (/verifying kernel package/.test(job.log)) return 3;
    if (/extracting kernel package/.test(job.log)) return 2;
    if (/downloading kernel package/.test(job.log)) return 1;
    return 0;
  }
  if (/verifying built kernel|verified and cached/.test(job.log)) return 4;
  if (/building Linux /.test(job.log)) return 3;
  if (/waiting for the host kernel builder/.test(job.log)) return 2;
  if (/downloading and verifying /.test(job.log)) return 1;
  return 0;
}

export function kernelBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 ** 2) return `${(bytes / 1024).toFixed(1)} KiB`;
  return `${(bytes / 1024 ** 2).toFixed(1)} MiB`;
}

/** Only received archive bytes provide a measured percentage. */
export function kernelDownloadPercent(
  job: KernelInstallResponse,
): number | null {
  if (!job.totalBytes || job.downloadedBytes === undefined) return null;
  return Math.min(
    100,
    Math.max(0, Math.floor((job.downloadedBytes / job.totalBytes) * 100)),
  );
}

export function kernelLastActivity(log: string): string {
  return (
    log
      .trim()
      .split("\n")
      .filter(Boolean)
      .at(-1)
      ?.replace(/^\[[\d:]+\]\s*/, "") ?? ""
  );
}
