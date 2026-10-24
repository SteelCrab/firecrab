import { useEffect, useId, useMemo, useRef, useState } from "react";
import type { KeyboardEvent as ReactKeyboardEvent } from "react";
import { Cpu, PanelRight, Plus, Search } from "lucide-react";
import type { LucideIcon } from "lucide-react";
import type { VmResponse } from "../bindings";
import { useI18n } from "../i18n";
import { isPaletteShortcut } from "../lib/shortcuts";
import { VM_STATE_LABELS } from "../model";
import { VIEWS, newVmHash, viewGroupLabel, viewLabel } from "../navigation";
import type { ViewId } from "../navigation";
import { VIEW_ICONS } from "../viewIcons";

type GroupId = "actions" | "views" | "vms";

interface PaletteItem {
  key: string;
  group: GroupId;
  icon: LucideIcon;
  label: string;
  hint: string;
  run: () => void;
}

interface CommandPaletteProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  vms: VmResponse[];
  onSelectView: (view: ViewId) => void;
  /** Selects a VM on the MicroVM screen and opens its detail. */
  onSelectVm: (id: string) => void;
  onToggleDetail: () => void;
}

/**
 * ⌘K palette: jump to a screen, open a MicroVM, or start an action. It opens
 * and closes without animation because it is driven from the keyboard.
 */
export default function CommandPalette({ open, onOpenChange, vms, onSelectView, onSelectVm, onToggleDetail }: CommandPaletteProps) {
  const { locale, t } = useI18n();

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (!isPaletteShortcut(event)) return;
      // Ctrl+K kills the line in a shell, so keys typed into a terminal stay there.
      if (event.target instanceof Element && event.target.closest("[data-terminal-engine]")) return;
      event.preventDefault();
      onOpenChange(!open);
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [open, onOpenChange]);

  // Built in group order, so filtering keeps every group contiguous.
  const items = useMemo<PaletteItem[]>(() => [
    {
      key: "new-vm",
      group: "actions",
      icon: Plus,
      label: t("New MicroVM", "새 MicroVM"),
      hint: "",
      run: () => { window.location.hash = newVmHash(); },
    },
    {
      key: "toggle-detail",
      group: "actions",
      icon: PanelRight,
      label: t("Toggle detail panel", "상세 패널 열기/닫기"),
      hint: "",
      run: onToggleDetail,
    },
    ...VIEWS.map((view): PaletteItem => ({
      key: `view-${view.id}`,
      group: "views",
      icon: VIEW_ICONS[view.id],
      label: viewLabel(view, locale),
      hint: viewGroupLabel(view.group, locale),
      run: () => onSelectView(view.id),
    })),
    ...vms.map((vm): PaletteItem => ({
      key: `vm-${vm.id}`,
      group: "vms",
      icon: Cpu,
      label: vm.name,
      hint: t(...VM_STATE_LABELS[vm.state]),
      run: () => {
        onSelectView("vms");
        onSelectVm(vm.id);
      },
    })),
  ], [locale, t, vms, onSelectView, onSelectVm, onToggleDetail]);

  if (!open) return null;
  // Mounted only while open, so every opening starts from an empty query.
  return <PaletteDialog items={items} onClose={() => onOpenChange(false)} />;
}

const GROUP_ORDER: GroupId[] = ["actions", "views", "vms"];

