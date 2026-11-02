import { useEffect, useRef, useState } from "react";
import { fetchJson } from "../api/client";
import { useI18n } from "../i18n";
import { VIEW_ICONS } from "../viewIcons";
import { resourceKind } from "./StackResources";

type Operation = {
  operationId: string;
  action: string;
  state: "running" | "needs_recovery" | "succeeded" | "failed";
  executionGeneration?: number;
  retainDisks?: boolean;
  errorCode?: string | null;
  ownership: {
    endpointId: string;
    resource: { kind: string; id: string };
  };
};
type Page = { operations: Operation[]; nextBefore: string | null };
const ACTIONS: Record<string, [string, string]> = {
  configure_database_broker: ["Configure database broker", "DB 브로커 설정"],
  create_database_events: ["Create database events", "DB 이벤트 생성"],
  pause_database_events: ["Change event delivery", "이벤트 전달 변경"],
  detach_database_events: ["Detach database events", "DB 이벤트 분리"],
  delete_database_events: ["Delete database events", "DB 이벤트 삭제"],
  create_database_binding: ["Create database binding", "DB 바인딩 생성"],
  delete_database_binding: ["Delete database binding", "DB 바인딩 삭제"],
  create_database_connection: ["Create database connection", "DB 연결 생성"],
  update_database_connection: ["Update database connection", "DB 연결 수정"],
  delete_vm: ["Delete VM", "VM 삭제"],
  delete_sandbox_project: [
    "Delete project workspace",
    "프로젝트 작업 공간 삭제",
  ],
  install_queue_engine: ["Install engine", "엔진 설치"],
  start_queue_engine: ["Start engine", "엔진 시작"],
  stop_queue_engine: ["Stop engine", "엔진 중지"],
  revoke_deploy_token: ["Revoke deploy token", "배포 토큰 회수"],
  delete_stack_source: ["Remove Git source", "Git 소스 제거"],
  revoke_sandbox_access: ["Revoke sandbox access", "Sandbox 접근 권한 회수"],
};
function validOperation(value: Operation): boolean {
  return (
    !!value &&
    typeof value.operationId === "string" &&
    typeof value.action === "string" &&
    ["running", "needs_recovery", "succeeded", "failed"].includes(
      value.state,
    ) &&
    typeof value.ownership?.endpointId === "string" &&
    typeof value.ownership?.resource?.kind === "string" &&
    typeof value.ownership?.resource?.id === "string"
  );
}
const RECOVERABLE = new Set([
  "configure_database_broker",
  "create_database_events",
  "pause_database_events",
  "detach_database_events",
  "delete_database_events",
  "create_database_binding",
  "delete_database_binding",
  "create_database_connection",
  "update_database_connection",
  "delete_vm",
  "delete_sandbox_project",
  "install_queue_engine",
  "start_queue_engine",
  "stop_queue_engine",
]);

