import { canvasPanes } from "../canvas-panes.js";
import {
  base,
  type Tab,
  type EditorClient,
  type ClientState,
  type Command,
} from "./types";
import { request, pause } from "./request";

type Changed = () => void;
const surface = (action: string, t: Tab, c: EditorClient, value?: boolean) =>
  canvasPanes(
    action,
    JSON.stringify({
      id: t.id,
      client: c.id,
      windowProof: c.windowProof,
      value,
    }),
  );
function valid(state: ClientState, client: EditorClient): ClientState {
  if (
    state.id !== client.id ||
    state.instance !== client.instance ||
    !Number.isSafeInteger(state.revision)
  )
    throw new Error(
      "The editor browser identity changed; keep its current document",
    );
  return state;
}
const path = (t: Tab, c: EditorClient) =>
  base(t.project) + "/editors/" + t.id + "/clients/" + c.id;
async function state(t: Tab, c: EditorClient) {
  return valid(await request<ClientState>("GET", path(t, c)), c);
}
function proof(t: Tab, c: EditorClient): EditorClient {
  const value = surface("editor-proof", t, c);
  if (typeof value !== "string" || !value)
    throw new Error(
      "The editor window is still connecting. Its document is retained; retry when it is ready.",
    );
  c.windowProof = value;
  return c;
}
async function command(
  t: Tab,
  c: EditorClient,
  current: ClientState,
  action: string,
): Promise<ClientState> {
  if (t.client !== c || !c.windowProof)
    throw new Error("The canvas or editor changed; keep its current document");
  const old = c.pending;
  const body: Command =
    old?.action === action &&
    old.expectedRevision === current.revision &&
    old.windowProof === c.windowProof
      ? old
      : {
          operationId: crypto.randomUUID(),
          expectedRevision: current.revision,
          action,
          windowProof: c.windowProof,
        };
  c.pending = body;
  const result = valid(
    await request<ClientState>("PUT", path(t, c) + "/control", body),
    c,
  );
  if (t.client !== c)
    throw new Error("The editor changed during its control request");
  c.pending = undefined;
  return result;
}
export async function detachEditor(t: Tab, changed: Changed): Promise<void> {
  const c = t.client;
  if (!c) {
    if (
      !t.editorOpened ||
      canvasPanes("has-editor", JSON.stringify({ id: t.id })) === false
    )
      return;
    throw new Error(
      "This earlier editor cannot confirm a safe browser close. Preserve its drafts before reopening.",
    );
  }
  if (surface("freeze-editor", t, c, true) !== true)
    throw new Error(
      "The editor surface changed; its browser document was retained",
    );
  proof(t, c);
  let current = await state(t, c);
  if (
    !["close", "detach"].includes(current.command) ||
    current.status === "error" ||
    current.windowProof !== c.windowProof
  )
    current = await command(t, c, current, "close");
  const deadline = Date.now() + 20000;
  while (!(current.command === "detach" && current.status === "detached")) {
    if (t.client !== c)
      throw new Error("The canvas changed while its editor was closing");
    if (current.command !== "close")
      throw new Error(
        "The browser close command changed; keep the editor and retry",
      );
    if (current.status === "ready") {
      current = await command(t, c, current, "detach");
      continue;
    }
    if (current.status === "error")
      throw new Error(current.error || "Private draft saving failed");
    if (Date.now() > deadline)
      throw new Error(
        "Private draft saving is not confirmed. The editor remains open. Retry closing or keep editing.",
      );
    await pause(250);
    current = await state(t, c);
  }
  if (surface("detach-editor", t, c, true) !== true)
    throw new Error(
      "The editor surface changed; its browser document was retained",
    );
  t.client = undefined;
  t.editor = undefined;
  t.editorOpened = false;
  changed();
}
export async function keepEditor(t: Tab, changed: Changed): Promise<void> {
  const c = t.client;
  if (!c) {
    t.closing = undefined;
    changed();
    return;
  }
  proof(t, c);
  let current = await state(t, c);
  if (current.command !== "cancel" || current.windowProof !== c.windowProof)
    current = await command(t, c, current, "cancel");
  const deadline = Date.now() + 20000;
  while (
    current.status !== "active" ||
    current.command !== "cancel" ||
    current.windowProof !== c.windowProof
  ) {
    if (Date.now() > deadline)
      throw new Error(
        "The editor has not confirmed that editing resumed. Keep its document and retry.",
      );
    await pause(250);
    current = await state(t, c);
  }
  if (surface("freeze-editor", t, c, false) !== true)
    throw new Error("The editor window changed before resume");
  t.closing = undefined;
  t.error = "";
  changed();
}
export async function openEditor(
  t: Tab,
  file: string | undefined,
  changed: Changed,
): Promise<void> {
  const opened = await request<{
    id: string;
    instance: string;
    workingDirectory: string;
    host: string;
    browserProtocol: number;
  }>("PUT", base(t.project) + "/editors/" + t.id, {
    viewId: t.id,
    workingDirectory: t.layout.directory,
    file: file ?? null,
    requestId: crypto.randomUUID(),
  });
  if (
    t.client?.instance === opened.instance &&
    t.client.directory === opened.workingDirectory
  ) {
    t.editor = t.client.url;
    changed();
    return;
  }
  if (t.client) {
    t.closing = "editor";
    changed();
    await detachEditor(t, changed);
  }
  if (!opened.host.endsWith(".editor.localhost") || /[/:]/.test(opened.host))
    throw new Error("Invalid editor origin");
  const id = opened.browserProtocol === 1 ? crypto.randomUUID() : undefined;
  const url = new URL(
    location.protocol +
      "//" +
      opened.host +
      (location.port ? ":" + location.port : "") +
      "/",
  );
  url.searchParams.set(
    "folder",
    "firecrab://" + opened.id + "/" + opened.workingDirectory,
  );
  if (id) url.searchParams.set("firecrabClient", id);
  if (
    canvasPanes(
      "can-attach-editor",
      JSON.stringify({ id: t.id, url: url.href, client: id ?? null }),
    ) !== true
  )
    throw new Error(
      "The previous browser editor is retained. Preserve its recovery data before replacing it.",
    );
  if (id)
    await request(
      "PUT",
      base(t.project) + "/editors/" + t.id + "/clients/" + id,
      { instance: opened.instance },
    );
  t.client = id
    ? {
        id,
        instance: opened.instance,
        directory: opened.workingDirectory,
        url: url.href,
      }
    : undefined;
  t.editor = url.href;
  t.editorOpened = true;
  t.closing = undefined;
  changed();
}
