import { useI18n } from "../i18n";
import type { AgentPreview } from "../api/agents";

/** The session's latest input and the CLI's reply to it, clamped to a few lines. */
export default function AgentCardPreview({
  preview,
}: {
  preview: AgentPreview | undefined;
}) {
  const { t } = useI18n();
  if (!preview?.input && !preview?.response)
    return (
      <span className="am-board-card-preview empty">
        {t("No messages yet", "아직 메시지가 없어요")}
      </span>
    );
  return (
    <span className="am-board-card-preview">
      <span className="am-board-card-message">
        <span className="am-board-card-label">{t("Input", "입력")}</span>
        <span className={`am-board-card-text${preview.input ? "" : " muted"}`}>
          {preview.input || "—"}
        </span>
      </span>
      <span className="am-board-card-message response">
        <span className="am-board-card-label">{t("Response", "응답")}</span>
        <span
          className={`am-board-card-text${preview.response ? "" : " muted"}`}
        >
          {preview.response ||
            t("Waiting for a response", "응답을 기다리고 있어요")}
        </span>
      </span>
    </span>
  );
}
