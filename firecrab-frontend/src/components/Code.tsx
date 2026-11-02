import { useCallback, useEffect, useRef, useState } from "react";
import Screen from "./layout/Screen";
import PipeAi from "./PipeAi";
import { useI18n } from "../i18n";
import { uuid } from "../api/code";
import { useAccounts, useCodeSnapshot } from "./code/hooks";
import CodeSetup from "./code/Setup";
import CodeWorkspace from "./code/Workspace";
import "../styles/code.css";

export default function Code({
  path,
  onUnsavedChange,
}: {
  path: string;
  onUnsavedChange: (dirty: boolean) => void;
}) {
  const { t } = useI18n();
  const [manager, setManager] = useState(false),
    [accountEpoch, accountUpdate] = useState(0);
  const [managerDirty, setManagerDirty] = useState(false),
    [query, setQuery] = useState("");
  const dirtyRef = useRef(false),
    managerRef = useRef(false);
  const parts = path.split("/");
  const creating = parts[1] === "new",
    id = uuid(parts[1]) ? parts[1] : null,
    editing = parts[2] === "editor";
  const snapshot = useCodeSnapshot(id),
    auth = useAccounts(accountEpoch);
  const setDirty = useCallback(
    (value: boolean) => {
      dirtyRef.current = value;
      onUnsavedChange(value || managerRef.current);
    },
    [onUnsavedChange],
  );
  const accountChanged = useCallback(
    (value: boolean) => {
      managerRef.current = value;
      setManagerDirty(value);
      onUnsavedChange(value || dirtyRef.current);
    },
    [onUnsavedChange],
  );
  useEffect(() => () => onUnsavedChange(false), [onUnsavedChange]);
  const selected =
    snapshot.rows.find((w) => w.id === id) ??
    (!id ? snapshot.rows[0] : undefined);
  useEffect(() => {
    if (!creating && !id && snapshot.loaded && !snapshot.error && selected)
      location.replace(`#/code/${selected.id}`);
  }, [creating, id, snapshot.loaded, snapshot.error, selected]);
  const openManager = () => setManager(true);
  return (
    <Screen
      view="code"
      heading={
        creating
          ? t("New workspace", "새 워크스페이스")
          : editing
            ? t("Editor", "편집기")
            : t("Workspaces", "워크스페이스")
      }
      parent={creating || editing ? "code" : undefined}
      actions={
        !creating && (
          <>
            <button className="btn" onClick={openManager}>
              AI AUTH
            </button>
            <a className="btn primary" href="#/code/new">
              {t("New workspace", "새 워크스페이스")}
            </a>
          </>
        )
      }
    >
      <section id="code-workspaces">
        <div hidden={manager}>
          {creating ? (
            <CodeSetup auth={auth} manage={openManager} onDirty={setDirty} />
          ) : (
            <>
              {snapshot.error ? (
                <p role="alert">
                  {t(
                    "Workspace status is unavailable. Retrying automatically.",
                    "워크스페이스 상태를 확인할 수 없습니다. 자동으로 다시 시도합니다.",
                  )}
                </p>
              ) : !snapshot.loaded ? (
                <p role="status">
                  {t("Loading workspaces…", "워크스페이스를 불러오는 중…")}
                </p>
              ) : (
                <>
                  {!editing && (
                    <>
                      <div className="code-toolbar">
                        <input
                          type="search"
                          aria-label={t(
                            "Search workspaces",
                            "워크스페이스 검색",
                          )}
                          placeholder={t(
                            "Search workspaces",
                            "워크스페이스 검색",
                          )}
                          value={query}
                          onChange={(e) => setQuery(e.target.value)}
                        />
                        <button className="btn" onClick={snapshot.refresh}>
                          {t("Refresh", "새로고침")}
                        </button>
                      </div>
                      <nav
                        className="code-list"
                        aria-label={t("Workspace list", "워크스페이스 목록")}
                      >
                        {snapshot.rows
                          .filter((w) =>
                            `${w.spec.name} ${w.spec.github.owner}/${w.spec.github.repository} ${w.spec.github.branch}`
                              .toLowerCase()
                              .includes(query.trim().toLowerCase()),
                          )
                          .map((w) => (
                            <a
                              key={w.id}
                              href={`#/code/${w.id}`}
                              aria-current={
                                selected?.id === w.id ? "page" : undefined
                              }
                            >
                              <strong>{w.spec.name}</strong>
                              <span>
                                {w.spec.github.owner}/{w.spec.github.repository}
                              </span>
                            </a>
                          ))}
                      </nav>
                    </>
                  )}
                  {selected ? (
                    <CodeWorkspace
                      key={selected.id}
                      workspace={selected}
                      runtime={snapshot.runtime}
                      runtimeLoaded={snapshot.loaded}
                      editing={editing}
                      auth={auth}
                      refresh={snapshot.refresh}
                      manage={openManager}
                      managing={manager}
                      onDirty={setDirty}
                    />
                  ) : (
                    <p role="status">
                      {snapshot.rows.length
                        ? t(
                            "This workspace no longer exists.",
                            "이 워크스페이스는 더 이상 존재하지 않습니다.",
                          )
                        : t(
                            "No workspaces yet.",
                            "아직 워크스페이스가 없습니다.",
                          )}
                    </p>
                  )}
                </>
              )}
            </>
          )}
        </div>
        {manager && (
          <section aria-label="AI AUTH">
            <button
              className="btn"
              disabled={managerDirty}
              onClick={() => {
                setManager(false);
                accountUpdate((n) => n + 1);
                snapshot.refresh();
              }}
            >
              {t("Back to workspace", "워크스페이스로 돌아가기")}
            </button>
            <PipeAi onUnsavedChange={accountChanged} />
          </section>
        )}
      </section>
    </Screen>
  );
}
