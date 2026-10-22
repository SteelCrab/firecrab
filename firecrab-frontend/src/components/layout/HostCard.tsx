import { useHostStatus } from "../../hostStatusContext";
import { useI18n } from "../../i18n";
import { formatUptimeShort } from "../../lib/format";

function usedPercent(available: number, total: number): number {
  return Math.round((1 - available / total) * 100);
}

/** Sidebar summary of the host (spec §6.1). The API reports no core count, so CPU load has no bar. */
export default function HostCard() {
  const { locale, t } = useI18n();
  const host = useHostStatus();
  const memory = host ? usedPercent(host.memoryAvailableMib, host.memoryTotalMib) : null;
  const disk = host ? usedPercent(host.diskAvailableGib, host.diskTotalGib) : null;

  const meters = [
    { key: "cpu", label: t("CPU load", "CPU 부하"), value: host ? host.loadAverage1m.toFixed(2) : "—", percent: null },
    { key: "memory", label: t("Memory", "메모리"), value: memory === null ? "—" : `${memory}%`, percent: memory },
    { key: "disk", label: t("Disk", "디스크"), value: disk === null ? "—" : `${disk}%`, percent: disk },
  ];

  return (
    <section className="host-card" aria-label={t("Host status", "호스트 상태")}>
      <div className="host-card-head">
        <span className={host ? "host-dot ok" : "host-dot"} aria-hidden="true" />
        {t("Host", "호스트")}
        {host && (
          <span className="host-card-uptime">
            {t(`up ${formatUptimeShort(host.uptimeSeconds, locale)}`, `업타임 ${formatUptimeShort(host.uptimeSeconds, locale)}`)}
          </span>
        )}
      </div>
      {meters.map((meter) => (
        <div key={meter.key} className="host-meter">
          <span>{meter.label}</span>
          <span className="host-meter-track">
            {meter.percent !== null && <i style={{ width: `${Math.min(100, meter.percent)}%` }} />}
          </span>
          <b className="host-meter-value">{meter.value}</b>
        </div>
      ))}
    </section>
  );
}
