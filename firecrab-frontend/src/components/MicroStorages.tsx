import { useEffect, useState } from "react";
import type { FormEvent } from "react";
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
import { HardDrive } from "lucide-react";
import { useI18n } from "../i18n";
import Screen from "./layout/Screen";
import { useScreenStatus } from "./layout/screenStatus";
import { DetailHead, DetailProp, DetailSection } from "./ui/DetailSection";
import StatusBadge from "./ui/StatusBadge";

/**
 * MicroStorage management — register host mount paths as named storage pools
 * and see which VMs use each. Creating a partition is *not* in scope; pick an
 * already-mounted path (or type one). See public-docs/guides/storage.md.
 */
export default function MicroStorages() {
  const { t } = useI18n();
  const { setDetailOpen } = useScreenStatus();
  const [pools, setPools] = useState<MicroStorageResponse[] | null>(null);
  const [devices, setDevices] = useState<StorageDeviceResponse[]>([]);
  const [name, setName] = useState("");
  const [path, setPath] = useState("");
  const [submitting, setSubmitting] = useState(false);
  const [fieldErrors, setFieldErrors] = useState<ApiClientError | null>(null);
  const [listError, setListError] = useState<string | null>(null);
  const [busyId, setBusyId] = useState<string | null>(null);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [detail, setDetail] = useState<MicroStorageDetailResponse | null>(null);
  const [detailError, setDetailError] = useState<string | null>(null);

  useEffect(() => {
    if (!selectedId) {
      setDetail(null);
      return;
    }
    setDetail(null);
    setDetailError(null);
    getMicroStorage(selectedId)
      .then(setDetail)
      .catch((error) => setDetailError((error as Error).message));
  }, [selectedId]);

  const refresh = async () => {
    try {
      setPools(await listMicroStorages());
      setListError(null);
    } catch (error) {
      setListError((error as Error).message);
    }
  };

  useEffect(() => {
    refresh();
    listStorageDevices()
      .then(setDevices)
      .catch(() => setDevices([]));
  }, []);

  const handleSubmit = async (event: FormEvent) => {
    event.preventDefault();
    if (submitting) return;
    setSubmitting(true);
    setFieldErrors(null);
    try {
      await createMicroStorage({ name: name.trim(), path: path.trim() });
      setName("");
      setPath("");
      await refresh();
    } catch (error) {
      setFieldErrors(error as ApiClientError);
    } finally {
      setSubmitting(false);
    }
  };

  const handleDelete = async (pool: MicroStorageResponse) => {
    if (busyId || !window.confirm(t(`Delete MicroStorage "${pool.name}"?`, `MicroStorage "${pool.name}"을(를) 삭제할까요?`))) return;
    setBusyId(pool.id);
    try {
      await deleteMicroStorage(pool.id);
      if (selectedId === pool.id) setSelectedId(null);
      await refresh();
    } catch (error) {
      setListError((error as Error).message);
    } finally {
      setBusyId(null);
    }
  };

  const pickDevice = (device: StorageDeviceResponse) => {
    setPath(device.mountpoint);
    if (!name.trim()) {
      const base =
        device.device ||
        device.mountpoint.replace(/^\//, "").replace(/\//g, "-") ||
        "pool";
      setName(base.slice(0, 64));
    }
  };

  const fieldError = (field: string) => (
    <span className="field-error">{fieldErrors?.fieldError(field) ?? ""}</span>
  );

  const selectedPool = pools?.find((pool) => pool.id === selectedId) ?? null;
  const select = (id: string) => {
    setSelectedId(id);
    setDetailOpen(true);
  };

  return (
    <Screen
      view="storages"
      detail={selectedPool ? <MicroStorageDetail pool={selectedPool} detail={detail} error={detailError} /> : null}
      detailLabel={t("MicroStorage detail", "MicroStorage 상세")}
      detailKind={<><HardDrive aria-hidden="true" />MicroStorage</>}
      onCloseDetail={() => setSelectedId(null)}
    >
    <section className="panel">
      <h2 className="panel-title">MicroStorage</h2>
      <p className="poll-note" style={{ marginBottom: "0.75rem" }}>
        {t(
          "Register a path already mounted on the host. Partitioning and formatting stay in the OS; mount it there, then name it here.",
          "호스트에 이미 마운트된 경로를 등록합니다. 파티션 생성·포맷은 하지 않습니다 — OS에서 마운트한 뒤 여기서 이름만 붙이면 됩니다.",
        )}
      </p>

      <form className="create-grid" onSubmit={handleSubmit}>
        <div className="field">
          <label htmlFor="ms-name">name</label>
          <input
            id="ms-name"
            placeholder="nvme1"
            value={name}
            onChange={(event) => setName(event.target.value)}
            required
            minLength={1}
            maxLength={64}
          />
          {fieldError("name")}
        </div>
        <div className="field">
          <label htmlFor="ms-path">path (absolute)</label>
          <input
            id="ms-path"
            placeholder="/mnt/disk2"
            value={path}
            onChange={(event) => setPath(event.target.value)}
            required
          />
          {fieldError("path")}
        </div>
        <div className="field">
          <label>&nbsp;</label>
          <button className="btn primary" type="submit" disabled={submitting}>
            {submitting ? t("Registering…", "등록 중…") : t("Register", "등록")}
          </button>
          <span className="field-error"></span>
        </div>
      </form>

      {devices.length > 0 && (
        <>
          <h3 className="panel-title" style={{ marginTop: "1rem" }}>
            {t("Mounted partitions (select to fill path)", "마운트된 파티션 (선택하면 path 채움)")}
          </h3>
          <div className="table-scroll">
            <table className="vm-table">
              <thead>
                <tr>
                  <th>device</th>
                  <th>mount</th>
                  <th>fs</th>
                  <th>free</th>
                  <th></th>
                </tr>
              </thead>
              <tbody>
                {devices.map((device) => (
                  <tr key={device.mountpoint}>
                    <td className="mono">{device.device || "—"}</td>
                    <td className="mono">{device.mountpoint}</td>
                    <td>{device.fstype}</td>
                    <td>
                      {device.availableGib} / {device.sizeGib} GiB
                    </td>
                    <td>
                      <button
                        type="button"
                        className="btn"
                        onClick={() => pickDevice(device)}
                      >
                        {t("Select", "선택")}
                      </button>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </>
      )}

      <h3 className="panel-title" style={{ marginTop: "1rem" }}>
        {t("Registered MicroStorage", "등록된 MicroStorage")}
      </h3>
      {listError && <div className="field-error">{listError}</div>}
      {pools === null ? (
        <div className="empty">{t("Loading…", "불러오는 중…")}</div>
      ) : pools.length === 0 ? (
        <div className="empty">{t("No MicroStorage registered.", "등록된 MicroStorage가 없습니다.")}</div>
      ) : (
        <div className="table-scroll">
          <table className="vm-table">
            <thead>
              <tr>
                <th>name</th>
                <th>path</th>
                <th>free</th>
                <th></th>
              </tr>
            </thead>
            <tbody>
              {pools.map((pool) => (
                <tr
                  key={pool.id}
                  className={selectedId === pool.id ? "selected" : undefined}
                  aria-selected={selectedId === pool.id}
                  onClick={() => select(pool.id)}
                  style={{ cursor: "pointer" }}
                >
                  <td>{pool.name}</td>
                  <td className="mono">{pool.path}</td>
                  <td>
                    {pool.availableGib} / {pool.totalGib} GiB
                  </td>
                  <td>
                    <button
                      type="button"
                      className="btn"
                      disabled={busyId === pool.id}
                      onClick={(event) => {
                        event.stopPropagation();
                        handleDelete(pool);
                      }}
                    >
                      {t("Delete", "삭제")}
                    </button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}

    </section>
    </Screen>
  );
}

function MicroStorageDetail({ pool, detail, error }: { pool: MicroStorageResponse; detail: MicroStorageDetailResponse | null; error: string | null }) {
  const { t } = useI18n();
  return (
    <>
      <DetailHead title={pool.name} meta={<span className="mono">{pool.path}</span>} />
      <div className="detail-body">
        {error && <p className="field-error">{error}</p>}
        <DetailSection title={t("Properties", "속성")}>
          <dl className="props">
            <DetailProp label="ID" copy={pool.id}><span className="mono">{pool.id}</span></DetailProp>
            <DetailProp label={t("Path", "경로")}><span className="mono">{pool.path}</span></DetailProp>
            <DetailProp label={t("Free", "여유 공간")}>{(detail ?? pool).availableGib} / {(detail ?? pool).totalGib} GiB</DetailProp>
          </dl>
        </DetailSection>
        <DetailSection title="VMs">
          {!detail ? (
            !error && <p className="detail-note">{t("Loading…", "불러오는 중…")}</p>
          ) : detail.vms.length === 0 ? (
            <p className="detail-note">{t("None", "없음")}</p>
          ) : (
            <ul className="detail-list">
              {detail.vms.map((vm) => (
                <li key={vm.id}>
                  <span className="detail-list-main">{vm.name}</span>
                  <span className="detail-list-sub">{vm.diskGb} GiB</span>
                  <StatusBadge state={vm.state} />
                </li>
              ))}
            </ul>
          )}
        </DetailSection>
      </div>
    </>
  );
}
