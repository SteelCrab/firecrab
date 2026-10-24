import type { TerminalDimensions } from "./wasmTerminal";

export const CONSOLE_PROTOCOL = "firecrab.console.v1";
export interface GuestResize {
  type: "resize";
  status: "pending" | "applied" | "unavailable";
  size: TerminalDimensions | null;
  viewers: number;
}

export function guestResizeMessage(text: string): GuestResize | null {
  try {
    const value = JSON.parse(text);
    const valid = (n: unknown) => Number.isInteger(n) && Number(n) >= 2 && Number(n) <= 1000;
    if (value?.type !== "resize" || !["pending", "applied", "unavailable"].includes(value.status) ||
        !Number.isInteger(value.viewers) || value.viewers < 1 || (value.status === "applied" && value.size === null) ||
        (value.size !== null && (!valid(value.size?.rows) || !valid(value.size?.cols)))) return null;
    return value;
  } catch { return null; }
}
