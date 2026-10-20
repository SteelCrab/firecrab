import { useI18n } from "../i18n";
import { canUsePipeApps, type PipeApp } from "../pipeApps";

interface Props {
  id: string;
  label: string;
  apps: PipeApp[];
  selected: string[];
  preserved?: string[];
  loading: boolean;
  unavailable: boolean;
  disabled: boolean;
  onChange: (addresses: string[]) => void;
}

export default function PipeAppPicker({ id, label, apps, selected, preserved = [], loading, unavailable, disabled, onChange }: Props) {
  const { t } = useI18n();
  const seen = new Set<string>();
  const choices = apps.filter(app => {
    if (!app.address) return true;
    if (seen.has(app.address)) return false;
    seen.add(app.address);
    return true;
  });
  for (const address of new Set(selected)) {
    if (!seen.has(address)) choices.push({ id: address, name: address, port: null, address, unavailable: null });
  }
  const reason = {
    notRunning: t("VM is not running", "VM이 실행 중이 아니다"),
    noAddress: t("Waiting for an IPv4 address", "IPv4 주소를 확인할 수 없다"),
    noPorts: t("No TCP ports registered", "등록된 TCP 포트가 없다"),
  };
  function toggle(address: string) {
    onChange(selected.includes(address) ? selected.filter(value => value !== address) : [...selected, address]);
  }

  return <fieldset className="field pipe-app-picker pipe-targets" disabled={disabled} aria-describedby={`${id}-help`}>
    <legend>{label}</legend>
    {loading ? <p className="poll-note" role="status">{t("Loading VMs…", "VM 조회 중…")}</p>
      : unavailable ? <p className="field-error" role="status">{t("VMs are unavailable.", "VM 조회 실패")}</p>
      : !apps.length ? <p className="poll-note">{t("No VMs.", "VM이 없다.")}</p> : null}
    {choices.length > 0 && <div className="pipe-app-list">
      {choices.map(app => {
        const checked = app.address !== null && selected.includes(app.address);
        const name = app.port === null ? app.name : `${app.name} · TCP ${app.port}`;
        return <label className="pipe-app-option" key={app.id}>
          <input type="checkbox" aria-label={name} checked={checked} disabled={loading || unavailable || (app.unavailable !== null && !checked)}
            onChange={() => { if (app.address) toggle(app.address); }} />
          <span><span className="pipe-app-name">{name}</span>{app.address && app.address !== name && <span className="pipe-app-address">{app.address}</span>}
            {app.unavailable && <span className="pipe-app-reason">{reason[app.unavailable]}</span>}</span>
        </label>;
      })}
    </div>}
    <span className="poll-note" id={`${id}-help`}>{t("Ports: VM → TCP port forwarding", "포트 추가: VM → TCP 포트포워딩")}</span>
    {selected.length > 0 && !canUsePipeApps(apps, selected, preserved) && <span className="field-error">{t("Select a running app again.", "실행 중인 앱을 다시 선택한다.")}</span>}
  </fieldset>;
}
