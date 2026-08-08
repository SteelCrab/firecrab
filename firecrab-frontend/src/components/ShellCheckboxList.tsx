import type { ShellResponse } from "../bindings";
import { useI18n } from "../i18n";

/** Matches firecrab-api `MAX_SHELLS_PER_VM`. */
export const MAX_SHELLS_PER_VM = 8;

export interface ShellCheckboxListProps {
  shells: ShellResponse[];
  selectedIds: string[];
  onChange: (nextIds: string[]) => void;
  /** Disable all inputs (e.g. while saving). */
  disabled?: boolean;
  /** Optional id prefix for labels (a11y when multiple lists exist). */
  idPrefix?: string;
  emptyLabel?: string;
}

/**
 * Checkbox picker for Shell repository pins.
 * Order of `selectedIds` is the inject order (catalog order among checked).
 */
export default function ShellCheckboxList({
  shells,
  selectedIds,
  onChange,
  disabled = false,
  idPrefix = "shell",
  emptyLabel,
}: ShellCheckboxListProps) {
  const { t } = useI18n();
  const atCap = selectedIds.length >= MAX_SHELLS_PER_VM;

  if (shells.length === 0) {
    return (
      <div className="shell-check-empty poll-note">
        {emptyLabel ?? t("No shells in catalog", "등록된 Shell 없음")}
      </div>
    );
  }

  const toggle = (shellId: string, checked: boolean) => {
    if (checked) {
      if (selectedIds.includes(shellId) || selectedIds.length >= MAX_SHELLS_PER_VM) return;
      onChange([...selectedIds, shellId]);
      return;
    }
    onChange(selectedIds.filter((id) => id !== shellId));
  };

  return (
    <fieldset
      className="shell-check-list"
      disabled={disabled}
      aria-label={t("Shells", "Shell")}
    >
      <ul className="shell-check-ul">
        {shells.map((shell) => {
          const checked = selectedIds.includes(shell.id);
          const inputId = `${idPrefix}-${shell.id}`;
          const blockNew = atCap && !checked;
          return (
            <li key={shell.id}>
              <label
                htmlFor={inputId}
                className={
                  blockNew ? "shell-check-label is-capped" : "shell-check-label"
                }
              >
                <input
                  id={inputId}
                  type="checkbox"
                  checked={checked}
                  disabled={disabled || blockNew}
                  onChange={(event) => toggle(shell.id, event.target.checked)}
                />
                <span className="shell-check-name" title={shell.description ?? shell.name}>
                  {shell.name}
                </span>
                <span className="shell-check-meta mono">v{shell.latestVersion}</span>
              </label>
            </li>
          );
        })}
      </ul>
      {atCap ? (
        <p className="poll-note shell-check-hint">
          {t(`Max ${MAX_SHELLS_PER_VM} shells`, `최대 ${MAX_SHELLS_PER_VM}개`)}
        </p>
      ) : null}
    </fieldset>
  );
}

