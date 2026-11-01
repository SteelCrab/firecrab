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
  const dialogRef = useRef<HTMLDialogElement>(null);

  // Focus moves into the dialog and back to whatever opened it.
  useEffect(() => {
    const opener =
      document.activeElement instanceof HTMLElement
        ? document.activeElement
        : null;
    const dialog = dialogRef.current;
    dialog?.showModal();
    return () => {
      dialog?.close();
      opener?.focus();
    };
  }, []);

  const title = `SSH — ${vm.name}`;
  return (
    <dialog
      ref={dialogRef}
      className="ssh-dialog"
      aria-label={title}
      onCancel={(event) => {
        event.preventDefault();
        onClose();
      }}
      onClick={(event) => {
        if (event.target !== event.currentTarget) return;
        const box = event.currentTarget.getBoundingClientRect();
        if (
          event.clientX < box.left ||
          event.clientX > box.right ||
          event.clientY < box.top ||
          event.clientY > box.bottom
        )
          onClose();
      }}
    >
      <div className="ssh-dialog-head">
        <span className="ssh-dialog-icon" aria-hidden="true">
          <KeyRound />
        </span>
        <div className="ssh-dialog-titles">
          <h2 className="ssh-dialog-title">{t("SSH connect", "SSH 연결")}</h2>
          <p className="ssh-dialog-sub">
            <span>{vm.name}</span>
            {vm.ipv4 && <span className="mono">{vm.ipv4}</span>}
          </p>
        </div>
        <StatusBadge state={vm.state} />
        <button
          type="button"
          className="icon-btn"
          aria-label={t("Close", "닫기")}
          title={`${t("Close", "닫기")} (esc)`}
          onClick={onClose}
        >
          <X aria-hidden="true" />
        </button>
      </div>
      <div className="ssh-dialog-body">
        <SshAccess vm={vm} />
      </div>
    </dialog>
  );
}
