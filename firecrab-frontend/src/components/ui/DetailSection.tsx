import { useState } from "react";
import type { ReactNode } from "react";
import { Check, Copy } from "lucide-react";
import { useI18n } from "../../i18n";
import { copyText } from "../../lib/textExport";

interface DetailHeadProps {
  title: string;
  /** One line under the title: state badge, address, version. */
  meta?: ReactNode;
  /** Buttons for what the item can do now. */
  actions?: ReactNode;
}

/** Title block at the top of a detail panel (spec §6.3). */
export function DetailHead({ title, meta, actions }: DetailHeadProps) {
  return (
    <div className="detail-head">
      <h2 className="detail-title">{title}</h2>
      {meta && <div className="detail-meta">{meta}</div>}
      {actions && <div className="detail-actions">{actions}</div>}
    </div>
  );
}

interface DetailSectionProps {
  title: string;
  /** Right side of the heading: a note or an edit button. */
  aside?: ReactNode;
  children: ReactNode;
}

/** A titled block of a detail panel; the title is also its accessible name. */
export function DetailSection({ title, aside, children }: DetailSectionProps) {
  return (
    <section className="detail-sec" aria-label={title}>
      <div className="detail-sec-head">
        <h3>{title}</h3>
        {aside}
      </div>
      {children}
    </section>
  );
}

interface DetailPropProps {
  label: string;
  children: ReactNode;
  /** Text the copy button puts on the clipboard. */
  copy?: string;
  /** Let a long or multi-line value wrap instead of ending in "…". */
  wrap?: boolean;
}

/** One label/value row of a `<dl className="props">`. */
export function DetailProp({ label, children, copy, wrap }: DetailPropProps) {
  return (
    <>
      <dt>{label}</dt>
      <dd>
        <span className={wrap ? "prop-value wrap" : "prop-value"}>{children}</span>
        {copy && <CopyButton text={copy} />}
      </dd>
    </>
  );
}

export function CopyButton({ text }: { text: string }) {
  const { t } = useI18n();
  const [copied, setCopied] = useState(false);
  return (
    <button
      type="button"
      className="icon-btn prop-action"
      aria-label={copied ? t("Copied", "복사됨") : t("Copy", "복사")}
      title={copied ? t("Copied", "복사됨") : t("Copy", "복사")}
      onClick={() => {
        void copyText(text).then((ok) => {
          if (!ok) return;
          setCopied(true);
          setTimeout(() => setCopied(false), 1500);
        });
      }}
    >
      {copied ? <Check aria-hidden="true" /> : <Copy aria-hidden="true" />}
    </button>
  );
}
