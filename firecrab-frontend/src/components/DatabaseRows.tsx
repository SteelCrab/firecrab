import { useEffect, useState, type FormEvent } from "react";
import { ChevronLeft, ChevronRight, RefreshCw, X } from "lucide-react";
import { useI18n } from "../i18n";
import {
  editableRows,
  getDatabaseTables,
  mutateRow,
  previewDatabase,
  type ColumnInfo,
  type DatabasePreview,
  type DatabaseTable,
  type EditablePage,
  type EditableRow,
  type PipeDatabase,
  type RowMutation,
} from "../api/pipeDatabases";
import { ApiClientError } from "../api/client";
import DatabaseServices from "./DatabaseServices";
import DatabaseMaintenance from "./DatabaseMaintenance";

interface Props {
  connection: PipeDatabase;
  onClose: () => void;
  onDirty: (dirty: boolean) => void;
}
export default function DatabaseRows({ connection, onClose, onDirty }: Props) {
  const { t } = useI18n();
  const [tables, setTables] = useState<DatabaseTable[]>([]),
    [truncated, setTruncated] = useState(false),
    [selected, setSelected] = useState<number | null>(null);
  const [preview, setPreview] = useState<DatabasePreview | null>(null),
    [editable, setEditable] = useState<EditablePage | null>(null),
    [history, setHistory] = useState<number[]>([]);
  const [busy, setBusy] = useState(true),
    [error, setError] = useState<string | null>(null),
    [notice, setNotice] = useState<string | null>(null),
    [revision, setRevision] = useState(0);
  const [editor, setEditor] = useState<{
    row: EditableRow | null;
    operation: "insert" | "update" | "delete";
  } | null>(null);
  const [fields, setFields] = useState<
    Record<string, { mode: "default" | "value" | "null"; value: string }>
  >({});
  const [maintenanceDirty, setMaintenanceDirty] = useState(false),
    [servicesDirty, setServicesDirty] = useState(false);
  const [pending, setPending] = useState<RowMutation | null>(null);
  useEffect(() => {
    onDirty(editor !== null || maintenanceDirty || servicesDirty);
    return () => onDirty(false);
  }, [editor, maintenanceDirty, servicesDirty, onDirty]);
  useEffect(() => {
    const controller = new AbortController();
    const timer = setTimeout(() => {
      void getDatabaseTables(connection.id, controller.signal)
        .then((result) => {
          if (!controller.signal.aborted) {
            setTables(result.tables);
            setTruncated(result.truncated);
            setError(null);
          }
        })
        .catch((e: Error) => {
          if (!controller.signal.aborted) setError(e.message);
        })
        .finally(() => {
          if (!controller.signal.aborted) setBusy(false);
        });
    }, 0);
    return () => {
      clearTimeout(timer);
      controller.abort();
    };
  }, [connection.id, connection.writeEnabled, revision]);
  async function read(index: number, offset = 0, nextHistory: number[] = []) {
    if (busy || editor || !tables[index]) return;
    setBusy(true);
    setError(null);
    setNotice(null);
    setSelected(index);
    setPreview(null);
    setEditable(null);
    try {
      if (connection.writeEnabled) {
        const page = await editableRows(connection.id, tables[index], offset);
        setEditable(page);
        setPreview({
          columns: page.definition.columns,
          rows: page.rows.map((row) =>
            page.definition.columns.map((column) => row.values[column.name]),
          ),
          offset: page.offset,
          hasMore: page.hasMore,
        });
      } else
        setPreview(await previewDatabase(connection.id, tables[index], offset));
      setHistory(nextHistory);
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setBusy(false);
    }
  }
  function edit(
    operation: "insert" | "update" | "delete",
    row: EditableRow | null,
  ) {
    setEditor({ operation, row });
    setError(null);
    setNotice(null);
    setPending(null);
    const values: typeof fields = {};
    for (const column of editable?.definition.columns ?? []) {
      const value = row?.values[column.name];
      values[column.name] = {
        mode:
          operation === "insert"
            ? "default"
            : value === null
              ? "null"
              : "value",
        value: value ?? "",
      };
    }
    setFields(values);
  }
  function canChange(column: ColumnInfo) {
    return (
      !column.generated &&
      !(editor?.operation === "update" && column.primaryKey) &&
      ![
        "geometry",
        "point",
        "linestring",
        "polygon",
        "multipoint",
        "multilinestring",
        "multipolygon",
        "geometrycollection",
      ].includes(column.dataType)
    );
  }
  async function save(event: FormEvent) {
    event.preventDefault();
    if (busy || !editor || selected === null || !editable) return;
    setBusy(true);
    setError(null);
    try {
      let request = pending;
      if (!request) {
        const values: Record<string, unknown> = {};
        if (editor.operation !== "delete")
          for (const column of editable.definition.columns) {
            const field = fields[column.name];
            if (!canChange(column) || field.mode === "default") continue;
            const value = field.mode === "null" ? null : field.value;
            if (
              editor.operation === "update" &&
              value === editor.row?.values[column.name]
            )
              continue;
            if (editor.row?.truncatedColumns.includes(column.name))
              throw new Error(
                t(
                  "Truncated fields cannot be edited in the console.",
                  "잘린 필드는 콘솔에서 수정할 수 없다.",
                ),
              );
            values[column.name] = value;
          }
        if (editor.operation === "update" && !Object.keys(values).length)
          throw new Error(
            t("Change at least one field.", "변경할 필드를 입력한다."),
          );
        request = {
          requestId: crypto.randomUUID(),
          schema: tables[selected].schema,
          table: tables[selected].name,
          operation: editor.operation,
          values,
          key: editor.row?.key ?? {},
          version: editor.row?.version ?? null,
        };
        setPending(request);
      }
      await mutateRow(connection.id, request);
      setPending(null);
      setEditor(null);
      setNotice(t("Change committed.", "변경을 저장했다."));
      const page = await editableRows(
        connection.id,
        tables[selected],
        preview?.offset ?? 0,
      );
      setEditable(page);
      setPreview({
        columns: page.definition.columns,
        rows: page.rows.map((row) =>
          page.definition.columns.map((column) => row.values[column.name]),
        ),
        offset: page.offset,
        hasMore: page.hasMore,
      });
    } catch (e) {
      if (
        e instanceof ApiClientError &&
        e.status &&
        e.status >= 400 &&
        e.status < 500
      )
        setPending(null);
      setError((e as Error).message);
    } finally {
      setBusy(false);
    }
  }
  return (
    <div
      className="pipe-database-browser"
      aria-label={t("Database browser", "DB 조회")}
    >
      <div className="pipe-database-browser-heading">
        <h3>{connection.name}</h3>
        <button
          className="icon-btn"
          type="button"
          disabled={busy || !!editor || maintenanceDirty || servicesDirty}
          aria-label={t("Close browser", "조회 닫기")}
          onClick={onClose}
        >
          <X aria-hidden="true" />
        </button>
      </div>
      {error && (
        <div className="banner error" role="alert">
          {error}
        </div>
      )}
      {notice && (
        <div className="banner info" role="status">
          {notice}
        </div>
      )}
      <div className="pipe-database-toolbar">
        <div className="field">
          <label htmlFor="pipe-database-table">{t("Table", "테이블")}</label>
          <select
            id="pipe-database-table"
            value={selected ?? ""}
            disabled={busy || !!editor || !tables.length}
            onChange={(e) => {
              if (e.target.value !== "") void read(Number(e.target.value));
            }}
          >
            <option value="" disabled>
              {t("Select a table", "테이블 선택")}
            </option>
            {tables.map((table, index) => (
              <option key={JSON.stringify(table)} value={index}>
                {table.schema}.{table.name}
              </option>
            ))}
          </select>
        </div>
        <button
          className="btn"
          type="button"
          disabled={busy || !!editor}
          aria-label={t("Refresh tables", "테이블 새로고침")}
          onClick={() => {
            setBusy(true);
            setTables([]);
            setTruncated(false);
            setHistory([]);
            setPreview(null);
            setEditable(null);
            setSelected(null);
            setRevision((r) => r + 1);
          }}
        >
          <RefreshCw aria-hidden="true" />
          {t("Tables", "테이블")}
        </button>
        <button
          className="btn"
          type="button"
          disabled={busy || !!editor || selected === null}
          aria-label={t("Refresh rows", "데이터 새로고침")}
          onClick={() => {
            if (selected !== null) void read(selected);
          }}
        >
          <RefreshCw aria-hidden="true" />
          {t("Rows", "데이터")}
        </button>
        {editable && (
          <button
            className="btn primary"
            type="button"
            disabled={busy || !!editor || !editable.definition.transactional}
            onClick={() => edit("insert", null)}
          >
            {t("Add row", "행 추가")}
          </button>
        )}
      </div>
      {busy && <p role="status">{t("Reading database…", "DB 처리 중…")}</p>}
      {!busy && !tables.length && (
        <p className="empty">
          {t(
            "No accessible tables. Check SELECT permissions or refresh the table list.",
            "조회할 수 있는 테이블이 없다. SELECT 권한을 확인하거나 목록을 새로고침한다.",
          )}
        </p>
      )}
      {truncated && (
        <p className="poll-note">
          {t("Showing the first 500 tables.", "처음 500개 테이블을 표시한다.")}
        </p>
      )}
      {editor && editable && (
        <form
          className="pipe-database-form"
          onSubmit={save}
          aria-label={t("Row editor", "행 편집")}
        >
          <h4>
            {editor.operation === "insert"
              ? t("Add row", "행 추가")
              : editor.operation === "update"
                ? t("Edit row", "행 수정")
                : t("Delete row", "행 삭제")}
          </h4>
          {editor.operation === "delete" ? (
            <p>
              {t(
                "Delete this primary-key row? This change commits immediately.",
                "선택한 기본 키의 행을 삭제한다. 저장하면 즉시 적용한다.",
              )}
            </p>
          ) : (
            <div className="pipe-database-fields">
              {editable.definition.columns.filter(canChange).map((column) => (
                <div className="field" key={column.name}>
                  <label htmlFor={`db-field-${column.name}`}>
                    {column.name}
                    <small> {column.dataType}</small>
                  </label>
                  <select
                    aria-label={t(
                      `${column.name} value mode`,
                      `${column.name} 입력 방식`,
                    )}
                    value={fields[column.name]?.mode ?? "default"}
                    disabled={
                      busy ||
                      !!pending ||
                      editor.row?.truncatedColumns.includes(column.name)
                    }
                    onChange={(e) =>
                      setFields((current) => ({
                        ...current,
                        [column.name]: {
                          ...current[column.name],
                          mode: e.target.value as "default" | "value" | "null",
                        },
                      }))
                    }
                  >
                    <option value="default">
                      {editor.operation === "insert"
                        ? t("Use database default", "DB 기본값 사용")
                        : t("Keep current value", "현재 값 유지")}
                    </option>
                    <option value="value">{t("Value", "직접 입력")}</option>
                    {column.nullable && <option value="null">NULL</option>}
                  </select>
                  {fields[column.name]?.mode === "value" && (
                    <textarea
                      id={`db-field-${column.name}`}
                      value={fields[column.name]?.value ?? ""}
                      rows={2}
                      maxLength={32768}
                      disabled={
                        busy ||
                        !!pending ||
                        editor.row?.truncatedColumns.includes(column.name)
                      }
                      spellCheck={false}
                      onChange={(e) =>
                        setFields((current) => ({
                          ...current,
                          [column.name]: {
                            mode: "value",
                            value: e.target.value,
                          },
                        }))
                      }
                    />
                  )}
                  {column.defaultValue && (
                    <small>
                      {t("Default: ", "기본값: ")}
                      {column.defaultValue}
                    </small>
                  )}
                </div>
              ))}
            </div>
          )}
          <div className="pipe-row-actions">
            <button
              className={`btn ${editor.operation === "delete" ? "danger" : "primary"}`}
              disabled={busy}
              type="submit"
            >
              {pending
                ? t("Retry same request", "같은 요청 재시도")
                : editor.operation === "delete"
                  ? t("Delete row", "행 삭제")
                  : t("Save change", "변경 저장")}
            </button>
            <button
              className="btn ghost"
              type="button"
              disabled={busy || !!pending}
              onClick={() => {
                setEditor(null);
                setPending(null);
              }}
            >
              {t("Cancel", "취소")}
            </button>
          </div>
          {pending && (
            <p className="poll-note">
              {t(
                "The result is unknown. Retry the same request to check or finish it without duplicate changes.",
                "처리 결과가 확인되지 않았다. 같은 요청을 재시도하면 중복 변경 없이 결과를 확인한다.",
              )}
            </p>
          )}
        </form>
      )}
      {preview && (
        <>
          <p className="poll-note">
            {connection.writeEnabled
              ? t(
                  "Writes enabled · updates check the primary key and original row version.",
                  "쓰기 사용 중 · 수정 시 기본 키와 기존 행 버전을 검사한다.",
                )
              : t(
                  "Read-only · up to 50 rows, 2048 characters per cell. Binary values use hex. Row order may change.",
                  "읽기 전용 · 최대 50행, 셀당 2048자를 표시한다. 바이너리 값은 16진수로 표시한다. 행 순서가 달라질 수 있다.",
                )}
          </p>
          {preview.rows.length ? (
            <div className="table-scroll">
              <table
                className="vm-table pipe-database-preview"
                aria-label={t("Data preview", "데이터 미리보기")}
              >
                <thead>
                  <tr>
                    {preview.columns.map((column) => (
                      <th scope="col" key={column.name} title={column.dataType}>
                        {column.name}
                        <small>{column.dataType}</small>
                      </th>
                    ))}
                    {editable && <th scope="col">{t("Actions", "작업")}</th>}
                  </tr>
                </thead>
                <tbody>
                  {preview.rows.map((row, index) => (
                    <tr key={index}>
                      {row.map((value, cell) => (
                        <td key={preview.columns[cell].name}>
                          <div
                            className="pipe-database-cell"
                            tabIndex={
                              value !== null && value.length > 160
                                ? 0
                                : undefined
                            }
                          >
                            {value === null ? (
                              <span className="pipe-database-null">NULL</span>
                            ) : (
                              value
                            )}
                          </div>
                        </td>
                      ))}
                      {editable && (
                        <td>
                          <div className="pipe-row-actions">
                            <button
                              className="btn sm"
                              type="button"
                              disabled={
                                busy ||
                                !!editor ||
                                !editable.definition.primaryKey.length ||
                                !editable.rows[index].version
                              }
                              onClick={() =>
                                edit("update", editable.rows[index])
                              }
                            >
                              {t("Edit", "수정")}
                            </button>
                            <button
                              className="btn sm danger"
                              type="button"
                              disabled={
                                busy ||
                                !!editor ||
                                !editable.definition.primaryKey.length ||
                                !editable.rows[index].version
                              }
                              onClick={() =>
                                edit("delete", editable.rows[index])
                              }
                            >
                              {t("Delete", "삭제")}
                            </button>
                          </div>
                        </td>
                      )}
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          ) : (
            <p className="empty">
              {t(
                "This table has no rows on this page.",
                "이 페이지에 데이터가 없다.",
              )}
            </p>
          )}
          <div className="pipe-database-pagination">
            <span>
              {t(
                `${preview.rows.length} rows · offset ${preview.offset}`,
                `${preview.rows.length}행 · 시작 위치 ${preview.offset}`,
              )}
            </span>
            <div className="pipe-row-actions">
              <button
                className="btn sm"
                type="button"
                disabled={busy || !!editor || !history.length}
                aria-label={t("Previous rows", "이전 행")}
                onClick={() => {
                  if (selected !== null)
                    void read(selected, history.at(-1)!, history.slice(0, -1));
                }}
              >
                <ChevronLeft aria-hidden="true" />
                {t("Previous", "이전")}
              </button>
              <button
                className="btn sm"
                type="button"
                disabled={
                  busy ||
                  !!editor ||
                  !preview.hasMore ||
                  !preview.rows.length ||
                  preview.offset + preview.rows.length > 100000
                }
                aria-label={t("Next rows", "다음 행")}
                onClick={() => {
                  if (selected !== null)
                    void read(selected, preview.offset + preview.rows.length, [
                      ...history,
                      preview.offset,
                    ]);
                }}
              >
                {t("Next", "다음")}
                <ChevronRight aria-hidden="true" />
              </button>
            </div>
          </div>
        </>
      )}
      <DatabaseServices
        connection={connection}
        tables={tables}
        onDirty={setServicesDirty}
      />
      <DatabaseMaintenance
        connection={connection}
        onDirty={setMaintenanceDirty}
      />
    </div>
  );
}
