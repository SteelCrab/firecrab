import {
  base,
  type Tab,
  type Version,
  type FileData,
  type Comparison,
  type FileOperation,
} from "./types";
import { request, message } from "./request";
type Changed = () => void;
export async function operations(t: Tab, changed: Changed) {
  t.operationsReady = false;
  changed();
  try {
    t.operations = await request<FileOperation[]>(
      "GET",
      base(t.project) + "/file-operations",
    );
    t.operationsReady = true;
    if (
      t.restore &&
      t.operations.some(
        (op) =>
          op.id === t.restore?.id && ["applied", "failed"].includes(op.status),
      )
    ) {
      t.restore = undefined;
      t.comparison = undefined;
    }
  } catch (error) {
    t.error = message(error);
  }
  changed();
}
export async function history(
  t: Tab,
  changed: Changed,
  path?: string,
  before?: number,
) {
  const generation = ++t.historyGeneration;
  t.historyLoading = true;
  if (before === undefined) t.historyPath = path;
  changed();
  try {
    const rows = await request<Version[]>(
      "GET",
      base(t.project) +
        "/files/history?path=" +
        encodeURIComponent(path || "") +
        (before === undefined ? "" : "&before=" + before),
    );
    if (t.historyGeneration !== generation) return;
    if (before === undefined) t.history = [];
    t.historyMore = rows.length === 100;
    for (const row of rows)
      if (!t.history.some((old) => old.id === row.id)) t.history.push(row);
  } catch (error) {
    if (t.historyGeneration === generation) t.error = message(error);
  } finally {
    if (t.historyGeneration === generation) t.historyLoading = false;
    changed();
  }
  if (before === undefined) void operations(t, changed);
}
export async function compare(t: Tab, changed: Changed, id: number) {
  if (t.revisionsBusy) return;
  const item = t.history.find((row) => row.id === id);
  if (!item) return;
  t.revisionsBusy = true;
  t.error = "";
  changed();
  try {
    const file = await request<FileData>(
      "GET",
      base(t.project) + "/files?path=" + encodeURIComponent(item.path),
    );
    t.comparison = await request<Comparison>(
      "GET",
      base(t.project) +
        "/files/diff?path=" +
        encodeURIComponent(item.path) +
        "&from=" +
        id +
        "&to=" +
        file.version.id,
    );
    t.revisionMessage = "";
  } catch (error) {
    t.error = message(error);
  } finally {
    t.revisionsBusy = false;
    changed();
  }
}
export async function restore(t: Tab, changed: Changed, retry = false) {
  if (t.revisionsBusy) return;
  if (!retry) {
    if (
      !t.operationsReady ||
      t.restore ||
      t.operations.some((op) => op.status === "pending") ||
      !t.comparison
    )
      return;
    const d = t.comparison;
    if (
      d.before.version.path !== d.after.version.path ||
      !d.before.version.id ||
      !d.after.version.id
    )
      return;
    t.restore = {
      id: crypto.randomUUID(),
      path: d.before.version.path,
      version: d.before.version.id,
      expectedVersion: d.after.version.id,
    };
  }
  const pending = t.restore;
  if (!pending) return;
  t.revisionsBusy = true;
  t.error = "";
  changed();
  try {
    await request(
      "POST",
      base(t.project) +
        "/files/restore?path=" +
        encodeURIComponent(pending.path),
      {
        operationId: pending.id,
        version: pending.version,
        expectedVersion: pending.expectedVersion,
      },
    );
    t.restore = undefined;
    t.comparison = undefined;
    t.revisionMessage = "File restored as a new version";
  } catch (error) {
    t.error = message(error);
  } finally {
    t.revisionsBusy = false;
    changed();
  }
  await history(t, changed);
}
export async function reconcile(t: Tab, changed: Changed, id: string) {
  if (t.revisionsBusy) return;
  t.revisionsBusy = true;
  t.error = "";
  changed();
  try {
    await request(
      "POST",
      base(t.project) +
        "/file-operations/" +
        encodeURIComponent(id) +
        "/reconcile",
      {},
    );
    if (t.restore?.id === id) t.restore = undefined;
    t.comparison = undefined;
    t.revisionMessage = "Saved outcome recovered";
  } catch (error) {
    t.error = message(error);
  } finally {
    t.revisionsBusy = false;
    changed();
  }
  await history(t, changed);
}
