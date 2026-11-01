import { useEffect, useState } from "react";
import { ApiClientError, fetchJson } from "../api/client";
import { useI18n } from "../i18n";
import Screen from "./layout/Screen";

type Stack = { name: string };
type Source = {
  source?: {
    repository?: string;
    ref?: string;
    syncMode?: string;
    paused?: boolean;
  };
  latest?: { state?: string; error?: string | null };
  desiredRevision?: { commit?: string };
  attemptedRevision?: { commit?: string };
  successfulRevision?: { commit?: string };
  recoveryOperationId?: string;
  generation?: number;
  lastApply?: {
    state?: string;
    changes?: {
      kind?: string;
      key?: string;
      resourceId?: string;
      state?: string;
    }[];
  };
};
type Row = { stack: Stack; source?: Source | null; error?: string };

export default function Stacks() {
  const { t } = useI18n();
  const [rows, setRows] = useState<Row[] | null>(null);
  const [error, setError] = useState("");
  useEffect(() => {
    let active = true;
    let timer = 0;
    const refresh = async () => {
      try {
        const stacks = await fetchJson<Stack[]>("/api/stacks");
        if (!Array.isArray(stacks)) throw new Error("Invalid stack response");
        const next = await Promise.all(
          stacks.map(async (stack) => {
            try {
              return {
                stack,
                source: await fetchJson<Source>(
                  `/api/stacks/${encodeURIComponent(stack.name)}/source`,
                ),
              };
            } catch (failure) {
              return failure instanceof ApiClientError && failure.status === 404
                ? { stack, source: null }
                : {
                    stack,
                    error:
                      failure instanceof Error
                        ? failure.message
                        : String(failure),
                  };
            }
          }),
        );
        if (active) {
          setRows(next);
          setError("");
        }
      } catch (failure) {
        if (active)
          setError(
            failure instanceof Error ? failure.message : String(failure),
          );
      }
      if (active) timer = window.setTimeout(() => void refresh(), 3000);
    };
    void refresh();
    return () => {
      active = false;
      window.clearTimeout(timer);
    };
  }, []);
  return (
    <Screen view="stacks" live={false}>
      {error && <p role="alert">{error}</p>}
      {rows === null && !error && <p>{t("Loading…", "불러오는 중…")}</p>}
      {rows?.length === 0 && !error && (
        <section
          className="panel stack-setup"
          aria-label={t("Set up a Stack", "Stack 구성 방법")}
        >
          <h2>{t("Set up a Stack", "Stack 구성 방법")}</h2>
          <p>{t("No registered stacks.", "등록된 Stack이 없습니다.")}</p>
          <p>
            {t(
              "Use the Firecrab CLI connected to this host.",
              "현재 호스트에 연결된 Firecrab CLI에서 진행하세요.",
            )}
          </p>
          <ol>
            <li>
              {t(
                "Write the network and required MicroVMs or MicroServices in stack.yaml.",
                "stack.yaml 파일에 네트워크와 필요한 MicroVM 또는 MicroService를 정의하세요.",
              )}
            </li>
            <li>
              {t(
                "Validate the file and register its Stack name.",
                "파일을 검증한 뒤 Stack 이름을 등록하세요.",
              )}
              <pre>
                firecrab stack validate --file stack.yaml{"\n"}firecrab stack
                register --name demo
              </pre>
            </li>
            <li>
              {t(
                "Review the planned changes, then apply them.",
                "변경 계획을 확인한 뒤 적용하세요.",
              )}
              <pre>
                firecrab stack plan --file stack.yaml --json &gt; plan.json
                {"\n"}firecrab stack apply --file stack.yaml --expected-plan
                plan.json
              </pre>
            </li>
            <li>
              {t(
                "The registered Stack appears here automatically.",
                "등록된 Stack은 이 화면에 자동으로 표시됩니다.",
              )}
            </li>
          </ol>
          <p>
            {t(
              "Replace demo with the name in stack.yaml.",
              "예시의 demo를 stack.yaml의 name 값으로 바꾸세요.",
            )}
          </p>
          <a
            href="https://github.com/SteelCrab/firecrab/blob/main/public-docs/guides/stacks.md#schema"
            target="_blank"
            rel="noopener noreferrer"
          >
            {t("View the YAML example", "YAML 작성 예시 보기")}
          </a>
        </section>
      )}
      {rows?.map(({ stack, source, error: sourceError }) => (
        <section className="panel" key={stack.name}>
          <h2>{stack.name}</h2>
          {sourceError ? (
            <p role="alert">
              {t("Source unavailable", "Git 상태를 불러오지 못했습니다")}:{" "}
              {sourceError}
            </p>
          ) : !source ? (
            <p>
              {t(
                "Git source is not configured.",
                "Git 소스가 설정되지 않았습니다.",
              )}
            </p>
          ) : (
            <>
              <dl className="stack-source">
                <dt>{t("Status", "상태")}</dt>
                <dd>
                  {source.latest?.state
                    ? source.latest.state === "recovery_required"
                      ? t("Recovery required", "복구 필요")
                      : source.latest.state
                          .replaceAll("_", " ")
                          .replace(/^./, (letter) => letter.toUpperCase())
                    : "—"}
                </dd>
                <dt>{t("Mode", "방식")}</dt>
                <dd>
                  {source.source?.paused
                    ? t("Paused", "일시 중지")
                    : source.source?.syncMode === "automatic"
                      ? t("Automatic", "자동")
                      : t("Detect only", "감지만")}
                </dd>
                <dt>{t("Repository", "저장소")}</dt>
                <dd>{source.source?.repository || "—"}</dd>
                <dt>{t("Ref", "참조")}</dt>
                <dd>{source.source?.ref || "—"}</dd>
                <dt>{t("Desired commit", "요청 커밋")}</dt>
                <dd>{source.desiredRevision?.commit || "—"}</dd>
                <dt>{t("Attempted commit", "시도 커밋")}</dt>
                <dd>{source.attemptedRevision?.commit || "—"}</dd>
                <dt>{t("Applied commit", "적용 커밋")}</dt>
                <dd>{source.successfulRevision?.commit || "—"}</dd>
              </dl>
              {source.latest?.error && (
                <p role="alert">{source.latest.error}</p>
              )}
              {source.recoveryOperationId && (
                <section>
                  <h3>{t("Recovery", "복구")}</h3>
                  <p>
                    {t(
                      "Inspect and repair the operation before recovery. Recovery creates a new plan.",
                      "작업과 리소스를 확인하고 필요한 내용을 수정한 뒤 복구하세요. 복구 시 새 계획이 생성됩니다.",
                    )}
                  </p>
                  <code>{`firecrab stack source sync ${stack.name} --generation ${source.generation ?? 0} --recover-operation-id ${source.recoveryOperationId}`}</code>
                  {source.lastApply && (
                    <>
                      <p>
                        {t("Operation status", "작업 상태")}:{" "}
                        {source.lastApply.state || "—"}
                      </p>
                      <ul>
                        {source.lastApply.changes?.map((change, index) => (
                          <li key={index}>
                            {[
                              change.kind,
                              change.key,
                              change.resourceId,
                              change.state,
                            ]
                              .map((value) => value || "—")
                              .join(" · ")}
                          </li>
                        ))}
                      </ul>
                    </>
                  )}
                </section>
              )}
              <section>
                <h3>{t("Revert", "되돌리기")}</h3>
                <p>
                  {t(
                    "Revert the manifest in Git and review the resulting commit. Guest disks and application data are not restored.",
                    "Git에서 매니페스트를 되돌린 뒤 새 커밋을 확인하세요. 게스트 디스크와 앱 데이터는 복원되지 않습니다.",
                  )}
                </p>
              </section>
            </>
          )}
        </section>
      ))}
    </Screen>
  );
}
