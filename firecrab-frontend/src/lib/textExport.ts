/**
 * Clipboard copy + file download for plain-text logs.
 * Shared by the VM detail modal, serial console, and (later) image install logs.
 */

/** Copy `text` to the system clipboard. Returns false if the API is unavailable or rejects. */
export async function copyText(text: string): Promise<boolean> {
  const value = text ?? "";
  try {
    if (navigator.clipboard?.writeText) {
      await navigator.clipboard.writeText(value);
      return true;
    }
  } catch {
    /* fall through to execCommand */
  }
  return copyTextFallback(value);
}

/** Best-effort fallback for older browsers / insecure contexts. */
function copyTextFallback(text: string): boolean {
  try {
    const area = document.createElement("textarea");
    area.value = text;
    area.setAttribute("readonly", "");
    area.style.position = "fixed";
    area.style.left = "-9999px";
    area.style.top = "0";
    document.body.appendChild(area);
    area.select();
    const ok = document.execCommand("copy");
    document.body.removeChild(area);
    return ok;
  } catch {
    return false;
  }
}

/**
 * Trigger a browser download of `text` as a UTF-8 `.txt` (or custom extension
 * in `filename`). Revokes the object URL after the click.
 */
export function downloadText(text: string, filename: string): void {
  const blob = new Blob([text ?? ""], { type: "text/plain;charset=utf-8" });
  const url = URL.createObjectURL(blob);
  const anchor = document.createElement("a");
  anchor.href = url;
  anchor.download = filename || "log.txt";
  anchor.rel = "noopener";
  document.body.appendChild(anchor);
  anchor.click();
  document.body.removeChild(anchor);
  // Defer revoke so the browser has time to start the download.
  window.setTimeout(() => URL.revokeObjectURL(url), 1_000);
}

/** Safe filename fragment from a VM name or id. */
export function sanitizeFilenamePart(value: string): string {
  const cleaned = value
    .trim()
    .replace(/[^\w.-]+/g, "-")
    .replace(/-+/g, "-")
    .replace(/^-|-$/g, "");
  return cleaned.slice(0, 64) || "vm";
}

/** `firecrab-<kind>-<name>-YYYYMMDD-HHMMSS.txt` */
export function logDownloadFilename(kind: string, nameOrId: string, at = new Date()): string {
  const pad = (n: number, w = 2) => String(n).padStart(w, "0");
  const stamp =
    `${at.getFullYear()}${pad(at.getMonth() + 1)}${pad(at.getDate())}` +
    `-${pad(at.getHours())}${pad(at.getMinutes())}${pad(at.getSeconds())}`;
  return `firecrab-${kind}-${sanitizeFilenamePart(nameOrId)}-${stamp}.txt`;
}
