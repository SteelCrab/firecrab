import type { Run } from "../../api/code";
import { useI18n } from "../../i18n";
export default function Observation({ row }: { row: Run }) {
  const { t } = useI18n();
  const names: Record<string, string> = {
    queued: t("Queued", "대기 중"),
    starting: t("Preparing agent…", "에이전트 준비 중…"),
    running: t("Running", "실행 중"),
    cancelling: t("Cancelling…", "취소 중…"),
    succeeded: t("Completed", "완료"),
    failed: t("Failed", "실패"),
    cancelled: t("Cancelled", "취소됨"),
    interrupted: t("Interrupted", "중단됨"),
  };
  return (
    <div key={row.id} className="code-agent-observation">
      <p role="status">
        <strong>{names[row.phase] ?? row.phase}</strong>
      </p>
      {row.output && (
        <pre tabIndex={0} aria-label={t("Agent output", "에이전트 출력")}>
          {row.output}
        </pre>
      )}
      {row.phase === "interrupted" && (
        <p>
          {t(
            "Files may already have changed. Review the workspace before starting a new task.",
            "파일이 이미 변경됐을 수 있습니다. 새 작업을 시작하기 전에 워크스페이스를 확인해 주세요.",
          )}
        </p>
      )}
      {row.truncated && (
        <p>
          {t("Output capture limit reached.", "출력 저장 한도에 도달했습니다.")}
        </p>
      )}
      <details>
        <summary>{t("Run details", "실행 상세")}</summary>
        <p>
          {row.agent} {row.cliVersion} ·{" "}
          {(row.modelsUsed ?? []).join(", ") || row.model} ·{" "}
          {row.providerRequests}
        </p>
        <code>{row.error}</code>
      </details>
    </div>
  );
}