export default function NativeOperations() {
  const { t } = useI18n();
  const [operations, setOperations] = useState<Operation[]>([]);
  const [pages, setPages] = useState(1);
  const [more, setMore] = useState(false);
  const [lookupError, setLookupError] = useState("");
  const [actionError, setActionError] = useState("");
  const [result, setResult] = useState("");
  const [resultId, setResultId] = useState("");
  const [busy, setBusy] = useState("");
  const mounted = useRef(false);
  const sequence = useRef(0);
  const [revision, setRevision] = useState(0);
  useEffect(() => {
    mounted.current = true;
    let active = true;
    let timer = 0;
    const refresh = async () => {
      const request = ++sequence.current;
      try {
        let before: string | null = null;
        const seen = new Set<string>();
        const next: Operation[] = [];
        for (let index = 0; index < pages; index++) {
          const page: Page = await fetchJson<Page>(
            `/api/iac/native-operations${before ? `?before=${encodeURIComponent(before)}` : ""}`,
          );
          if (
            !Array.isArray(page.operations) ||
            !(page.nextBefore === null || typeof page.nextBefore === "string")
          ) {
            throw new Error("Invalid native operation page");
          }
          if (
            !page.operations.every(
              (operation) =>
                validOperation(operation) &&
                ["running", "needs_recovery"].includes(operation.state),
            )
          )
            throw new Error("Invalid active operation");
          next.push(...page.operations);
          before = page.nextBefore;
          if (!before) break;
          if (seen.has(before))
            throw new Error("Repeated native operation cursor");
          seen.add(before);
        }
        if (active && request === sequence.current) {
          setOperations(next);
          setMore(before !== null);
          setLookupError("");
        }
      } catch (failure) {
        if (active && request === sequence.current)
          setLookupError(
            failure instanceof Error ? failure.message : String(failure),
          );
      }
      if (active) timer = window.setTimeout(() => void refresh(), 3000);
    };
    void refresh();
    return () => {
      active = false;
      mounted.current = false;
      window.clearTimeout(timer);
    };
  }, [pages, revision]);

  const recover = async (operation: Operation) => {
    setBusy(operation.operationId);
    setActionError("");
    setResult("");
    setResultId(operation.operationId);
    sequence.current++;
    try {
      let observed: Operation;
      const path = `/api/iac/native-operations/${encodeURIComponent(operation.operationId)}`;
      try {
        observed = await fetchJson<Operation>(`${path}/recover`, {
          method: "POST",
          headers: { "x-firecrab-endpoint-id": operation.ownership.endpointId },
        });
      } catch (failure) {
        // A lost response cannot justify dispatching another action.
        observed = await fetchJson<Operation>(path).catch(() => {
          throw failure;
        });
        if (observed.state === "needs_recovery") throw failure;
      }
      if (
        !validOperation(observed) ||
        observed.action !== operation.action ||
        observed.retainDisks !== operation.retainDisks ||
        observed.operationId !== operation.operationId ||
        observed.ownership.endpointId !== operation.ownership.endpointId ||
        observed.ownership.resource.kind !==
          operation.ownership.resource.kind ||
        observed.ownership.resource.id !== operation.ownership.resource.id
      ) {
        throw new Error("Native operation identity changed");
      }
      if (observed.state === "failed") {
        if (mounted.current)
          setOperations((current) =>
            current.filter((item) => item.operationId !== observed.operationId),
          );
        throw new Error(
          t(
            "Original operation failed before changes. Submit corrected configuration.",
            "변경 전에 원래 작업이 실패했다. 수정한 설정을 제출한다.",
          ),
        );
      }
      if (observed.state === "needs_recovery")
        throw new Error(
          t(
            "Original operation still needs recovery.",
            "원래 작업에 복구가 계속 필요하다.",
          ),
        );
      if (mounted.current) {
        setResult(
          observed.state === "succeeded"
            ? t("Original operation completed.", "원래 작업을 완료했다.")
            : t("Original operation is running.", "원래 작업을 실행 중이다."),
        );
        setOperations((current) =>
          observed.state === "succeeded"
            ? current.filter(
                (item) => item.operationId !== observed.operationId,
              )
            : current.map((item) =>
                item.operationId === observed.operationId ? observed : item,
              ),
        );
      }
    } catch (failure) {
      if (mounted.current)
        setActionError(
          failure instanceof Error ? failure.message : String(failure),
        );
    } finally {
      if (mounted.current) {
        setBusy("");
        setRevision((value) => value + 1);
      }
    }
  };

  if (!operations.length && !lookupError && !actionError && !result)
    return null;
  return (
    <section
      className="panel stack-native-work"
      aria-label={t("Native operations", "Native 작업")}
    >
      <h2>{t("Native operations", "Native 작업")}</h2>
      <p>
        {t(
          "Uses each original resource, operation ID and recorded policy.",
          "원래 리소스, 작업 ID, 기록된 정책을 사용한다.",
        )}
      </p>
      {(lookupError || actionError) && (
        <p role="alert">
          {t(
            "Native operations unavailable",
            "Native 작업 조회 또는 복구 실패",
          )}
          : {lookupError || actionError}
        </p>
      )}
      {result && (
        <>
          <p role="status">{result}</p>
          <p className="mono stack-native-result">{resultId}</p>
        </>
      )}
      {operations.map((operation) => {
        const [view, en, ko] = resourceKind(operation.ownership.resource.kind);
        const Icon = VIEW_ICONS[view];
        const recoverable =
          RECOVERABLE.has(operation.action) &&
          (operation.action !== "delete_vm" ||
            typeof operation.retainDisks === "boolean");
        return (
          <article
            className="stack-native-operation"
            data-operation-id={operation.operationId}
            key={operation.operationId}
          >
            <header className="stack-resource-heading">
              <Icon aria-hidden="true" />
              <h3>{t(en, ko)}</h3>
            </header>
            <dl className="stack-status">
              <dt>{t("Resource ID", "리소스 ID")}</dt>
              <dd className="mono">{operation.ownership.resource.id}</dd>
              <dt>{t("Operation ID", "작업 ID")}</dt>
              <dd className="mono">{operation.operationId}</dd>
              <dt>{t("Action", "동작")}</dt>
              <dd>
                {t(
                  ...(ACTIONS[operation.action] ?? [
                    "Unsupported action",
                    "지원하지 않는 동작",
                  ]),
                )}
              </dd>
              <dt>{t("State", "상태")}</dt>
              <dd>
                {operation.state === "needs_recovery"
                  ? t("Needs recovery", "복구 필요")
                  : t("Running", "실행 중")}
              </dd>
              {operation.errorCode && (
                <>
                  <dt>{t("Error category", "오류 종류")}</dt>
                  <dd className="mono">{operation.errorCode}</dd>
                </>
              )}
              <dt>{t("Recovery attempt", "복구 차수")}</dt>
              <dd>{operation.executionGeneration ?? 0}</dd>
              {operation.action === "delete_vm" && (
                <>
                  <dt>{t("VM artifacts", "VM 파일")}</dt>
                  <dd>
                    {operation.retainDisks === true
                      ? t("Retain private artifacts", "개인 파일 보존")
                      : operation.retainDisks === false
                        ? t("Delete private artifacts", "개인 파일 삭제")
                        : t("Policy unavailable", "정책 조회 불가")}
                  </dd>
                </>
              )}
            </dl>
            {operation.state === "needs_recovery" && recoverable && (
              <button
                type="button"
                className={
                  operation.action.startsWith("delete_") ? "btn danger" : "btn"
                }
                disabled={!!busy || !!lookupError}
                onClick={() => void recover(operation)}
              >
                {busy === operation.operationId
                  ? t("Recovering…", "복구 중…")
                  : operation.action === "delete_vm"
                    ? t("Resume VM deletion", "VM 삭제 재개")
                    : t("Recover original operation", "원래 작업 복구")}
              </button>
            )}
          </article>
        );
      })}
      {more && (
        <button
          type="button"
          className="btn"
          disabled={!!busy || !!lookupError}
          onClick={() => setPages((value) => value + 1)}
        >
          {t("Load more active operations", "진행 중 작업 더 보기")}
        </button>
      )}
    </section>
  );
}
