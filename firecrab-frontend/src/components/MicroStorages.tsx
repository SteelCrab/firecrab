import { useEffect, useState, type FormEvent } from "react";
import { HardDrive, Trash2 } from "lucide-react";
import type {
  MicroStorageDetailResponse,
  MicroStorageResponse,
  StorageDeviceResponse,
} from "../bindings";
import {
  ApiClientError,
  createMicroStorage,
  deleteMicroStorage,
  getMicroStorage,
  listMicroStorages,
  listStorageDevices,
} from "../api/client";
import { useI18n } from "../i18n";
import Screen from "./layout/Screen";
import { useScreenStatus } from "./layout/screenStatus";
import { DetailHead, DetailProp, DetailSection } from "./ui/DetailSection";
import StatusBadge from "./ui/StatusBadge";

/** Named host folders hold VM system disks and MicroBlockStorage disk files. */
export default function MicroStorages({
  onUnsavedChange,
}: {
  onUnsavedChange?: (dirty: boolean) => void;
}) {
  const { t } = useI18n(),
    { setDetailOpen } = useScreenStatus();
  const [pools, setPools] = useState<MicroStorageResponse[] | null>(null),
    [devices, setDevices] = useState<StorageDeviceResponse[]>([]);
  const [selected, setSelected] = useState<string | null>(null),
    [detail, setDetail] = useState<MicroStorageDetailResponse | null>(null),
    [detailError, setDetailError] = useState("");
  const [creating, setCreating] = useState(false),
    [name, setName] = useState(""),
    [path, setPath] = useState("");
  const [saving, setSaving] = useState(false),
    [busyId, setBusyId] = useState<string | null>(null),
    [error, setError] = useState(""),
    [formError, setFormError] = useState<ApiClientError | null>(null);
  const dirty = creating && !!(name || path);
  useEffect(() => {
    onUnsavedChange?.(dirty);
    return () => onUnsavedChange?.(false);
  }, [dirty, onUnsavedChange]);
  const refresh = async () => {
    try {
      setPools(await listMicroStorages());
      setError("");
    } catch (cause) {
      setError(String(cause));
    }
  };
  useEffect(() => {
    let active = true;
    listMicroStorages()
      .then((rows) => {
        if (active) setPools(rows);
      })
      .catch((cause) => {
        if (active) setError(String(cause));
      });
    listStorageDevices()
      .then((rows) => {
        if (active) setDevices(rows);
      })
      .catch(() => {});
    return () => {
      active = false;
    };
  }, []);
  useEffect(() => {
    setDetail(null);
    setDetailError("");
    if (!selected) return;
    let active = true;
    getMicroStorage(selected)
      .then((value) => {
        if (active) setDetail(value);
      })
      .catch((cause) => {
        if (active) setDetailError(String(cause));
      });
    return () => {
      active = false;
    };
  }, [selected]);
  const close = () => {
    if (saving) return false;
    if (
      dirty &&
      !window.confirm(
        t("Discard unsaved changes?", "저장하지 않은 변경을 버릴까요?"),
      )
    )
      return false;
    setCreating(false);
    setName("");
    setPath("");
    setFormError(null);
    setSelected(null);
    return true;
  };
  const save = async (event: FormEvent) => {
    event.preventDefault();
    if (saving) return;
    setSaving(true);
    setFormError(null);
    try {
      await createMicroStorage({ name: name.trim(), path: path.trim() });
      setCreating(false);
      setName("");
      setPath("");
      onUnsavedChange?.(false);
      await refresh();
    } catch (cause) {
      setFormError(
        cause instanceof ApiClientError
          ? cause
          : ApiClientError.transport(String(cause)),
      );
    } finally {
      setSaving(false);
    }
  };
  const remove = async (pool: MicroStorageResponse) => {
    if (
      busyId ||
      !window.confirm(
        t(
          `Unregister storage location "${pool.name}"?`,
          `저장 위치 "${pool.name}" 등록을 해제할까요?`,
        ),
      )
    )
      return;
    setBusyId(pool.id);
    try {
      await deleteMicroStorage(pool.id);
      if (selected === pool.id) setSelected(null);
      await refresh();
    } catch (cause) {
      setError(String(cause));
    } finally {
      setBusyId(null);
    }
  };
  const pool = pools?.find((row) => row.id === selected);
  const capacity = (free: number, total: number) =>
    total > 0
      ? `${free} / ${total} GiB`
      : t("Capacity unavailable", "용량 확인 불가");
  const add = (
    <button
      className="btn primary"
      onClick={() => {
        if (creating) return;
        setSelected(null);
        setCreating(true);
        setDetailOpen(true);
      }}
    >
      {t("Register storage location", "저장 위치 등록")}
    </button>
  );
  const editor = (
    <section
      className="storage-editor"
      aria-label={t("Register storage location", "저장 위치 등록")}
      data-editing
    >
      <DetailHead title={t("Register storage location", "저장 위치 등록")} />
      <p>
        {t(
          "Enter an absolute folder on the Firecrab host. On macOS and Windows, use the path inside the management Linux VM.",
          "macOS·Windows에서는 관리용 Linux VM 안의 경로를 입력한다.",
        )}
      </p>
      <p>
        {t(
          "Missing folders are created when registered. This does not partition or format a disk.",
          "폴더가 없으면 등록할 때 만든다. 디스크를 파티션으로 나누거나 포맷하지 않는다.",
        )}
      </p>
      <form id="editor" className="storage-location-form" onSubmit={save}>
        <div className="field">
          <label htmlFor="storage-name">{t("Name", "이름")}</label>
          <input
            id="storage-name"
            value={name}
            required
            maxLength={64}
            disabled={saving}
            onChange={(event) => setName(event.target.value)}
          />
          {formError?.fieldError("name") && (
            <p className="field-error">{formError.fieldError("name")}</p>
          )}
        </div>
        <div className="field">
          <label htmlFor="storage-path">
            {t("Host folder path", "호스트 폴더 경로")}
          </label>
          <input
            id="storage-path"
            value={path}
            placeholder="/mnt/ssd/firecrab"
            required
            pattern="/.*"
            disabled={saving}
            onChange={(event) => setPath(event.target.value)}
          />
          {formError?.fieldError("path") && (
            <p className="field-error">{formError.fieldError("path")}</p>
          )}
        </div>
        {!!devices.length && (
          <details>
            <summary>
              {t("Choose a mounted disk", "마운트된 디스크에서 선택")}
            </summary>
            <div className="storage-device-list">
              {devices.map((device) => (
                <button
                  className="btn"
                  type="button"
                  key={device.mountpoint}
                  disabled={saving}
                  onClick={() => {
                    setPath(device.mountpoint);
                    if (!name)
                      setName(
                        (device.device || device.mountpoint)
                          .replace(/^\//, "")
                          .replaceAll("/", "-")
                          .slice(0, 64),
                      );
                  }}
                >
                  <span className="mono">{device.mountpoint}</span> ·{" "}
                  {device.fstype} ·{" "}
                  {capacity(device.availableGib, device.sizeGib)}
                </button>
              ))}
            </div>
          </details>
        )}
        {formError && (
          <p role="alert" className="field-error">
            {formError.message}
          </p>
        )}
        <div className="form-footer">
          <button
            type="button"
            className="btn ghost"
            disabled={saving}
            onClick={close}
          >
            {t("Cancel", "취소")}
          </button>
          <button className="btn primary" disabled={saving}>
            {saving ? t("Registering…", "등록 중…") : t("Register", "등록")}
          </button>
        </div>
      </form>
    </section>
  );
  const inspected = pool ? (
    <>
      <DetailHead
        title={pool.name}
        actions={
          <button
            className="btn danger"
            disabled={busyId === pool.id}
            onClick={() => void remove(pool)}
          >
            <Trash2 aria-hidden="true" />
            {t("Unregister", "등록 해제")}
          </button>
        }
      />
      <p>
        {t(
          "Stores MicroVM operating-system disks and MicroBlockStorage data disk files.",
          "MicroVM 운영체제 디스크와 MicroBlockStorage 데이터 디스크 파일을 저장한다.",
        )}
      </p>
      {detailError && (
        <p role="alert" className="field-error">
          {detailError}
        </p>
      )}
      <DetailSection title={t("Storage location", "저장 위치")}>
        <dl className="props">
          <DetailProp
            label={t("Host folder path", "호스트 폴더 경로")}
            copy={pool.path}
            wrap
          >
            <code>{pool.path}</code>
          </DetailProp>
          <DetailProp label={t("Free space", "남은 공간")}>
            {(detail ?? pool).totalGib > 0
              ? `${(detail ?? pool).availableGib} GiB`
              : t("Capacity unavailable", "용량 확인 불가")}
          </DetailProp>
          <DetailProp label={t("Filesystem capacity", "파일시스템 전체 용량")}>
            {(detail ?? pool).totalGib > 0
              ? `${(detail ?? pool).totalGib} GiB`
              : t("Capacity unavailable", "용량 확인 불가")}
          </DetailProp>
        </dl>
      </DetailSection>
      <DetailSection
        title={t("MicroVMs using this location", "이 위치를 사용하는 MicroVM")}
      >
        {detail ? (
          detail.vms.length ? (
            <ul className="detail-list">
              {detail.vms.map((vm) => (
                <li key={vm.id}>
                  <a href={`#/vms?vm=${encodeURIComponent(vm.id)}`}>
                    {vm.name}
                  </a>
                  <span>{vm.diskGb} GiB</span>
                  <StatusBadge state={vm.state} />
                </li>
              ))}
            </ul>
          ) : (
            <p>
              {t(
                "No MicroVMs use this location yet.",
                "이 위치를 사용하는 MicroVM이 아직 없다.",
              )}
            </p>
          )
        ) : (
          !detailError && <p>{t("Loading…", "불러오는 중…")}</p>
        )}
      </DetailSection>
      <details>
        <summary>{t("Technical details", "기술 정보")}</summary>
        <dl className="props">
          <DetailProp label="ID" copy={pool.id} wrap>
            <code>{pool.id}</code>
          </DetailProp>
        </dl>
      </details>
    </>
  ) : null;
  return (
    <Screen
      view="storages"
      actions={add}
      detail={creating ? editor : inspected}
      detailLabel={t("MicroStorage detail", "MicroStorage 상세")}
      detailKind={
        <>
          <HardDrive aria-hidden="true" />
          {t("VM disk storage", "VM 디스크 저장소")}
        </>
      }
      onCloseDetail={close}
    >
      <section
        className="storage-explanation"
        aria-label={t(
          "Host folders for VM disk files",
          "VM 디스크 파일을 저장하는 호스트 폴더",
        )}
      >
        <h2>
          {t(
            "Host folders for VM disk files",
            "VM 디스크 파일을 저장하는 호스트 폴더",
          )}
        </h2>
        <p>
          {t(
            "Choose where Firecrab stores MicroVM operating-system disks and MicroBlockStorage data disk files. Select this location when creating a VM or data disk.",
            "MicroVM의 운영체제 디스크와 MicroBlockStorage의 데이터 디스크 파일을 저장할 위치를 관리한다. VM이나 데이터 디스크를 만들 때 이 저장 위치를 선택한다.",
          )}
        </p>
        <a href="#/volumes">
          {t(
            "Create a data disk to attach inside a VM → MicroBlockStorage",
            "VM 안에 연결할 데이터 디스크 만들기 → MicroBlockStorage",
          )}
        </a>
      </section>
      {error && (
        <div role="alert" className="field-error">
          {error}
          <button className="btn" onClick={() => void refresh()}>
            {t("Retry", "다시 시도")}
          </button>
        </div>
      )}
      {pools === null ? (
        !error && <p>{t("Loading…", "불러오는 중…")}</p>
      ) : pools.length ? (
        <div className="table-scroll">
          <table className="vm-table">
            <thead>
              <tr>
                <th>{t("Name", "이름")}</th>
                <th>{t("Host folder path", "호스트 폴더 경로")}</th>
                <th>{t("Free / total", "남은 공간 / 전체")}</th>
              </tr>
            </thead>
            <tbody>
              {pools.map((row) => (
                <tr
                  key={row.id}
                  aria-selected={selected === row.id}
                  onClick={() => {
                    if (creating && !close()) return;
                    setSelected(row.id);
                    setDetailOpen(true);
                  }}
                >
                  <td>{row.name}</td>
                  <td>
                    <code>{row.path}</code>
                  </td>
                  <td>{capacity(row.availableGib, row.totalGib)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      ) : (
        <div className="empty-state">
          <HardDrive aria-hidden="true" />
          <h3>
            {t("No storage locations registered", "등록한 저장 위치가 없다")}
          </h3>
          <p>
            {t(
              "Register a host folder to select it for VM and data disks.",
              "호스트 폴더를 등록하면 VM과 데이터 디스크의 저장 위치로 선택할 수 있다.",
            )}
          </p>
        </div>
      )}
    </Screen>
  );
}
