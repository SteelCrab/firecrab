import { acquirePoolLease } from "../api/client";

// A transport error cannot tell whether the server committed the lease. Keep
// its key until a response arrives, including across navigation and reloads.
const pending = new Map<string, string>();

export async function acquireWithRetryKey(poolId: string) {
  const storageKey = `firecrab.pool.acquire.${poolId}`;
  let key = pending.get(poolId);
  try { key ??= sessionStorage.getItem(storageKey) ?? undefined; } catch { /* Storage can be disabled. */ }
  if (!key) {
    // getRandomValues also works on HTTP hosts where randomUUID is unavailable.
    key = Array.from(crypto.getRandomValues(new Uint8Array(16)), (byte) => byte.toString(16).padStart(2, "0")).join("");
  }
  pending.set(poolId, key);
  try { sessionStorage.setItem(storageKey, key); } catch { /* Keep the in-memory key. */ }
  const lease = await acquirePoolLease(poolId, key);
  pending.delete(poolId);
  try { sessionStorage.removeItem(storageKey); } catch { /* Storage can be disabled. */ }
  return lease;
}
