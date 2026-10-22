import type { Locale } from "../i18n";

/** Days, hours and minutes, e.g. `12d 3h 5m` / `12일 3시간 5분`. */
export function formatUptime(seconds: number, locale: Locale): string {
  const days = Math.floor(seconds / 86400);
  const hours = Math.floor((seconds % 86400) / 3600);
  const minutes = Math.floor((seconds % 3600) / 60);
  const parts = [];
  if (days > 0) parts.push(locale === "ko" ? `${days}일` : `${days}d`);
  if (days > 0 || hours > 0) parts.push(locale === "ko" ? `${hours}시간` : `${hours}h`);
  parts.push(locale === "ko" ? `${minutes}분` : `${minutes}m`);
  return parts.join(" ");
}

/** Only the largest unit, for tight spaces: `12d`, `5h`, `40m`. */
export function formatUptimeShort(seconds: number, locale: Locale): string {
  const days = Math.floor(seconds / 86400);
  if (days > 0) return locale === "ko" ? `${days}일` : `${days}d`;
  const hours = Math.floor(seconds / 3600);
  if (hours > 0) return locale === "ko" ? `${hours}시간` : `${hours}h`;
  const minutes = Math.floor(seconds / 60);
  return locale === "ko" ? `${minutes}분` : `${minutes}m`;
}
