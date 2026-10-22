import { Plus, Trash2 } from "lucide-react";
import type { VmResponse } from "../../../bindings";
import { useI18n } from "../../../i18n";
import { DetailSection } from "../../ui/DetailSection";
import { EditActions, EditButton, FieldError } from "./parts";
import type { SectionEdit } from "./useSectionEdit";
import type { VmDetailData } from "./useVmDetail";

const MAX_ROWS = 64;

/** Keys whose values are shown masked; they are still stored in plaintext. */
const SECRET_KEY = /TOKEN|SECRET|PASSWORD/i;

interface EnvTabProps {
  vm: VmResponse;
  detail: VmDetailData;
  edit: SectionEdit;
}

export default function EnvTab({ vm, detail, edit }: EnvTabProps) {
  const { t } = useI18n();
  const draft = edit.draft;
  const entries = Object.entries(vm.env ?? {});
  const image = detail.images.find((entry) => entry.alias === vm.template);
  const ignored = Boolean(image && !image.hasGuestService);

  return (
    <DetailSection
      title={t("Environment", "환경 변수")}
      aside={<EditButton vm={vm} section="env" edit={edit} label={t("Edit environment", "환경 변수 편집")} />}
    >
      {draft?.section === "env" ? (
        <>
          <div className="kv vm-env-editor">
            {draft.rows.map((row, index) => {
              const set = (field: "key" | "value", value: string) =>
                edit.update({ ...draft, rows: draft.rows.map((entry, i) => (i === index ? { ...entry, [field]: value } : entry)) });
              return (
                <div key={index} className="kv-row">
                  <input
                    id={`vm-edit-env-key-${index}`}
                    className="input mono"
                    placeholder="APP_NAME"
                    aria-label={t(`Key ${index + 1}`, `키 ${index + 1}`)}
                    value={row.key}
                    maxLength={256}
                    autoComplete="off"
                    spellCheck={false}
                    onChange={(event) => set("key", event.target.value)}
                  />
                  <input
                    id={`vm-edit-env-value-${index}`}
                    className="input mono"
                    placeholder="web"
                    aria-label={t(`Value ${index + 1}`, `값 ${index + 1}`)}
                    value={row.value}
                    maxLength={4096}
                    autoComplete="off"
                    spellCheck={false}
                    onChange={(event) => set("value", event.target.value)}
                  />
                  <button
                    type="button"
                    className="icon-btn"
                    aria-label={t(`Remove variable ${index + 1}`, `변수 ${index + 1} 삭제`)}
                    onClick={() => edit.update({ ...draft, rows: draft.rows.filter((_, i) => i !== index) })}
                  >
                    <Trash2 aria-hidden="true" />
                  </button>
                </div>
              );
            })}
          </div>
          <button
            type="button"
            id="vm-edit-env-add"
            className="btn sm ghost"
            disabled={draft.rows.length >= MAX_ROWS}
            onClick={() => edit.update({ ...draft, rows: [...draft.rows, { key: "", value: "" }] })}
          >
            <Plus aria-hidden="true" />
            {t("Add variable", "변수 추가")}
          </button>
          <FieldError edit={edit} field="env" />
          <EditActions edit={edit} />
        </>
      ) : entries.length === 0 ? (
        <p className="detail-note">{t("No environment variables.", "환경 변수가 없습니다.")}</p>
      ) : (
        <div className="kv">
          {entries.map(([key, value]) => (
            <div key={key} className="kv-row vm-env-row">
              <span className="kv-key mono">{key}</span>
              <span className={SECRET_KEY.test(key) ? "kv-value mono muted" : "kv-value mono"}>{SECRET_KEY.test(key) ? "••••••••" : value}</span>
            </div>
          ))}
        </div>
      )}
      <p className="detail-note">{t("Values are stored in plaintext.", "값은 평문으로 저장됩니다.")}</p>
      {ignored && (
        <p className="detail-note">{t("Runtime env is ignored: this image has no guest service.", "게스트 서비스가 없는 이미지라 런타임 환경 변수는 적용되지 않습니다.")}</p>
      )}
      {vm.state === "running" && (
        <p className="detail-note">{t("Saving restarts the guest service so the new env takes effect.", "저장하면 게스트 서비스를 재시작해 새 환경 변수를 적용합니다.")}</p>
      )}
    </DetailSection>
  );
}
