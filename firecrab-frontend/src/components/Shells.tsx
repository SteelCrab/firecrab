import { useEffect, useState } from "react";
import type { FormEvent } from "react";
import type {
  ShellDetailResponse,
  ShellResponse,
  ShellRevisionResponse,
  ShellRevisionSummary,
} from "../bindings";
import {
  ApiClientError,
  createShell,
  createShellRevision,
  deleteShell,
  getShell,
  getShellRevision,
  listShells,
} from "../api/client";
import { useI18n } from "../i18n";

/** Max body size accepted by the API (matches firecrab-api shells module). */
const MAX_CONTENT_BYTES = 32 * 1024;

function formatRevisionTime(ms: number): string {
  try {
    return new Date(ms).toLocaleString();
  } catch {
    return String(ms);
  }
}

/**
 * Shell repository — versioned guest scripts injected on MicroVM start.
 * Page icon: `public/bash.png` (same art as `assets/dashboard/bash.png`).
 */
export default function Shells() {
  const { t } = useI18n();
  const [shells, setShells] = useState<ShellResponse[] | null>(null);
  const [listError, setListError] = useState<string | null>(null);
  const [name, setName] = useState("");
  const [description, setDescription] = useState("");
  const [content, setContent] = useState(
    "#!/bin/sh\n# Portable across Alpine, Ubuntu, and Rocky\necho hello from firecrab shell\n",
  );
  const [submitting, setSubmitting] = useState(false);
  const [fieldErrors, setFieldErrors] = useState<ApiClientError | null>(null);
  const [busyId, setBusyId] = useState<string | null>(null);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [detail, setDetail] = useState<ShellDetailResponse | null>(null);
  const [detailError, setDetailError] = useState<string | null>(null);
  const [reviseContent, setReviseContent] = useState("");
  const [revising, setRevising] = useState(false);
  /** Currently inspected revision (any version, including past). */
  const [viewingRevisionId, setViewingRevisionId] = useState<string | null>(null);
  const [viewedRevision, setViewedRevision] = useState<ShellRevisionResponse | null>(null);
  const [revisionLoadError, setRevisionLoadError] = useState<string | null>(null);
  const [revisionLoading, setRevisionLoading] = useState(false);

  const refresh = async () => {
    try {
      setShells(await listShells());
      setListError(null);
    } catch (error) {
      setListError((error as Error).message);
    }
  };

  useEffect(() => {
    refresh();
  }, []);

  useEffect(() => {
    if (!selectedId) {
      setDetail(null);
      setReviseContent("");
      setViewingRevisionId(null);
      setViewedRevision(null);
      setRevisionLoadError(null);
      return;
    }
    setDetail(null);
    setDetailError(null);
    setViewingRevisionId(null);
    setViewedRevision(null);
    setRevisionLoadError(null);
    getShell(selectedId)
      .then((next) => {
        setDetail(next);
        setReviseContent(next.latestContent ?? "");
        // Open the latest revision by default so history is one click away.
        const latest = next.revisions[0];
        if (latest) setViewingRevisionId(latest.id);
      })
      .catch((error) => setDetailError((error as Error).message));
  }, [selectedId]);

  useEffect(() => {
    if (!selectedId || !viewingRevisionId || !detail) return;

    // Latest body is already on the detail payload — avoid an extra round-trip.
    const latest = detail.revisions[0];
    if (latest && latest.id === viewingRevisionId && detail.latestContent != null) {
      setViewedRevision({
        shellId: detail.id,
        revisionId: latest.id,
        version: latest.version,
        contentSha256: latest.contentSha256,
        content: detail.latestContent,
        createdAtMs: latest.createdAtMs,
      });
      setRevisionLoading(false);
      setRevisionLoadError(null);
      return;
    }

    let cancelled = false;
    setRevisionLoading(true);
    setRevisionLoadError(null);
    setViewedRevision(null);
    getShellRevision(selectedId, viewingRevisionId)
      .then((rev) => {
        if (!cancelled) {
          setViewedRevision(rev);
          setRevisionLoading(false);
        }
      })
      .catch((error) => {
        if (!cancelled) {
          setViewedRevision(null);
          setRevisionLoadError((error as Error).message);
          setRevisionLoading(false);
        }
      });
    return () => {
      cancelled = true;
    };
  }, [selectedId, viewingRevisionId, detail]);

  const handleCreate = async (event: FormEvent) => {
    event.preventDefault();
    if (submitting) return;
    setSubmitting(true);
    setFieldErrors(null);
    try {
      const created = await createShell({
        name: name.trim(),
        description: description.trim() || null,
        content,
      });
      setName("");
      setDescription("");
      setContent(
        "#!/bin/sh\n# Portable across Alpine, Ubuntu, and Rocky\necho hello from firecrab shell\n",
      );
      setSelectedId(created.shellId);
      await refresh();
    } catch (error) {
      setFieldErrors(error as ApiClientError);
    } finally {
      setSubmitting(false);
    }
  };

  const handleRevise = async (event: FormEvent) => {
    event.preventDefault();
    if (!selectedId || revising) return;
    setRevising(true);
    setFieldErrors(null);
    try {
      const published = await createShellRevision(selectedId, { content: reviseContent });
      const next = await getShell(selectedId);
      setDetail(next);
      setReviseContent(next.latestContent ?? "");
      setViewingRevisionId(published.revisionId);
      setViewedRevision(published);
      setRevisionLoadError(null);
      await refresh();
    } catch (error) {
      setFieldErrors(error as ApiClientError);
    } finally {
      setRevising(false);
    }
  };

  const handleDelete = async (shell: ShellResponse) => {
    if (
      busyId ||
      !window.confirm(
        t(`Delete shell "${shell.name}" and all revisions?`, `Shell "${shell.name}"과(와) 모든 버전을 삭제할까요?`),
      )
    ) {
      return;
    }
    setBusyId(shell.id);
    try {
      await deleteShell(shell.id);
      if (selectedId === shell.id) setSelectedId(null);
      await refresh();
    } catch (error) {
      setListError((error as Error).message);
    } finally {
      setBusyId(null);
    }
  };

  const selectRevision = (rev: ShellRevisionSummary) => {
    if (viewingRevisionId === rev.id) return;
    setViewingRevisionId(rev.id);
    setRevisionLoadError(null);
  };

  const useViewedAsNewBase = () => {
    if (viewedRevision) {
      setReviseContent(viewedRevision.content);
    }
  };

  const fieldError = (field: string) => (
    <span className="field-error">{fieldErrors?.fieldError(field) ?? ""}</span>
  );

  const latestRevisionId = detail?.revisions[0]?.id ?? null;

  return (
    <section className="panel">
      <h2 className="panel-title panel-title-with-icon">
        <img className="panel-brand-icon" src="/bash.png" alt="" width={28} height={28} />
        <span>{t("Shell repository", "Shell 저장소")}</span>
      </h2>
      <p className="poll-note" style={{ marginBottom: "0.75rem" }}>
        {t(
          "Versioned guest scripts for Alpine / Ubuntu / Rocky. Prefer #!/bin/sh for all-image compatibility (Alpine has no bash). Pin on a MicroVM; injected and run once after network-ready each start. Watch FIRECRAB_SHELL_* in the console.",
          "Alpine·Ubuntu·Rocky 공용 게스트 스크립트입니다. 전 이미지 호환을 위해 #!/bin/sh 를 권장합니다 (Alpine에는 bash 없음). MicroVM에 연결 후 시작 시 network-ready 이후 한 번 실행됩니다. 콘솔의 FIRECRAB_SHELL_* 마커를 확인하세요.",
        )}
      </p>

      <form className="create-grid shell-create-grid" onSubmit={handleCreate}>
        <div className="field">
          <label htmlFor="shell-name">name</label>
          <input
            id="shell-name"
            placeholder="web-init"
            value={name}
            onChange={(event) => setName(event.target.value)}
            required
            minLength={1}
            maxLength={64}
          />
          {fieldError("name")}
        </div>
        <div className="field">
          <label htmlFor="shell-description">{t("description (optional)", "설명 (선택)")}</label>
          <input
            id="shell-description"
            placeholder={t("What this script does", "이 스크립트가 하는 일")}
            value={description}
            onChange={(event) => setDescription(event.target.value)}
            maxLength={512}
          />
          {fieldError("description")}
        </div>
        <div className="field field-span-2">
          <label htmlFor="shell-content">
            content{" "}
            <span className="poll-note">
              (#!/bin/sh portable · max {MAX_CONTENT_BYTES} bytes)
            </span>
          </label>
          <textarea
            id="shell-content"
            className="shell-content-editor mono"
            rows={8}
            value={content}
            onChange={(event) => setContent(event.target.value)}
            required
            spellCheck={false}
          />
          {fieldError("content")}
        </div>
        <div className="field field-submit">
          <label>&nbsp;</label>
          <button className="btn primary" type="submit" disabled={submitting}>
            {submitting ? t("Saving…", "저장 중…") : t("Create shell", "Shell 생성")}
          </button>
          <span className="field-error" />
        </div>
      </form>

      <h3 className="panel-title" style={{ marginTop: "1.25rem" }}>
        {t("Catalog", "목록")}
      </h3>
      {listError && <div className="field-error">{listError}</div>}
      {shells === null ? (
        <div className="empty">{t("Loading…", "불러오는 중…")}</div>
      ) : shells.length === 0 ? (
        <div className="empty">{t("No shells yet.", "등록된 Shell이 없습니다.")}</div>
      ) : (
        <div className="table-scroll">
          <table className="vm-table">
            <thead>
              <tr>
                <th>name</th>
                <th>{t("latest", "최신")}</th>
                <th>sha256</th>
                <th></th>
              </tr>
            </thead>
            <tbody>
              {shells.map((shell) => (
                <tr
                  key={shell.id}
                  className={selectedId === shell.id ? "selected" : undefined}
                  onClick={() => setSelectedId(shell.id)}
                  style={{ cursor: "pointer" }}
                >
                  <td>
                    <strong>{shell.name}</strong>
                    {shell.description ? (
                      <div className="poll-note">{shell.description}</div>
                    ) : null}
                  </td>
                  <td className="mono">v{shell.latestVersion}</td>
                  <td className="mono">
                    {(shell.contentSha256 ?? "").slice(0, 12)}
                    {(shell.contentSha256 ?? "").length > 12 ? "…" : ""}
                  </td>
                  <td>
                    <button
                      type="button"
                      className="btn"
                      disabled={busyId === shell.id}
                      onClick={(event) => {
                        event.stopPropagation();
                        handleDelete(shell);
                      }}
                    >
                      {t("Delete", "삭제")}
                    </button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}

      {selectedId && (
        <div className="shell-inspect" style={{ marginTop: "1.25rem" }}>
          {detailError && <div className="field-error">{detailError}</div>}
          {!detail && !detailError && <div className="empty">{t("Loading…", "불러오는 중…")}</div>}
          {detail && (
            <div className="shell-inspect-split">
              {/* Left: catalog metadata only — no script body */}
              <aside className="shell-inspect-meta panel" aria-label={t("Shell details", "Shell 상세")}>
                <h3 className="panel-title">{detail.name}</h3>
                {detail.description ? (
                  <p className="poll-note shell-inspect-desc">{detail.description}</p>
                ) : null}
                <dl className="shell-inspect-dl mono">
                  <dt>id</dt>
                  <dd title={detail.id}>{detail.id.slice(0, 8)}…</dd>
                  <dt>{t("created", "생성")}</dt>
                  <dd>{formatRevisionTime(detail.createdAtMs)}</dd>
                  <dt>{t("updated", "갱신")}</dt>
                  <dd>{formatRevisionTime(detail.updatedAtMs)}</dd>
                </dl>

                <h4 className="shell-inspect-subhead">{t("Versions", "버전")}</h4>
                <p className="poll-note shell-inspect-hint">
                  {t(
                    "Select a version to open its file on the right.",
                    "버전을 선택하면 오른쪽에서 파일을 엽니다.",
                  )}
                </p>
                <div className="table-scroll">
                  <table className="vm-table shell-rev-table">
                    <thead>
                      <tr>
                        <th>ver</th>
                        <th>sha256</th>
                        <th>size</th>
                        <th>{t("created", "생성")}</th>
                      </tr>
                    </thead>
                    <tbody>
                      {detail.revisions.map((rev) => {
                        const isViewing = viewingRevisionId === rev.id;
                        const isLatest = rev.id === latestRevisionId;
                        return (
                          <tr
                            key={rev.id}
                            className={isViewing ? "selected" : undefined}
                            onClick={() => selectRevision(rev)}
                            style={{ cursor: "pointer" }}
                            title={t("Open script file", "스크립트 파일 열기")}
                          >
                            <td className="mono">
                              v{rev.version}
                              {isLatest ? (
                                <span className="poll-note"> ({t("latest", "최신")})</span>
                              ) : null}
                            </td>
                            <td className="mono">{rev.contentSha256.slice(0, 10)}…</td>
                            <td>{rev.sizeBytes} B</td>
                            <td className="mono">{formatRevisionTime(rev.createdAtMs)}</td>
                          </tr>
                        );
                      })}
                    </tbody>
                  </table>
                </div>

                <h4 className="shell-inspect-subhead">{t("Publish new revision", "새 버전 게시")}</h4>
                <form className="shell-inspect-publish" onSubmit={handleRevise}>
                  <div className="field">
                    <label htmlFor="shell-revise">
                      {t("Script body", "스크립트 본문")}
                    </label>
                    <textarea
                      id="shell-revise"
                      className="shell-content-editor mono"
                      rows={6}
                      value={reviseContent}
                      onChange={(event) => setReviseContent(event.target.value)}
                      spellCheck={false}
                    />
                    {fieldError("content")}
                  </div>
                  <div className="shell-inspect-actions">
                    <button className="btn primary" type="submit" disabled={revising}>
                      {revising
                        ? t("Publishing…", "게시 중…")
                        : t("Publish", "게시")}
                    </button>
                    <button
                      type="button"
                      className="btn"
                      disabled={!viewedRevision}
                      onClick={useViewedAsNewBase}
                      title={t(
                        "Copy the open file into the editor",
                        "열린 파일을 편집기에 복사",
                      )}
                    >
                      {t("Copy from file", "파일에서 복사")}
                    </button>
                  </div>
                </form>
              </aside>

              {/* Right: shell file only */}
              <section
                className="shell-inspect-file panel"
                aria-label={t("Shell file", "Shell 파일")}
              >
                <div className="shell-inspect-file-bar">
                  <h3 className="panel-title">
                    {t("Shell file", "Shell 파일")}
                    {viewedRevision ? (
                      <span className="mono shell-inspect-file-ver">
                        {" "}
                        · {detail.name} · v{viewedRevision.version}
                      </span>
                    ) : null}
                  </h3>
                </div>
                {revisionLoadError && <div className="field-error">{revisionLoadError}</div>}
                {revisionLoading && !viewedRevision && (
                  <div className="empty">{t("Loading file…", "파일 불러오는 중…")}</div>
                )}
                {!revisionLoading && !viewedRevision && !revisionLoadError && (
                  <div className="empty">
                    {t("Select a version on the left.", "왼쪽에서 버전을 선택하세요.")}
                  </div>
                )}
                {viewedRevision && (
                  <pre className="shell-file-body mono" tabIndex={0}>
                    {viewedRevision.content}
                  </pre>
                )}
              </section>
            </div>
          )}
        </div>
      )}
    </section>
  );
}
