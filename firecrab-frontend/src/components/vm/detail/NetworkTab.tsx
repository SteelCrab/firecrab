import { ArrowRight, Globe, Plus, Shield, Trash2 } from "lucide-react";
import type { EgressPolicy, PortProtocol, VmResponse } from "../../../bindings";
import { useI18n } from "../../../i18n";
import { DetailProp, DetailSection } from "../../ui/DetailSection";
import { EditActions, EditButton, FieldError } from "./parts";
import type { SectionEdit } from "./useSectionEdit";
import type { VmDetailData } from "./useVmDetail";

interface NetworkTabProps {
  vm: VmResponse;
  detail: VmDetailData;
  edit: SectionEdit;
}

export default function NetworkTab({ vm, detail, edit }: NetworkTabProps) {
  const { t } = useI18n();
  const draft = edit.draft;
  const network = detail.networks.find((entry) => entry.id === vm.microNetworkId);
  const forwards = vm.portForwards ?? [];
  const egressLabel = (policy: EgressPolicy) =>
    policy === "internet" ? t("Internet access", "인터넷 허용") : t("Isolated (gateway only)", "격리(게이트웨이만 허용)");

  return (
    <>
      <DetailSection title={t("Addresses", "주소")}>
        <dl className="props">
          <DetailProp label="IPv4" copy={vm.ipv4 ?? undefined}><span className="mono">{vm.ipv4 ?? "—"}</span></DetailProp>
          <DetailProp label="IPv6" copy={vm.ipv6 ?? undefined}><span className="mono">{vm.ipv6 ?? "—"}</span></DetailProp>
          <DetailProp label="MAC"><span className="mono">{vm.mac ?? "—"}</span></DetailProp>
          <DetailProp label="MicroNetwork">
            {!vm.microNetworkId ? t("Default network", "기본 네트워크") : network ? `${network.name} (${network.subnetCidr})` : vm.microNetworkId}
          </DetailProp>
        </dl>
      </DetailSection>

      <DetailSection title={t("Egress", "외부 통신")} aside={<EditButton vm={vm} section="egress" edit={edit} label={t("Edit egress", "외부 통신 편집")} />}>
        {draft?.section === "egress" ? (
          <>
            <div className="segmented" role="group" aria-label={t("Egress", "외부 통신")}>
              {(["internet", "isolated"] as EgressPolicy[]).map((policy) => (
                <button
                  key={policy}
                  type="button"
                  aria-pressed={draft.egressPolicy === policy}
                  onClick={() => edit.update({ ...draft, egressPolicy: policy })}
                >
                  {policy === "internet" ? <Globe aria-hidden="true" /> : <Shield aria-hidden="true" />}
                  {egressLabel(policy)}
                </button>
              ))}
            </div>
            <EditActions edit={edit} />
          </>
        ) : (
          <p className="detail-value-line">{egressLabel(vm.egressPolicy)}</p>
        )}
        <p className="detail-note">{t("Isolated VMs reach only the gateway.", "격리하면 게이트웨이와만 통신합니다.")}</p>
      </DetailSection>

      <DetailSection
        title={t("Port forwarding", "포트 포워딩")}
        aside={<EditButton vm={vm} section="ports" edit={edit} label={t("Edit port forwarding", "포트 포워딩 편집")} />}
      >
        {draft?.section === "ports" ? (
          <>
            <div className="kv vm-port-editor">
              {draft.portForwards.map((forward, index) => {
                const set = (field: "hostPort" | "guestPort" | "protocol", value: number | PortProtocol) =>
                  edit.update({ ...draft, portForwards: draft.portForwards.map((entry, i) => (i === index ? { ...entry, [field]: value } : entry)) });
                return (
                  <div key={index} className="kv-row">
                    <input
                      className="input"
                      type="number"
                      placeholder="8080"
                      aria-label={t(`Host port ${index + 1}`, `호스트 포트 ${index + 1}`)}
                      value={forward.hostPort || ""}
                      onChange={(event) => set("hostPort", Number(event.target.value))}
                    />
                    <ArrowRight className="kv-arrow" aria-hidden="true" />
                    <input
                      className="input"
                      type="number"
                      placeholder="80"
                      aria-label={t(`Guest port ${index + 1}`, `게스트 포트 ${index + 1}`)}
                      value={forward.guestPort || ""}
                      onChange={(event) => set("guestPort", Number(event.target.value))}
                    />
                    <select
                      className="input"
                      aria-label={t(`Protocol ${index + 1}`, `프로토콜 ${index + 1}`)}
                      value={forward.protocol}
                      onChange={(event) => set("protocol", event.target.value as PortProtocol)}
                    >
                      <option value="tcp">tcp</option>
                      <option value="udp">udp</option>
                    </select>
                    <button
                      type="button"
                      className="icon-btn"
                      aria-label={t(`Remove rule ${index + 1}`, `규칙 ${index + 1} 삭제`)}
                      onClick={() => edit.update({ ...draft, portForwards: draft.portForwards.filter((_, i) => i !== index) })}
                    >
                      <Trash2 aria-hidden="true" />
                    </button>
                  </div>
                );
              })}
            </div>
            <button
              type="button"
              className="btn sm ghost"
              onClick={() => edit.update({ ...draft, portForwards: [...draft.portForwards, { hostPort: 8080, guestPort: 80, protocol: "tcp" }] })}
            >
              <Plus aria-hidden="true" />
              {t("Add rule", "규칙 추가")}
            </button>
            <FieldError edit={edit} field="portForwards" />
            <EditActions edit={edit} />
          </>
        ) : forwards.length === 0 ? (
          <p className="detail-note">{t("No forwarding rules.", "포워딩 규칙이 없습니다.")}</p>
        ) : (
          <div className="kv">
            <div className="kv-row kv-head">
              <span>{t("Host", "호스트")}</span>
              <span />
              <span>{t("Guest", "게스트")}</span>
              <span />
            </div>
            {forwards.map((forward) => (
              <div key={`${forward.protocol}-${forward.hostPort}`} className="kv-row">
                <span className="mono">{forward.hostPort}</span>
                <ArrowRight className="kv-arrow" aria-hidden="true" />
                <span className="mono">{forward.guestPort}</span>
                <span className="kv-proto">{forward.protocol}</span>
              </div>
            ))}
          </div>
        )}
        {vm.state === "running" && <p className="detail-note">{t("Saving applies host NAT immediately.", "저장하면 호스트 NAT가 바로 적용됩니다.")}</p>}
      </DetailSection>
    </>
  );
}
