import { useEffect, useRef } from "react";
import { KeyRound, X } from "lucide-react";
import type { VmResponse } from "../../bindings";
import { useI18n } from "../../i18n";
import StatusBadge from "../ui/StatusBadge";
import SshAccess from "./SshAccess";

interface SshDialogProps {
  vm: VmResponse;
  onClose: () => void;
}

/** SSH access for one VM over the list and detail panel; Esc or the backdrop closes it. */
export default function SshDialog({ vm, onClose }: SshDialogProps) {
  const { t } = useI18n();
  const dialogRef = useRef<HTMLDivElement>(null);

  // Focus moves into the dialog and back to whatever opened it.
  useEffect(() => {
    const opener = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    dialogRef.current?.focus();
    return () => opener?.focus();
  }, []);

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key !== "Escape" || event.defaultPrevented) return;
      event.preventDefault();
      onClose();
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [onClose]);

  const title = `SSH — ${vm.name}`;
  return (
    <div className="ssh-overlay" role="presentation" onMouseDown={(event) => event.target === event.currentTarget && onClose()}>
      <div ref={dialogRef} className="ssh-dialog" role="dialog" aria-modal="true" aria-label={title} tabIndex={-1}>
        <div className="ssh-dialog-head">
          <span className="ssh-dialog-icon" aria-hidden="true"><KeyRound /></span>
          <div className="ssh-dialog-titles">
            <h2 className="ssh-dialog-title">{t("SSH connect", "SSH 연결")}</h2>
            <p className="ssh-dialog-sub">
              <span>{vm.name}</span>
              {vm.ipv4 && <span className="mono">{vm.ipv4}</span>}
            </p>
          </div>
          <StatusBadge state={vm.state} />
          <button type="button" className="icon-btn" aria-label={t("Close", "닫기")} title={`${t("Close", "닫기")} (esc)`} onClick={onClose}>
            <X aria-hidden="true" />
          </button>
        </div>
        <div className="ssh-dialog-body">
          <SshAccess vm={vm} />
        </div>
      </div>
    </div>
  );
}
