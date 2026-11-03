import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type CSSProperties,
  type ReactNode,
} from "react";
import {
  ChevronRight,
  File,
  FileSymlink,
  Folder,
  FolderGit2,
  FolderOpen,
  FolderTree,
  GitBranch,
  PanelLeftClose,
  RefreshCw,
} from "lucide-react";
import {
  agentRequest,
  type ProjectFolder,
  type VmRepository,
} from "../api/agents";
import { useI18n } from "../i18n";
import { useMediaQuery } from "../lib/useMediaQuery";
import "../styles/agent-files.css";

type Listing =
  | { state: "loading" }
  | { state: "error"; error: string }
  | ({ state: "ready" } & Omit<ProjectFolder, "path">);

const OPEN_KEY = "firecrab.agentFiles.open";
/** Narrower docks squeeze the split terminals beside the session list. */
const DOCKED = "(min-width: 1600px)";
/** Matches the phone layout in agent-files.css. */
const STACKED = "(max-width: 700px)";
const join = (folder: string, name: string) =>
  folder ? `${folder}/${name}` : name;
/** An open folder is on screen only when every folder above it is open too. */
const shown = (path: string, expanded: string[]) =>
  path
    .split("/")
    .slice(0, -1)
    .every((_, i, parts) => expanded.includes(parts.slice(0, i + 1).join("/")));

