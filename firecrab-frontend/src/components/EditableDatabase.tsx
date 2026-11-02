import { useEffect, useState, type FormEvent } from "react";
import { ApiClientError, fetchJson } from "../api/client";
import type { DatabaseTable } from "../api/pipeDatabases";
import { useI18n } from "../i18n";

type Column = {
  name: string;
  dataType: string;
  nullable: boolean;
  defaultValue: string | null;
  generated: boolean;
  primaryKey: boolean;
  binary: boolean;
};
type Row = {
  values: Record<string, string | null>;
  key: Record<string, unknown>;
  version: string;
  truncatedColumns: string[];
};
type Page = {
  definition: {
    columns: Column[];
    primaryKey: string[];
    transactional: boolean;
  };
  rows: Row[];
  offset: number;
  hasMore: boolean;
};
type Draft = {
  id: string;
  operation: "insert" | "update" | "delete";
  row: Row | null;
  values: Record<string, string>;
  modes: Record<string, "keep" | "default" | "null" | "value">;
  uncertain: boolean;
};
const enc = encodeURIComponent;
const post = (body: object): RequestInit => ({
  method: "POST",
  headers: { "Content-Type": "application/json" },
  body: JSON.stringify(body),
});
export default function EditableDatabase({
  databaseId,
  table,
}: {
  databaseId: string;
  table: DatabaseTable;
}) {
  const { t } = useI18n(),
    base = `/api/pipe/databases/${enc(databaseId)}`;
  const [page, setPage] = useState<Page | null>(null),
    [offset, setOffset] = useState(0),
    [draft, setDraft] = useState<Draft | null>(null),
    [busy, setBusy] = useState(false),
    [error, setError] = useState("");
  const load = async () => {
    setPage(
      await fetchJson<Page>(
        `${base}/rows`,
        post({ schema: table.schema, table: table.name, offset, limit: 50 }),
      ),
    );
  };
  useEffect(() => {
    let live = true;
    fetchJson<Page>(
      `${base}/rows`,
      post({ schema: table.schema, table: table.name, offset, limit: 50 }),
    )
      .then((value) => {
        if (live) setPage(value);
      })
      .catch((e) => {
        if (live) setError(e instanceof Error ? e.message : String(e));
      });
    return () => {
      live = false;
    };
  }, [databaseId, table.schema, table.name, offset]);
  const editable =
    !!page?.definition.transactional && !!page.definition.primaryKey.length;
  const open = (operation: Draft["operation"], row: Row | null) => {
    if (!page) return;
    const values: Record<string, string> = {},
      modes: Draft["modes"] = {};
    for (const c of page.definition.columns) {
      values[c.name] = row?.values[c.name] ?? "";
      modes[c.name] =
        c.generated || c.binary || (!!row && c.primaryKey)
          ? "keep"
          : !row
            ? c.defaultValue !== null
              ? "default"
              : c.nullable
                ? "null"
                : "value"
            : row.values[c.name] === null
              ? "null"
              : "value";
    }
    setDraft({
      id: crypto.randomUUID(),
      operation,
      row,
      values,
      modes,
      uncertain: false,
    });
  };
  const save = async (event: FormEvent) => {
    event.preventDefault();
    if (!draft || busy) return;
    if (
      draft.operation === "delete" &&
      !window.confirm(
        t(
          "Delete this row? This changes the source database.",
          "이 행을 삭제할까요? 원본 DB에도 반영됩니다.",
        ),
      )
    )
      return;
    const values: Record<string, string | null> = {};
    if (draft.operation !== "delete")
      for (const [key, mode] of Object.entries(draft.modes)) {
        if (
          mode === "value" &&
          (draft.operation !== "update" ||
            draft.values[key] !== draft.row?.values[key])
        )
          values[key] = draft.values[key];
        if (
          mode === "null" &&
          (draft.operation !== "update" || draft.row?.values[key] !== null)
        )
          values[key] = null;
      }
    if (draft.operation === "update" && !Object.keys(values).length) {
      setError(
        t("Change at least one field.", "하나 이상의 필드를 수정하세요."),
      );
      return;
    }
    setBusy(true);
    setError("");
    try {
      await fetchJson(
        `${base}/mutations`,
        post({
          requestId: draft.id,
          schema: table.schema,
          table: table.name,
          operation: draft.operation,
          key: draft.row?.key || {},
          version: draft.row?.version || null,
          values,
        }),
      );
      setDraft(null);
      try {
        await load();
      } catch (e) {
        setError(e instanceof Error ? e.message : String(e));
      }
    } catch (e) {
      if (
        !(e instanceof ApiClientError) ||
        e.status === undefined ||
        e.status >= 500
      )
        setDraft({ ...draft, uncertain: true });
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };
  return (
    <section className="panel">
      <h4>
        {t("Editable rows", "편집 가능한 행")} · {table.schema}.{table.name}
      </h4>
      {error && <p role="alert">{error}</p>}
      {!page ? (
        <p>{t("Loading…", "불러오는 중…")}</p>
      ) : (
        <>
          {!editable && (
            <p>
              {t(
                "Row edits require a transactional table with a supported primary key.",
                "행을 편집하려면 테이블이 트랜잭션을 지원하고 기본 키가 있어야 합니다.",
              )}
            </p>
          )}
          <button
            className="btn"
            disabled={!editable || busy || !!draft}
            onClick={() => open("insert", null)}
          >
            {t("Add row", "행 추가")}
          </button>
          <div className="table-scroll">
            <table className="vm-table">
              <thead>
                <tr>
                  {page.definition.columns.map((c) => (
                    <th key={c.name}>{c.name}</th>
                  ))}
                  <th>{t("Actions", "작업")}</th>
                </tr>
              </thead>
              <tbody>
                {page.rows.map((row, i) => (
                  <tr key={row.version + ":" + i}>
                    {page.definition.columns.map((c) => (
                      <td key={c.name}>{row.values[c.name] ?? "NULL"}</td>
                    ))}
                    <td>
                      <button
                        className="btn"
                        disabled={
                          !editable ||
                          busy ||
                          !!draft ||
                          !!row.truncatedColumns.length
                        }
                        onClick={() => open("update", row)}
                      >
                        {t("Edit", "편집")}
                      </button>
                      <button
                        className="btn danger"
                        disabled={
                          !editable ||
                          busy ||
                          !!draft ||
                          !!row.truncatedColumns.length
                        }
                        onClick={() => open("delete", row)}
                      >
                        {t("Delete", "삭제")}
                      </button>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
          <button
            className="btn"
            disabled={busy || !!draft || offset === 0}
            onClick={() => setOffset((v) => Math.max(0, v - 50))}
          >
            {t("Previous", "이전")}
          </button>
          <button
            className="btn"
            disabled={busy || !!draft || !page.hasMore}
            onClick={() => setOffset((v) => v + page.rows.length)}
          >
            {t("Next", "다음")}
          </button>
        </>
      )}
      {draft && (
        <form
          aria-label={t("Row editor", "행 편집")}
          onSubmit={(event) => void save(event)}
        >
          <h4>
            {t("Row change", "행 변경")} · {draft.operation}
          </h4>
          {draft.uncertain && (
            <p role="status">
              {t(
                "The result is unknown. Retry the same request.",
                "결과를 확인하지 못했습니다. 같은 요청으로 다시 시도하세요.",
              )}
            </p>
          )}
          {draft.operation !== "delete" &&
            page?.definition.columns.map((c) => (
              <fieldset
                key={c.name}
                disabled={
                  busy || draft.uncertain || draft.modes[c.name] === "keep"
                }
              >
                <legend>
                  {c.name} · {c.dataType}
                </legend>
                <select
                  aria-label={t(`${c.name} value mode`, `${c.name} 값 방식`)}
                  value={draft.modes[c.name]}
                  onChange={(e) =>
                    setDraft({
                      ...draft,
                      modes: {
                        ...draft.modes,
                        [c.name]: e.target.value as Draft["modes"][string],
                      },
                    })
                  }
                >
                  <option value="value">{t("Value", "값")}</option>
                  {c.nullable && <option value="null">NULL</option>}
                  {draft.operation === "insert" && c.defaultValue !== null && (
                    <option value="default">{t("Default", "기본값")}</option>
                  )}
                </select>
                {draft.modes[c.name] === "value" && (
                  <input
                    aria-label={t(`${c.name} value`, `${c.name} 값`)}
                    value={draft.values[c.name]}
                    onChange={(e) =>
                      setDraft({
                        ...draft,
                        values: { ...draft.values, [c.name]: e.target.value },
                      })
                    }
                  />
                )}
              </fieldset>
            ))}
          <button className="btn primary" disabled={busy}>
            {draft.uncertain ? t("Retry", "재시도") : t("Save", "저장")}
          </button>
          <button
            type="button"
            className="btn"
            disabled={busy || draft.uncertain}
            onClick={() => setDraft(null)}
          >
            {t("Cancel", "취소")}
          </button>
        </form>
      )}
    </section>
  );
}
