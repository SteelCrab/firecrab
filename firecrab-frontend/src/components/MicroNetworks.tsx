import { useEffect, useState } from "react";
import type { FormEvent } from "react";
import type { MicroNetworkDetailResponse, MicroNetworkResponse } from "../bindings";
import {
  ApiClientError,
  createMicroNetwork,
  deleteMicroNetwork,
  getMicroNetwork,
  listMicroNetworks,
  updateMicroNetwork,
} from "../api/client";
import { useI18n } from "../i18n";

/**
 * MicroNetwork management (`docs/30-tasks/task-micro-network.md`) — firecrab's own
 * virtual networks. Creating one reserves the CIDR, provisions its host
 * bridge, and gives it its own DHCP range and NAT rule; VMs then pick one on
 * the create form. Deleting is refused while VMs are still in it.
 */
export default function MicroNetworks() {
  const { t } = useI18n();
  const [networks, setNetworks] = useState<MicroNetworkResponse[] | null>(null);
  const [name, setName] = useState("");
  const [subnetCidr, setSubnetCidr] = useState("");
  const [internetEnabled, setInternetEnabled] = useState(true);
  const [submitting, setSubmitting] = useState(false);
  const [fieldErrors, setFieldErrors] = useState<ApiClientError | null>(null);
  const [listError, setListError] = useState<string | null>(null);
  const [busyId, setBusyId] = useState<string | null>(null);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [detail, setDetail] = useState<MicroNetworkDetailResponse | null>(null);
  const [detailError, setDetailError] = useState<string | null>(null);

  // Refetched on every selection rather than derived from the list row: the
  // detail carries live counts (leases in use, TAPs attached) the list
  // doesn't have.
  useEffect(() => {
    if (!selectedId) {
      setDetail(null);
      return;
    }
    setDetail(null);
    setDetailError(null);
    getMicroNetwork(selectedId)
      .then(setDetail)
      .catch((error) => setDetailError((error as Error).message));
  }, [selectedId]);

  const refresh = async () => {
    try {
      setNetworks(await listMicroNetworks());
      setListError(null);
    } catch (error) {
      setListError((error as Error).message);
    }
  };

  useEffect(() => {
    refresh();
  }, []);

  const handleSubmit = async (event: FormEvent) => {
    event.preventDefault();
    if (submitting) return;

    setSubmitting(true);
    setFieldErrors(null);
    try {
      await createMicroNetwork({
        name: name.trim(),
        subnetCidr: subnetCidr.trim(),
        internetEnabled,
      });
      setName("");
      setSubnetCidr("");
      setInternetEnabled(true);
      await refresh();
    } catch (error) {
      setFieldErrors(error as ApiClientError);
    } finally {
      setSubmitting(false);
    }
  };

  const handleDelete = async (network: MicroNetworkResponse) => {
    if (busyId || !window.confirm(t(`Delete MicroNetwork "${network.name}"?`, `MicroNetwork "${network.name}"을(를) 삭제할까요?`))) return;
    setBusyId(network.id);
    try {
      await deleteMicroNetwork(network.id);
      if (selectedId === network.id) setSelectedId(null);
      await refresh();
    } catch (error) {
      setListError((error as Error).message);
    } finally {
      setBusyId(null);
    }
  };

  // Reloads both the row (the list's badge) and the panel (its NAT line), so
  // the two can't disagree about a network that was just toggled.
  const handleToggleInternet = async (network: MicroNetworkResponse) => {
    if (busyId) return;
    setBusyId(network.id);
    try {
      await updateMicroNetwork(network.id, { internetEnabled: !network.internetEnabled });
      await refresh();
      if (selectedId === network.id) setDetail(await getMicroNetwork(network.id));
    } catch (error) {
      setListError((error as Error).message);
    } finally {
      setBusyId(null);
    }
  };

  const fieldError = (field: string) => (
    <span className="field-error">{fieldErrors?.fieldError(field) ?? ""}</span>
  );

  return (
    <section className="panel">
      <h2 className="panel-title">MicroNetwork</h2>
      <form className="create-grid" onSubmit={handleSubmit}>
        <div className="field">
          <label htmlFor="mn-name">name</label>
          <input
            id="mn-name"
            placeholder="prod"
            value={name}
            onChange={(event) => setName(event.target.value)}
            required
            minLength={1}
            maxLength={64}
          />
          {fieldError("name")}
        </div>
        <div className="field">
          <label htmlFor="mn-subnet">subnet CIDR</label>
          <input
            id="mn-subnet"
            placeholder="172.31.0.0/24"
            value={subnetCidr}
            onChange={(event) => setSubnetCidr(event.target.value)}
            required
          />
          {fieldError("subnetCidr")}
        </div>
        <div className="field">
          <label htmlFor="mn-internet">{t("Internet", "인터넷")}</label>
          <select
            id="mn-internet"
            value={internetEnabled ? "on" : "off"}
            onChange={(event) => setInternetEnabled(event.target.value === "on")}
          >
            <option value="on">{t("Enabled (NAT)", "연결 (NAT)")}</option>
            <option value="off">{t("Blocked (internal only)", "차단 (내부 전용)")}</option>
          </select>
          <span className="field-error"></span>
        </div>
        <div className="field">
          <label>&nbsp;</label>
          <button className="btn primary" type="submit" disabled={submitting}>
            {submitting ? t("Creating…", "생성 중…") : t("Create", "생성")}
          </button>
          <span className="field-error"></span>
        </div>
      </form>

      {listError && <div className="field-error">{listError}</div>}

      {networks === null ? (
        <div className="empty">{t("Loading…", "불러오는 중…")}</div>
      ) : networks.length === 0 ? (
        <div className="empty">{t("No MicroNetworks yet — create one above.", "MicroNetwork가 없습니다 — 위에서 생성하세요")}</div>
      ) : (
        <div className="table-scroll">
          <table className="vm-table">
            <thead>
              <tr>
                <th>name</th>
                <th>subnet CIDR</th>
                <th>gateway</th>
                <th>{t("Internet", "인터넷")}</th>
                <th>id</th>
                <th className="actions">{t("Actions", "작업")}</th>
              </tr>
            </thead>
            <tbody>
              {networks.map((network) => (
                <tr
                  key={network.id}
                  className={selectedId === network.id ? "selected" : undefined}
                  onClick={() => setSelectedId(selectedId === network.id ? null : network.id)}
                >
                  <td className="name">{network.name}</td>
                  <td className="mono">{network.subnetCidr}</td>
                  <td className="mono">{network.gateway}</td>
                  <td>{network.internetEnabled ? t("Enabled", "연결") : t("Blocked", "차단")}</td>
                  <td className="mono" title={network.id}>
                    {network.id.split("-")[0]}
                  </td>
                  <td className="actions">
                    <button
                      className="btn"
                      disabled={busyId === network.id}
                      title={
                        network.internetEnabled
                          ? t("Remove NAT and block outbound traffic", "NAT을 떼고 외부로 나가는 트래픽을 차단합니다")
                          : t("Attach NAT and allow outbound traffic", "NAT을 붙여 외부 통신을 허용합니다")
                      }
                      onClick={(event) => {
                        event.stopPropagation();
                        handleToggleInternet(network);
                      }}
                    >
                      {network.internetEnabled ? t("Block internet", "인터넷 차단") : t("Enable internet", "인터넷 연결")}
                    </button>
                    <button
                      className="btn danger"
                      disabled={busyId === network.id}
                      onClick={(event) => {
                        event.stopPropagation();
                        handleDelete(network);
                      }}
                    >
                      {t("Delete", "삭제")}
                    </button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}

      {selectedId && <MicroNetworkDetail detail={detail} error={detailError} />}
    </section>
  );
}

/** Renders one network's services. Kept in this file because it is only ever
 *  shown from the row it belongs to. */
function MicroNetworkDetail({
  detail,
  error,
}: {
  detail: MicroNetworkDetailResponse | null;
  error: string | null;
}) {
  const { t } = useI18n();
  if (error) return <div className="field-error">{error}</div>;
  if (!detail) return <div className="empty">{t("Loading details…", "상세 불러오는 중…")}</div>;

  const { subnet, bridge, nat, firewall } = detail;
  return (
    <div className="subpanel">
      <dl className="detail-fields mono">
        <dt>{t("Network ID", "네트워크 ID")}</dt>
        <dd>{detail.id}</dd>

        <dt>{t("Subnet", "서브넷")}</dt>
        <dd>
          {subnet.cidr} · gateway {subnet.gateway}
          <br />
          {t("Addresses", "주소")} {subnet.allocatedAddresses}/{subnet.usableAddresses} {t("used", "사용 중")} · {subnet.dhcp}
        </dd>

        <dt>{t("Bridge", "브릿지")}</dt>
        <dd>
          {bridge.name} · TAP {bridge.attachedTaps} {t("attached", "개 연결")}
        </dd>

        <dt>NAT</dt>
        <dd>
          {nat.enabled
            ? `${nat.sourceCidr} → ${nat.uplink || t("(no uplink)", "(uplink 없음)")}`
            : t("Internet blocked — no masquerading; outbound traffic is dropped", "인터넷 차단 — 마스커레이드 없음, 외부로 나가는 트래픽 drop")}
        </dd>

        <dt>{t("Firewall", "방화벽")}</dt>
        <dd>
          {[
            firewall.eastWestBlocked && t("VM-to-VM blocked", "VM 간 차단"),
            firewall.crossNetworkBlocked && t("Cross-network blocked", "다른 네트워크 차단"),
            firewall.antiSpoofing && t("IP/MAC spoofing blocked", "IP/MAC 위조 차단"),
          ]
            .filter(Boolean)
            .join(" · ")}
          <br />
          {t("Default egress", "기본 외부 통신")}: {firewall.defaultEgress}
        </dd>

        <dt>{t("Member VMs", "소속 VM")}</dt>
        <dd>
          {detail.vms.length === 0
            ? t("None", "없음")
            : detail.vms
                .map((vm) => `${vm.name} (${vm.ipv4 ?? t("no address", "주소 없음")}, ${vm.state})`)
                .join(", ")}
        </dd>
      </dl>
    </div>
  );
}
