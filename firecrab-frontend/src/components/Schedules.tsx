import { useEffect, useState } from "react";
import { CalendarClock, Check, CheckCircle2, ChevronRight, CircleAlert, Cpu, Loader2, Play, Plus, Square, Trash2, X } from "lucide-react";
import type { ScheduleResponse, ScheduleRun, VmResponse } from "../bindings";
import { deleteSchedule, listScheduleRuns, setScheduleEnabled } from "../api/client";
import { useI18n } from "../i18n";
import { newVmHash, schedulesHash, vmDetailsHash } from "../navigation";
import Screen from "./layout/Screen";
import { useScreenStatus } from "./layout/screenStatus";
import StatusBadge from "./ui/StatusBadge";
import ScheduleForm from "./schedules/ScheduleForm";
import { executionTime, scheduleZone, timingLabel } from "./schedules/timing";
import { useSchedules } from "./schedules/useSchedules";
import "../styles/schedules.css";

function consumed(schedule: ScheduleResponse): boolean {
  return !schedule.enabled && schedule.timing.kind === "once" && schedule.lastRun?.scheduledAtMs === Date.parse(schedule.timing.at);
}

function RunResult({ status }: { status: ScheduleRun["status"] }) {
  const { t } = useI18n();
  const label = { running: t("Running", "실행 중"), succeeded: t("Succeeded", "성공"), failed: t("Failed", "실패"), interrupted: t("Interrupted", "중단") }[status];
  const Icon = status === "running" ? Loader2 : status === "succeeded" ? CheckCircle2 : CircleAlert;
  return <span className={"schedule-result " + status}><Icon className={status === "running" ? "spin" : undefined} aria-hidden="true" />{label}</span>;
}

function ScheduleSwitch({ schedule, disabled, onToggle }: { schedule: ScheduleResponse; disabled: boolean; onToggle: () => void }) {
  const { t } = useI18n();
  const done = consumed(schedule);
  return <div className="schedule-switch-wrap"><span>{done ? t("Completed", "완료") : schedule.enabled ? t("On", "사용 중") : t("Paused", "일시정지")}</span>
    <button type="button" className="schedule-switch" role="switch" aria-checked={schedule.enabled}
      aria-label={t("Enable " + schedule.name, schedule.name + " 사용")} disabled={disabled || done || (!schedule.enabled && schedule.lastRun?.status === "running")}
      title={done ? t("Edit the execution time to use this schedule again.", "다시 사용하려면 실행 시각을 수정한다.") : undefined} onClick={onToggle}><span aria-hidden="true" /></button>
  </div>;
}

interface Props { vms: VmResponse[]; vmsLoaded: boolean; vmId?: string; create?: boolean; scheduleId?: string }

