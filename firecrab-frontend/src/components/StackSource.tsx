import { useI18n } from "../i18n";

export type Source = {
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

export default function StackSource({
  name,
  source,
  error,
}: {
  name: string;
  source?: Source | null;
  error?: string;
}) {
  const { t } = useI18n();
  const code = source?.latest?.error;
  const guidance =
    code === "credential_unavailable" ||
    code === "source_access_denied_or_rate_limited"
      ? t(
          "Check the GitHub connection, repository access and rate limit, then check again.",
          "GitHub 연결, 저장소 권한과 요청 제한을 확인한 뒤 다시 확인한다.",
        )
      : code === "source_not_found" ||
          code === "manifest_path_not_found" ||
          code === "manifest_path_not_regular"
        ? t(
            "Check the repository, ref and manifest path, then check again.",
            "저장소, 참조와 매니페스트 경로를 확인한 뒤 다시 확인한다.",
          )
        : code === "invalid_manifest" || code === "manifest_stack_name_mismatch"
          ? t(
              "Validate the manifest and stack name in Git, then check again.",
              "Git의 매니페스트와 Stack 이름을 검증한 뒤 다시 확인한다.",
            )
          : t(
              "Inspect the source status and resolve the error before retrying.",
              "Git 상태를 확인하고 오류를 해결한 뒤 다시 시도한다.",
            );
  return (
    <section className="stack-source-panel">
      <h3>{t("Git source", "Git 소스")}</h3>
      {error ? (
        <p role="alert">
          {t("Source unavailable", "Git 상태를 불러오지 못했다")}: {error}
        </p>
      ) : !source ? (
        <p>
          {t("Git source is not configured.", "Git 소스가 설정되지 않았다.")}
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
            <>
              <p role="alert">{source.latest.error}</p>
              <p>{guidance}</p>
            </>
          )}
          {source.recoveryOperationId && (
            <section>
              <h3>{t("Recovery", "복구")}</h3>
              <p>
                {t(
                  "Inspect and repair the operation before recovery. Recovery creates a new plan.",
                  "작업과 리소스를 확인하고 수정한 뒤 복구한다. 복구는 새 계획을 만든다.",
                )}
              </p>
              <code>{`firecrab stack source sync ${name} --generation ${source.generation ?? 0} --recover-operation-id ${source.recoveryOperationId}`}</code>
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
                "Git에서 매니페스트를 되돌리고 새 커밋을 확인한다. 게스트 디스크와 앱 데이터는 복원되지 않는다.",
              )}
            </p>
            <p>
              {t(
                "Automatic mode applies the commit on the next poll when active. In detect-only or paused mode, run a manual sync.",
                "자동 방식이 활성 상태면 다음 확인 때 커밋을 적용한다. 감지만 사용하거나 일시 중지한 경우 수동 동기화가 필요하다.",
              )}
            </p>
          </section>
        </>
      )}
    </section>
  );
}
