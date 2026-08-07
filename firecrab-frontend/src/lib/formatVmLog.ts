import type { StartupStep, StartupStepRun, VmResponse } from "../bindings";

const STARTUP_STEP_LABEL: Record<StartupStep, string> = {
  preparingDisk: "Preparing disk",
  generatingConfig: "Generating configuration",
  startingProcess: "Starting process",
  configuringNetwork: "Checking network",
};

function durationMs(millis: number): string {
  if (millis < 1000) return `${millis}ms`;
  const seconds = Math.round(millis / 1000);
  if (seconds < 60) return `${seconds}s`;
  return `${Math.floor(seconds / 60)}m ${seconds % 60}s`;
}

/** One timeline row per known step run, plain text for copy/download. */
export function formatStartupTimeline(timeline: StartupStepRun[]): string {
  if (timeline.length === 0) return "(no timeline)";
  return timeline
    .map((run) => {
      const label = STARTUP_STEP_LABEL[run.step] ?? run.step;
      const elapsed =
        run.endedAtMs != null ? durationMs(run.endedAtMs - run.startedAtMs) : "running";
      const detail = run.detail ? ` — ${run.detail}` : "";
      return `${label}\t${run.outcome}\t${elapsed}${detail}`;
    })
    .join("\n");
}

/**
 * Bundle header + startup timeline + guest console for export.
 * Used by the serial console page (server log) and can wrap detail-modal text.
 */
export function formatVmExportBundle(opts: {
  vm: VmResponse | null;
  vmId: string;
  consoleLog: string;
  /** Extra block (e.g. xterm buffer) shown after the server log. */
  liveTerminal?: string;
  at?: Date;
}): string {
  const { vm, vmId, consoleLog, liveTerminal, at = new Date() } = opts;
  const name = vm?.name ?? vmId.slice(0, 8);
  const id = vm?.id ?? vmId;
  const lines: string[] = [
    `# firecrab VM log — ${name}`,
    `# id: ${id}`,
    `# state: ${vm?.state ?? "unknown"}`,
    `# exported: ${at.toISOString()}`,
    "",
    "## startup timeline",
    formatStartupTimeline(vm?.startupTimeline ?? []),
    "",
    "## console log (server)",
    consoleLog.trimEnd() || "(empty)",
  ];
  if (liveTerminal !== undefined) {
    lines.push("", "## terminal buffer (session)", liveTerminal.trimEnd() || "(empty)");
  }
  return lines.join("\n");
}

/** Serialize an xterm buffer to plain text (no serialize addon required). */
export function serializeXtermBuffer(term: {
  buffer: { active: { length: number; getLine: (y: number) => { translateToString: (trimRight?: boolean) => string } | undefined } };
}): string {
  const buf = term.buffer.active;
  const rows: string[] = [];
  for (let y = 0; y < buf.length; y++) {
    const line = buf.getLine(y);
    rows.push(line ? line.translateToString(true) : "");
  }
  // Drop trailing empty rows from the scrollback floor.
  while (rows.length > 0 && rows[rows.length - 1] === "") {
    rows.pop();
  }
  return rows.join("\n");
}
