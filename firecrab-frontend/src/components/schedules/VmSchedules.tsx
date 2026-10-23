import { CalendarClock, ChevronRight, Play, Plus, Square } from "lucide-react";
import type { VmResponse } from "../../bindings";
import { useI18n } from "../../i18n";
import { schedulesHash } from "../../navigation";
import { DetailSection } from "../ui/DetailSection";
import { timingLabel } from "./timing";
import { useSchedules } from "./useSchedules";
import "../../styles/schedules.css";

export default function VmSchedules({ vm }: { vm: VmResponse }) {
  const { t, locale } = useI18n();
  const { items, error, reload } = useSchedules();
  const schedules = items?.filter(item => item.vmId === vm.id) ?? [];
  return <DetailSection title={t("Schedules", "예약")} aside={<a className="schedule-section-link" href={schedulesHash(vm.id)}>{t("Manage", "관리")}<ChevronRight aria-hidden="true" /></a>}>
    {items === null && !error && <p className="detail-note">{t("Loading schedules…", "예약을 확인하는 중이다.")}</p>}
    {error && <p className="detail-note">{t("Schedules are unavailable.", "예약 정보를 불러오지 못했다.")} <button className="link-button" onClick={() => void reload()}>{t("Retry", "다시 시도")}</button></p>}
    {items !== null && schedules.length === 0 && <p className="detail-note">{t("Schedule this VM to start or stop automatically.", "이 VM의 시작과 중지를 자동으로 예약한다.")}</p>}
    <div className="vm-schedule-list">{schedules.slice(0, 3).map(item => <a key={item.id} href={schedulesHash(vm.id, false, item.id)}>
      <span className={"schedule-action-icon " + item.action}>{item.action === "start" ? <Play aria-hidden="true" /> : <Square aria-hidden="true" />}</span>
      <span><strong>{item.action === "start" ? t("Start", "시작") : t("Stop", "중지")}</strong><small>{timingLabel(item.timing, locale)}</small></span>
      <span className={item.enabled ? "vm-schedule-on" : "schedule-muted"}>{item.enabled ? t("On", "사용 중") : t("Paused", "일시정지")}</span>
    </a>)}</div>
    <a className="btn schedule-vm-add" href={schedulesHash(vm.id, true)} aria-label={t("Add schedule for " + vm.name, vm.name + " 예약 추가")}><Plus aria-hidden="true" />{t("Add schedule", "예약 추가")}</a>
    {schedules.length === 0 && !error && <span className="schedule-vm-hint"><CalendarClock aria-hidden="true" />{t("Weekdays, every day, or just once", "평일 · 매일 · 한 번만")}</span>}
  </DetailSection>;
}
