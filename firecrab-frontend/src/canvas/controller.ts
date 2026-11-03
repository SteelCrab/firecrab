import {
  base,
  viewPath,
  blankLayout,
  newTab,
  validLayout,
  type Tab,
  type Project,
  type View,
  type Entry,
  type Node,
  type Workspace,
  type TerminalRecord,
} from "./types";
import { request, message, WorkspaceError } from "./request";
import { openEditor, detachEditor, keepEditor } from "./editor";
import { history, operations, compare, restore, reconcile } from "./history";
import { FolderAgents } from "./agents";
import { pause } from "./request";
import { terminalSessions } from "./terminals";

export class CanvasController {
  tabs: Tab[] = [];
  active = "";
  projects: Project[] = [];
  error = "";
  version = 0;
  private listeners = new Set<() => void>();
  private started = false;
  subscribe = (listener: () => void) => {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  };
  snapshot = () => this.version;
  changed = () => {
    this.version++;
    for (const fn of this.listeners) fn();
  };
  agents = new FolderAgents({
    edit: (t) => this.edit(t),
    changed: () => this.changed(),
    persist: (t) => this.flush(t),
    owned: (t, n) => this.tabs.includes(t) && t.layout.nodes.includes(n),
  });
  async flush(t: Tab) {
    const target = t.changed,
      deadline = Date.now() + 30000;
    while (t.saved < target || t.pending) {
      if (!this.tabs.includes(t) || Date.now() > deadline)
        throw new Error(
          "Confirm the private canvas save before dispatching this request",
        );
      if (t.saving) {
        await pause(50);
        continue;
      }
      await this.save(t);
      if (t.error) throw new Error(t.error);
    }
  }
  createAgent(t: Tab) {
    const id = crypto.randomUUID();
    this.add(
      t,
      "agent",
      id,
      JSON.stringify({
        schema: 1,
        project: t.project,
        directory: t.layout.directory,
        draft: "",
      }),
    );
  }
  current = () => this.tabs.find((t) => t.id === this.active);
  dirty = () => this.tabs.some((t) => t.changed !== t.saved || !!t.pending);
  remember() {
    try {
      localStorage.setItem(
        "firecrab.canvas.tabs",
        JSON.stringify({
          tabs: this.tabs.map(({ id, project }) => ({ id, project })),
          active: this.active,
        }),
      );
    } catch {
      /* Content remains in the server and this live view. */
    }
  }
  async start() {
    if (!this.started) {
      this.started = true;
      try {
        const saved = JSON.parse(
          localStorage.getItem("firecrab.canvas.tabs") || "{}",
        );
        if (Array.isArray(saved.tabs))
          for (const item of saved.tabs.slice(0, 24))
            if (
              typeof item.id === "string" &&
              item.id &&
              !this.tabs.some((t) => t.id === item.id) &&
              typeof item.project === "string"
            )
              this.tabs.push(newTab(item.id, item.project));
        this.active = this.tabs.some((t) => t.id === saved.active)
          ? saved.active
          : this.tabs[0]?.id || "";
      } catch {
        /* Invalid local identities never become saved content. */
      }
      if (!this.tabs.length) this.newCanvas();
    }
    try {
      this.projects = await request<Project[]>(
        "GET",
        "/api/ai-sandbox/projects",
      );
      this.error = "";
    } catch (error) {
      this.error = message(error);
    }
    this.changed();
    await this.open(this.current());
  }
  newCanvas() {
    if (this.tabs.length >= 24) return;
    const t = newTab();
    this.tabs.push(t);
    this.active = t.id;
    this.remember();
    this.changed();
  }
  select(id: string) {
    if (!this.tabs.some((t) => t.id === id)) return;
    this.active = id;
    this.remember();
    this.changed();
    void this.open(this.current());
  }
  async connect(project: string) {
    const t = this.current();
    if (!t || t.project || !this.projects.some((p) => p.id === project)) return;
    t.project = project;
    this.remember();
    await this.open(t);
  }
  async run(t: Tab, work: () => Promise<void>) {
    if (t.busy) return;
    t.busy = true;
    t.error = "";
    this.changed();
    try {
      await work();
    } catch (error) {
      t.error = message(error);
    } finally {
      t.busy = false;
      this.changed();
    }
  }
  async open(t?: Tab) {
    if (!t || !t.project || t.loaded || t.busy) return;
    await this.run(t, async () => {
      const workspace = await request<Workspace>("GET", base(t.project));
      let view: Pick<View, "revision" | "document">;
      try {
        view = await request<View>("GET", viewPath(t));
      } catch (error) {
        if (error instanceof WorkspaceError && error.status === 404)
          view = { revision: 0, document: blankLayout() };
        else throw error;
      }
      view.document.directory ??= "";
      for (const node of view.document.nodes || []) node.content ??= "";
      if (!validLayout(view.document))
        throw new Error("Saved canvas format or bounds are invalid");
      if (!this.tabs.includes(t)) return;
      t.workspace = workspace;
      t.layout = view.document;
      t.revision = view.revision;
      t.loaded = true;
      if (!t.revision) t.changed = 1;
    });
    if (t.loaded) {
      void this.save(t);
      void this.files(t);
      void this.catalog(t);
      void this.views(t);
    }
  }
  async save(t = this.current()) {
    if (
      !t?.loaded ||
      t.saving ||
      !t.project ||
      (t.changed === t.saved && !t.pending)
    )
      return;
    const pending = t.pending ?? {
      changed: t.changed,
      body: {
        operationId: crypto.randomUUID(),
        expectedRevision: t.revision,
        document: structuredClone(t.layout),
      },
    };
    t.pending = pending;
    t.saving = true;
    this.changed();
    try {
      const response = await request<View>(
        "PUT",
        viewPath(t),
        pending.body,
        false,
      );
      t.revision = response.revision;
      t.saved = pending.changed;
      t.pending = undefined;
      t.retryable = false;
      t.error = "";
    } catch (error) {
      t.retryable =
        error instanceof WorkspaceError &&
        (error.status === undefined || error.code === "workspace_busy");
      t.error = message(error);
    } finally {
      t.saving = false;
      this.changed();
    }
  }
  edit(t: Tab) {
    t.changed++;
    this.changed();
  }
  hideEditor(id: string) {
    const t = this.tabs.find((item) => item.id === id);
    if (t) {
      t.editor = undefined;
      this.changed();
    }
  }
  hideComparison(id: string) {
    const t = this.tabs.find((item) => item.id === id);
    if (t) {
      t.comparison = undefined;
      this.changed();
    }
  }
  note(id: string, nodeId: string, value: string) {
    const t = this.tabs.find((item) => item.id === id);
    const node = t?.layout.nodes.find((item) => item.id === nodeId);
    if (t && node?.kind === "note") {
      node.content = value;
      this.edit(t);
    }
  }
  reset(id: string) {
    const t = this.tabs.find((item) => item.id === id);
    if (t) {
      t.layout.x = 40;
      t.layout.y = 55;
      t.layout.zoom = 1;
      this.edit(t);
    }
  }
  move(
    id: string,
    nodeId: string | undefined,
    resize: boolean,
    x: number,
    y: number,
  ) {
    const t = this.tabs.find((item) => item.id === id);
    if (!t) return;
    if (nodeId) {
      const node = t.layout.nodes.find((item) => item.id === nodeId);
      if (!node) return;
      if (resize) {
        node.width = Math.min(1200, Math.max(240, x));
        node.height = Math.min(1200, Math.max(180, y));
      } else {
        node.x = x;
        node.y = y;
      }
    } else {
      t.layout.x = x;
      t.layout.y = y;
    }
    this.edit(t);
  }
  zoom(id: string, dx: number, x: number, y: number) {
    const t = this.tabs.find((item) => item.id === id);
    if (!t) return;
    const l = t.layout;
    const next = Math.min(2, Math.max(0.25, l.zoom * Math.exp(-dx * 0.002)));
    l.x = x - ((x - l.x) * next) / l.zoom;
    l.y = y - ((y - l.y) * next) / l.zoom;
    l.zoom = next;
    this.edit(t);
  }

