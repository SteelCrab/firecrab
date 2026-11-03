export type Identity = { id: string; project: string };
export type Node = {
  id: string;
  kind: "folder" | "note" | "history" | "terminal" | "agent";
  x: number;
  y: number;
  width: number;
  height: number;
  content: string;
};
export type Layout = {
  schema: 1;
  nodes: Node[];
  x: number;
  y: number;
  zoom: number;
  directory: string;
};
export type Project = { id: string; spec: { name: string } };
export type Workspace = { status: string; initialized?: boolean };
export type Entry = { name: string; directory: boolean };
export type Version = {
  id: number;
  path: string;
  sha256: string;
  byteLength: number;
  createdAt: number;
  operationId?: string;
};
export type FileData = { version: Version; contentBase64: string };
export type Comparison = {
  path: string;
  before: FileData;
  after: FileData;
  identical: boolean;
};
export type FileOperation = {
  id: string;
  path: string;
  status: string;
  error?: string;
};
export type Control = { clientId: string; generation: number; token: string };
export type TerminalRecord = {
  id: string;
  label: string;
  status: string;
  rows: number;
  columns: number;
  controlGeneration: number;
  controller?: { clientId: string } | null;
};
export type ClientState = {
  id: string;
  instance: string;
  revision: number;
  command: string;
  status: string;
  windowProof?: string;
  error?: string;
};
export type Command = {
  operationId: string;
  expectedRevision: number;
  action: string;
  windowProof: string;
};
export type EditorClient = {
  id: string;
  instance: string;
  directory: string;
  url: string;
  pending?: Command;
  windowProof?: string;
};
export type View = { id: string; revision: number; document: Layout };
export type Restore = {
  id: string;
  path: string;
  version: number;
  expectedVersion: number;
};
export type Tab = Identity & {
  layout: Layout;
  revision: number;
  changed: number;
  saved: number;
  saving: boolean;
  retryable: boolean;
  pending?: {
    changed: number;
    body: { operationId: string; expectedRevision: number; document: Layout };
  };
  editor?: string;
  client?: EditorClient;
  editorOpened: boolean;
  closing?: "tab" | "editor";
  workspace?: Workspace;
  files: Entry[];
  terminals: TerminalRecord[];
  views: View[];
  history: Version[];
  historyPath?: string;
  historyGeneration: number;
  historyLoading: boolean;
  historyMore: boolean;
  comparison?: Comparison;
  restore?: Restore;
  operations: FileOperation[];
  operationsReady: boolean;
  revisionsBusy: boolean;
  revisionMessage: string;
  loaded: boolean;
  busy: boolean;
  error: string;
};
export function blankLayout(): Layout {
  return { schema: 1, nodes: [], x: 40, y: 55, zoom: 1, directory: "" };
}
export function newTab(id: string = crypto.randomUUID(), project = ""): Tab {
  return {
    id,
    project,
    layout: blankLayout(),
    revision: 0,
    changed: 0,
    saved: 0,
    saving: false,
    retryable: false,
    editorOpened: false,
    files: [],
    terminals: [],
    views: [],
    history: [],
    historyGeneration: 0,
    historyLoading: false,
    historyMore: false,
    operations: [],
    operationsReady: false,
    revisionsBusy: false,
    revisionMessage: "",
    loaded: false,
    busy: false,
    error: "",
  };
}
export function validLayout(value: Layout): boolean {
  return (
    value?.schema === 1 &&
    Array.isArray(value.nodes) &&
    value.nodes.length <= 24 &&
    Number.isFinite(value.x) &&
    Number.isFinite(value.y) &&
    value.zoom >= 0.25 &&
    value.zoom <= 2 &&
    typeof value.directory === "string" &&
    value.nodes.every(
      (n) =>
        ["folder", "note", "history", "terminal", "agent"].includes(n.kind) &&
        Number.isFinite(n.x) &&
        Number.isFinite(n.y) &&
        n.width >= 240 &&
        n.width <= 1200 &&
        n.height >= 180 &&
        n.height <= 1200 &&
        typeof n.content === "string",
    )
  );
}
export const base = (project: string) =>
  "/api/ai-sandbox/projects/" + encodeURIComponent(project) + "/workspace";
export const viewPath = (tab: Identity) =>
  base(tab.project) + "/views/" + encodeURIComponent(tab.id);
