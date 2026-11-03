import {
  validSkillSource,
  type SkillInstall,
  type SkillInstallation,
  type SkillRef,
} from "./skills.ts";
import { request, WorkspaceError, message } from "./request.ts";
import { base, type Tab, type Node } from "./types.ts";
import {
  agentDocument,
  agentLabel,
  assertTask,
  selectionKey,
  activeTask,
  type AgentDocument,
  type AgentState,
  type AgentTask,
  type Choice,
  type Selection,
  type Context,
} from "./agent-types.ts";
type Api = typeof request;
type Host = {
  edit: (t: Tab) => void;
  changed: () => void;
  persist: (t: Tab) => Promise<void>;
  owned: (t: Tab, n: Node) => boolean;
};
const tasks = (p: string) =>
  "/api/ai-sandbox/projects/" + encodeURIComponent(p) + "/tasks";
const taskPath = (id: string) =>
  "/api/ai-sandbox/tasks/" + encodeURIComponent(id);
export class FolderAgents {
  private states = new Map<string, AgentState>();
  private host: Host;
  private api: Api;
  constructor(host: Host, api: Api = request) {
    this.host = host;
    this.api = api;
  }
  get(t: Tab, n: Node): AgentState {
    const d = agentDocument(n, t.project),
      scope = JSON.stringify([t.project, d?.directory, agentLabel(n)]),
      key = t.id + "/" + n.id;
    let state = this.states.get(key);
    if (!state || state.scope !== scope) {
      state = {
        scope,
        models: [],
        rows: [],
        loaded: false,
        refreshing: false,
        busy: false,
        more: false,
        error: d
          ? ""
          : "Invalid saved agent session; no request will be dispatched",
      };
      this.states.set(key, state);
    }
    return state;
  }
  forget(t: Tab, nodeId?: string) {
    for (const key of this.states.keys())
      if (nodeId ? key === t.id + "/" + nodeId : key.startsWith(t.id + "/"))
        this.states.delete(key);
  }
  private valid(t: Tab, n: Node, s: AgentState) {
    return this.host.owned(t, n) && this.get(t, n) === s;
  }
  private document(t: Tab, n: Node) {
    const d = agentDocument(n, t.project);
    if (!d) throw new Error("Invalid saved agent session");
    return d;
  }
  private store(t: Tab, n: Node, d: AgentDocument) {
    n.content = JSON.stringify(d);
    this.host.edit(t);
  }
  change(
    t: Tab,
    n: Node,
    patch: {
      draft?: string;
      selection?: Selection;
      shell?: boolean;
      skills?: SkillRef[];
    },
  ) {
    const d = this.document(t, n);
    if (
      (patch.selection ||
        patch.shell !== undefined ||
        patch.skills !== undefined) &&
      d.pending
    )
      return;
    this.store(t, n, { ...d, ...patch });
  }
  async models(t: Tab, n: Node, discover = false) {
    if (!agentDocument(n, t.project) || !this.host.owned(t, n)) return;
    const s = this.get(t, n);
    if (s.busy) return;
    s.busy = true;
    s.error = "";
    this.host.changed();
    try {
      const connections = await this.api<
        {
          id: string;
          name: string;
          provider: string;
          authentication?: string;
        }[]
      >("GET", "/api/ai-sandbox/connections");
      const catalogs = await Promise.allSettled(
        connections.map(async (connection) => {
          const catalog = await this.api<{
            models: {
              id: string;
              name: string;
              verified: boolean;
              toolsVerified: boolean;
            }[];
          }>(
            "GET",
            "/api/ai-sandbox/connections/" +
              encodeURIComponent(connection.id) +
              "/models" +
              (discover ? "?discover=true" : ""),
          );
          const provider =
            connection.provider === "anthropic"
              ? "Claude"
              : connection.authentication === "chatgpt"
                ? "ChatGPT"
                : "OpenAI";
          return catalog.models.map(
            (m): Choice => ({
              selection: { connectionId: connection.id, model: m.id },
              label: provider + " · " + connection.name + " · " + m.name,
              source: validSkillSource(connection.provider)
                ? connection.provider
                : undefined,
              verified: m.verified && m.toolsVerified,
            }),
          );
        }),
      );
      if (!this.valid(t, n, s)) return;
      s.models = catalogs.flatMap((r) =>
        r.status === "fulfilled" ? r.value : [],
      );
      s.error = catalogs
        .filter((r) => r.status === "rejected")
        .map((r) => message(r.reason))
        .join("; ");
    } catch (error) {
      if (this.valid(t, n, s)) s.error = message(error);
    } finally {
      s.busy = false;
      this.host.changed();
    }
  }
  async installSkill(
    t: Tab,
    n: Node,
    input?: SkillInstall,
  ): Promise<SkillInstallation | undefined> {
    if (!agentDocument(n, t.project) || !this.host.owned(t, n)) return;
    const s = this.get(t, n);
    if (s.busy || s.refreshing) return;
    s.busy = true;
    s.error = "";
    this.host.changed();
    let fresh = false;
    try {
      let d = this.document(t, n);
      fresh = !d.skillInstall;
      if (
        d.skillInstall &&
        input &&
        ["source", "kind", "path", "revision"].some(
          (key) =>
            d.skillInstall!.request[key as keyof SkillInstall] !==
            input[key as keyof SkillInstall],
        )
      )
        throw new Error(
          "Resolve the saved installation before selecting another package",
        );
      if (!d.skillInstall) {
        if (!input) throw new Error("No saved installation request");
        this.store(t, n, {
          ...d,
          skillInstall: { id: crypto.randomUUID(), request: input },
        });
      }
      d = this.document(t, n);
      const pending = d.skillInstall!;
      await this.host.persist(t);
      if (!this.valid(t, n, s)) return;
      const result = await this.api<SkillInstallation>(
        "PUT",
        base(t.project) + "/skills/installations/" + pending.id,
        pending.request,
      );
      if (
        !["installed", "removed"].includes(result.status) ||
        result.id !== pending.id ||
        result.entry.source !== pending.request.source ||
        result.entry.path !== pending.request.path ||
        result.entry.revision !== pending.request.revision ||
        result.entry.kind !== pending.request.kind
      )
        throw new Error("Installed skill differs from the saved request");
      if (
        this.valid(t, n, s) &&
        this.document(t, n).skillInstall?.id === pending.id
      )
        this.store(t, n, { ...this.document(t, n), skillInstall: undefined });
      return result;
    } catch (error) {
      // A completed rejection plus an absent server record proves there was no install admission.
      if (
        fresh &&
        error instanceof WorkspaceError &&
        error.status !== undefined &&
        error.status >= 400 &&
        error.status < 500 &&
        this.valid(t, n, s)
      ) {
        const pending = this.document(t, n).skillInstall;
        if (pending)
          try {
            await this.api(
              "GET",
              base(t.project) + "/skills/installations/" + pending.id,
            );
          } catch (lookup) {
            if (
              lookup instanceof WorkspaceError &&
              lookup.status === 404 &&
              this.valid(t, n, s) &&
              this.document(t, n).skillInstall?.id === pending.id
            )
              this.store(t, n, {
                ...this.document(t, n),
                skillInstall: undefined,
              });
          }
      }
      if (this.valid(t, n, s)) s.error = message(error);
    } finally {
      s.busy = false;
      this.host.changed();
    }
  }
  discardSkillInstallDraft(t: Tab, n: Node) {
    const s = this.get(t, n);
    if (
      s.busy ||
      s.refreshing ||
      !agentDocument(n, t.project) ||
      !this.host.owned(t, n)
    )
      return;
    this.store(t, n, { ...this.document(t, n), skillInstall: undefined });
  }
  async verify(t: Tab, n: Node) {
    const d = this.document(t, n),
      s = this.get(t, n);
    if (s.busy || !d.selection || d.pending) return;
    s.busy = true;
    s.error = "";
    this.host.changed();
    try {
      await this.api(
        "POST",
        "/api/ai-sandbox/connections/" +
          encodeURIComponent(d.selection.connectionId) +
          "/verify",
        { model: d.selection.model, tools: true },
      );
    } catch (error) {
      s.error = message(error);
    } finally {
      s.busy = false;
      this.host.changed();
    }
    if (!s.error && this.valid(t, n, s)) await this.models(t, n);
  }
  private async context(t: Tab, n: Node, s: AgentState, id: string) {
    const snapshot = await this.api<Context>(
      "GET",
      base(t.project) + "/instructions/snapshots/" + encodeURIComponent(id),
    );
    if (this.valid(t, n, s) && s.task?.request.instructionSnapshotId === id)
      s.context = {
        id: snapshot.id,
        files: snapshot.files.map(({ path, version }) => ({ path, version })),
      };
  }
  async refresh(t: Tab, n: Node, before?: string) {
    if (!agentDocument(n, t.project) || !this.host.owned(t, n)) return;
    const s = this.get(t, n);
    if (s.busy || s.refreshing) return;
    const d = this.document(t, n);
    s.refreshing = true;
    this.host.changed();
    try {
      const q = new URLSearchParams({
        projectDirectory: d.directory,
        assignedAgent: agentLabel(n),
      });
      if (before) q.set("before", before);
      const rows = await this.api<AgentTask[]>(
        "GET",
        tasks(t.project) + "?" + q,
      );
      for (const row of rows)
        assertTask(row, t.project, d.directory, agentLabel(n));
      if (!this.valid(t, n, s)) return;
      const known = new Set(s.rows.map((r) => r.id)),
        incoming = rows.filter((r) => !known.has(r.id));
      const updates = new Map(rows.map((r) => [r.id, r]));
      s.rows = before
        ? [...incoming, ...s.rows.map((r) => updates.get(r.id) || r)]
        : [...s.rows.map((r) => updates.get(r.id) || r), ...incoming];
      if (before || !s.loaded) s.more = rows.length === 50;
      s.loaded = true;
      const id = d.pending?.id || d.taskId;
      if (id) {
        try {
          const task = await this.api<AgentTask>("GET", taskPath(id));
          assertTask(task, t.project, d.directory, agentLabel(n), id);
          if (!this.valid(t, n, s)) return;
          s.task = task;
          if (d.pending) this.accept(t, n, s, task, d.pending);
          if (s.context?.id !== task.request.instructionSnapshotId) {
            try {
              await this.context(t, n, s, task.request.instructionSnapshotId);
            } catch {
              /* The task retains its captured rules even if the selector row is unavailable. */
            }
          }
        } catch (error) {
          if (
            !(
              error instanceof WorkspaceError &&
              error.status === 404 &&
              d.pending
            )
          )
            throw error;
        }
      }
    } catch (error) {
      if (this.valid(t, n, s)) s.error = message(error);
    } finally {
      s.refreshing = false;
      this.host.changed();
    }
  }
  private accept(
    t: Tab,
    n: Node,
    s: AgentState,
    task: AgentTask,
    pending: NonNullable<AgentDocument["pending"]>,
  ) {
    if (
      !!task.request.projectShell !== !!pending.request.projectShell ||
      JSON.stringify(task.request.selectedSkillIds || []) !==
        JSON.stringify(pending.request.selectedSkillIds || []) ||
      task.request.instructionSnapshotId !== pending.snapshot ||
      task.request.input !== pending.request.input.trim() ||
      selectionKey(task.selection) !== selectionKey(pending.request.selection)
    )
      throw new Error("The accepted task differs from the saved request");
    const d = this.document(t, n);
    if (d.pending?.id !== pending.id)
      throw new Error("Saved request identity changed");
    s.task = task;
    this.store(t, n, {
      ...d,
      pending: undefined,
      taskId: task.id,
      draft: d.draft.trim() === pending.request.input.trim() ? "" : d.draft,
    });
  }
  async submit(t: Tab, n: Node) {
    if (!agentDocument(n, t.project) || !this.host.owned(t, n)) return;
    const s = this.get(t, n);
    if (s.busy || s.refreshing) return;
    let d = this.document(t, n);
    const fresh = !d.pending;
    if (!d.pending) {
      if (
        !s.loaded ||
        t.workspace?.status !== "ready" ||
        !d.draft.trim() ||
        activeTask(s.task) ||
        s.rows.some(activeTask) ||
        !s.models.some(
          (m) =>
            m.verified &&
            selectionKey(m.selection) === selectionKey(d.selection),
        )
      )
        return;
      const choice = s.models.find(
        (m) => selectionKey(m.selection) === selectionKey(d.selection),
      )!;
      const snapshot = crypto.randomUUID();
      d = {
        ...d,
        pending: {
          id: crypto.randomUUID(),
          snapshot,
          request: {
            kind: "code",
            ...(d.shell ? { projectShell: true as const } : {}),
            ...((d.skills || []).some((s) => s.source === choice.source)
              ? {
                  selectedSkillIds: (d.skills || [])
                    .filter((s) => s.source === choice.source)
                    .map((s) => s.id),
                }
              : {}),
            input: d.draft.trim(),
            projectDirectory: d.directory,
            instructionSnapshotId: snapshot,
            selection: { ...d.selection! },
            coordination: { assignedAgent: agentLabel(n) },
          },
        },
      };
      this.store(t, n, d);
    }
    const pending = d.pending!;
    s.busy = true;
    s.error = "";
    this.host.changed();
    try {
      // Nothing reaches the runtime until the exact request IDs are in the private saved view.
      await this.host.persist(t);
      if (!this.valid(t, n, s)) return;
      let task: AgentTask;
      try {
        task = await this.api<AgentTask>("GET", taskPath(pending.id));
      } catch (error) {
        if (!(error instanceof WorkspaceError && error.status === 404))
          throw error;
        await this.api(
          "PUT",
          base(t.project) + "/instructions/snapshots/" + pending.snapshot,
          { directory: d.directory },
        );
        if (!this.valid(t, n, s)) return;
        task = await this.api<AgentTask>(
          "PUT",
          tasks(t.project) + "/" + pending.id,
          pending.request,
        );
      }
      assertTask(task, t.project, d.directory, agentLabel(n), pending.id);
      if (!this.valid(t, n, s)) return;
      this.accept(t, n, s, task, pending);
      await this.host.persist(t);
    } catch (error) {
      // A fresh rejected admission may be edited; an earlier ambiguous request keeps its identity.
      if (
        fresh &&
        error instanceof WorkspaceError &&
        error.status !== undefined &&
        [400, 404, 409, 422].includes(error.status) &&
        this.valid(t, n, s)
      ) {
        try {
          await this.api("GET", taskPath(pending.id));
        } catch (lookup) {
          if (
            lookup instanceof WorkspaceError &&
            lookup.status === 404 &&
            this.valid(t, n, s) &&
            this.document(t, n).pending?.id === pending.id
          )
            this.store(t, n, { ...this.document(t, n), pending: undefined });
        }
      }
      if (this.valid(t, n, s)) s.error = message(error);
    } finally {
      s.busy = false;
      this.host.changed();
    }
    if (this.valid(t, n, s)) await this.refresh(t, n);
  }
  async inspect(t: Tab, n: Node, id: string) {
    const s = this.get(t, n),
      d = this.document(t, n);
    if (s.busy || s.refreshing || d.pending) return;
    this.store(t, n, { ...d, taskId: id });
    await this.refresh(t, n);
  }
  async control(t: Tab, n: Node, action: "cancel" | "reconcile") {
    const s = this.get(t, n);
    if (s.busy || s.refreshing || !s.task) return;
    const id = s.task.id;
    s.busy = true;
    s.error = "";
    this.host.changed();
    try {
      await this.api("POST", taskPath(id) + "/" + action, {});
    } catch (error) {
      s.error = message(error);
    } finally {
      s.busy = false;
      this.host.changed();
    }
    if (this.valid(t, n, s)) await this.refresh(t, n);
  }
}
