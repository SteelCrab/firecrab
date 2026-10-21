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
import { SquareTerminal } from "lucide-react";
import { useI18n } from "../i18n";
import Screen from "./layout/Screen";
import { useScreenStatus } from "./layout/screenStatus";
import { DetailHead, DetailSection } from "./ui/DetailSection";

/** Max body size accepted by the API (matches firecrab-api shells module). */
const MAX_CONTENT_BYTES = 32 * 1024;

function formatRevisionTime(ms: number): string {
  try {
    return new Date(ms).toLocaleString();
  } catch {
    return String(ms);
  }
}

/** Shell repository — versioned guest scripts injected on MicroVM start. */
export default function Shells() {
  const { t } = useI18n();
  const { setDetailOpen } = useScreenStatus();
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
      const rev = {
        shellId: detail.id,
        revisionId: latest.id,
        version: latest.version,
        contentSha256: latest.contentSha256,
        content: detail.latestContent,
        createdAtMs: latest.createdAtMs,
      };
      setViewedRevision(rev);
      setReviseContent(rev.content);
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
          setReviseContent(rev.content);
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
      setDetailOpen(true);
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

  const fieldError = (field: string) => (
    <span className="field-error">{fieldErrors?.fieldError(field) ?? ""}</span>
  );

  const latestRevisionId = detail?.revisions[0]?.id ?? null;
  const selectedShell = shells?.find((shell) => shell.id === selectedId) ?? null;

  const detailPanel = selectedShell ? (
    <>
      <DetailHead
        title={detail?.name ?? selectedShell.name}
        meta={(
          <>
            <span className="mono">v{selectedShell.latestVersion}</span>
            {viewedRevision && viewedRevision.revisionId !== latestRevisionId && (
              <span>{t(`viewing v${viewedRevision.version}`, `v${viewedRevision.version} 보는 중`)}</span>
            )}
          </>
        )}
      />
      <div className="detail-body">
        {detailError && <p className="field-error">{detailError}</p>}
        {!detail && !detailError && <p className="detail-note">{t("Loading…", "불러오는 중…")}</p>}
        {detail && (
          <>
            {detail.description ? <p className="detail-note shell-detail-desc">{detail.description}</p> : null}
            <DetailSection title={t("Revisions", "버전")}>
              <ul className="detail-list shell-revisions">
                {detail.revisions.map((rev) => {
                  const isViewing = viewingRevisionId === rev.id;
                  const isLatest = rev.id === latestRevisionId;
                  return (
                    <li key={rev.id}>
                      <button
                        type="button"
                        className={isViewing ? "shell-revision active" : "shell-revision"}
                        aria-pressed={isViewing}
                        title={t("Load this version into the editor", "이 버전을 편집기에 불러오기")}
                        onClick={() => selectRevision(rev)}
                      >
                        <span className="shell-revision-version mono">
                          v{rev.version}
                          {isLatest ? <span className="muted"> ({t("latest", "최신")})</span> : null}
                        </span>
                        <span className="shell-revision-meta">
                          <span className="mono">{rev.contentSha256.slice(0, 10)}…</span> · {rev.sizeBytes} B · {formatRevisionTime(rev.createdAtMs)}
                        </span>
                      </button>
                    </li>
                  );
                })}
              </ul>
            </DetailSection>
            <DetailSection title={t("Shell file", "Shell 파일")}>
              <form className="shell-inspect-publish" onSubmit={handleRevise}>
                <div className="field">
                  <label htmlFor="shell-revise">{t("Edit and publish as a new revision", "수정 후 새 버전으로 게시")}</label>
                  {revisionLoadError && <div className="field-error">{revisionLoadError}</div>}
                  {revisionLoading && !viewedRevision ? (
                    <p className="detail-note">{t("Loading…", "불러오는 중…")}</p>
                  ) : (
                    <textarea
                      id="shell-revise"
                      className="shell-content-editor shell-file-editor mono"
                      rows={14}
                      value={reviseContent}
                      onChange={(event) => setReviseContent(event.target.value)}
                      spellCheck={false}
                    />
                  )}
                  {fieldError("content")}
                </div>
                <div className="detail-sec-buttons">
                  <button className="btn primary" type="submit" disabled={revising || revisionLoading}>
                    {revising ? t("Publishing…", "게시 중…") : t("Publish new revision", "새 버전 게시")}
                  </button>
                </div>
              </form>
            </DetailSection>
          </>
        )}
      </div>
    </>
  ) : null;

  return (
    <Screen
      view="shells"
      detail={detailPanel}
      detailLabel={t("Shell detail", "Shell 상세")}
      detailKind={<><SquareTerminal aria-hidden="true" />Shell</>}
      onCloseDetail={() => setSelectedId(null)}
    >
    <section className="panel">
      <h2 className="panel-title">{t("Shell repository", "Shell 저장소")}</h2>
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
                  aria-selected={selectedId === shell.id}
                  onClick={() => {
                    setSelectedId(shell.id);
                    setDetailOpen(true);
                  }}
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

    </section>
    </Screen>
  );
}