  fork() {
    const source = this.current();
    if (!source?.loaded || this.tabs.length >= 24) return;
    const t = newTab(undefined, source.project);
    t.layout = structuredClone(source.layout);
    t.workspace = source.workspace;
    t.files = [...source.files];
    t.loaded = true;
    t.changed = 1;
    this.tabs.push(t);
    this.active = t.id;
    this.remember();
    this.changed();
  }
  async close(t: Tab, kind: "tab" | "editor") {
    if (t.busy) return;
    if (kind === "tab" && (t.changed !== t.saved || t.pending)) {
      t.error = "Save this canvas layout before closing it";
      this.changed();
      return;
    }
    t.closing = kind;
    await this.run(t, () => detachEditor(t, this.changed));
    if (t.error) return;
    t.closing = undefined;
    if (kind === "tab") {
      this.agents.forget(t);
      this.tabs = this.tabs.filter((item) => item !== t);
      if (this.active === t.id) this.active = this.tabs[0]?.id || "";
      if (!this.tabs.length) this.newCanvas();
      this.remember();
    }
    this.changed();
  }
  async keep(t: Tab) {
    await this.run(t, () => keepEditor(t, this.changed));
  }
  async editor(t: Tab, file?: string) {
    if (t.closing) return;
    await this.run(t, () => openEditor(t, file, this.changed));
  }
  async files(t = this.current()) {
    if (!t?.loaded) return;
    const directory = t.layout.directory;
    try {
      const files = await request<Entry[]>(
        "GET",
        base(t.project) + "/files/tree?path=" + encodeURIComponent(directory),
      );
      if (t.layout.directory === directory) t.files = files;
    } catch (error) {
      t.error = message(error);
    }
    this.changed();
  }
  async directory(t: Tab, path: string) {
    t.layout.directory = path;
    this.edit(t);
    await this.files(t);
  }
  async views(t = this.current()) {
    if (!t?.loaded) return;
    try {
      t.views = await request<View[]>("GET", base(t.project) + "/views");
    } catch (error) {
      t.error = message(error);
    }
    this.changed();
  }
  reopen(t: Tab, id: string) {
    if (!t.views.some((v) => v.id === id)) return;
    const existing = this.tabs.find((item) => item.id === id);
    if (existing && existing.project !== t.project) {
      t.error = "This saved canvas conflicts with an open project tab";
      this.changed();
      return;
    }
    if (!existing) {
      if (this.tabs.length >= 24) return;
      this.tabs.push(newTab(id, t.project));
    }
    this.select(id);
  }
  async catalog(t = this.current()) {
    if (!t?.loaded) return;
    try {
      t.terminals = await request<TerminalRecord[]>(
        "GET",
        base(t.project) + "/terminals",
      );
    } catch (error) {
      t.error = message(error);
    }
    this.changed();
  }
  add(
    t: Tab,
    kind: Node["kind"],
    id: string = crypto.randomUUID(),
    content = "",
  ) {
    if (
      t.layout.nodes.length >= 24 ||
      (kind === "note" &&
        t.layout.nodes.filter((n) => n.kind === "note").length >= 6)
    )
      return;
    const n = t.layout.nodes.length;
    t.layout.nodes.push({
      id,
      kind,
      content,
      x: 40 + (n % 2) * 640,
      y: 40 + Math.floor(n / 2) * 420,
      width: kind === "agent" ? 540 : kind === "terminal" ? 600 : 380,
      height: kind === "agent" ? 620 : kind === "terminal" ? 380 : 330,
    });
    this.edit(t);
    if (kind === "history") void this.history(t);
  }
  remove(t: Tab, id: string) {
    this.agents.forget(t, id);
    t.layout.nodes = t.layout.nodes.filter((n) => n.id !== id);
    this.edit(t);
  }
  async createTerminal(t: Tab) {
    if (t.layout.nodes.length >= 24) return;
    const id = crypto.randomUUID();
    const body = {
      label:
        "Terminal " +
        (t.layout.nodes.filter((n) => n.kind === "terminal").length + 1),
      workingDirectory: t.layout.directory,
      rows: 24,
      columns: 80,
    };
    this.add(t, "terminal", id, JSON.stringify(body));
    try {
      await request("PUT", base(t.project) + "/terminals/" + id, body);
      void this.catalog(t);
    } catch (error) {
      t.error = message(error);
      this.changed();
    }
  }
  attachTerminal(t: Tab, id: string) {
    if (
      t.terminals.some((s) => s.id === id) &&
      !t.layout.nodes.some((n) => n.id === id)
    )
      this.add(t, "terminal", id);
  }
  async verify(t: Tab) {
    await this.run(t, async () => {
      t.workspace = await request("POST", base(t.project) + "/reconcile", {});
    });
    void this.files(t);
  }
  history = (t: Tab, path?: string, before?: number) =>
    history(t, this.changed, path, before);
  operations = (t: Tab) => operations(t, this.changed);
  compare = (t: Tab, id: number) => compare(t, this.changed, id);
  restore = (t: Tab, retry = false) => restore(t, this.changed, retry);
  reconcile = (t: Tab, id: string) => reconcile(t, this.changed, id);
  tick() {
    for (const t of this.tabs)
      if (
        t.loaded &&
        (t.changed !== t.saved || t.pending) &&
        (!t.pending || t.retryable)
      )
        void this.save(t);
    terminalSessions.tick(this.tabs, this.changed);
  }
}
export const canvasController = new CanvasController();
