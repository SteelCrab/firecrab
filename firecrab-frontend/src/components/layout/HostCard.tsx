import { useHostStatus } from "../../hostStatusContext";
import { useI18n } from "../../i18n";
import { formatUptimeShort } from "../../lib/format";

function usedPercent(available: number, total: number): number | null {
  if (total <= 0) return null;
  return Math.max(0, Math.min(100, Math.round((1 - available / total) * 100)));
}

/** Sidebar summary of the host (spec §6.1). The API reports no core count, so CPU load has no bar. */
export default function HostCard() {
  const { locale, t } = useI18n();
  const host = useHostStatus();
  const memory = host
    ? usedPercent(host.memoryAvailableMib, host.memoryTotalMib)
    : null;
  const disk = host
    ? usedPercent(host.diskAvailableGib, host.diskTotalGib)
    : null;

  const meters = [
    {
      key: "cpu",
      label: t("CPU load", "CPU 부하"),
      value: host ? host.loadAverage1m.toFixed(2) : "—",
      percent: null,
    },
    {
      key: "memory",
      label: t("Memory", "메모리"),
      value: memory === null ? "—" : `${memory}%`,
      percent: memory,
    },
    {
      key: "disk",
      label: t("Disk", "디스크"),
      value: disk === null ? "—" : `${disk}%`,
      percent: disk,
    },
  ];

  return (
    <section className="host-card" aria-label={t("Host status", "호스트 상태")}>
      <div className="host-card-head">
        <span
          id="host-dot"
          className={host ? "host-dot ok" : "host-dot"}
          aria-hidden="true"
        />
        <a id="host-link" href="#/host">
          {!host
            ? t("Host status unavailable", "호스트 상태 확인 불가")
            : host.platform && host.platform.os !== "linux"
              ? t("Management VM healthy", "관리 VM 정상")
              : t("Host healthy", "호스트 정상")}
        </a>
        {host && (
          <span className="host-card-uptime">
            {t(
              `up ${formatUptimeShort(host.uptimeSeconds, locale)}`,
              `업타임 ${formatUptimeShort(host.uptimeSeconds, locale)}`,
            )}
          </span>
        )}
      </div>
      <p className="host-status-values">
        {meters.map((meter, index) => (
          <span key={meter.key}>
            {index > 0 ? " · " : ""}
            {meter.label}{" "}
            <b
              className="host-meter-value"
              id={`host-${meter.key === "cpu" ? "load" : meter.key}`}
            >
              {meter.value}
            </b>
          </span>
        ))}
      </p>
    </section>
  );
}
