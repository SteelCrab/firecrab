import type { VmResponse } from "../../../bindings";
import { useI18n } from "../../../i18n";
import MicroNetworkSelect from "../../MicroNetworkSelect";
import { EditableProp, FieldError } from "./parts";
import type { SectionEdit } from "./useSectionEdit";
import type { VmDetailData } from "./useVmDetail";

export default function MicroNetworkProp({
  vm,
  detail,
  edit,
}: {
  vm: VmResponse;
  detail: VmDetailData;
  edit: SectionEdit;
}) {
  const { t } = useI18n();
  const draft = edit.draft?.section === "network" ? edit.draft : null;
  const network = detail.networks.find(
    (network) => network.id === vm.microNetworkId,
  );
  return (
    <EditableProp
      vm={vm}
      edit={edit}
      section="network"
      label="MicroNetwork"
      editLabel={t("Edit MicroNetwork", "MicroNetwork 편집")}
      lockedNote={t("Stop to change", "중지 후 변경")}
      editor={
        draft && (
          <>
            <div className="field">
              <label htmlFor="vm-edit-network">MicroNetwork</label>
              <MicroNetworkSelect
                id="vm-edit-network"
                value={draft.microNetworkId}
                networks={detail.networks}
                disabled={edit.saving}
                onChange={(microNetworkId) =>
                  edit.update({ ...draft, microNetworkId })
                }
              />
              <FieldError edit={edit} field="microNetworkId" />
            </div>
            <div className="field">
              <label htmlFor="vm-edit-network-security-group">
                {t("Security group", "보안그룹")}
              </label>
              <select
                id="vm-edit-network-security-group"
                disabled={edit.saving || detail.securityGroups === null}
                value={draft.securityGroupId ?? ""}
                onChange={(event) =>
                  edit.update({
                    ...draft,
                    securityGroupId: event.target.value || null,
                  })
                }
              >
                <option value="">{t("None", "없음")}</option>
                {detail.securityGroups
                  ?.filter(
                    (group) =>
                      group.microNetworkId === draft.microNetworkId ||
                      group.id === draft.securityGroupId,
                  )
                  .map((group) => (
                    <option
                      value={group.id}
                      key={group.id}
                      disabled={group.microNetworkId !== draft.microNetworkId}
                    >
                      {group.name}
                      {group.microNetworkId !== draft.microNetworkId
                        ? ` · ${t("Different network", "다른 네트워크")}`
                        : ""}
                    </option>
                  ))}
                {draft.securityGroupId &&
                  !detail.securityGroups?.some(
                    (group) => group.id === draft.securityGroupId,
                  ) && (
                    <option value={draft.securityGroupId} disabled>
                      {draft.securityGroupId}
                    </option>
                  )}
              </select>
              <FieldError edit={edit} field="securityGroupId" />
            </div>
            <p className="detail-note">
              {t(
                "The next start uses a new address in the selected network. Select a compatible security group or choose None; the current group is not removed automatically.",
                "다음 시작부터 선택한 네트워크의 새 주소를 사용한다. 맞는 보안그룹이나 없음을 선택한다. 기존 보안그룹을 자동 해제하지 않는다.",
              )}
            </p>
            {detail.securityGroups === null && (
              <p className="field-error">
                {t(
                  "Security groups could not be loaded.",
                  "보안그룹을 불러오지 못했다.",
                )}{" "}
                <button
                  className="btn sm"
                  type="button"
                  disabled={edit.saving}
                  onClick={detail.reloadSecurityGroups}
                >
                  {t("Retry", "다시 시도")}
                </button>
              </p>
            )}
          </>
        )
      }
    >
      {network
        ? `${network.name} (${network.subnetCidr})`
        : vm.microNetworkId || t("Default network", "기본 네트워크")}
    </EditableProp>
  );
}
