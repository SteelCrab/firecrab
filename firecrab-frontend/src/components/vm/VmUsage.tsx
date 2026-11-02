import type { VmResponse, VmUsageSample } from "../../bindings";
import { useI18n } from "../../i18n";
import Sparkline from "../ui/Sparkline";
import {
  HIGH_USAGE_PERCENT,
  formatMib,
  formatPercent,
  usageSeries,
} from "./vmFormat";

function UsageChart({
  history,
  metric,
  label,
  tone,
}: {
  history: VmUsageSample[];
  metric: "cpuUsagePercent" | "memoryUsedPercent";
  label: string;
  tone: "neutral" | "accent" | "warn";
}) {
  const { locale, t } = useI18n();
  const values = usageSeries(history, (sample) => sample[metric]);
  if (values.length < 2)
    return (
      <p className="vm-usage-history-note">
        {t("Collecting usage history…", "사용량 기록 수집 중…")}
      </p>
    );
  const timed = history.filter(
    (sample) =>
      sample[metric] != null && Number.isFinite(sample.atMs) && sample.atMs > 0,
  );
  const time = (atMs: number) =>
    new Date(atMs).toLocaleTimeString(locale, {
      hour: "2-digit",
      minute: "2-digit",
      second: "2-digit",
      hour12: false,
    });
  return (
    <div className="vm-usage-chart" role="img" aria-label={label}>
      <div className="vm-usage-chart-plot">
        <div className="vm-usage-chart-scale" aria-hidden="true">
          <span>100%</span>
          <span>0%</span>
        </div>
        <Sparkline
          values={values}
          width={240}
          height={56}
          area
          tone={tone}
          scaleMax={100}
        />
      </div>
      <div className="vm-usage-chart-time">
        <span>
          {timed.length > 1 ? time(timed[0].atMs) : t("Earlier", "이전")}
        </span>
        <span>
          {timed.length > 1
            ? time(timed[timed.length - 1].atMs)
            : t("Latest", "최근")}
        </span>
      </div>
    </div>
  );
}

/** Guest CPU busy percentage and memory used (MemTotal − MemAvailable). */
export default function VmUsage({ vm }: { vm: VmResponse }) {
  const { t } = useI18n();
  if (vm.state !== "running") {
    return (
      <p className="detail-note">
        {t(
          "CPU and memory usage are measured while this VM runs.",
          "VM이 실행 중일 때 CPU와 메모리 사용량을 측정합니다.",
        )}
      </p>
    );
  }
  const history = vm.usageHistory ?? [];
  const hasData =
    vm.cpuUsagePercent != null ||
    vm.memoryUsedPercent != null ||
    vm.memoryUsedMib != null ||
    history.some(
      (sample) =>
        sample.cpuUsagePercent != null || sample.memoryUsedPercent != null,
    );
  if (!hasData)
    return (
      <p className="detail-note">
        {t(
          "Waiting for measurements from this VM…",
          "이 VM의 측정값을 기다리는 중…",
        )}
      </p>
    );
  const cpuHot = (vm.cpuUsagePercent ?? 0) >= HIGH_USAGE_PERCENT;
  const memoryHot = (vm.memoryUsedPercent ?? 0) >= HIGH_USAGE_PERCENT;
  return (
    <div className="vm-usage-details">
      <p className="vm-usage-description">
        {t(
          "Measured inside this VM. Charts show recent usage from 0 to 100%.",
          "VM 내부에서 측정한 값입니다. 그래프는 최근 사용률을 0~100%로 표시합니다.",
        )}
      </p>
      <div className="vm-usage-cards">
        <section
          className="vm-usage-card"
          aria-label={t("VM CPU usage", "VM CPU 사용률")}
        >
          <div className="vm-usage-card-heading">
            <h4 className="vm-usage-card-label">
              {t("CPU usage", "CPU 사용률")}
            </h4>
            <div className="vm-usage-card-value">
              {vm.cpuUsagePercent == null
                ? "—"
                : formatPercent(vm.cpuUsagePercent)}
            </div>
          </div>
          <p className="vm-usage-card-basis">
            {t(
              `Average across this VM's ${vm.cpu} vCPU(s)`,
              `이 VM의 ${vm.cpu} vCPU 전체 평균`,
            )}
          </p>
          <UsageChart
            history={history}
            metric="cpuUsagePercent"
            label={t(
              "CPU usage history, 0 to 100 percent",
              "CPU 사용률 추이, 0~100%",
            )}
            tone={cpuHot ? "warn" : "accent"}
          />
        </section>
        <section
          className="vm-usage-card"
          aria-label={t("VM memory usage", "VM 메모리 사용률")}
        >
          <div className="vm-usage-card-heading">
            <h4 className="vm-usage-card-label">
              {t("Memory usage", "메모리 사용률")}
            </h4>
            <div className="vm-usage-card-value">
              {vm.memoryUsedPercent == null
                ? "—"
                : formatPercent(vm.memoryUsedPercent)}
            </div>
          </div>
          <p className="vm-usage-card-basis">
            {vm.memoryUsedMib == null ? (
              t("Waiting for memory measurement…", "메모리 측정값 대기 중…")
            ) : (
              <>
                {formatMib(vm.memoryUsedMib)}
                {vm.memoryTotalMib != null ? (
                  <>
                    {" "}
                    / {formatMib(vm.memoryTotalMib)} ·{" "}
                    {t("used / guest total", "사용 / VM 내 전체")}
                  </>
                ) : (
                  <> · {t("used", "사용 중")}</>
                )}
              </>
            )}
          </p>
          <UsageChart
            history={history}
            metric="memoryUsedPercent"
            label={t(
              "Memory usage history, 0 to 100 percent",
              "메모리 사용률 추이, 0~100%",
            )}
            tone={memoryHot ? "warn" : "neutral"}
          />
        </section>
      </div>
    </div>
  );
}
