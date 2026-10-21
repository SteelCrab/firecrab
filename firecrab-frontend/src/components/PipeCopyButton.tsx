import { useEffect, useRef, useState } from "react";
import { copyText } from "../lib/textExport";
import { useI18n } from "../i18n";

export default function PipeCopyButton({ url, onError }: { url: string; onError: (message: string) => void }) {
  const { t } = useI18n();
  const [copiedUrl, setCopiedUrl] = useState<string | null>(null);
  const copied = copiedUrl === url;
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  useEffect(() => () => { if (timer.current) clearTimeout(timer.current); }, []);
  async function copy() {
    if (!await copyText(url)) { onError(t("Could not copy the address. Select the link to copy it.", "주소를 복사하지 못했다. 링크를 선택해 직접 복사한다.")); return; }
    setCopiedUrl(url);
    if (timer.current) clearTimeout(timer.current);
    timer.current = setTimeout(() => setCopiedUrl(null), 1800);
  }
  return <button type="button" className={"btn pipe-copy-button" + (copied ? " copied" : "")} aria-label={t("Copy URL", "주소 복사")} title={copied ? t("Copied", "복사됨") : t("Copy URL", "주소 복사")} onClick={() => void copy()}>
    <svg aria-hidden="true" width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.7" strokeLinecap="round" strokeLinejoin="round">
      {copied ? <path d="m5 12 4 4L19 6" /> : <><rect x="8" y="8" width="12" height="12" rx="2" /><path d="M16 8V5a2 2 0 0 0-2-2H5a2 2 0 0 0-2 2v9a2 2 0 0 0 2 2h3" /></>}
    </svg>
  </button>;
}