/** Read-only folder tree of the VM project. Folders load one level at a time. */
export default function AgentFileTree({
  vmId,
  project,
}: {
  vmId: string;
  project: VmRepository;
}) {
  const { t } = useI18n();
  // Only very wide screens keep the panel beside the tasks. Smaller screens
  // open it over the tasks, and phones above them, so there it starts closed
  // every time and the saved choice does not apply.
  const wide = useMediaQuery(DOCKED);
  const overlay = !useMediaQuery(STACKED) && !wide;
  const [pinned, setPinned] = useState(() => {
    try {
      return localStorage.getItem(OPEN_KEY) !== "closed";
    } catch {
      return true;
    }
  });
  const [drawer, setDrawer] = useState(false);
  const open = wide ? pinned : drawer;
  const panel = useRef<HTMLElement>(null);
  const moved = useRef(false);
  const setOpen = (value: boolean) => {
    moved.current = true;
    if (!wide) return setDrawer(value);
    setPinned(value);
    try {
      localStorage.setItem(OPEN_KEY, value ? "open" : "closed");
    } catch {
      // The panel still works without a saved choice.
    }
  };
  // Keep keyboard focus on the panel's own toggle when it opens or closes.
  useEffect(() => {
    if (!moved.current) return;
    moved.current = false;
    panel.current?.querySelector<HTMLElement>("[data-files-toggle]")?.focus();
  }, [open]);
  useEffect(() => {
    if (!open || !overlay) return;
    const outside = (e: PointerEvent) => {
      if (!panel.current?.contains(e.target as Node)) setDrawer(false);
    };
    document.addEventListener("pointerdown", outside);
    return () => document.removeEventListener("pointerdown", outside);
  }, [open, overlay]);
  const [folders, setFolders] = useState<Record<string, Listing>>({});
  const [expanded, setExpanded] = useState<string[]>([]);
  const [selected, setSelected] = useState("");
  const [refreshing, setRefreshing] = useState(false);
  // Answers to requests sent before the last refresh or unmount are dropped.
  const generation = useRef(0);
  const requested = useRef(false);

  const load = useCallback(
    async (path: string, keep = false) => {
      const version = generation.current;
      if (!keep)
        setFolders((old) => ({ ...old, [path]: { state: "loading" } }));
      let next: Listing;
      try {
        const value = await agentRequest<ProjectFolder>(
          `/project/files?path=${encodeURIComponent(path)}`,
          undefined,
          undefined,
          vmId,
        );
        next = {
          state: "ready",
          entries: value.entries,
          truncated: value.truncated,
        };
      } catch (e) {
        next = {
          state: "error",
          error: e instanceof Error ? e.message : String(e),
        };
      }
      if (version === generation.current)
        setFolders((old) => ({ ...old, [path]: next }));
    },
    [vmId],
  );
  // The root loads when the panel is first opened, not on every status poll.
  useEffect(() => {
    if (!open || requested.current) return;
    requested.current = true;
    void load("", true);
  }, [open, load]);
  useEffect(
    () => () => {
      generation.current++;
      requested.current = false;
    },
    [],
  );

  const toggle = (path: string) => {
    if (expanded.includes(path)) {
      setExpanded(expanded.filter((p) => p !== path));
      return;
    }
    setExpanded([...expanded, path]);
    if (!folders[path] || folders[path].state === "error") void load(path);
  };
  const refresh = async () => {
    generation.current++;
    requested.current = true;
    // Folders under a closed folder close too, so every open folder reloads.
    const visible = expanded.filter((path) => shown(path, expanded));
    setExpanded(visible);
    setFolders((old) =>
      Object.fromEntries(
        Object.entries(old).filter(
          ([path]) => path === "" || visible.includes(path),
        ),
      ),
    );
    setRefreshing(true);
    await Promise.all(["", ...visible].map((path) => load(path, true)));
    setRefreshing(false);
  };

  const label = t("Project files", "프로젝트 파일");
  const name =
    project.repository
      ?.replace(/^https:\/\/github.com\//, "")
      .replace(/\.git$/, "") || t("VM workspace", "VM 작업 폴더");

  const folder = (path: string, depth: number): ReactNode => {
    const listing = folders[path];
    const indent = { "--depth": depth } as CSSProperties;
    if (!listing || listing.state === "loading")
      return (
        <li className="am-file-note" style={indent}>
          <p role="status">{t("Loading files…", "파일 목록을 불러오는 중…")}</p>
        </li>
      );
    if (listing.state === "error")
      return (
        <li className="am-file-note is-error" style={indent}>
          <p role="alert">
            <strong>
              {t("Could not load the files.", "파일 목록을 불러오지 못했어요.")}
            </strong>{" "}
            {listing.error}
          </p>
        </li>
      );
    if (!listing.entries.length)
      return (
        <li className="am-file-note" style={indent}>
          {t("This folder is empty.", "빈 폴더예요.")}
        </li>
      );
    return (
      <>
        {listing.entries.map((entry) => {
          const full = join(path, entry.name);
          if (entry.kind === "directory") {
            const isOpen = expanded.includes(full);
            const Icon = isOpen ? FolderOpen : Folder;
            return (
              <li key={entry.name}>
                <button
                  type="button"
                  className="am-file-row is-folder"
                  style={indent}
                  title={full}
                  aria-expanded={isOpen}
                  onClick={() => toggle(full)}
                >
                  <ChevronRight
                    className="am-file-chevron"
                    aria-hidden="true"
                  />
                  <Icon aria-hidden="true" />
                  <span className="am-file-name">{entry.name}</span>
                </button>
                {isOpen && <ul>{folder(full, depth + 1)}</ul>}
              </li>
            );
          }
          const link = entry.kind === "symlink";
          const Icon = link ? FileSymlink : File;
          return (
            <li key={entry.name}>
              <button
                type="button"
                className="am-file-row"
                style={indent}
                title={
                  link ? `${full} · ${t("Symbolic link", "심볼릭 링크")}` : full
                }
                aria-current={selected === full || undefined}
                onClick={() => setSelected(full)}
              >
                <span className="am-file-chevron" aria-hidden="true" />
                <Icon aria-hidden="true" />
                <span className="am-file-name">{entry.name}</span>
              </button>
            </li>
          );
        })}
        {listing.truncated && (
          <li className="am-file-note" style={indent}>
            {t("Some items are not shown.", "일부 항목은 보이지 않아요.")}
          </li>
        )}
      </>
    );
  };

  return (
    <aside
      ref={panel}
      className={`am-files${open ? " is-open" : ""}${overlay ? " is-drawer" : ""}`}
      aria-label={label}
      onKeyDown={(e) => {
        if (e.key === "Escape" && open && overlay) setOpen(false);
      }}
    >
      {open ? (
        <div className="am-files-panel">
          <header className="am-files-head">
            <FolderGit2 aria-hidden="true" />
            <strong title={name}>{name}</strong>
            <button
              type="button"
              className="icon-btn"
              aria-label={t("Refresh files", "파일 새로고침")}
              title={t("Refresh files", "파일 새로고침")}
              disabled={refreshing}
              onClick={() => void refresh()}
            >
              <RefreshCw aria-hidden="true" />
            </button>
            <button
              type="button"
              className="icon-btn"
              data-files-toggle
              aria-label={t("Hide project files", "프로젝트 파일 숨기기")}
              title={t("Hide project files", "프로젝트 파일 숨기기")}
              onClick={() => setOpen(false)}
            >
              <PanelLeftClose aria-hidden="true" />
            </button>
          </header>
          <dl className="am-files-meta">
            <div>
              <dt>
                <GitBranch aria-hidden="true" />
                <span className="sr-only">{t("Branch", "브랜치")}</span>
              </dt>
              <dd>{project.branch || t("Default branch", "기본 브랜치")}</dd>
            </div>
            <div>
              <dt>
                <Folder aria-hidden="true" />
                <span className="sr-only">{t("Folder in VM", "VM 폴더")}</span>
              </dt>
              <dd title={project.path}>{project.path}</dd>
            </div>
          </dl>
          <ul className="am-file-tree">{folder("", 0)}</ul>
        </div>
      ) : (
        <button
          type="button"
          className="am-files-toggle"
          data-files-toggle
          aria-label={t("Show project files", "프로젝트 파일 보기")}
          title={t("Show project files", "프로젝트 파일 보기")}
          onClick={() => setOpen(true)}
        >
          <FolderTree aria-hidden="true" />
          <span>{label}</span>
        </button>
      )}
    </aside>
  );
}
