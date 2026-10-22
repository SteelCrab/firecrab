import { X } from "lucide-react";
import { useI18n } from "../i18n";

interface BannerProps {
  kind: "error" | "info";
  text: string;
  onDismiss: () => void;
}

export default function Banner({ kind, text, onDismiss }: BannerProps) {
  const { t } = useI18n();
  return (
    <div className={`banner ${kind}`} role={kind === "error" ? "alert" : "status"}>
      <span>{text}</span>
      <button type="button" className="icon-btn dismiss" onClick={onDismiss} aria-label={t("Dismiss", "닫기")}>
        <X aria-hidden="true" />
      </button>
    </div>
  );
}
