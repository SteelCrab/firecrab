interface ProgressGaugeProps {
  /** Overall progress, 0 to 100. */
  percent: number;
  label: string;
  failed?: boolean;
}

/** A horizontal gauge with its percent beside it, readable by assistive technology as a progress bar. */
export default function ProgressGauge({ percent, label, failed = false }: ProgressGaugeProps) {
  const value = Math.max(0, Math.min(100, Math.round(percent)));
  return (
    <div className="gauge">
      <div
        className="gauge-track"
        role="progressbar"
        aria-label={label}
        aria-valuemin={0}
        aria-valuemax={100}
        aria-valuenow={value}
      >
        <div className={`gauge-fill${failed ? " is-failed" : ""}`} style={{ width: `${value}%` }} />
      </div>
      <span className="gauge-percent">{value}%</span>
    </div>
  );
}
