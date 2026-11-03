import {
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  useSyncExternalStore,
  type PointerEvent as ReactPointerEvent,
} from "react";
import {
  Bot,
  ArrowLeft,
  ArrowUpLeft,
  CopyPlus,
  FileText,
  Folder,
  FolderOpen,
  History,
  Monitor,
  PanelsTopLeft,
  Plus,
  RefreshCw,
  RotateCcw,
  Save,
  SquareTerminal,
  StickyNote,
  X,
  type LucideIcon,
} from "lucide-react";
import { useI18n } from "../i18n";
import { canvasController as c } from "../canvas/controller";
import { canvasPanes } from "../canvas-panes.js";
import { terminalSessions } from "../canvas/terminals";
import type { Tab, Node as CanvasNode } from "../canvas/types";
import "../styles/ai-canvas.css";
import CanvasAgent from "./CanvasAgent";
import ProjectBackups from "./ProjectBackups";
import CreateVm from "./CreateVm";
import AppDialog from "./ui/AppDialog";
import { agentDocument } from "../canvas/agent-types";

type Props = { onUnsavedChange: (dirty: boolean) => void };
const panelIcons = {
  agent: Bot,
  folder: Folder,
  terminal: SquareTerminal,
  note: StickyNote,
  history: History,
};
const button = (
  action: () => void,
  label: string,
  disabled = false,
  Icon?: LucideIcon,
) => (
  <button
    type="button"
    className={Icon ? "cw-icon-label" : undefined}
    onClick={action}
    disabled={disabled}
  >
    {Icon && <Icon size={14} aria-hidden="true" />}
    <span>{label}</span>
  </button>
);
export default function AiCanvas({ onUnsavedChange }: Props) {
  "use no memo";
  const { t, locale, setLocale } = useI18n();
  const version = useSyncExternalStore(c.subscribe, c.snapshot);
  const tab = c.current();
  const board = useRef<HTMLElement>(null);
  const active = useRef(tab);
  useLayoutEffect(() => {
    active.current = tab;
  }, [tab]);
  const drag = useRef<
    | {
        id?: string;
        resize: boolean;
        pointer: number;
        x: number;
        y: number;
        sx: number;
        sy: number;
      }
    | undefined
  >(undefined);
  const [project, setProject] = useState("");
  const [existing, setExisting] = useState("");
  const [savedView, setSavedView] = useState("");
  const [creatingVm, setCreatingVm] = useState(false);
  const [createVmError, setCreateVmError] = useState("");
  const [createdVm, setCreatedVm] = useState("");
  useEffect(() => {
    void c.start();
    terminalSessions.install(c.changed);
    const timer = setInterval(() => c.tick(), 1000);
    return () => {
      clearInterval(timer);
      canvasPanes("prepare");
    };
  }, []);
  useEffect(() => {
    onUnsavedChange(c.dirty());
  }, [version, onUnsavedChange]);
  useLayoutEffect(() => {
    if (!tab?.loaded) {
      canvasPanes("prepare");
      return;
    }
    const surfaces = tab.layout.nodes
      .filter((n) => n.kind === "terminal")
      .map((n) => ({
        id: tab.id + "-" + n.id,
        element: "cw-pty-" + n.id,
        key: terminalSessions.key(tab.project, n.id, tab.id),
        options: { cols: 80, rows: 24 },
      }));
    const keep = c.tabs.flatMap((v) =>
      v.layout.nodes
        .filter((n) => n.kind === "terminal")
        .map((n) => v.id + "-" + n.id),
    );
    canvasPanes(
      "attach",
      JSON.stringify({
        keep,
        surfaces,
        editor: tab.editor
          ? {
              id: tab.id,
              url: tab.editor,
              client: tab.client?.id || "",
              element: "cw-editor-slot",
            }
          : null,
      }),
    );
    for (const n of tab.layout.nodes)
      if (n.kind === "terminal")
        terminalSessions.connect(tab.project, n.id, c.changed, tab.id);
  }, [version, tab]);
  useEffect(() => {
    const element = board.current;
    if (!element) return;
    const wheel = (event: WheelEvent) => {
      const current = active.current;
      if (!current || (!event.ctrlKey && !event.metaKey)) return;
      event.preventDefault();
      const rect = element.getBoundingClientRect();
      c.zoom(
        current.id,
        event.deltaY,
        event.clientX - rect.left,
        event.clientY - rect.top,
      );
    };
    element.addEventListener("wheel", wheel, { passive: false });
    return () => element.removeEventListener("wheel", wheel);
  }, [tab?.id, tab?.loaded]);
  const pointerDown = (event: ReactPointerEvent<HTMLElement>) => {
    if (
      !tab ||
      event.button !== 0 ||
      (event.target as Element).closest(
        "button,input,select,textarea,a,.cw-pty",
      )
    )
      return;
    const target = event.target as Element;
    const card = target.closest<HTMLElement>("[data-node]");
    const resize = !!target.closest(".cw-resize");
    if (card && !resize && !target.closest(".cw-card-header")) return;
    const node = tab.layout.nodes.find((n) => n.id === card?.dataset.node);
    drag.current = {
      id: node?.id,
      resize,
      pointer: event.pointerId,
      x: event.clientX,
      y: event.clientY,
      sx: node ? (resize ? node.width : node.x) : tab.layout.x,
      sy: node ? (resize ? node.height : node.y) : tab.layout.y,
    };
    event.currentTarget.setPointerCapture(event.pointerId);
    event.preventDefault();
  };
  const pointerMove = (event: ReactPointerEvent<HTMLElement>) => {
    const state = drag.current;
    if (!state || !tab || state.pointer !== event.pointerId) return;
    const dx = event.clientX - state.x,
      dy = event.clientY - state.y;
    c.move(
      tab.id,
      state.id,
      state.resize,
      state.sx + dx / (state.id ? tab.layout.zoom : 1),
      state.sy + dy / (state.id ? tab.layout.zoom : 1),
    );
  };
  const title = (item: Tab) =>
    c.projects.find((p) => p.id === item.project)?.spec.name ||
    t("New canvas", "새 캔버스");
  return (
    <div className="cw-app">
      <aside className="cw-sidebar">
        <a className="cw-brand" href="#/ai-canvas">
          <img src="/firecrab.png" alt="" width={24} height={24} />
          <div>
            <strong>firecrab</strong>
            <small>private microvm cloud</small>
          </div>
        </a>
        <p className="cw-nav-label">WORKSPACE</p>
        <button
          type="button"
          className="cw-nav-current"
          onClick={() => {
            if (tab) c.hideEditor(tab.id);
          }}
        >
          <PanelsTopLeft size={16} aria-hidden="true" />
          <span>AI Canvas</span>
        </button>
        {tab?.loaded && (
          <section className="cw-project-nav">
            <h2>{title(tab)}</h2>
            <small>/workspace/shared</small>
            <ProjectBackups key={tab.project} project={tab.project}/>
            {button(
              () => void c.editor(tab),
              t("Open folder", "폴더 열기"),
              false,
              FolderOpen,
            )}
            {button(
              () => void c.editor(tab),
              t("Personal Page · Web VS Code", "개인 페이지 · Web VS Code"),
              false,
              Monitor,
            )}
            <div className="cw-tree">
              {tab.layout.directory &&
                button(
                  () =>
                    void c.directory(
                      tab,
                      tab.layout.directory.split("/").slice(0, -1).join("/"),
                    ),
                  "..",
                  false,
                  ArrowUpLeft,
                )}
              {tab.files.map((file) => {
                const path =
                  (tab.layout.directory ? tab.layout.directory + "/" : "") +
                  file.name;
                return (
                  <button
                    key={path}
                    onClick={() =>
                      file.directory
                        ? void c.directory(tab, path)
                        : void c.editor(tab, path)
                    }
                  >
                    {file.directory ? (
                      <Folder size={14} aria-hidden="true" />
                    ) : (
                      <FileText size={14} aria-hidden="true" />
                    )}
                    <span>{file.name}</span>
                  </button>
                );
              })}
            </div>
            <h3>{t("Project resources", "프로젝트 자료")}</h3>
            {button(
              () => c.add(tab, "folder"),
              t("Shared folder", "공용 작업 폴더"),
              false,
              Folder,
            )}
            {button(
              () => c.add(tab, "history"),
              t("Version history", "버전 기록"),
              false,
              History,
            )}
            {button(
              () => void c.editor(tab, "AGENTS.md"),
              "AGENTS.md",
              false,
              FileText,
            )}
            {button(
              () => c.createAgent(tab),
              t("Folder agent", "폴더 에이전트"),
              false,
              Bot,
            )}
            <h3>{t("Workspace", "작업 공간")}</h3>
            {button(
              () => void c.createTerminal(tab),
              t("Add terminal", "터미널 추가"),
              false,
              SquareTerminal,
            )}
            <div className="cw-terminal-picker">
              <select
                id="cw-existing-terminal"
                aria-label={t("Existing terminals", "기존 터미널")}
                value={existing}
                onChange={(e) => setExisting(e.target.value)}
              >
                <option value="">
                  {t("Choose a retained terminal", "저장된 터미널 선택")}
                </option>
                {tab.terminals.map((s) => (
                  <option key={s.id} value={s.id}>
                    {s.label} · {s.status}
                  </option>
                ))}
              </select>
              {button(
                () => c.attachTerminal(tab, existing),
                t("Attach", "연결"),
              )}
              {button(
                () => void c.catalog(tab),
                t("Refresh sessions", "세션 새로고침"),
                false,
                RefreshCw,
              )}
            </div>
            {button(
              () => c.add(tab, "note"),
              t("Private note", "개인 메모"),
              false,
              StickyNote,
            )}
            {button(
              () => void c.files(tab),
              t("Refresh files", "파일 새로고침"),
              false,
              RefreshCw,
            )}
            <div className="cw-terminal-picker">
              <select
                id="cw-saved-view"
                aria-label={t("Saved canvases", "저장된 캔버스")}
                value={savedView}
                onChange={(e) => setSavedView(e.target.value)}
              >
                <option value="">
                  {t("Choose a private canvas", "개인 캔버스 선택")}
                </option>
                {tab.views.map((v, i) => (
                  <option key={v.id} value={v.id}>
                    {t("Saved canvas", "저장된 캔버스")} {i + 1}
                  </option>
                ))}
              </select>
              {button(() => c.reopen(tab, savedView), t("Reopen", "다시 열기"))}
              {button(
                () => void c.views(tab),
                t("Refresh saved canvases", "저장 목록 새로고침"),
                false,
                RefreshCw,
              )}
            </div>
          </section>
        )}
        <footer>
          <a href="#/vms">{t("Dashboard", "대시보드")}</a>
          <label htmlFor="cw-language">KO / EN</label>
          <select
            id="cw-language"
            value={locale}
            onChange={(e) => setLocale(e.target.value as "en" | "ko")}
          >
            <option value="ko">한국어</option>
            <option value="en">English</option>
          </select>
        </footer>
      </aside>
      <main className="cw-main">
        <header className="cw-header">
          <span>
            AI / <strong>AI Canvas</strong>
          </span>
          <span id="cw-save-state">
            {c.dirty()
              ? t("Unsaved changes", "미저장 변경")
              : t("Saved privately", "개인 화면 저장됨")}
          </span>
          {button(
            () => void c.save(),
            t("Save layout", "화면 저장"),
            false,
            Save,
          )}
          {button(
            () => c.fork(),
            t("Preserve layout in a new tab", "화면 배치를 새 탭으로 보존"),
            false,
            CopyPlus,
          )}
        </header>
        <div className="cw-tabs">
          <div
            role="tablist"
            aria-label={t("Independent canvas tabs", "독립 캔버스 탭")}
          >
            {c.tabs.map((item) => (
              <div
                key={item.id}
                className={"cw-tab " + (item.id === c.active ? "active" : "")}
              >
                <button
                  role="tab"
                  aria-selected={item.id === c.active}
                  onClick={() => c.select(item.id)}
                >
                  <PanelsTopLeft size={14} aria-hidden="true" />
                  <span>{title(item)}</span>
                </button>
                <button
                  aria-label={t("Close tab", "탭 닫기")}
                  onClick={() => void c.close(item, "tab")}
                >
                  <X size={13} aria-hidden="true" />
                </button>
              </div>
            ))}
          </div>
          <button
            onClick={() => c.newCanvas()}
            aria-label={t("New empty canvas tab", "새 빈 캔버스 탭")}
          >
            <Plus size={17} aria-hidden="true" />
          </button>
        </div>
        <div id="cw-error" role="status">
          {tab?.error || c.error}
        </div>
        {c.tabs
          .filter((item) => item.closing)
          .map((item) => (
            <div key={item.id} className="cw-close-notice" role="status">
              <span>
                {item.busy
                  ? t(
                      "Confirming private draft save…",
                      "개인 초안 저장을 확인한다…",
                    )
                  : t(
                      "Editor retained until its private drafts are confirmed.",
                      "개인 초안 저장을 확인할 때까지 에디터를 유지한다.",
                    )}
              </span>
              {button(
                () => void c.close(item, item.closing!),
                t("Retry close", "닫기 재시도"),
                item.busy,
              )}
              {button(
                () => void c.keep(item),
                t("Keep editing", "계속 편집"),
                item.busy,
              )}
            </div>
          ))}
        <div className="cw-body">
          {tab && !tab.project && (
            <div className="cw-empty">
              <div className="cw-empty-icon">
                <PanelsTopLeft size={28} aria-hidden="true" />
              </div>
              <h1>{t("A new workspace", "새 작업 공간")}</h1>
              <p>
                {t(
                  "Open a project to use its retained files. This tab starts empty.",
                  "프로젝트를 열면 저장된 작업 폴더에 연결한다. 새 탭은 빈 화면에서 시작한다.",
                )}
              </p>
              <label htmlFor="cw-project-select">
                {t("Project", "프로젝트")}
              </label>
              <select
                id="cw-project-select"
                value={project}
                onChange={(e) => setProject(e.target.value)}
              >
                <option value="">
                  {t("Choose a project", "프로젝트 선택")}
                </option>
                {c.projects.map((p) => (
                  <option key={p.id} value={p.id}>
                    {p.spec.name}
                  </option>
                ))}
              </select>
              {button(
                () => void c.connect(project),
                t("Open project", "프로젝트 열기"),
              )}
              {button(
                () => {
                  setCreateVmError("");
                  setCreatingVm(true);
                },
                t("Create workspace MicroVM", "작업용 MicroVM 만들기"),
              )}
              {createdVm && (
                <p role="status">
                  {t(
                    `Created ${createdVm}. Assign it to the project and bind a separate workspace volume before opening the project.`,
                    `${createdVm} VM을 만들었다. 프로젝트에 VM을 지정하고 별도 작업 볼륨을 연결한 후 프로젝트를 연다.`,
                  )}
                </p>
              )}
              <a
                href="https://github.com/SteelCrab/firecrab/blob/feat/550-ai-canvas/public-docs/guides/project-workspaces.md"
                target="_blank"
                rel="noreferrer"
              >
                {t("Workspace setup guide", "작업 공간 설정 안내")}
              </a>
            </div>
          )}
          {tab?.project && !tab.loaded && (
            <div className="cw-empty">
              <h1>{t("Opening project", "프로젝트 여는 중")}</h1>
              <p>{tab.error}</p>
              <ProjectBackups key={tab.project} project={tab.project}/>
              {button(
                () => void c.open(tab),
                t("Retry connection", "다시 연결"),
              )}
            </div>
          )}
          {tab?.loaded && (
            <>
              {tab.workspace?.status !== "ready" && (
                <div className="cw-runtime-notice">
                  {tab.workspace?.status} ·{" "}
                  {t(
                    "Files and commands require a running, verified project VM. Retained records remain available.",
                    "파일과 명령은 실행 중인 프로젝트 VM에 연결한다. 저장된 기록은 계속 조회할 수 있다.",
                  )}
                  {button(
                    () => void c.verify(tab),
                    t("Verify workspace", "작업 폴더 확인"),
                  )}
                </div>
              )}
              <section
                ref={board}
                id="cw-board"
                aria-label={t("AI Canvas workspace", "AI Canvas 작업 공간")}
                onPointerDown={pointerDown}
                onPointerMove={pointerMove}
                onPointerUp={() => {
                  drag.current = undefined;
                }}
                onPointerCancel={() => {
                  drag.current = undefined;
                }}
              >
                <div className="cw-board-label">CANVAS / {title(tab)}</div>
                <div className="cw-board-help">
                  {t(
                    "Drag empty space to pan · Ctrl/⌘ scroll to zoom",
                    "빈 공간 드래그로 이동 · Ctrl/⌘ 스크롤로 확대",
                  )}
                </div>
                <div
                  id="cw-world"
                  style={{
                    transform:
                      "translate(" +
                      tab.layout.x +
                      "px," +
                      tab.layout.y +
                      "px) scale(" +
                      tab.layout.zoom +
                      ")",
                  }}
                >
                  {tab.layout.nodes.map((node) => (
                    <CanvasCard key={node.id} tab={tab} node={node} />
                  ))}
                </div>
                <div className="cw-tools">
                  {button(
                    () => c.add(tab, "folder"),
                    t("Folder", "폴더"),
                    false,
                    Folder,
                  )}
                  {button(
                    () => void c.createTerminal(tab),
                    t("Terminal", "터미널"),
                    false,
                    SquareTerminal,
                  )}
                  {button(
                    () => c.add(tab, "note"),
                    t("Note", "메모"),
                    false,
                    StickyNote,
                  )}
                  {button(
                    () => c.createAgent(tab),
                    t("Agent", "에이전트"),
                    false,
                    Bot,
                  )}
                  <span>{Math.round(tab.layout.zoom * 100)}%</span>
                  {button(
                    () => {
                      c.reset(tab.id);
                    },
                    t("Reset view", "화면 초기화"),
                    false,
                    RotateCcw,
                  )}
                </div>
              </section>
              <section id="cw-editor-pane" hidden={!tab.editor}>
                <div className="cw-editor-bar">
                  {button(
                    () => {
                      c.hideEditor(tab.id);
                    },
                    t("Back to canvas", "캔버스로 돌아가기"),
                    false,
                    ArrowLeft,
                  )}
                  {button(
                    () => void c.close(tab, "editor"),
                    t("Close editor", "에디터 닫기"),
                    false,
                    X,
                  )}
                  <strong>
                    {t(
                      "Personal Page · Web VS Code",
                      "개인 페이지 · Web VS Code",
                    )}
                  </strong>
                </div>
                <div className="cw-editor-content">
                  <div id="cw-editor-slot" />
                  {tab.editor && <EditorAgents key={tab.id} tab={tab} />}
                </div>
              </section>
            </>
          )}
        </div>
      </main>
      {creatingVm && (
        <AppDialog
          label={t("New workspace MicroVM", "작업용 MicroVM 만들기")}
          onClose={() => setCreatingVm(false)}
        >
          <h2>{t("New workspace MicroVM", "작업용 MicroVM 만들기")}</h2>
          <p>
            {t(
              "Ubuntu, 4 vCPU, 8 GiB memory and a 16 GiB system disk are the initial settings. The workspace volume is separate.",
              "Ubuntu, 4 vCPU, 메모리 8 GiB, 시스템 디스크 16 GiB를 기본값으로 사용한다. 작업 볼륨은 별도다.",
            )}
          </p>
          {createVmError && <p role="alert">{createVmError}</p>}
          <CreateVm
            initialResources={{ cpu: 4, ram: 8192, diskGb: 16 }}
            preferredImageFamily="ubuntu"
            onCancel={() => setCreatingVm(false)}
            onError={setCreateVmError}
            onCreated={(vm) => {
              setCreatedVm(vm.name);
              setCreatingVm(false);
            }}
          />
        </AppDialog>
      )}
    </div>
  );
}
function EditorAgents({ tab }: { tab: Tab }) {
  "use no memo";
  const { t } = useI18n();
  const [selected, setSelected] = useState("");
  const nodes = tab.layout.nodes.filter((n) => n.kind === "agent");
  const node = nodes.find((n) => n.id === selected) || nodes.at(-1);
  return (
    <aside
      className="cw-editor-agents"
      aria-label={t("Editor folder agents", "에디터 폴더 에이전트")}
    >
      <div className="cw-editor-agent-picker">
        <Bot size={16} aria-hidden="true" />
        <select
          aria-label={t("Folder agent sessions", "폴더 에이전트 세션 목록")}
          value={node?.id || ""}
          onChange={(e) => setSelected(e.target.value)}
        >
          {!nodes.length && (
            <option value="">{t("No sessions", "세션 없음")}</option>
          )}
          {nodes.map((n, i) => (
            <option key={n.id} value={n.id}>
              {t("Session", "세션")} {i + 1} · /
              {agentDocument(n, tab.project)?.directory || ""}
            </option>
          ))}
        </select>
        {button(
          () => c.createAgent(tab),
          t("New folder agent", "새 폴더 에이전트"),
          false,
          Plus,
        )}
      </div>
      {node ? (
        <CanvasAgent key={node.id} tab={tab} node={node} />
      ) : (
        <p className="cw-agent-empty">
          {t(
            "Add a session to work on this folder with ChatGPT or Claude.",
            "세션을 추가하면 ChatGPT 또는 Claude로 이 폴더에서 작업할 수 있다.",
          )}
        </p>
      )}
    </aside>
  );
}

