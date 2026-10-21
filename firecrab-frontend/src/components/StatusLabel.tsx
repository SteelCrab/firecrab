import { useCallback, useEffect, useId, useLayoutEffect, useRef, useState, type ReactNode } from "react";
import { createPortal } from "react-dom";

const tooltipOpenEvent = "firecrab:status-tooltip-open";

interface StatusLabelProps {
  children: string;
  className: string;
  accessibleLabel: string;
  tooltip: ReactNode;
  state?: string;
}

export default function StatusLabel({ children, className, accessibleLabel, tooltip, state }: StatusLabelProps) {
  const id = useId();
  const trigger = useRef<HTMLElement>(null);
  const panel = useRef<HTMLDivElement>(null);
  const hovered = useRef(false);
  const focused = useRef(false);
  const closeTimer = useRef<number | null>(null);
  const [open, setOpen] = useState(false);

  const cancelClose = useCallback(() => {
    if (closeTimer.current !== null) window.clearTimeout(closeTimer.current);
    closeTimer.current = null;
  }, []);

  function show() {
    cancelClose();
    document.dispatchEvent(new CustomEvent(tooltipOpenEvent, { detail: id }));
    setOpen(true);
  }

  const dismiss = useCallback(() => {
    cancelClose();
    setOpen(false);
  }, [cancelClose]);

  function leave() {
    hovered.current = false;
    cancelClose();
    closeTimer.current = window.setTimeout(() => {
      if (!hovered.current && !focused.current) dismiss();
    }, 120);
  }

  useEffect(() => cancelClose, [cancelClose]);

  useLayoutEffect(() => {
    if (!open) return;
    function onTooltipOpen(event: Event) {
      if ((event as CustomEvent<string>).detail !== id) dismiss();
    }
    document.addEventListener(tooltipOpenEvent, onTooltipOpen);
    return () => document.removeEventListener(tooltipOpenEvent, onTooltipOpen);
  }, [open, id, dismiss]);

  useLayoutEffect(() => {
    if (!open || !trigger.current || !panel.current) return;
    function position() {
      if (!trigger.current || !panel.current) return;
      const anchor = trigger.current.getBoundingClientRect();
      const box = panel.current.getBoundingClientRect();
      const margin = 12;
      const gap = 8;
      const below = window.innerHeight - margin - anchor.bottom - gap;
      const above = anchor.top - gap - margin;
      const left = Math.max(margin, Math.min(anchor.left, window.innerWidth - box.width - margin));
      const top = below >= box.height || below >= above
        ? Math.max(margin, Math.min(anchor.bottom + gap, window.innerHeight - box.height - margin))
        : Math.max(margin, anchor.top - gap - box.height);
      panel.current.style.left = `${left}px`;
      panel.current.style.top = `${top}px`;
      panel.current.style.visibility = "visible";
    }
    position();
    window.addEventListener("resize", position);
    window.addEventListener("scroll", position, true);
    return () => {
      window.removeEventListener("resize", position);
      window.removeEventListener("scroll", position, true);
    };
  }, [open, tooltip]);

  useEffect(() => {
    if (!open) return;
    function onKeyDown(event: KeyboardEvent) {
      if (event.key === "Escape") dismiss();
    }
    function onOutsideInteraction(event: Event) {
      if (!(event.target instanceof Node)) return;
      if (trigger.current?.contains(event.target) || panel.current?.contains(event.target)) return;
      dismiss();
    }
    document.addEventListener("keydown", onKeyDown);
    document.addEventListener("pointerdown", onOutsideInteraction, true);
    document.addEventListener("focusin", onOutsideInteraction);
    return () => {
      cancelClose();
      document.removeEventListener("keydown", onKeyDown);
      document.removeEventListener("pointerdown", onOutsideInteraction, true);
      document.removeEventListener("focusin", onOutsideInteraction);
    };
  }, [open, dismiss, cancelClose]);

  return (
    <>
      <strong
        ref={trigger}
        className={`state-label ${className}`}
        role="status"
        aria-label={accessibleLabel}
        aria-describedby={open ? id : undefined}
        data-state={state}
        tabIndex={0}
        onMouseEnter={() => { hovered.current = true; show(); }}
        onMouseLeave={leave}
        onFocus={() => { focused.current = true; show(); }}
        onBlur={() => { focused.current = false; leave(); }}
      >
        {children}
      </strong>
      {open && createPortal(
        <div
          ref={panel}
          id={id}
          role="tooltip"
          className="status-tooltip"
          style={{ visibility: "hidden" }}
          onMouseEnter={() => { hovered.current = true; cancelClose(); }}
          onMouseLeave={leave}
        >
          {tooltip}
        </div>,
        document.body,
      )}
    </>
  );
}
