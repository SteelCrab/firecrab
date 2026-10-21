import { useCallback, useState } from "react";
import { Cpu, Plus, Search } from "lucide-react";
import type { VmResponse } from "../../bindings";
import { useI18n } from "../../i18n";
import { useListNavigation } from "../../lib/listNavigation";
import type { VmAction } from "../../model";
import { newVmHash } from "../../navigation";
import Screen from "../layout/Screen";
import { useScreenStatus } from "../layout/screenStatus";
import VmDetailPanel from "./VmDetailPanel";
import VmList from "./VmList";
import VmSummary from "./VmSummary";
import { VM_FILTERS, filterVms } from "./vmFilters";
import type { VmFilter } from "./vmFilters";

interface VmScreenProps {
  vms: VmResponse[];
  loaded: boolean;
  busy: Set<string>;
  selectedVmId: string | null;
  onSelectVm: (id: string | null) => void;
  onAction: (id: string, action: VmAction) => void;
}

/** MicroVM screen (spec §8): state tabs and text filter, allocation summary, list, detail panel. */
export default function VmScreen({ vms, loaded, busy, selectedVmId, onSelectVm, onAction }: VmScreenProps) {
  const { t } = useI18n();
  const { setDetailOpen } = useScreenStatus();
  const [filter, setFilter] = useState<VmFilter>("all");
  const [query, setQuery] = useState("");
  const visible = filterVms(vms, filter, query);
  const selected = vms.find((vm) => vm.id === selectedVmId) ?? null;

  const select = useCallback((id: string) => {
    onSelectVm(id);
    setDetailOpen(true);
  }, [onSelectVm, setDetailOpen]);
  useListNavigation({ ids: visible.map((vm) => vm.id), selectedId: selectedVmId, onSelect: select, enabled: loaded });

  const toolbar = (
    <div className="vm-toolbar">
      <div className="vm-filters" role="tablist" aria-label={t("MicroVM state", "MicroVM 상태")}>
        {VM_FILTERS.map((entry) => {
          const count = vms.filter(entry.matches).length;
          return (
            <button
              key={entry.id}
              type="button"
              role="tab"
              aria-selected={filter === entry.id}
              className="vm-filter"
              onClick={() => setFilter(entry.id)}
            >
              {t(...entry.labels)}{" "}<span className="vm-filter-count">{count}</span>
            </button>
          );
        })}
      </div>
      <label className="vm-search">
        <Search aria-hidden="true" />
        <input
          type="search"
          aria-label={t("Filter MicroVMs", "MicroVM 필터")}
          placeholder={t("Name, IP, or image", "이름, IP, 이미지")}
          value={query}
          onChange={(event) => setQuery(event.target.value)}
        />
      </label>
    </div>
  );

  return (
    <Screen
      view="vms"
      live
      flush
      actions={(
        <a id="vm-list-add" className="btn primary" href={newVmHash()}>
          <Plus aria-hidden="true" />
          {t("New MicroVM", "새 MicroVM")}
        </a>
      )}
      toolbar={toolbar}
      detail={selected ? <VmDetailPanel key={selected.id} vm={selected} busy={busy.has(selected.id)} onAction={onAction} /> : null}
      detailLabel={t("MicroVM detail", "MicroVM 상세")}
      detailKind={<><Cpu aria-hidden="true" />MicroVM</>}
      onCloseDetail={() => onSelectVm(null)}
    >
      <VmSummary vms={vms} />
      {!loaded ? (
        <div className="empty-state">{t("Loading…", "불러오는 중…")}</div>
      ) : vms.length === 0 ? (
        <div className="empty-state">
          <div className="empty-state-icon"><Cpu aria-hidden="true" /></div>
          <div className="empty-state-title">{t("No MicroVMs yet", "아직 MicroVM이 없습니다")}</div>
          <a className="btn" href={newVmHash()}><Plus aria-hidden="true" />{t("New MicroVM", "새 MicroVM")}</a>
        </div>
      ) : visible.length === 0 ? (
        <div className="empty-state">{t("No MicroVMs match the filter.", "조건에 맞는 MicroVM이 없습니다.")}</div>
      ) : (
        <div className="vm-list-scroll">
          <VmList vms={visible} busy={busy} selectedId={selectedVmId} onSelect={select} onAction={onAction} />
        </div>
      )}
    </Screen>
  );
}