export default function Schedules({ vms, vmsLoaded, vmId, create, scheduleId }: Props) {
  const { t, locale } = useI18n();
  const { setDetailOpen } = useScreenStatus();
  const { items, error: pollError, reload } = useSchedules();
  const [editor, setEditor] = useState<ScheduleResponse | null | undefined>(create ? null : undefined);
  const [selectedId, setSelectedId] = useState<string | null>(scheduleId ?? null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const visible = items?.filter(schedule => !vmId || schedule.vmId === vmId) ?? [];
  const selected = items?.find(schedule => schedule.id === selectedId) ?? null;
  const contextVm = vms.find(vm => vm.id === vmId);
  const close = () => {
    setEditor(undefined); setSelectedId(null);
    if (create || scheduleId) window.location.hash = schedulesHash(vmId);
  };
  const edit = (schedule: ScheduleResponse | null) => { setEditor(schedule); setSelectedId(null); setDetailOpen(true); };
  const show = (schedule: ScheduleResponse) => { setEditor(undefined); setSelectedId(schedule.id); setDetailOpen(true); };
  const toggle = async (schedule: ScheduleResponse) => {
    if (busy) return;
    setBusy(true); setError(null); setNotice(null);
    try {
      await setScheduleEnabled(schedule.id, !schedule.enabled);
      setNotice(schedule.enabled ? t("Future runs are paused. An active operation will finish.", "다음 실행을 일시정지했다. 실행 중인 작업은 계속 진행한다.") : t("Schedule enabled.", "예약을 사용한다."));
      await reload();
    } catch (failure) { setError((failure as Error).message); }
    finally { setBusy(false); }
  };
  const remove = async (schedule: ScheduleResponse) => {
    if (!window.confirm(t("Delete “" + schedule.name + "” and its history? The VM will be kept.", "‘" + schedule.name + "’ 예약과 기록을 삭제할까요? VM은 유지한다."))) return;
    setBusy(true); setError(null);
    try { await deleteSchedule(schedule.id); close(); setNotice(t("Schedule deleted.", "예약을 삭제했다.")); await reload(); }
    catch (failure) { setError((failure as Error).message); }
    finally { setBusy(false); }
  };
  const detail = editor !== undefined ? <ScheduleForm key={editor?.id ?? "new"} schedule={editor} vms={vms} vmId={vmId} onCancel={close} onSaved={() => {
    setNotice(editor ? t("Changes saved.", "변경을 저장했다.") : t("Schedule created.", "예약을 추가했다."));
    close(); void reload();
  }} /> : selected ? <ScheduleDetails schedule={selected} vm={vms.find(vm => vm.id === selected.vmId)} busy={busy} onEdit={() => edit(selected)} onDelete={() => void remove(selected)} onToggle={() => void toggle(selected)} /> : null;

  return <Screen view="schedules" actions={<button className="btn primary" disabled={busy || !vmsLoaded || vms.length === 0} onClick={() => edit(null)}><Plus aria-hidden="true" />{t("New schedule", "예약 추가")}</button>}
    detail={detail ? <div className="detail-body detail-embed">{detail}</div> : undefined}
    detailLabel={t("Schedule detail", "예약 상세")} detailKind={<><CalendarClock aria-hidden="true" />{t("Schedule", "예약")}</>} onCloseDetail={close}>
    <section className="schedules-page" aria-label={t("Schedules", "스케줄러")}>
      <div className="schedule-intro"><h2>{t("Ready when you need it.", "필요한 시간에, 자동으로.")}</h2><p>{t("Set a time to start or stop your MicroVMs.", "MicroVM의 시작과 중지를 예약한다.")}</p></div>
      <div className="schedule-toolbar"><label className="schedule-filter"><Cpu aria-hidden="true" /><span className="sr-only">{t("Filter by VM", "VM별 보기")}</span><select value={vmId ?? ""} onChange={event => { window.location.hash = schedulesHash(event.target.value || undefined); }}>
        <option value="">{t("All MicroVMs", "모든 MicroVM")}</option>
        {vmId && !contextVm && <option value={vmId}>{t("VM unavailable", "VM 확인 필요")}</option>}
        {vms.map(vm => <option key={vm.id} value={vm.id}>{vm.name}</option>)}
      </select></label><span className="schedule-count">{items === null ? t("Loading…", "불러오는 중…") : t(visible.filter(item => item.enabled).length + " active · " + visible.length + " total", visible.length + "개 예약 · " + visible.filter(item => item.enabled).length + "개 사용 중")}</span>
        {contextVm && <a className="schedule-context-link" href={vmDetailsHash(contextVm.id)} onClick={() => setDetailOpen(true)}>{t("Open VM", "VM 보기")}<ChevronRight aria-hidden="true" /></a>}
      </div>
      {(error || pollError) && <p className="alert error" role="alert">{error || pollError}</p>}
      {notice && <div className="schedule-notice" role="status"><Check aria-hidden="true" /><span>{notice}</span><button className="icon-btn" aria-label={t("Dismiss notification", "알림 닫기")} onClick={() => setNotice(null)}><X aria-hidden="true" /></button></div>}
      {items !== null && visible.length === 0 ? <div className="schedule-empty"><CalendarClock aria-hidden="true" /><h3>{t("Make room for a routine.", "반복되는 작업을 예약한다.")}</h3><p>{vmsLoaded && vms.length === 0 ? t("Create a MicroVM, then schedule when it starts and stops.", "먼저 MicroVM을 만든 뒤 시작과 중지를 예약한다.") : t("Start your development VM each morning, or stop it after work.", "아침에는 개발 VM을 시작한다. 업무가 끝나면 자동으로 중지한다.")}</p>
        {vmsLoaded && vms.length === 0 ? <a className="btn" href={newVmHash()}>{t("Create a MicroVM", "MicroVM 만들기")}</a> : <button className="btn" disabled={!vmsLoaded} onClick={() => edit(null)}><Plus aria-hidden="true" />{t("Create your first schedule", "첫 예약 만들기")}</button>}
      </div> : <div className="schedule-list" role="list" aria-label={t("Saved schedules", "저장된 예약")}>
        {visible.length > 0 && <div className="schedule-list-head" aria-hidden="true"><span>{t("Schedule", "예약")}</span><span>MicroVM</span><span>{t("Next run", "다음 실행")}</span><span>{t("Status", "상태")}</span></div>}
        {visible.map(schedule => {
          const vm = vms.find(vm => vm.id === schedule.vmId);
          return <article className={"schedule-list-item" + (selectedId === schedule.id || editor?.id === schedule.id ? " selected" : "")} role="listitem" key={schedule.id} aria-label={schedule.name}>
            <button className="schedule-row-main" onClick={() => show(schedule)} aria-label={t("View schedule " + schedule.name, schedule.name + " 예약 보기")}><span className={"schedule-action-icon " + schedule.action} aria-hidden="true">{schedule.action === "start" ? <Play /> : <Square />}</span><span className="schedule-row-copy"><strong>{schedule.name}</strong><small>{timingLabel(schedule.timing, locale)}</small>{schedule.lastRun?.status === "failed" && <span className="schedule-row-alert">{t("Last run failed", "최근 실행 실패")}</span>}</span></button>
            <div className="schedule-row-vm">{vm ? <a href={vmDetailsHash(vm.id)} onClick={() => setDetailOpen(true)}><Cpu aria-hidden="true" />{vm.name}</a> : <span className="schedule-muted">{vmsLoaded ? t("VM unavailable", "VM 확인 필요") : t("Loading VM…", "VM 확인 중…")}</span>}</div>
            <div className="schedule-next">{schedule.lastRun?.status === "running" ? <RunResult status="running" /> : <><span>{schedule.enabled ? executionTime(schedule.nextRunAtMs, scheduleZone(schedule.timing), locale) : consumed(schedule) ? t("Execution finished", "실행 완료") : t("No upcoming runs", "다음 실행 없음")}</span><small>{scheduleZone(schedule.timing)}</small></>}</div>
            <ScheduleSwitch schedule={schedule} disabled={busy} onToggle={() => void toggle(schedule)} />
          </article>;
        })}
      </div>}
      {items !== null && visible.length > 0 && <p className="schedule-page-note"><CalendarClock aria-hidden="true" />{t("Schedules follow their selected time zone. Open a schedule to see its execution history.", "각 예약의 시간대를 기준으로 실행한다. 예약을 선택하면 실행 기록을 확인한다.")}</p>}
    </section>
  </Screen>;
}

function ScheduleDetails({ schedule, vm, busy, onEdit, onDelete, onToggle }: {
  schedule: ScheduleResponse; vm?: VmResponse; busy: boolean; onEdit: () => void; onDelete: () => void; onToggle: () => void;
}) {
  const { t, locale } = useI18n();
  const running = schedule.lastRun?.status === "running";
  return <div className="schedule-details"><div className="schedule-detail-title"><h2>{schedule.name}</h2><p>{timingLabel(schedule.timing, locale)}</p></div>
    <div className="schedule-linked-vm"><Cpu aria-hidden="true" /><div>{vm ? <a href={vmDetailsHash(vm.id)}>{vm.name}<ChevronRight aria-hidden="true" /></a> : <strong>{t("VM unavailable", "VM 확인 필요")}</strong>}{vm && <StatusBadge state={vm.state} />}</div></div>
    <dl className="schedule-properties"><dt>{t("Operation", "작업")}</dt><dd>{schedule.action === "start" ? t("Start VM", "VM 시작") : t("Stop VM", "VM 중지")}</dd><dt>{t("Next run", "다음 실행")}</dt><dd>{executionTime(schedule.nextRunAtMs, scheduleZone(schedule.timing), locale)}</dd><dt>{t("Time zone", "시간대")}</dt><dd>{scheduleZone(schedule.timing)}</dd><dt>{t("Enabled", "예약 사용")}</dt><dd><ScheduleSwitch schedule={schedule} disabled={busy} onToggle={onToggle} /></dd></dl>
    <div className="schedule-detail-actions"><button className="btn" disabled={busy || running} onClick={onEdit}>{t("Edit schedule", "예약 수정")}</button><button className="icon-btn schedule-delete" aria-label={t("Delete " + schedule.name, schedule.name + " 삭제")} disabled={busy || running} onClick={onDelete}><Trash2 aria-hidden="true" /></button></div>
    <RunHistory key={schedule.id} schedule={schedule} />
  </div>;
}

function RunHistory({ schedule }: { schedule: ScheduleResponse }) {
  const { t, locale } = useI18n();
  const [runs, setRuns] = useState<ScheduleRun[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [all, setAll] = useState(false);
  useEffect(() => {
    let stopped = false;
    let timer: ReturnType<typeof setTimeout>;
    const poll = async () => {
      try { const next = await listScheduleRuns(schedule.id); if (!stopped) { setRuns(next); setError(null); } }
      catch (failure) { if (!stopped) setError((failure as Error).message); }
      if (!stopped) timer = setTimeout(poll, 5000);
    };
    void poll();
    return () => { stopped = true; clearTimeout(timer); };
  }, [schedule.id]);
  return <section className="schedule-history-section" aria-label={t("Execution history", "실행 기록")}><h3>{t("Recent activity", "최근 실행")}</h3>
    {error && <p className="field-error" role="alert">{error}</p>}
    {runs === null ? !error && <p className="schedule-muted">{t("Loading…", "불러오는 중…")}</p> : runs.length === 0 ? <p className="schedule-muted">{t("No runs yet. The first result will appear here.", "아직 실행 기록이 없다. 첫 실행 결과가 여기에 표시된다.")}</p> : <>
      <ol className="schedule-history">{runs.slice(0, all ? 100 : 5).map(run => <li key={run.id}><div><RunResult status={run.status} /><time dateTime={new Date(run.startedAtMs).toISOString()}>{executionTime(run.startedAtMs, scheduleZone(schedule.timing), locale)}</time></div>{run.error && <p className="field-error">{run.error}</p>}<small>{t("Finished", "종료")} · {executionTime(run.finishedAtMs, scheduleZone(schedule.timing), locale)}</small></li>)}</ol>
      {runs.length > 5 && <button className="link-button" onClick={() => setAll(!all)}>{all ? t("Show less", "접기") : t("Show all recent runs", "최근 실행 전체 보기")}</button>}
    </>}
  </section>;
}