function CanvasCard({ tab, node }: { tab: Tab; node: CanvasNode }) {
  "use no memo";
  const { t } = useI18n();
  const session = terminalSessions.get(tab.project, node.id, tab.id);
  const Icon = panelIcons[node.kind];
  const names = {
    agent: t("Folder agent", "폴더 에이전트"),
    folder: t("Shared folder", "공용 작업 폴더"),
    terminal: t("Terminal", "터미널"),
    note: t("Private note", "개인 메모"),
    history: t("Version history", "버전 기록"),
  };
  return (
    <article
      className={"cw-node cw-" + node.kind}
      data-node={node.id}
      style={{
        left: node.x,
        top: node.y,
        width: node.width,
        height: node.height,
      }}
    >
      <header className="cw-card-header">
        <div data-cw-drag>
          <span className={"cw-node-icon cw-icon-" + node.kind}>
            <Icon size={15} aria-hidden="true" />
          </span>
          <strong>{names[node.kind]}</strong>
        </div>
        <button
          type="button"
          aria-label={
            node.kind === "agent"
              ? t(
                  "Close agent panel, keep task",
                  "에이전트 패널만 닫기, 작업 유지",
                )
              : t("Remove panel", "패널 제거")
          }
          onClick={() => c.remove(tab, node.id)}
        >
          <X size={14} aria-hidden="true" />
        </button>
      </header>
      <div className="cw-node-body">
        {node.kind === "note" && (
          <textarea
            aria-label={t("Private note", "개인 메모")}
            value={node.content}
            maxLength={8192}
            onChange={(event) => {
              c.note(tab.id, node.id, event.target.value);
            }}
          />
        )}
        {node.kind === "folder" && (
          <div className="cw-card-body">
            <small>/workspace/shared/{tab.layout.directory}</small>
            {tab.files.map((file) => {
              const path =
                (tab.layout.directory ? tab.layout.directory + "/" : "") +
                file.name;
              return (
                <div key={path}>
                  {button(
                    () =>
                      file.directory
                        ? void c.directory(tab, path)
                        : void c.editor(tab, path),
                    file.name,
                    false,
                    file.directory ? Folder : FileText,
                  )}
                  {!file.directory &&
                    button(
                      () => void c.history(tab, path),
                      t("History", "기록"),
                    )}
                </div>
              );
            })}
            {button(
              () => void c.editor(tab),
              t("Open in Web VS Code", "Web VS Code에서 열기"),
            )}
          </div>
        )}
        {node.kind === "terminal" && (
          <>
            <div className="cw-pty-status">
              <span>{session?.metadata?.status || "connecting"}</span>
              <span>
                {session?.control
                  ? t("You control input", "내가 입력 제어 중")
                  : t("View only", "보기 전용")}
              </span>
            </div>
            <div className="cw-pty" id={"cw-pty-" + node.id} />
            <footer>
              {button(
                () =>
                  void terminalSessions.control(tab, node.id, false, c.changed),
                t("Take input control", "입력 제어권 가져오기"),
              )}
              {button(
                () =>
                  void terminalSessions.control(tab, node.id, true, c.changed),
                t("Stop", "종료"),
                !session?.control,
              )}
              <span>{session?.error}</span>
            </footer>
          </>
        )}
        {node.kind === "agent" && !tab.editor && (
          <CanvasAgent tab={tab} node={node} />
        )}
        {node.kind === "history" && <HistoryCard tab={tab} />}
      </div>
      <div
        className="cw-resize"
        role="separator"
        aria-label={t("Resize panel", "패널 크기 조절")}
      />
    </article>
  );
}
function HistoryCard({ tab }: { tab: Tab }) {
  "use no memo";
  const { t } = useI18n();
  const [path, setPath] = useState(tab.historyPath || "");
  const diff = tab.comparison;
  return (
    <div className="cw-node-history">
      <form
        onSubmit={(e) => {
          e.preventDefault();
          void c.history(tab, path || undefined);
        }}
      >
        <label htmlFor={"cw-history-path-" + tab.id}>
          {t("File path", "파일 경로")}
        </label>
        <input
          id={"cw-history-path-" + tab.id}
          value={path}
          onChange={(e) => setPath(e.target.value)}
        />
        <button disabled={tab.historyLoading}>
          {t("Load retained history", "저장된 기록 불러오기")}
        </button>
      </form>
      {!tab.operationsReady && (
        <p>{t("Loading operation receipts…", "작업 영수증을 읽는다…")}</p>
      )}
      {tab.operations
        .filter((op) => op.status === "pending")
        .map((op) => (
          <div key={op.id}>
            {op.path} · {op.status}
            {button(
              () => void c.reconcile(tab, op.id),
              t("Reconcile operation", "작업 복구"),
              tab.revisionsBusy,
            )}
          </div>
        ))}
      <ul className="cw-history">
        {tab.history.map((v) => (
          <li key={v.id}>
            <span>
              v{v.id} · {v.path}
            </span>
            <small>{v.sha256?.slice(0, 12)}</small>
            {button(
              () => void c.compare(tab, v.id),
              t("Compare", "비교"),
              tab.revisionsBusy || !tab.operationsReady,
            )}
          </li>
        ))}
      </ul>
      {tab.historyMore &&
        button(
          () => void c.history(tab, tab.historyPath, tab.history.at(-1)?.id),
          t("Load older history", "이전 기록 불러오기"),
          tab.historyLoading,
        )}
      {diff && (
        <section className="cw-comparison">
          <h4>
            {diff.before.version.path} · v{diff.before.version.id} → v
            {diff.after.version.id}
          </h4>
          <div className="cw-diff-columns">
            <section>
              <h5>{t("Selected version", "선택 버전")}</h5>
              <FilePreview file={diff.before} />
            </section>
            <section>
              <h5>{t("Current saved file", "현재 저장 파일")}</h5>
              <FilePreview file={diff.after} />
            </section>
          </div>
          <p>
            {t(
              "Restoring appends a new saved version. The selected and current versions remain in history.",
              "복원하면 새 저장 버전을 추가한다. 선택 버전과 현재 버전은 기록에 남는다.",
            )}
          </p>
          {button(
            () => void c.restore(tab),
            t("Restore selected version", "선택한 버전 복원"),
            tab.revisionsBusy ||
              !!tab.restore ||
              !tab.operationsReady ||
              tab.operations.some((op) => op.status === "pending"),
          )}
          {button(
            () => {
              c.hideComparison(tab.id);
            },
            t("Close comparison", "비교 닫기"),
          )}
        </section>
      )}
      {tab.restore && (
        <div className="cw-restore-pending">
          <p>
            {t("Unconfirmed restore", "복원 결과 확인 필요")}:{" "}
            {tab.restore.path}
          </p>
          {button(
            () => void c.restore(tab, true),
            t("Retry the same restore", "같은 복원 요청 재시도"),
            tab.revisionsBusy,
          )}
          {button(
            () => void c.reconcile(tab, tab.restore!.id),
            t("Check saved outcome", "저장 결과 확인"),
            tab.revisionsBusy,
          )}
        </div>
      )}
      <p role="status">{tab.revisionMessage}</p>
    </div>
  );
}

function decode(value: string) {
  try {
    return new TextDecoder("utf-8", { fatal: true }).decode(
      Uint8Array.from(atob(value), (c) => c.charCodeAt(0)),
    );
  } catch {
    return "Invalid retained UTF-8 content";
  }
}

function FilePreview({ file }: { file: import("../canvas/types").FileData }) {
  const { t } = useI18n();
  return (
    <>
      <small>
        v{file.version.id} · {file.version.sha256}
      </small>
      {file.contentBase64.length > 64000 ? (
        <p>
          {t(
            "Large file: download to inspect the exact bytes.",
            "큰 파일은 내려받아 확인한다.",
          )}
        </p>
      ) : (
        <pre>{decode(file.contentBase64)}</pre>
      )}
      <a
        download={file.version.path.replaceAll("/", "_")}
        href={"data:application/octet-stream;base64," + file.contentBase64}
      >
        {t("Download this version", "이 버전 다운로드")}
      </a>
    </>
  );
}
