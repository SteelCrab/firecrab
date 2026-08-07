import type { VmUsageSample } from "../bindings";
import { useI18n } from "../i18n";

interface UsageChartsProps {
  history: VmUsageSample[];
  /** Allocated RAM in MiB — used as a soft scale upper bound for memory. */
  ramMib: number;
  /**
   * Layout size:
   * - `compact` — console side strip
   * - `default` — inline / modest
   * - `large` — dedicated observability panel
   */
  size?: "compact" | "default" | "large";
  /** @deprecated Prefer `size="compact"`. */
  compact?: boolean;
}

function formatCpuPercent(value: number): string {
  const rounded = value >= 10 ? Math.round(value) : Math.round(value * 10) / 10;
  return `${rounded}%`;
}

function lastCpu(history: VmUsageSample[]): number | null {
  for (let i = history.length - 1; i >= 0; i -= 1) {
    const v = history[i]?.cpuUsagePercent;
    if (v != null) return v;
  }
  return null;
}

function lastMem(history: VmUsageSample[]): number | null {
  for (let i = history.length - 1; i >= 0; i -= 1) {
    const v = history[i]?.memoryUsedMib;
    if (v != null) return v;
  }
  return null;
}

const SIZE_GEOMETRY = {
  compact: { height: 36, width: 160, strokeWidth: 1.5 },
  default: { height: 48, width: 220, strokeWidth: 1.5 },
  large: { height: 96, width: 420, strokeWidth: 2.25 },
} as const;

/**
 * Dual sparklines for host Firecracker process CPU % and RSS.
 * Pure SVG — no chart library.
 */
export default function UsageCharts({
  history,
  ramMib,
  size,
  compact = false,
}: UsageChartsProps) {
  const { t } = useI18n();
  const resolvedSize = size ?? (compact ? "compact" : "default");
  if (history.length < 2) {
    return (
      <p className={`usage-charts-empty${resolvedSize === "large" ? " is-large" : ""}`}>
        {t("Collecting samples…", "샘플 수집 중…")}
      </p>
    );
  }

  const cpuValues = history.map((s) => s.cpuUsagePercent);
  const memValues = history.map((s) =>
    s.memoryUsedMib != null ? s.memoryUsedMib : null,
  );
  const cpuNow = lastCpu(history);
  const memNow = lastMem(history);
  const { height, width, strokeWidth } = SIZE_GEOMETRY[resolvedSize];

  return (
    <div className={`usage-charts is-${resolvedSize}`}>
      <Sparkline
        label={t("CPU", "CPU")}
        unit={cpuNow != null ? formatCpuPercent(cpuNow) : "—"}
        values={cpuValues}
        width={width}
        height={height}
        stroke="var(--ember, #c43e12)"
        fill="rgba(196, 62, 18, 0.12)"
        yMaxHint={100}
        strokeWidth={strokeWidth}
        size={resolvedSize}
      />
      <Sparkline
        label={t("Memory", "메모리")}
        unit={
          memNow != null
            ? `${memNow} / ${ramMib} MiB`
            : "—"
        }
        values={memValues}
        width={width}
        height={height}
        stroke="var(--ready, #2f9e6b)"
        fill="rgba(47, 158, 107, 0.12)"
        yMaxHint={Math.max(ramMib, 1)}
        strokeWidth={strokeWidth}
        size={resolvedSize}
      />
    </div>
  );
}

interface SparklineProps {
  label: string;
  unit: string;
  values: Array<number | null>;
  width: number;
  height: number;
  stroke: string;
  fill: string;
  /** Preferred upper bound; raised if data exceeds it. */
  yMaxHint: number;
  strokeWidth?: number;
  size?: "compact" | "default" | "large";
}

function Sparkline({
  label,
  unit,
  values,
  width,
  height,
  stroke,
  fill,
  yMaxHint,
  strokeWidth = 1.5,
  size = "default",
}: SparklineProps) {
  const padX = size === "large" ? 4 : 2;
  const padY = size === "large" ? 6 : 3;
  const innerW = width - padX * 2;
  const innerH = height - padY * 2;
  const known = values.filter((v): v is number => v != null && Number.isFinite(v));
  const dataMax = known.length > 0 ? Math.max(...known) : 0;
  const yMax = Math.max(yMaxHint, dataMax * 1.05, 1);
  const n = values.length;
  const xAt = (i: number) =>
    n <= 1 ? padX + innerW / 2 : padX + (i / (n - 1)) * innerW;
  const yAt = (v: number) => padY + innerH * (1 - Math.min(v, yMax) / yMax);

  const lineParts: string[] = [];
  let penDown = false;
  values.forEach((v, i) => {
    if (v == null || !Number.isFinite(v)) {
      penDown = false;
      return;
    }
    const cmd = penDown ? "L" : "M";
    lineParts.push(`${cmd}${xAt(i).toFixed(1)} ${yAt(v).toFixed(1)}`);
    penDown = true;
  });

  // Area under contiguous segments that have values — simple path from first to last known.
  const firstIdx = values.findIndex((v) => v != null && Number.isFinite(v));
  let lastIdx = -1;
  for (let i = values.length - 1; i >= 0; i -= 1) {
    if (values[i] != null && Number.isFinite(values[i]!)) {
      lastIdx = i;
      break;
    }
  }
  let areaD = "";
  if (firstIdx >= 0 && lastIdx >= firstIdx && lineParts.length > 0) {
    const baseY = padY + innerH;
    areaD = `${lineParts.join(" ")} L${xAt(lastIdx).toFixed(1)} ${baseY.toFixed(1)} L${xAt(firstIdx).toFixed(1)} ${baseY.toFixed(1)} Z`;
  }

  return (
    <div className={`usage-spark${size === "large" ? " is-large" : ""}`}>
      <div className="usage-spark-head">
        <span className="usage-spark-label">{label}</span>
        <span className="usage-spark-unit mono">{unit}</span>
      </div>
      <svg
        className="usage-spark-svg"
        viewBox={`0 0 ${width} ${height}`}
        width="100%"
        height={height}
        role="img"
        aria-label={`${label} ${unit}`}
        preserveAspectRatio="none"
      >
        {areaD && <path d={areaD} fill={fill} stroke="none" />}
        {lineParts.length > 0 && (
          <path
            d={lineParts.join(" ")}
            fill="none"
            stroke={stroke}
            strokeWidth={strokeWidth}
            strokeLinejoin="round"
            strokeLinecap="round"
          />
        )}
      </svg>
    </div>
  );
}
