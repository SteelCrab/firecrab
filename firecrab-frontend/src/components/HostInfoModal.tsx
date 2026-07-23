import { useEffect, useState } from "react";
import type { HostStatusResponse, NetworkInfoResponse } from "../bindings";
import { getHostStatus, getNetworkInfo } from "../api/client";

const POLL_MILLIS = 2000;

interface HostInfoModalProps {
  onClose: () => void;
}

/** Read-only view of the host's network config and live resource status. */
export default function HostInfoModal({ onClose }: HostInfoModalProps) {
  const [network, setNetwork] = useState<NetworkInfoResponse | null>(null);
  const [status, setStatus] = useState<HostStatusResponse | null>(null);

  useEffect(() => {
    let cancelled = false;

    const tick = async () => {
      try {
        const [nextNetwork, nextStatus] = await Promise.all([getNetworkInfo(), getHostStatus()]);
        if (cancelled) return;
        setNetwork(nextNetwork);
        setStatus(nextStatus);
      } catch {
        // Transient poll miss — keep the last known state, try again next tick.
      }
    };

    tick();
    const interval = setInterval(tick, POLL_MILLIS);
    return () => {
      cancelled = true;
      clearInterval(interval);
    };
  }, []);

  return (
    <div className="console-overlay">
      <div className="console-panel">
        <div className="console-bar">
          <span className="console-title">HOST 정보</span>
          <button className="btn console-close" onClick={onClose}>
            ✕
          </button>
        </div>
        <div className="detail-body">
          {network ? (
            <dl className="detail-fields mono">
              <dt>bridge</dt>
              <dd>{network.bridgeName}</dd>
              <dt>subnet</dt>
              <dd>{network.subnetCidr}</dd>
              <dt>gateway</dt>
              <dd>{network.gateway}</dd>
            </dl>
          ) : (
            <div className="empty">불러오는 중…</div>
          )}
          {status ? (
            <dl className="detail-fields mono">
              <dt>load avg (1m)</dt>
              <dd>{status.loadAverage1m.toFixed(2)}</dd>
              <dt>memory</dt>
              <dd>
                {formatMib(status.memoryTotalMib - status.memoryAvailableMib)} / {formatMib(status.memoryTotalMib)} 사용 중
              </dd>
              <dt>disk</dt>
              <dd>
                {formatGib(status.diskTotalGib - status.diskAvailableGib)} / {formatGib(status.diskTotalGib)} 사용 중
              </dd>
              <dt>uptime</dt>
              <dd>{formatUptime(status.uptimeSeconds)}</dd>
            </dl>
          ) : (
            <div className="empty">불러오는 중…</div>
          )}
        </div>
      </div>
    </div>
  );
}

function formatMib(mib: number): string {
  return mib >= 1024 ? `${(mib / 1024).toFixed(1)} GiB` : `${mib} MiB`;
}

function formatGib(gib: number): string {
  return `${gib.toFixed(1)} GiB`;
}

function formatUptime(seconds: number): string {
  const days = Math.floor(seconds / 86400);
  const hours = Math.floor((seconds % 86400) / 3600);
  const minutes = Math.floor((seconds % 3600) / 60);
  const parts = [];
  if (days > 0) parts.push(`${days}일`);
  if (days > 0 || hours > 0) parts.push(`${hours}시간`);
  parts.push(`${minutes}분`);
  return parts.join(" ");
}
