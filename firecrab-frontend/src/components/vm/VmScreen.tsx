import { useCallback, useRef, useState } from "react";
import { Cpu, Plus, Search } from "lucide-react";
import type { VmResponse } from "../../bindings";
import { useI18n } from "../../i18n";
import { useListNavigation } from "../../lib/listNavigation";
import type { VmAction } from "../../model";
import { newVmHash } from "../../navigation";
import Screen from "../layout/Screen";
import { useScreenStatus } from "../layout/screenStatus";
import SshDialog from "../ssh/SshDialog";
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
  onSelectVm: (id: string | null) => boolean | void;
  onUnsavedChange: (dirty: boolean) => void;
  onAction: (id: string, action: VmAction) => void;
}

/** MicroVM screen (spec §8): state tabs and text filter, allocation summary, list, detail panel. */
export default function VmScreen({
  vms,
  loaded,
  busy,
  selectedVmId,
  onSelectVm,
  onUnsavedChange,
  onAction,
}: VmScreenProps) {
  const { t } = useI18n();
  const { setDetailOpen } = useScreenStatus();
  const [filter, setFilter] = useState<VmFilter>("all");
  const [query, setQuery] = useState("");
  const search = useRef<HTMLInputElement>(null);
  const clearFilters = () => {
    setQuery("");
    setFilter("all");
    search.current?.focus();
  };
  const visible = filterVms(vms, filter, query);
  const selected = vms.find((vm) => vm.id === selectedVmId) ?? null;
  const [sshVmId, setSshVmId] = useState<string | null>(null);
  const sshVm = vms.find((vm) => vm.id === sshVmId) ?? null;
  const closeSsh = useCallback(() => setSshVmId(null), []);

  const select = useCallback(
    (id: string) => {
      if (onSelectVm(id) === false) return;
      setDetailOpen(true);
    },
    [onSelectVm, setDetailOpen],
  );
  useListNavigation({
    ids: visible.map((vm) => vm.id),
    selectedId: selectedVmId,
    onSelect: select,
    enabled: loaded,
  });

  const toolbar = (
    <div className="vm-toolbar">
      <div
        className="vm-filters"
        role="tablist"
        aria-label={t("MicroVM state", "MicroVM 상태")}
        onKeyDown={(event) => {
          if (!["ArrowLeft", "ArrowRight", "Home", "End"].includes(event.key))
            return;
          event.preventDefault();
          event.stopPropagation();
          const current = VM_FILTERS.findIndex((entry) => entry.id === filter),
            size = VM_FILTERS.length;
          const next =
            event.key === "Home"
              ? 0
              : event.key === "End"
                ? size - 1
                : (current + (event.key === "ArrowRight" ? 1 : -1) + size) %
                  size;
          setFilter(VM_FILTERS[next].id);
          event.currentTarget
            .querySelectorAll<HTMLButtonElement>('[role="tab"]')
            [next]?.focus();
        }}
      >
        {VM_FILTERS.map((entry) => {
          const count = vms.filter(entry.matches).length;
          return (
            <button
              key={entry.id}
              type="button"
              role="tab"
              aria-selected={filter === entry.id}
              tabIndex={filter === entry.id ? 0 : -1}
              className="vm-filter"
              onClick={() => setFilter(entry.id)}
            >
              {t(...entry.labels)}{" "}
              <span className="vm-filter-count">{count}</span>
            </button>
          );
        })}
      </div>
      <label className="vm-search">
        <Search aria-hidden="true" />
        <input
          type="search"
          ref={search}
          onKeyDown={(event) => {
            if (event.key === "Escape" && query) {
              event.preventDefault();
              event.stopPropagation();
              setQuery("");
            }
          }}
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
      actions={
        <a id="vm-list-add" className="btn primary" href={newVmHash()}>
          <Plus aria-hidden="true" />
          {t("New MicroVM", "새 MicroVM")}
        </a>
      }
      toolbar={
        <>
          <VmSummary vms={vms} />
          {toolbar}
        </>
      }
      detail={
        selected ? (
          <VmDetailPanel
            key={selected.id}
            vm={selected}
            onUnsavedChange={onUnsavedChange}
            busy={busy.has(selected.id)}
            onAction={onAction}
            onOpenSsh={() => setSshVmId(selected.id)}
          />
        ) : null
      }
      detailLabel={t("MicroVM detail", "MicroVM 상세")}
      detailKind={
        <>
          <Cpu aria-hidden="true" />
          MicroVM
        </>
      }
      onCloseDetail={() => onSelectVm(null)}
    >
      {!loaded ? (
        <div className="empty-state">{t("Loading…", "불러오는 중…")}</div>
      ) : vms.length === 0 ? (
        <div id="vm-empty" className="empty-state">
          <div className="empty-state-icon">
            <Cpu aria-hidden="true" />
          </div>
          <div className="empty-state-title">
            {t("Create your first MicroVM", "첫 MicroVM을 만든다")}
          </div>
          <p>
            {t(
              "Pick an image and resources. It starts in a few seconds.",
              "이미지와 사양을 고르면 몇 초 안에 시작한다.",
            )}
          </p>
          <a className="btn" href={newVmHash()}>
            <Plus aria-hidden="true" />
            {t("New MicroVM", "새 MicroVM")}
          </a>
        </div>
      ) : visible.length === 0 ? (
        <div id="vm-empty" className="empty-state">
          <h2 className="empty-state-title">
            {t("No results", "검색 결과 없음")}
          </h2>
          <p>
            {t("No MicroVMs match the filter.", "조건에 맞는 MicroVM이 없다.")}
          </p>
          <button className="btn" onClick={clearFilters}>
            {t("Clear filters", "필터 초기화")}
          </button>
        </div>
      ) : (
        <div id="vm-scroll" className="vm-list-scroll">
          <VmList
            vms={visible}
            busy={busy}
            selectedId={selectedVmId}
            onSelect={select}
            onAction={onAction}
            onOpenSsh={(vm) => setSshVmId(vm.id)}
          />
        </div>
      )}
      {loaded && (
        <footer className="vm-list-footer">
          <span id="vm-result-count" role="status">
            {t(
              `${visible.length} of ${vms.length} MicroVMs`,
              `전체 ${vms.length}개 중 ${visible.length}개 표시`,
            )}
          </span>
        </footer>
      )}
      {sshVm && <SshDialog vm={sshVm} onClose={closeSsh} />}
    </Screen>
  );
}
