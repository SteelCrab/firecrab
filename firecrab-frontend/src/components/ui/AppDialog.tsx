import { useEffect, useRef, type ReactNode } from "react";

/** Native modal behavior keeps keyboard focus inside and restores its opener. */
export default function AppDialog({
  label,
  busy = false,
  onClose,
  children,
}: {
  label: string;
  busy?: boolean;
  onClose: () => void;
  children: ReactNode;
}) {
  const dialog = useRef<HTMLDialogElement>(null);
  useEffect(() => {
    const opener =
      document.activeElement instanceof HTMLElement
        ? document.activeElement
        : null;
    const modal = dialog.current;
    modal?.showModal();
    return () => {
      modal?.close();
      opener?.focus();
    };
  }, []);
  return (
    <dialog
      className="app-dialog"
      ref={dialog}
      aria-label={label}
      onCancel={(event) => {
        event.preventDefault();
        if (!busy) onClose();
      }}
    >
      {children}
    </dialog>
  );
}
