import { stepRamValue } from "../model";

interface RamStepperProps {
  id: string;
  value: string;
  onChange: (value: string) => void;
}

/** cpu uses a plain number input's native up/down arrows; ram can't (the
 * valid values are powers of two, not a fixed step), so this reproduces the
 * same up/down interaction with buttons that jump between the fixed set of
 * options in `RAM_OPTIONS_MIB`. */
export default function RamStepper({ id, value, onChange }: RamStepperProps) {
  const step = (direction: 1 | -1) => {
    onChange(String(stepRamValue(Number(value) || 0, direction)));
  };

  return (
    <div className="ram-stepper">
      <input id={id} className="ram-stepper-value" value={value} readOnly inputMode="numeric" />
      <div className="ram-stepper-buttons">
        <button type="button" className="ram-stepper-btn" onClick={() => step(1)} aria-label="ram 증가">
          ▲
        </button>
        <button type="button" className="ram-stepper-btn" onClick={() => step(-1)} aria-label="ram 감소">
          ▼
        </button>
      </div>
    </div>
  );
}
