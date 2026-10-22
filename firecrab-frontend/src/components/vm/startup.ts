import type { StartupStep, VmResponse } from "../../bindings";

export const STARTUP_STEPS: StartupStep[] = [
  "preparingDisk",
  "generatingConfig",
  "startingProcess",
  "configuringNetwork",
];

/** Step names as `t(english, korean)` arguments. */
export const STARTUP_STEP_LABELS: Record<StartupStep, [string, string]> = {
  preparingDisk: ["Preparing disk", "디스크 준비"],
  generatingConfig: ["Generating configuration", "구성 생성"],
  startingProcess: ["Starting process", "프로세스 시작"],
  configuringNetwork: ["Checking network", "네트워크 확인"],
};

/** Localised step name; a step this dashboard has no label for shows by its API name. */
export function startupStepLabel(step: StartupStep, t: (english: string, korean: string) => string): string {
  const label: [string, string] | undefined = STARTUP_STEP_LABELS[step];
  return label ? t(...label) : step;
}

// Derived client-side from the polled `startupStep` value — no dedicated
// backend log field. See public-docs/guides/dashboard.md for why.
export const STARTUP_STEP_LOG_LINE: Record<StartupStep, string> = {
  preparingDisk: "Preparing disk (copying rootfs template)…",
  generatingConfig: "Disk ready → generating Firecracker configuration…",
  startingProcess: "Configuration ready → starting Firecracker process…",
  configuringNetwork: "Process started → checking guest DHCP/DNS…",
};

/** Formats a span the way a build log does: `820ms`, `3s`, `1m 32s`. */
export function duration(millis: number): string {
  if (millis < 1000) return `${millis}ms`;
  const seconds = Math.round(millis / 1000);
  if (seconds < 60) return `${seconds}s`;
  return `${Math.floor(seconds / 60)}m ${seconds % 60}s`;
}

/** Wall-clock `HH:MM:SS.mmm`, to line up with the console log. */
export function clockTime(epochMillis: number): string {
  const at = new Date(epochMillis);
  const pad = (value: number, width = 2) => String(value).padStart(width, "0");
  return (
    `${pad(at.getHours())}:${pad(at.getMinutes())}:${pad(at.getSeconds())}` +
    `.${pad(at.getMilliseconds(), 3)}`
  );
}

/** Gives the operator a safe, action-oriented explanation for an nft IP-map
 * collision instead of exposing the raw ruleset syntax in the VM timeline. */
export function startupFailureDetail(detail: string): string {
  const conflict = detail.match(
    /(?:IPv4 lease|vm_egress\s*\{)\s*([0-9]+(?:\.[0-9]+){3})(?:\s+conflicts|\s*:)/,
  );
  if (conflict && detail.includes("File exists")) {
    return `IP ${conflict[1]}은 기존 MicroVM이 사용 중입니다. 기존 VM을 중지하거나 복구한 뒤 다시 시도하세요.`;
  }
  if (detail.includes("conflicts with an existing host firewall policy")) {
    return "기존 MicroVM의 호스트 네트워크 정책과 충돌합니다. 기존 VM을 중지하거나 복구한 뒤 다시 시도하세요.";
  }
  return detail;
}

/** Why the last start failed, from the timeline; `null` when no step failed. */
export function startupFailure(vm: VmResponse): string | null {
  const failed = [...vm.startupTimeline].reverse().find((run) => run.outcome === "failed");
  return failed?.detail ? startupFailureDetail(failed.detail) : null;
}

export function logTimestamp(): string {
  // ISO 8601 with a 9-digit fractional suffix (matches the console log's
  // timestamp shape); the browser only has millisecond precision, so the
  // trailing 6 digits are zero-padded rather than fabricated.
  return `${new Date().toISOString().slice(0, -1)}000000`;
}
