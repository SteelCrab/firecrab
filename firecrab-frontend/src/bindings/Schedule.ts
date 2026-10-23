export type ScheduleAction = "start" | "stop";
export type ScheduleTiming = { kind: "once"; at: string } | { kind: "cron"; expression: string; timeZone: string };
export interface ScheduleSpec {
  name: string;
  vmId: string;
  action: ScheduleAction;
  timing: ScheduleTiming;
  enabled: boolean;
}
export interface ScheduleRun {
  id: string;
  scheduleId: string;
  scheduledAtMs: number;
  startedAtMs: number;
  finishedAtMs: number | null;
  status: "running" | "succeeded" | "failed" | "interrupted";
  error: string | null;
}
export interface ScheduleResponse extends ScheduleSpec {
  id: string;
  nextRunAtMs: number | null;
  createdAtMs: number;
  updatedAtMs: number;
  lastRun: ScheduleRun | null;
}
