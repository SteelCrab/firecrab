import { useEffect, useRef, type ReactNode } from "react";
import { X } from "lucide-react";
import { useI18n } from "../../i18n";
import { isPaletteShortcut } from "../../lib/shortcuts";

export default function StorageDialog({ title, busy, onClose, children }: {
  title: string; busy: boolean; onClose: () => void; children: ReactNode;
}) {
  const { t } = useI18n();
  const dialog = useRef<HTMLDialogElement>(null);
  useEffect(() => {
    const element = dialog.current!;
    const opener = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    element.showModal();
    element.querySelector<HTMLInputElement>("input:not([type=checkbox]):not(:disabled)")?.focus();
    return () => { element.close(); if (opener?.isConnected) opener.focus({ preventScroll: true }); };
  }, []);
  return <dialog ref={dialog} className="s3-dialog" aria-label={title}
    onKeyDown={event => { if (isPaletteShortcut(event.nativeEvent)) { event.preventDefault(); event.stopPropagation(); } }}
    onCancel={event => { event.preventDefault(); if (!busy) onClose(); }}>
    <header className="s3-dialog-head"><h2>{title}</h2><button type="button" className="icon-btn" aria-label={t("Close", "닫기")} disabled={busy} onClick={onClose}><X aria-hidden="true" /></button></header>
    {children}
  </dialog>;
}
