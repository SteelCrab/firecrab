/**
 * Clipboard copy + file download for plain-text logs.
 * Shared by the VM detail modal, serial console, and (later) image install logs.
 */

/**
 * `navigator.clipboard.writeText` is async. On HTTP (LAN phone) and after an
 * `await`, the user gesture is already gone, so a failed write-then-fallback
 * never reaches `execCommand`. Only call the async API when it can succeed
 * in this same turn.
 */
export function shouldPreferAsyncClipboard(
  isSecureContext: boolean,
  hasWriteText: boolean,
): boolean {
  return isSecureContext && hasWriteText;
}

/**
 * iOS ignores `execCommand('copy')` when the source is off-screen (`left: -9999px`).
 * Keep a 1×1 transparent node at the origin instead.
 */
export function prepareClipboardFallbackTextarea(area: HTMLTextAreaElement): void {
  area.setAttribute("readonly", "");
  area.setAttribute("aria-hidden", "true");
  area.style.position = "fixed";
  area.style.top = "0";
  area.style.left = "0";
  area.style.width = "1px";
  area.style.height = "1px";
  area.style.padding = "0";
  area.style.border = "0";
  area.style.outline = "none";
  area.style.boxShadow = "none";
  area.style.opacity = "0.01";
  area.style.zIndex = "10000";
}

/** Copy `text` to the system clipboard. Returns false if the API is unavailable or rejects. */
export async function copyText(text: string): Promise<boolean> {
  const value = text ?? "";
  const hasWriteText = typeof navigator.clipboard?.writeText === "function";
  if (shouldPreferAsyncClipboard(window.isSecureContext, hasWriteText)) {
    try {
      await navigator.clipboard.writeText(value);
      return true;
    } catch {
      /* fall through to execCommand — gesture may already be gone */
    }
  }
  return copyTextFallback(value);
}

/** Best-effort fallback for older browsers / insecure contexts. */
function copyTextFallback(text: string): boolean {
  try {
    const area = document.createElement("textarea");
    area.value = text;
    prepareClipboardFallbackTextarea(area);
    document.body.appendChild(area);
    selectFallbackTextarea(area, text);
    const ok = document.execCommand("copy");
    document.body.removeChild(area);
    return ok;
  } catch {
    return false;
  }
}

function selectFallbackTextarea(area: HTMLTextAreaElement, text: string): void {
  const ios =
    /ipad|iphone|ipod/i.test(navigator.userAgent) ||
    (navigator.platform === "MacIntel" && navigator.maxTouchPoints > 1);
  if (ios) {
    area.contentEditable = "true";
    area.readOnly = false;
    const range = document.createRange();
    range.selectNodeContents(area);
    const selection = window.getSelection();
    selection?.removeAllRanges();
    selection?.addRange(range);
    area.setSelectionRange(0, text.length);
    return;
  }
  area.focus();
  area.select();
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
