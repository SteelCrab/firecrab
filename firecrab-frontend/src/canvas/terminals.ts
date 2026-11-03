import { base, type Tab, type TerminalRecord, type Control } from "./types";
import { request, message } from "./request";
import { canvasPanes } from "../canvas-panes.js";
type Session = {
  project: string;
  id: string;
  view: string;
  cursor: number;
  epoch: number;
  socket?: WebSocket;
  metadata?: TerminalRecord;
  control?: Control;
  error: string;
  next: number;
};
class TerminalSessions {
  sessions = new Map<string, Session>();
  private installed = false;
  key = (project: string, id: string, view: string) =>
    view + "/" + project + "/" + id;
  get = (project: string, id: string, view: string) =>
    this.sessions.get(this.key(project, id, view));
  send(s: Session, value: unknown) {
    if (s.socket?.readyState !== WebSocket.OPEN)
      throw new Error("Terminal is disconnected");
    s.socket.send(JSON.stringify(value));
  }
  connect(project: string, id: string, changed: () => void, view: string) {
    const key = this.key(project, id, view);
    let s = this.sessions.get(key);
    if (!s) {
      s = { project, id, view, cursor: 0, epoch: 0, error: "", next: 0 };
      this.sessions.set(key, s);
    }
    if (
      s.next > Date.now() ||
      s.socket?.readyState === WebSocket.OPEN ||
      s.socket?.readyState === WebSocket.CONNECTING
    )
      return;
    s.socket?.close();
    const epoch = ++s.epoch;
    const url = new URL(
      base(project) + "/terminals/" + encodeURIComponent(id) + "/socket",
      location.href,
    );
    url.protocol = location.protocol === "https:" ? "wss:" : "ws:";
    url.searchParams.set("cursor", String(s.cursor));
    const socket = new WebSocket(url, "firecrab.terminal.v1");
    s.socket = socket;
    const session = s;
    socket.onopen = () => {
      if (session.epoch === epoch) {
        session.error = "";
        changed();
      }
    };
    socket.onmessage = (event) => {
      if (session.epoch !== epoch || typeof event.data !== "string") return;
      let value;
      try {
        value = JSON.parse(event.data);
      } catch {
        return;
      }
      if (value.type === "state") {
        session.metadata = value.terminal;
        if (
          session.control &&
          (value.terminal.controller?.clientId !== session.control.clientId ||
            value.terminal.controlGeneration !== session.control.generation)
        )
          session.control = undefined;
        canvasPanes(
          "shared",
          JSON.stringify({
            key,
            rows: value.terminal.rows,
            cols: value.terminal.columns,
          }),
        );
        changed();
      } else if (value.type === "output") {
        const page = value.output;
        if (
          !Number.isSafeInteger(page.start) ||
          !Number.isSafeInteger(page.next) ||
          page.next < session.cursor
        )
          return;
        if (page.start > session.cursor)
          session.error = "Earlier output was truncated";
        const bytes = Uint8Array.from(atob(page.contentBase64), (c: string) =>
          c.charCodeAt(0),
        );
        const skip = Math.max(0, session.cursor - page.start);
        if (skip < bytes.length)
          canvasPanes("write", { key, bytes: bytes.slice(skip) });
        session.cursor = page.next;
      } else if (value.type === "control_error") {
        session.control = undefined;
        session.error = value.message;
        changed();
      }
    };
    const lost = () => {
      if (session.epoch !== epoch) return;
      session.control = undefined;
      session.error = "Connection lost; input was not replayed";
      session.next = Date.now() + 3000;
      changed();
    };
    socket.onclose = lost;
    socket.onerror = lost;
  }
  install(changed: () => void) {
    if (this.installed) return;
    this.installed = true;
    document.addEventListener("firecrab-canvas-terminal", (event) => {
      let value;
      try {
        value = JSON.parse((event as CustomEvent<string>).detail);
      } catch {
        return;
      }
      const s = this.sessions.get(value.key);
      if (!s?.control) return;
      const control = s.control;
      try {
        if (value.type === "resize")
          this.send(s, {
            type: "resize",
            control,
            rows: value.rows,
            columns: value.cols,
          });
        else if (
          value.type === "input" &&
          Array.isArray(value.bytes) &&
          value.bytes.length > 0 &&
          value.bytes.length <= 16384 &&
          value.bytes.every(
            (b: unknown) =>
              Number.isInteger(b) && Number(b) >= 0 && Number(b) <= 255,
          )
        )
          this.send(s, {
            type: "input",
            control,
            contentBase64: btoa(String.fromCharCode(...value.bytes)),
          });
      } catch (error) {
        s.error = message(error);
        changed();
      }
    });
  }
  async control(t: Tab, id: string, stop: boolean, changed: () => void) {
    const s = this.get(t.project, id, t.id);
    if (!s) return;
    if (stop && !s.control) return;
    const body = stop
      ? s.control
      : {
          clientId: crypto.randomUUID(),
          expectedGeneration: s.metadata?.controlGeneration || 0,
          takeOver: !!s.metadata?.controller,
        };
    try {
      const result = await request<Control>(
        "POST",
        base(t.project) +
          "/terminals/" +
          encodeURIComponent(id) +
          (stop ? "/stop" : "/control"),
        body,
      );
      s.control = stop
        ? undefined
        : {
            clientId: result.clientId,
            generation: result.generation,
            token: result.token,
          };
      s.error = "";
      canvasPanes(
        "report",
        JSON.stringify({ key: this.key(t.project, id, t.id) }),
      );
    } catch (error) {
      s.error = message(error);
    }
    changed();
  }
  tick(tabs: Tab[], changed: () => void) {
    const keep = new Set(
      tabs.flatMap((t) =>
        t.layout.nodes
          .filter((n) => n.kind === "terminal")
          .map((n) => this.key(t.project, n.id, t.id)),
      ),
    );
    for (const [key, s] of this.sessions) {
      if (!keep.has(key)) {
        if (s.control)
          void request(
            "DELETE",
            base(s.project) + "/terminals/" + s.id + "/control",
            s.control,
          ).catch(() => {});
        s.epoch++;
        s.socket?.close();
        this.sessions.delete(key);
        continue;
      }
      if (s.control)
        try {
          this.send(s, { type: "renew", control: s.control });
        } catch (error) {
          s.error = message(error);
          s.control = undefined;
        }
      this.connect(s.project, s.id, changed, s.view);
    }
  }
}
export const terminalSessions = new TerminalSessions();
