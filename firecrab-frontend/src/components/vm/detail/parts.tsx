import type { ReactNode } from "react";
import { Lock, Pencil } from "lucide-react";
import type { VmResponse } from "../../../bindings";
import { ApiClientError } from "../../../api/client";
import { useI18n } from "../../../i18n";
import { isSectionEditable } from "./sectionSave";
import type { VmSection } from "./sectionSave";
import type { SectionEdit } from "./useSectionEdit";

/** Field error for `field` when the save failed validation there. */
export function FieldError({ edit, field }: { edit: SectionEdit; field: string }) {
  const message = edit.error instanceof ApiClientError ? edit.error.fieldError(field) : undefined;
  return message ? <span className="field-error">{message}</span> : null;
}

/** Save / Cancel under the section being edited, with the save error. */
export function EditActions({ edit }: { edit: SectionEdit }) {
  const { t } = useI18n();
  return (
    <div className="detail-edit-actions" data-editing>
      <button type="button" className="btn sm primary" onClick={edit.save} disabled={edit.saving}>
        {edit.saving ? t("Saving…", "저장 중…") : t("Save", "저장")}
      </button>
      <button type="button" className="btn sm" onClick={edit.cancel} disabled={edit.saving}>
        {t("Cancel", "취소")}
      </button>
      {edit.error && <span className="field-error">{edit.error.message}</span>}
    </div>
  );
}

interface EditButtonProps {
  vm: VmResponse;
  section: VmSection;
  edit: SectionEdit;
  /** Accessible name, e.g. "Edit spec". */
  label: string;
}

/** "Edit" for a whole section; hidden while editing it or while the VM state forbids it. */
export function EditButton({ vm, section, edit, label }: EditButtonProps) {
  const { t } = useI18n();
  if (edit.draft?.section === section || !isSectionEditable(vm.state, section)) return null;
  return (
    <button type="button" className="btn sm ghost" aria-label={label} onClick={() => edit.start(section)}>
      <Pencil aria-hidden="true" />
      {t("Edit", "편집")}
    </button>
  );
}

interface EditablePropProps {
  vm: VmResponse;
  edit: SectionEdit;
  section: VmSection;
  label: string;
  editLabel: string;
  /** Shown instead of the edit button while the state forbids editing. */
  lockedNote?: string;
  editor: ReactNode;
  children: ReactNode;
}

/** A property row that turns into inputs in place (spec, storage). */
export function EditableProp({ vm, edit, section, label, editLabel, lockedNote, editor, children }: EditablePropProps) {
  const { t } = useI18n();
  const editing = edit.draft?.section === section;
  return (
    <>
      <dt>{label}</dt>
      <dd className={editing ? "editing" : undefined}>
        {editing ? (
          <div className="prop-editor">
            {editor}
            <EditActions edit={edit} />
          </div>
        ) : (
          <>
            <span className="prop-value">{children}</span>
            {isSectionEditable(vm.state, section) ? (
              <button type="button" className="icon-btn prop-action" aria-label={editLabel} title={editLabel} onClick={() => edit.start(section)}>
                <Pencil aria-hidden="true" />
              </button>
            ) : lockedNote ? (
              <span className="prop-lock" title={t("Stop the VM to change this.", "VM을 중지하면 바꿀 수 있습니다.")}>
                <Lock aria-hidden="true" />
                {lockedNote}
              </span>
            ) : null}
          </>
        )}
      </dd>
    </>
  );
}
