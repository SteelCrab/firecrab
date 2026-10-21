import { useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";

interface Item {
  label: string;
  disabled?: boolean;
  danger?: boolean;
  onSelect: () => void;
}

interface Props {
  label: string;
  disabled: boolean;
  items: Item[];
}

export default function PipeServiceMenu({ label, disabled, items }: Props) {
  const trigger = useRef<HTMLButtonElement>(null);
  const menu = useRef<HTMLUListElement>(null);
  const [anchor, setAnchor] = useState<{ top: number; right: number } | null>(null);
  const open = anchor !== null;

  useEffect(() => {
    if (!open) return;
    const close = () => setAnchor(null);
    const place = () => {
      const rect = trigger.current?.getBoundingClientRect();
      if (!rect || rect.bottom < 0 || rect.top > window.innerHeight) { close(); return; }
      const height = menu.current?.offsetHeight ?? 0;
      const top = rect.bottom + height + 6 <= window.innerHeight - 8 ? rect.bottom + 6 : Math.max(8, rect.top - height - 6);
      const right = Math.max(8, window.innerWidth - rect.right);
      setAnchor(current => current && (current.top !== top || current.right !== right) ? { top, right } : current);
    };
    const outside = (event: Event) => {
      const target = event.target as Node;
      if (!trigger.current?.contains(target) && !menu.current?.contains(target)) close();
    };
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.preventDefault();
        close();
        trigger.current?.focus({ preventScroll: true });
        return;
      }
      if (!["ArrowDown", "ArrowUp", "Home", "End"].includes(event.key)) return;
      event.preventDefault();
      const buttons = Array.from(menu.current?.querySelectorAll<HTMLButtonElement>("button:not(:disabled)") ?? []);
      if (!buttons.length) return;
      const current = buttons.findIndex(button => button === document.activeElement);
      const next = event.key === "Home" ? 0 : event.key === "End" ? buttons.length - 1 : (current + (event.key === "ArrowUp" ? -1 : 1) + buttons.length) % buttons.length;
      buttons[next].focus({ preventScroll: true });
    };
    place();
    menu.current?.querySelector<HTMLButtonElement>("button:not(:disabled)")?.focus({ preventScroll: true });
    document.addEventListener("pointerdown", outside);
    document.addEventListener("focusin", outside);
    document.addEventListener("keydown", onKey);
    window.addEventListener("resize", place);
    window.addEventListener("scroll", place, true);
    return () => {
      document.removeEventListener("pointerdown", outside);
      document.removeEventListener("focusin", outside);
      document.removeEventListener("keydown", onKey);
      window.removeEventListener("resize", place);
      window.removeEventListener("scroll", place, true);
    };
  }, [open]);

  return <div className="options-menu vm-actions pipe-service-actions">
    <button ref={trigger} className="options-menu-trigger" type="button" aria-label={label} title={label} aria-haspopup="menu" aria-expanded={anchor !== null} disabled={disabled} onClick={event => {
      const rect = event.currentTarget.getBoundingClientRect();
      setAnchor(anchor ? null : { top: rect.bottom + 6, right: Math.max(8, window.innerWidth - rect.right) });
    }}>⋯</button>
    {anchor && createPortal(<ul ref={menu} className="options-menu-list vm-actions-menu pipe-service-menu" role="menu" aria-label={label} style={{ top: anchor.top, right: anchor.right }}>
      {items.map(item => <li key={item.label} role="none"><button className={`options-menu-item${item.danger ? " danger" : ""}`} type="button" role="menuitem" disabled={disabled || item.disabled} onClick={() => { setAnchor(null); item.onSelect(); }}>{item.label}</button></li>)}
    </ul>, document.body)}
  </div>;
}
