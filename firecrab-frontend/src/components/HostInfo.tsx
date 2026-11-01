import { useEffect, useState, type ReactNode } from "react";
import type { HostStatusResponse, NetworkInfoResponse } from "../bindings";
import { getHostStatus, getNetworkInfo } from "../api/client";
import { useI18n } from "../i18n";
import ManagerResources from "./ManagerResources";
import { formatUptime } from "../lib/format";

const POLL_MILLIS = 2000;

/** Host status and native microManager resource controls. */
export default function HostInfo({ children }: { children?: ReactNode }) {
  const { locale, t } = useI18n();
  const [network, setNetwork] = useState<NetworkInfoResponse | null>(null);
  const [status, setStatus] = useState<HostStatusResponse | null>(null);

  useEffect(() => {
    let cancelled = false;

    const tick = async () => {
      try {
        const [nextNetwork, nextStatus] = await Promise.all([
          getNetworkInfo(),
          getHostStatus(),
        ]);
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

  const platform = status?.platform;
  const managed = !!platform?.virtualization && platform.os !== "linux";
  const resources = status && (
    <dl className="host-resources">
      <div>
        <dt>{t("Uptime", "가동 시간")}</dt>
        <dd>{formatUptime(status.uptimeSeconds, locale)}</dd>
      </div>
      <div>
        <dt>{t("Load", "CPU 부하")}</dt>
        <dd>{status.loadAverage1m.toFixed(2)}</dd>
      </div>
      <div>
        <dt>{t("Memory", "메모리")}</dt>
        <dd>
          {formatMib(status.memoryTotalMib - status.memoryAvailableMib)}{" "}
          <small>/ {formatMib(status.memoryTotalMib)}</small>
        </dd>
      </div>
      <div>
        <dt>{t("Disk", "디스크")}</dt>
        <dd>
          {formatGib(status.diskTotalGib - status.diskAvailableGib)}{" "}
          <small>/ {formatGib(status.diskTotalGib)}</small>
        </dd>
      </div>
    </dl>
  );
  return (
    <div className="host-screen">
      <h2 className="host-title">
        {platform
          ? [platform.name, platform.version].filter(Boolean).join(" ")
          : t("Host info", "호스트 정보")}
      </h2>
      {!managed && (
        <section aria-label={t("Host usage", "호스트 사용량")}>
          {resources}
        </section>
      )}
      {platform && (
        <section
          className="host-section"
          aria-label={t("Host system", "호스트 시스템")}
        >
          <header>
            <h3>{t("Host system", "호스트 시스템")}</h3>
            <p>
              {t("The machine running Firecrab", "Firecrab을 실행하는 호스트")}
            </p>
          </header>
          <dl className="host-values">
            <dt>{t("Operating system", "운영체제")}</dt>
            <dd>{platform.name}</dd>
            <dt>{t("Architecture", "아키텍처")}</dt>
            <dd>{platform.architecture}</dd>
            {platform.virtualization && (
              <>
                <dt>{t("Virtualization", "가상화")}</dt>
                <dd>{platform.virtualization}</dd>
              </>
            )}
          </dl>
        </section>
      )}
      {platform && (
        <section
          className="host-section"
          aria-label={t("Management Linux", "관리 Linux 환경")}
        >
          <header>
            <h3>{t("Management Linux", "관리 Linux 환경")}</h3>
            <p>
              {t(
                "The Linux environment MicroVMs run in",
                "MicroVM이 실행되는 Linux 환경",
              )}
            </p>
          </header>
          <dl className="host-values">
            <dt>{t("System", "시스템")}</dt>
            <dd>{platform.system}</dd>
            <dt>{t("Kernel", "커널")}</dt>
            <dd>{platform.kernel}</dd>
            <dt>Controller ID</dt>
            <dd className="mono">{platform.controllerId}</dd>
          </dl>
        </section>
      )}
      {children}
      {managed && (
        <section
          className="host-section host-manager"
          aria-label="microManager"
        >
          <header>
            <h3>{t("Management VM", "관리 VM")}</h3>
            <p>
              {t(
                "microManager usage and allocated resources",
                "microManager 사용량과 할당된 리소스",
              )}
            </p>
          </header>
          <div>
            {resources}
            <ManagerResources controllerId={platform?.controllerId} />
          </div>
        </section>
      )}
      {network && (
        <details className="host-network">
          <summary>{t("Host network details", "호스트 네트워크 상세")}</summary>
          <dl className="host-values">
            <dt>Bridge</dt>
            <dd>{network.bridgeName}</dd>
            <dt>Subnet</dt>
            <dd>{network.subnetCidr}</dd>
            <dt>Gateway</dt>
            <dd>{network.gateway}</dd>
            <dt>Uplink</dt>
            <dd>{network.uplink}</dd>
          </dl>
        </details>
      )}
      {!status && (
        <p className="detail-note">{t("Loading…", "불러오는 중…")}</p>
      )}
    </div>
  );
}
function formatMib(mib: number) {
  return mib >= 1024 ? `${(mib / 1024).toFixed(1)} GiB` : `${mib} MiB`;
}
function formatGib(gib: number) {
  return `${gib.toFixed(1)} GiB`;
}
