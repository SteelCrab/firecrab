import { useState } from "react";
import type { VmResponse } from "../../../bindings";
import { draftFor, saveVmSection } from "./sectionSave";
import type { VmSection, VmSectionDraft } from "./sectionSave";

export interface SectionEdit {
  /** The section being edited and its inputs; `null` when nothing is. */
  draft: VmSectionDraft | null;
  saving: boolean;
  error: Error | null;
  start: (section: VmSection) => void;
  update: (draft: VmSectionDraft) => void;
  save: () => void;
  cancel: () => void;
}

/**
 * One section of the VM detail at a time is in edit mode (spec §8.3). The
 * draft is plain state, so list and detail polls that replace the VM object
 * leave the inputs alone.
 */
export function useSectionEdit(vm: VmResponse, onSaved: (vm: VmResponse) => void, onStartShells: () => void): SectionEdit {
  const [draft, setDraft] = useState<VmSectionDraft | null>(null);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<Error | null>(null);

  const start = (section: VmSection) => {
    setDraft(draftFor(vm, section));
    setError(null);
    // The newest revisions should be on the checkboxes.
    if (section === "shells") onStartShells();
  };

  const cancel = () => {
    setDraft(null);
    setError(null);
  };

  const save = async () => {
    if (!draft) return;
    setSaving(true);
    setError(null);
    try {
      onSaved(await saveVmSection(vm, draft));
      setDraft(null);
    } catch (caught) {
      setError(caught instanceof Error ? caught : new Error(String(caught)));
    } finally {
      setSaving(false);
    }
  };

  return { draft, saving, error, start, update: setDraft, save, cancel };
}