function PaletteDialog({ items, onClose }: { items: PaletteItem[]; onClose: () => void }) {
  const { t } = useI18n();
  const [query, setQuery] = useState("");
  const [active, setActive] = useState(0);
  const inputRef = useRef<HTMLInputElement>(null);
  const returnFocus = useRef<Element | null>(null);
  const listId = useId();

  useEffect(() => {
    returnFocus.current = document.activeElement;
    inputRef.current?.focus();
  }, []);

  const needle = query.trim().toLowerCase();
  const visible = needle
    ? items.filter((item) => item.label.toLowerCase().includes(needle) || item.hint.toLowerCase().includes(needle))
    : items;
  const current = visible.length ? Math.min(active, visible.length - 1) : -1;
  const optionId = (index: number) => `${listId}-option-${index}`;
  const groupLabels: Record<GroupId, string> = {
    actions: t("Actions", "작업"),
    views: t("Go to", "이동"),
    vms: "MicroVM",
  };

  useEffect(() => {
    if (current >= 0) document.getElementById(`${listId}-option-${current}`)?.scrollIntoView({ block: "nearest" });
  }, [current, listId]);

  // Closing without choosing puts focus back where it was; choosing leaves it
  // to whatever the item opens.
  const dismiss = () => {
    onClose();
    if (returnFocus.current instanceof HTMLElement) returnFocus.current.focus();
  };
  const run = (item: PaletteItem) => {
    onClose();
    item.run();
  };

  const onKeyDown = (event: ReactKeyboardEvent<HTMLInputElement>) => {
    // Enter while composing Hangul only commits the syllable.
    if (event.nativeEvent.isComposing) return;
    switch (event.key) {
      case "ArrowDown":
      case "ArrowUp": {
        event.preventDefault();
        if (!visible.length) return;
        const step = event.key === "ArrowDown" ? 1 : -1;
        setActive((current + step + visible.length) % visible.length);
        return;
      }
      case "Enter": {
        event.preventDefault();
        const item = visible[current];
        if (item) run(item);
        return;
      }
      case "Escape":
        // Do not also close a dialog underneath the palette.
        event.preventDefault();
        event.stopPropagation();
        dismiss();
        return;
      case "Tab":
        // The search box is the palette's only stop.
        event.preventDefault();
    }
  };

  const groups = GROUP_ORDER.map((group) => ({
    group,
    entries: visible.flatMap((item, index) => (item.group === group ? [{ item, index }] : [])),
  })).filter((entry) => entry.entries.length > 0);

  return (
    <div
      className="command-palette-backdrop"
      onMouseDown={(event) => {
        if (event.target === event.currentTarget) dismiss();
      }}
    >
      <div className="command-palette" role="dialog" aria-modal="true" aria-label={t("Command palette", "명령 팔레트")}>
        <div className="command-palette-search">
          <Search className="command-palette-icon" aria-hidden="true" />
          <input
            ref={inputRef}
            role="combobox"
            aria-expanded="true"
            aria-controls={listId}
            aria-autocomplete="list"
            aria-activedescendant={current >= 0 ? optionId(current) : undefined}
            aria-label={t("Search screens, MicroVMs, and actions", "화면, MicroVM, 작업 검색")}
            placeholder={t("Search screens, MicroVMs, and actions…", "화면, MicroVM, 작업 검색…")}
            value={query}
            onChange={(event) => {
              setQuery(event.target.value);
              setActive(0);
            }}
            onKeyDown={onKeyDown}
            autoComplete="off"
            spellCheck={false}
          />
          <kbd>esc</kbd>
        </div>
        <div className="command-palette-list" id={listId} role="listbox" aria-label={t("Results", "결과")}>
          {groups.map(({ group, entries }) => (
            <div key={group} role="group" aria-labelledby={`${listId}-${group}`}>
              <div className="command-palette-group" id={`${listId}-${group}`} role="presentation">
                {groupLabels[group]}
              </div>
              {entries.map(({ item, index }) => {
                const Icon = item.icon;
                return (
                  <div
                    key={item.key}
                    id={optionId(index)}
                    role="option"
                    aria-selected={index === current}
                    className={index === current ? "command-palette-item active" : "command-palette-item"}
                    onMouseMove={() => {
                      if (index !== current) setActive(index);
                    }}
                    // Keep focus in the search box while clicking.
                    onMouseDown={(event) => event.preventDefault()}
                    onClick={() => run(item)}
                  >
                    <Icon className="command-palette-icon" aria-hidden="true" />
                    <span className="command-palette-label">{item.label}</span>
                    {item.hint && <span className="command-palette-hint">{item.hint}</span>}
                  </div>
                );
              })}
            </div>
          ))}
        </div>
        {visible.length === 0 && <div className="command-palette-empty">{t("No results", "결과 없음")}</div>}
        <div className="command-palette-foot" aria-hidden="true">
          <span><kbd>↑</kbd><kbd>↓</kbd>{t("Move", "이동")}</span>
          <span><kbd>↵</kbd>{t("Open", "열기")}</span>
          <span><kbd>esc</kbd>{t("Close", "닫기")}</span>
        </div>
      </div>
    </div>
  );
}
