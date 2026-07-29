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

interface MicroNetworksModalProps {
  onClose: () => void;
}

/**
 * MicroNetwork management (`docs/task-micro-network.md`) — firecrab's own
 * virtual networks. Creating one reserves the CIDR, provisions its host
 * bridge, and gives it its own DHCP range and NAT rule; VMs then pick one on
 * the create form. Deleting is refused while VMs are still in it.
 */
export default function MicroNetworksModal({ onClose }: MicroNetworksModalProps) {
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
    if (busyId || !window.confirm(`MicroNetwork "${network.name}"을(를) 삭제할까요?`)) return;
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
    <div className="console-overlay">
      <div className="console-panel">
        <div className="console-bar">
          <span className="console-title">MicroNetwork</span>
          <button className="btn console-close" onClick={onClose}>
            ✕
          </button>
        </div>
        <div className="detail-body">
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
              <label htmlFor="mn-internet">인터넷</label>
              <select
                id="mn-internet"
                value={internetEnabled ? "on" : "off"}
                onChange={(event) => setInternetEnabled(event.target.value === "on")}
              >
                <option value="on">연결 (NAT)</option>
                <option value="off">차단 (내부 전용)</option>
              </select>
              <span className="field-error"></span>
            </div>
            <div className="field">
              <label>&nbsp;</label>
              <button className="btn primary" type="submit" disabled={submitting}>
                {submitting ? "생성 중…" : "생성"}
              </button>
              <span className="field-error"></span>
            </div>
          </form>

          {listError && <div className="field-error">{listError}</div>}

          {networks === null ? (
            <div className="empty">불러오는 중…</div>
          ) : networks.length === 0 ? (
            <div className="empty">MicroNetwork가 없습니다 — 위에서 생성하세요</div>
          ) : (
            <table className="vm-table">
              <thead>
                <tr>
                  <th>name</th>
                  <th>subnet CIDR</th>
                  <th>gateway</th>
                  <th>인터넷</th>
                  <th>id</th>
                  <th className="actions">actions</th>
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
                    <td>{network.internetEnabled ? "연결" : "차단"}</td>
                    <td className="mono" title={network.id}>
                      {network.id.split("-")[0]}
                    </td>
                    <td className="actions">
                      <button
                        className="btn"
                        disabled={busyId === network.id}
                        title={
                          network.internetEnabled
                            ? "NAT을 떼고 외부로 나가는 트래픽을 차단합니다"
                            : "NAT을 붙여 외부 통신을 허용합니다"
                        }
                        onClick={(event) => {
                          event.stopPropagation();
                          handleToggleInternet(network);
                        }}
                      >
                        {network.internetEnabled ? "인터넷 차단" : "인터넷 연결"}
                      </button>
                      <button
                        className="btn danger"
                        disabled={busyId === network.id}
                        onClick={(event) => {
                          event.stopPropagation();
                          handleDelete(network);
                        }}
                      >
                        delete
                      </button>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}

          {selectedId && <MicroNetworkDetail detail={detail} error={detailError} />}
        </div>
      </div>
    </div>
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
  if (error) return <div className="field-error">{error}</div>;
  if (!detail) return <div className="empty">상세 불러오는 중…</div>;

  const { subnet, bridge, nat, firewall } = detail;
  return (
    <div className="detail-body">
      <dl className="detail-fields mono">
        <dt>네트워크 ID</dt>
        <dd>{detail.id}</dd>

        <dt>서브넷</dt>
        <dd>
          {subnet.cidr} · gateway {subnet.gateway}
          <br />
          주소 {subnet.allocatedAddresses}/{subnet.usableAddresses} 사용 중 · {subnet.dhcp}
        </dd>

        <dt>브릿지</dt>
        <dd>
          {bridge.name} · TAP {bridge.attachedTaps}개 연결
        </dd>

        <dt>NAT</dt>
        <dd>
          {nat.enabled
            ? `${nat.sourceCidr} → ${nat.uplink || "(uplink 없음)"}`
            : "인터넷 차단 — 마스커레이드 없음, 외부로 나가는 트래픽 drop"}
        </dd>

        <dt>방화벽</dt>
        <dd>
          {[
            firewall.eastWestBlocked && "VM 간 차단",
            firewall.crossNetworkBlocked && "다른 네트워크 차단",
            firewall.antiSpoofing && "IP/MAC 위조 차단",
          ]
            .filter(Boolean)
            .join(" · ")}
          <br />
          기본 외부 통신: {firewall.defaultEgress}
        </dd>

        <dt>소속 VM</dt>
        <dd>
          {detail.vms.length === 0
            ? "없음"
            : detail.vms
                .map((vm) => `${vm.name} (${vm.ipv4 ?? "주소 없음"}, ${vm.state})`)
                .join(", ")}
        </dd>
      </dl>
    </div>
  );
}
