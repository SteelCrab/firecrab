export interface BenchmarkTrendPoint {
  label: string;
  value: number;
  detail: string;
}

interface BenchmarkTrendChartProps {
  title: string;
  points: BenchmarkTrendPoint[];
  unit: string;
  color: string;
  fill: string;
  yMaxHint?: number;
  emptyLabel: string;
}

const WIDTH = 520;
const HEIGHT = 170;
const LEFT = 42;
const RIGHT = 12;
const TOP = 12;
const BOTTOM = 30;

/** Accessible SVG trend chart used by the Benchmark dashboard. */
export default function BenchmarkTrendChart({
  title,
  points,
  unit,
  color,
  fill,
  yMaxHint = 0,
  emptyLabel,
}: BenchmarkTrendChartProps) {
  const values = points.map((point) => point.value).filter(Number.isFinite);
  const latest = points.at(-1);
  if (values.length === 0 || !latest) {
    return (
      <article className="benchmark-chart is-empty">
        <div className="benchmark-chart-head">
          <h4>{title}</h4>
          <strong>—</strong>
        </div>
        <p>{emptyLabel}</p>
      </article>
    );
  }

  const innerWidth = WIDTH - LEFT - RIGHT;
  const innerHeight = HEIGHT - TOP - BOTTOM;
  const maximum = Math.max(yMaxHint, ...values, 1);
  const xAt = (index: number) =>
    points.length === 1 ? LEFT + innerWidth / 2 : LEFT + (index / (points.length - 1)) * innerWidth;
  const yAt = (value: number) => TOP + innerHeight * (1 - Math.max(0, value) / maximum);
  const line = points
    .map((point, index) => `${index === 0 ? "M" : "L"}${xAt(index).toFixed(1)} ${yAt(point.value).toFixed(1)}`)
    .join(" ");
  const baseline = TOP + innerHeight;
  const area = `${line} L${xAt(points.length - 1).toFixed(1)} ${baseline} L${xAt(0).toFixed(1)} ${baseline} Z`;
  const ticks = [maximum, maximum / 2, 0];

  return (
    <article className="benchmark-chart">
      <div className="benchmark-chart-head">
        <h4>{title}</h4>
        <strong className="mono">{formatValue(latest.value, unit)}</strong>
      </div>
      <svg
        viewBox={`0 0 ${WIDTH} ${HEIGHT}`}
        role="img"
        aria-label={`${title}: ${formatValue(latest.value, unit)}`}
        preserveAspectRatio="none"
      >
        {ticks.map((tick, index) => {
          const y = yAt(tick);
          return (
            <g key={index}>
              <line className="benchmark-chart-guide" x1={LEFT} x2={WIDTH - RIGHT} y1={y} y2={y} />
              <text className="benchmark-chart-axis" x={LEFT - 5} y={y + 3} textAnchor="end">
                {compactNumber(tick)}
              </text>
            </g>
          );
        })}
        <path d={area} fill={fill} stroke="none" />
        <path d={line} fill="none" stroke={color} strokeWidth="2.5" strokeLinejoin="round" />
        {points.map((point, index) => (
          <circle key={`${point.label}-${index}`} cx={xAt(index)} cy={yAt(point.value)} r="3" fill={color}>
            <title>{`${point.detail}: ${formatValue(point.value, unit)}`}</title>
          </circle>
        ))}
        <text className="benchmark-chart-axis" x={LEFT} y={HEIGHT - 7} textAnchor="start">
          {points[0]?.label}
        </text>
        <text className="benchmark-chart-axis" x={WIDTH - RIGHT} y={HEIGHT - 7} textAnchor="end">
          {latest.label}
        </text>
      </svg>
    </article>
  );
}

function formatValue(value: number, unit: string): string {
  return `${compactNumber(value)}${unit}`;
}

function compactNumber(value: number): string {
  if (Math.abs(value) >= 1000) return value.toLocaleString(undefined, { maximumFractionDigits: 0 });
  if (Math.abs(value) >= 10) return value.toFixed(1).replace(/\.0$/, "");
  return value.toFixed(2).replace(/0+$/, "").replace(/\.$/, "");
}
