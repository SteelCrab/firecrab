import type { ScheduleTiming } from "../../bindings";
import type { Locale } from "../../i18n";

export const DAYS = [
  { value: "Mon", en: "Mon", ko: "월" }, { value: "Tue", en: "Tue", ko: "화" },
  { value: "Wed", en: "Wed", ko: "수" }, { value: "Thu", en: "Thu", ko: "목" },
  { value: "Fri", en: "Fri", ko: "금" }, { value: "Sat", en: "Sat", ko: "토" },
  { value: "Sun", en: "Sun", ko: "일" },
] as const;

export type Repeat = "weekdays" | "daily" | "weekly" | "once" | "cron";
export interface TimingDraft {
  repeat: Repeat;
  time: string;
  date: string;
  days: string[];
  expression: string;
  timeZone: string;
}

export function localZone(): string { return Intl.DateTimeFormat().resolvedOptions().timeZone; }
const pad = (n: number) => String(n).padStart(2, "0");
export function localDate(at: Date): string { return `${at.getFullYear()}-${pad(at.getMonth() + 1)}-${pad(at.getDate())}`; }

export function timingDraft(timing?: ScheduleTiming): TimingDraft {
  const draft: TimingDraft = { repeat: "weekdays", time: "09:00", date: "", days: ["Mon"], expression: "0 9 * * Mon-Fri", timeZone: localZone() };
  if (!timing) return draft;
  if (timing.kind === "once") {
    const at = new Date(timing.at);
    return { ...draft, repeat: "once", date: localDate(at), time: `${pad(at.getHours())}:${pad(at.getMinutes())}` };
  }
  const [minute, hour, day, month, weekday] = timing.expression.trim().split(/\s+/);
  const base = { ...draft, expression: timing.expression, timeZone: timing.timeZone };
  if (!/^\d+$/.test(minute ?? "") || !/^\d+$/.test(hour ?? "") || day !== "*" || month !== "*") return { ...base, repeat: "cron" };
  const time = `${pad(Number(hour))}:${pad(Number(minute))}`;
  if (weekday === "*") return { ...base, repeat: "daily", time };
  if (weekday?.toLowerCase() === "mon-fri") return { ...base, repeat: "weekdays", time };
  const days = weekday?.split(",").map(value => DAYS.find(day => day.value.toLowerCase() === value.toLowerCase())?.value);
  if (days?.length && days.every((day): day is typeof DAYS[number]["value"] => !!day)) return { ...base, repeat: "weekly", time, days };
  return { ...base, repeat: "cron" };
}

/** Return null for nonexistent DST times instead of silently shifting the user's time. */
export function buildTiming(draft: TimingDraft): ScheduleTiming | null {
  if (draft.repeat === "cron") return { kind: "cron", expression: draft.expression.trim(), timeZone: draft.timeZone.trim() };
  if (!/^([01]\d|2[0-3]):[0-5]\d$/.test(draft.time)) return null;
  if (draft.repeat === "once") {
    const at = new Date(`${draft.date}T${draft.time}:00`);
    if (Number.isNaN(at.getTime()) || localDate(at) !== draft.date || `${pad(at.getHours())}:${pad(at.getMinutes())}` !== draft.time) return null;
    return { kind: "once", at: at.toISOString() };
  }
  const [hour, minute] = draft.time.split(":").map(Number);
  const days = DAYS.filter(day => draft.days.includes(day.value)).map(day => day.value);
  if (draft.repeat === "weekly" && days.length === 0) return null;
  const weekday = draft.repeat === "weekdays" ? "Mon-Fri" : draft.repeat === "daily" ? "*" : days.join(",");
  return { kind: "cron", expression: `${minute} ${hour} * * ${weekday}`, timeZone: draft.timeZone.trim() };
}

export function timingLabel(timing: ScheduleTiming, locale: Locale): string {
  if (timing.kind === "once") return new Intl.DateTimeFormat(locale === "ko" ? "ko-KR" : "en-US", { year: "numeric", month: "short", day: "numeric", hour: "numeric", minute: "2-digit" }).format(new Date(timing.at));
  const draft = timingDraft(timing);
  if (draft.repeat === "cron") return locale === "ko" ? `직접 설정 · ${timing.expression}` : `Custom · ${timing.expression}`;
  const repeat = draft.repeat === "weekdays" ? (locale === "ko" ? "평일" : "Weekdays")
    : draft.repeat === "daily" ? (locale === "ko" ? "매일" : "Daily")
      : DAYS.filter(day => draft.days.includes(day.value)).map(day => day[locale]).join("·");
  return `${repeat} ${draft.time}`;
}

export function scheduleZone(timing: ScheduleTiming): string { return timing.kind === "cron" ? timing.timeZone : localZone(); }

export function executionTime(at: number | null, zone: string, locale: Locale): string {
  return at === null ? "—" : new Intl.DateTimeFormat(locale === "ko" ? "ko-KR" : "en-US", { timeZone: zone, month: "short", day: "numeric", weekday: "short", hour: "numeric", minute: "2-digit" }).format(new Date(at));
}
