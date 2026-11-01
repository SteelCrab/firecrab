interface SparklineProps {
  /** Percentages, oldest first. */
  values: number[];
  /** Fixed upper bound when charts must share a scale. Defaults to an adaptive scale. */
  scaleMax?: number;
  width: number;
  height: number;
  /** Fill under the line. */
  area?: boolean;
  tone?: "neutral" | "accent" | "warn";
  className?: string;
}

/** Trend line with an optional fixed scale; callers provide labels and axes. */
export default function Sparkline({
  values,
  width,
  height,
  area = false,
  tone = "neutral",
  className,
  scaleMax,
}: SparklineProps) {
  if (values.length < 2) return null;
  const top = scaleMax ?? Math.max(...values, 1) * 1.15;
  const step = width / (values.length - 1);
  const line = values
    .map(
      (value, index) =>
        `${index ? "L" : "M"}${(index * step).toFixed(1)} ${(height - 1 - (Math.min(Math.max(value, 0), top) / top) * (height - 2)).toFixed(1)}`,
    )
    .join("");

  return (
    <svg
      className={`sparkline ${tone}${className ? ` ${className}` : ""}`}
      viewBox={`0 0 ${width} ${height}`}
      width={width}
      height={height}
      preserveAspectRatio="none"
      aria-hidden="true"
    >
      {area && (
        <path
          className="sparkline-area"
          d={`${line}L${width} ${height}L0 ${height}Z`}
        />
      )}
      <path className="sparkline-line" d={line} />
    </svg>
  );
}
