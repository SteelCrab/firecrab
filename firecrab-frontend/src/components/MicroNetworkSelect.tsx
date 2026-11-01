import type { MicroNetworkResponse } from "../bindings";
import { useI18n } from "../i18n";

/** Shared catalog picker for VM creation and inactive-VM network changes. */
export default function MicroNetworkSelect({
  id,
  value,
  networks,
  onChange,
  disabled = false,
}: {
  id: string;
  value: string;
  networks: MicroNetworkResponse[];
  onChange: (id: string) => void;
  disabled?: boolean;
}) {
  const { t } = useI18n();
  return (
    <select
      id={id}
      value={value}
      disabled={disabled}
      onChange={(event) => onChange(event.target.value)}
    >
      <option value="" disabled>
        {networks.length === 0
          ? t("Create a MicroNetwork first", "먼저 MicroNetwork를 만드세요")
          : t("Select a MicroNetwork", "MicroNetwork 선택")}
      </option>
      {value && !networks.some((network) => network.id === value) && (
        <option value={value} disabled>
          {value} · {t("Unavailable", "사용 불가")}
        </option>
      )}
      {networks.map((network) => (
        <option key={network.id} value={network.id}>
          {network.name} ({network.subnetCidr})
        </option>
      ))}
    </select>
  );
}
