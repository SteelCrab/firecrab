import { useEffect, useState } from "react";
import type { FormEvent } from "react";
import type { MicroNetworkResponse } from "../bindings";
import { ApiClientError, createMicroNetwork, deleteMicroNetwork, listMicroNetworks } from "../api/client";

interface MicroNetworksModalProps {
  onClose: () => void;
}

/**
 * MicroNetwork management (`docs/task-micro-network.md`) — firecrab's
 * VPC-equivalent. This first slice only manages the named CIDR reservation
 * itself; bridge/gateway/route-table provisioning and VM membership aren't
 * wired up yet, so there's nothing to show beyond name + subnet here.
 */
export default function MicroNetworksModal({ onClose }: MicroNetworksModalProps) {
  const [networks, setNetworks] = useState<MicroNetworkResponse[] | null>(null);
  const [name, setName] = useState("");
  const [subnetCidr, setSubnetCidr] = useState("");
  const [submitting, setSubmitting] = useState(false);
  const [fieldErrors, setFieldErrors] = useState<ApiClientError | null>(null);
  const [listError, setListError] = useState<string | null>(null);
  const [busyId, setBusyId] = useState<string | null>(null);

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
      await createMicroNetwork({ name: name.trim(), subnetCidr: subnetCidr.trim() });
      setName("");
      setSubnetCidr("");
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
      await refresh();
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
                  <th>id</th>
                  <th className="actions">actions</th>
                </tr>
              </thead>
              <tbody>
                {networks.map((network) => (
                  <tr key={network.id}>
                    <td className="name">{network.name}</td>
                    <td className="mono">{network.subnetCidr}</td>
                    <td className="mono" title={network.id}>
                      {network.id.split("-")[0]}
                    </td>
                    <td className="actions">
                      <button
                        className="btn danger"
                        disabled={busyId === network.id}
                        onClick={() => handleDelete(network)}
                      >
                        delete
                      </button>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </div>
      </div>
    </div>
  );
}
