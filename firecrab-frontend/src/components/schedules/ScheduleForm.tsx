import { useState } from "react";
import type { FormEvent } from "react";
import { CalendarClock, Play, Square } from "lucide-react";
import type { ScheduleResponse, ScheduleSpec, VmResponse } from "../../bindings";
import { ApiClientError, saveSchedule } from "../../api/client";
import { useI18n } from "../../i18n";
import StatusBadge from "../ui/StatusBadge";
import { buildTiming, DAYS, localZone, timingDraft, timingLabel } from "./timing";
import type { TimingDraft } from "./timing";

function shortName(value: string): string {
  let result = "";
  for (const char of value) {
    if (new TextEncoder().encode(result + char).length > 64) break;
    result += char;
  }
  return result;
}

export default function ScheduleForm({ schedule, vms, vmId, onCancel, onSaved }: {
  schedule: ScheduleResponse | null; vms: VmResponse[]; vmId?: string;
  onCancel: () => void; onSaved: () => void;
}) {
  const { t, locale } = useI18n();
  const [target, setTarget] = useState(schedule?.vmId ?? vmId ?? vms[0]?.id ?? "");
  const [action, setAction] = useState<ScheduleSpec["action"]>(schedule?.action ?? "start");
  const [draft, setDraft] = useState(() => timingDraft(schedule?.timing));
  const [name, setName] = useState(() => {
    if (!schedule) return "";
    const vm = vms.find(vm => vm.id === schedule.vmId);
    const labels = schedule.action === "start" ? ["Start", "시작"] : ["Stop", "중지"];
    return vm && labels.some(label => shortName(`${vm.name} · ${label}`) === schedule.name) ? "" : schedule.name;
  });
  const [enabled, setEnabled] = useState(schedule?.enabled ?? true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [fields, setFields] = useState<Record<string, string>>({});
  const vm = vms.find(vm => vm.id === target);
  const change = (patch: Partial<TimingDraft>) => setDraft(previous => ({ ...previous, ...patch }));
  const original = schedule?.timing.kind === "once" ? timingDraft(schedule.timing) : null;
  const timing = schedule?.timing.kind === "once" && original && draft.repeat === "once" && draft.date === original.date && draft.time === original.time ? schedule.timing : buildTiming(draft);
  const generatedName = shortName(`${vm?.name ?? "MicroVM"} · ${action === "start" ? t("Start", "시작") : t("Stop", "중지")}`);
  const fieldError = (key: string) => fields[key] && <span className="field-error" role="alert">{fields[key]}</span>;

  const submit = async (event: FormEvent) => {
    event.preventDefault();
    if (saving) return;
    setFields({}); setError(null);
    if (!timing) {
      setFields({ timing: draft.repeat === "weekly" && draft.days.length === 0 ? t("Choose at least one day.", "실행할 요일을 하나 이상 선택한다.") : t("Check the date and time. This local time may not exist during a daylight saving change.", "날짜와 시각을 확인한다. 서머타임 변경으로 존재하지 않는 시각은 선택할 수 없다.") });
      return;
    }
    setSaving(true);
    try {
      await saveSchedule(schedule?.id ?? null, { name: name.trim() || generatedName, vmId: target, action, enabled, timing });
      onSaved();
    } catch (failure) {
      setError((failure as Error).message);
      if (failure instanceof ApiClientError) setFields(failure.apiError?.fields ?? {});
      setSaving(false);
    }
  };

  return <div className="schedule-editor">
    <div className="schedule-detail-title"><h2>{schedule ? t("Edit schedule", "예약 수정") : t("New schedule", "예약 추가")}</h2><p>{t("Choose a VM, an operation, and a time.", "VM, 작업과 실행 시간을 선택한다.")}</p></div>
    {error && <p className="field-error" role="alert">{error}</p>}
    <form className="schedule-form" onSubmit={submit}>
      <div className="field"><label htmlFor="schedule-vm">MicroVM</label><select id="schedule-vm" value={target} onChange={event => setTarget(event.target.value)} required disabled={saving} autoFocus>
        <option value="">{t("Select a VM", "VM 선택")}</option>
        {target && !vm && <option value={target}>{t("VM unavailable", "VM 확인 필요")}</option>}
        {vms.map(vm => <option key={vm.id} value={vm.id}>{vm.name}</option>)}
      </select>{vm && <div className="schedule-vm-state"><StatusBadge state={vm.state} /><span>{vm.cpu} vCPU · {vm.ram} MiB</span></div>}{fieldError("vmId")}</div>
      <fieldset className="schedule-operation"><legend>{t("Operation", "작업")}</legend><div className="schedule-segments">
        <button type="button" aria-pressed={action === "start"} disabled={saving} onClick={() => setAction("start")}><Play aria-hidden="true" />{t("Start VM", "VM 시작")}</button>
        <button type="button" aria-pressed={action === "stop"} disabled={saving} onClick={() => setAction("stop")}><Square aria-hidden="true" />{t("Stop VM", "VM 중지")}</button>
      </div></fieldset>
      <div className="field"><label htmlFor="schedule-repeat">{t("Repeat", "반복")}</label><select id="schedule-repeat" value={draft.repeat} onChange={event => change({ repeat: event.target.value as TimingDraft["repeat"] })} disabled={saving}>
        <option value="weekdays">{t("Weekdays · Monday to Friday", "평일 · 월요일부터 금요일")}</option>
        <option value="daily">{t("Every day", "매일")}</option>
        <option value="weekly">{t("Choose days", "요일 선택")}</option>
        <option value="once">{t("One time", "한 번만")}</option>
        <option value="cron">{t("Custom cron", "Cron 직접 입력")}</option>
      </select></div>
      {draft.repeat === "weekly" && <fieldset className="schedule-operation"><legend>{t("Days", "요일")}</legend><div className="schedule-days">{DAYS.map(day => <button key={day.value} type="button" aria-label={locale === "ko" ? `${day.ko}요일` : day.en} aria-pressed={draft.days.includes(day.value)} disabled={saving} onClick={() => change({ days: draft.days.includes(day.value) ? draft.days.filter(value => value !== day.value) : [...draft.days, day.value] })}>{day[locale]}</button>)}</div></fieldset>}
      {draft.repeat === "cron" ? <div className="field"><label htmlFor="schedule-cron">{t("Cron expression", "Cron 표현식")}</label><input id="schedule-cron" value={draft.expression} onChange={event => change({ expression: event.target.value })} required maxLength={256} disabled={saving} /><span className="poll-note">{t("Minute · hour · day · month · weekday", "분 · 시 · 일 · 월 · 요일")}</span></div>
        : <div className={draft.repeat === "once" ? "schedule-time-row" : undefined}>
          {draft.repeat === "once" && <div className="field"><label htmlFor="schedule-date">{t("Date", "날짜")}</label><input id="schedule-date" type="date" value={draft.date} onChange={event => change({ date: event.target.value })} required disabled={saving} /></div>}
          <div className="field"><label htmlFor="schedule-time">{t("Time", "실행 시각")}</label><input id="schedule-time" type="time" value={draft.time} onChange={event => change({ time: event.target.value })} required disabled={saving} /></div>
        </div>}
      {fieldError("timing")}
      <div className="schedule-preview"><CalendarClock aria-hidden="true" /><div><strong>{timing ? timingLabel(timing, locale) : t("Choose an execution time", "실행 시간을 선택한다")}</strong><span>{vm?.name ?? t("Choose a VM", "VM 선택")} · {action === "start" ? t("Start", "시작") : t("Stop", "중지")}</span><small>{draft.repeat === "once" ? localZone() : draft.timeZone}</small></div></div>
      <details className="schedule-advanced" open={fields.name ? true : undefined}><summary>{t("More options", "상세 설정")}</summary><div className="schedule-form">
        <div className="field"><label htmlFor="schedule-name">{t("Name (optional)", "예약 이름 (선택)")}</label><input id="schedule-name" value={name} onChange={event => setName(event.target.value)} placeholder={generatedName} maxLength={64} disabled={saving} />{fieldError("name")}</div>
        {draft.repeat !== "once" && <div className="field"><label htmlFor="schedule-zone">{t("Time zone", "시간대")}</label><input id="schedule-zone" value={draft.timeZone} onChange={event => change({ timeZone: event.target.value })} list="schedule-zones" required disabled={saving} /><datalist id="schedule-zones">{[...new Set([localZone(), "UTC", "Asia/Seoul", "America/New_York", "Europe/London"])].map(value => <option key={value} value={value} />)}</datalist></div>}
        <label className="schedule-check"><input type="checkbox" checked={enabled} onChange={event => setEnabled(event.target.checked)} disabled={saving} />{t("Enable this schedule", "예약 사용")}</label>
      </div></details>
      <p className="schedule-footnote">{t("Keep Firecrab running for scheduled operations to execute.", "예약을 실행하려면 Firecrab이 실행 중이어야 한다.")}</p>
      <div className="schedule-form-actions"><button className="btn" type="button" disabled={saving} onClick={onCancel}>{t("Cancel", "취소")}</button><button className="btn primary" type="submit" disabled={saving || !target}>{saving ? t("Saving…", "저장 중…") : schedule ? t("Save changes", "변경 저장") : t("Create schedule", "예약 만들기")}</button></div>
    </form>
  </div>;
}
