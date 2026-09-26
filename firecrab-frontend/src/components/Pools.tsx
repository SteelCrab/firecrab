import { useCallback, useEffect, useState } from "react";
import type { FormEvent } from "react";
import type {
  ImageResponse,
  MicroNetworkResponse,
  PoolLeaseResponse,
  PoolMemberState,
  PoolResponse,
} from "../bindings";
import {
  ApiClientError,
  acquirePoolLease,
  createPool,
  deletePool,
  listImages,
  listMicroNetworks,
  listPoolLeases,
  listPools,
  releasePoolLease,
  updatePool,
} from "../api/client";
import { consolePageUrl } from "../navigation";
import { useI18n } from "../i18n";

const POLL_MS = 3000;

function count(pool: PoolResponse, state: PoolMemberState): number {
  return pool.members.filter((member) => member.state === state).length;
}

function formatTime(ms: number): string {
  return new Date(ms).toLocaleString();
}

/**
 * Warm MicroVM pools (#291): booted, never-used VMs kept ready to lease. A
 * released or expired member is deleted and replaced, never handed out again.
 */
export default function Pools() {
  const { t } = useI18n();
  const [pools, setPools] = useState<PoolResponse[] | null>(null);
  const [images, setImages] = useState<ImageResponse[]>([]);
  const [networks, setNetworks] = useState<MicroNetworkResponse[]>([]);
  const [listError, setListError] = useState<string | null>(null);
  const [formError, setFormError] = useState<ApiClientError | null>(null);
  const [submitting, setSubmitting] = useState(false);
  const [busyId, setBusyId] = useState<string | null>(null);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [leases, setLeases] = useState<PoolLeaseResponse[]>([]);
  const [detailError, setDetailError] = useState<string | null>(null);

  const [name, setName] = useState("");
  const [template, setTemplate] = useState("");
  const [networkId, setNetworkId] = useState("");
  const [cpu, setCpu] = useState("1");
  const [ram, setRam] = useState("512");
  const [diskGb, setDiskGb] = useState("2");
  const [minReady, setMinReady] = useState("2");
  const [maxSize, setMaxSize] = useState("4");
  const [leaseTtl, setLeaseTtl] = useState("600");

  const [editMin, setEditMin] = useState("");
  const [editMax, setEditMax] = useState("");
  const [editTtl, setEditTtl] = useState("");

  const selected = pools?.find((pool) => pool.id === selectedId) ?? null;

  const refresh = useCallback(async () => {
    try {
      setPools(await listPools());
      setListError(null);
    } catch (error) {
      setListError((error as Error).message);
    }
    if (selectedId) {
      try {
        setLeases(await listPoolLeases(selectedId));
        setDetailError(null);
      } catch (error) {
        setDetailError((error as Error).message);
      }
    }
  }, [selectedId]);

  useEffect(() => {
    const first = setTimeout(() => void refresh(), 0);
    const interval = setInterval(() => void refresh(), POLL_MS);
    return () => {
      clearTimeout(first);
      clearInterval(interval);
    };
  }, [refresh]);

  useEffect(() => {
    listImages()
      .then((next) => {
        const installed = next.filter((image) => image.installed);
        setImages(installed);
        setTemplate((current) => current || installed[0]?.alias || "");
      })
      .catch(() => setImages([]));
    listMicroNetworks()
      .then((next) => {
        setNetworks(next);
        setNetworkId((current) => current || next[0]?.id || "");
      })
      .catch(() => setNetworks([]));
  }, []);

  const select = (pool: PoolResponse) => {
    setSelectedId(pool.id);
    setLeases([]);
    setEditMin(String(pool.minReady));
    setEditMax(String(pool.maxSize));
    setEditTtl(String(pool.leaseTtlSeconds));
  };

  const handleCreate = async (event: FormEvent) => {
    event.preventDefault();
    if (submitting) return;
    setSubmitting(true);
    setFormError(null);
    try {
      const created = await createPool({
        name: name.trim(),
        template,
        cpu: Number(cpu),
        ram: Number(ram),
        diskGb: Number(diskGb),
        egressPolicy: "internet",
        microNetworkId: networkId,
        storageRoot: null,
        minReady: Number(minReady),
        maxSize: Number(maxSize),
        leaseTtlSeconds: Number(leaseTtl),
      });
      setName("");
      await refresh();
      select(created);
    } catch (error) {
      setFormError(error as ApiClientError);
    } finally {
      setSubmitting(false);
    }
  };

  const run = async (id: string, action: () => Promise<unknown>) => {
    if (busyId) return;
    setBusyId(id);
    try {
      await action();
      await refresh();
    } catch (error) {
      setDetailError((error as Error).message);
    } finally {
      setBusyId(null);
    }
  };

  const handleDelete = (pool: PoolResponse) => {
    if (!window.confirm(t(`Delete pool "${pool.name}" and its VMs?`, `풀 "${pool.name}"과(와) VM을 삭제할까요?`))) return;
    void run(pool.id, () => deletePool(pool.id));
  };

  const handleSave = (pool: PoolResponse) =>
    run(pool.id, () =>
      updatePool(pool.id, {
        minReady: Number(editMin),
        maxSize: Number(editMax),
        leaseTtlSeconds: Number(editTtl),
      }),
    );

  const fieldError = (field: string) => (
    <span className="field-error">{formError?.fieldError(field) ?? ""}</span>
  );

  return (
    <section className="panel">
      <h2 className="panel-title">{t("Pools", "풀")}</h2>
      <p className="poll-note" style={{ marginBottom: "0.75rem" }}>
        {t(
          "Keep booted MicroVMs ready to lease. A released or expired VM is deleted and replaced, never handed out again.",
          "부팅된 MicroVM을 lease할 수 있게 준비해 둡니다. 반납되거나 만료된 VM은 삭제 후 새 VM으로 교체되며 다시 쓰이지 않습니다.",
        )}
      </p>

      <form className="create-grid" onSubmit={handleCreate}>
        <div className="field">
          <label htmlFor="pool-name">name</label>
          <input id="pool-name" placeholder="ci" value={name} onChange={(e) => setName(e.target.value)} required maxLength={40} />
          {fieldError("name")}
        </div>
        <div className="field">
          <label htmlFor="pool-image">{t("image", "이미지")}</label>
          <select id="pool-image" value={template} onChange={(e) => setTemplate(e.target.value)} required>
            {images.map((image) => (
              <option key={image.alias} value={image.alias}>
                {image.alias}
              </option>
            ))}
          </select>
          {fieldError("template")}
        </div>
        <div className="field">
          <label htmlFor="pool-network">{t("network", "네트워크")}</label>
          <select id="pool-network" value={networkId} onChange={(e) => setNetworkId(e.target.value)} required>
            {networks.map((network) => (
              <option key={network.id} value={network.id}>
                {network.name} ({network.subnetCidr})
              </option>
            ))}
          </select>
          {fieldError("microNetworkId")}
        </div>
        <div className="field">
          <label htmlFor="pool-cpu">vCPU</label>
          <input id="pool-cpu" type="number" min={1} max={32} value={cpu} onChange={(e) => setCpu(e.target.value)} />
          {fieldError("cpu")}
        </div>
        <div className="field">
          <label htmlFor="pool-ram">RAM (MiB)</label>
          <input id="pool-ram" type="number" min={128} step={128} value={ram} onChange={(e) => setRam(e.target.value)} />
          {fieldError("ram")}
        </div>
        <div className="field">
          <label htmlFor="pool-disk">{t("disk (GiB)", "디스크 (GiB)")}</label>
          <input id="pool-disk" type="number" min={1} value={diskGb} onChange={(e) => setDiskGb(e.target.value)} />
          {fieldError("diskGb")}
        </div>
        <div className="field">
          <label htmlFor="pool-min">minReady</label>
          <input id="pool-min" type="number" min={0} value={minReady} onChange={(e) => setMinReady(e.target.value)} />
          {fieldError("minReady")}
        </div>
        <div className="field">
          <label htmlFor="pool-max">maxSize</label>
          <input id="pool-max" type="number" min={1} max={32} value={maxSize} onChange={(e) => setMaxSize(e.target.value)} />
          {fieldError("maxSize")}
        </div>
        <div className="field">
          <label htmlFor="pool-ttl">{t("lease TTL (s)", "lease TTL (초)")}</label>
          <input id="pool-ttl" type="number" min={60} value={leaseTtl} onChange={(e) => setLeaseTtl(e.target.value)} />
          {fieldError("leaseTtlSeconds")}
        </div>
        <div className="field">
          <label>&nbsp;</label>
          <button className="btn primary" type="submit" disabled={submitting}>
            {submitting ? t("Creating…", "생성 중…") : t("Create pool", "풀 생성")}
          </button>
          {fieldError("storageRoot")}
        </div>
      </form>

      <h3 className="panel-title" style={{ marginTop: "1rem" }}>
        {t("Pools", "풀 목록")}
      </h3>
      {listError && <div className="field-error">{listError}</div>}
      {pools === null ? (
        <div className="empty">{t("Loading…", "불러오는 중…")}</div>
      ) : pools.length === 0 ? (
        <div className="empty">{t("No pools yet.", "아직 풀이 없습니다.")}</div>
      ) : (
        <div className="table-scroll">
          <table className="vm-table">
            <thead>
              <tr>
                <th>name</th>
                <th>{t("image", "이미지")}</th>
                <th>ready / min</th>
                <th>{t("leased", "lease 중")}</th>
                <th>{t("members / max", "멤버 / 최대")}</th>
                <th>TTL</th>
                <th></th>
              </tr>
            </thead>
            <tbody>
              {pools.map((pool) => (
                <tr
                  key={pool.id}
                  className={selectedId === pool.id ? "selected" : undefined}
                  onClick={() => select(pool)}
                  style={{ cursor: "pointer" }}
                >
                  <td>
                    {pool.name}
                    {pool.deleting && <span className="state-badge stopping"> {t("deleting", "삭제 중")}</span>}
                  </td>
                  <td className="mono">{pool.templateVersion}</td>
                  <td>
                    {count(pool, "ready")} / {pool.minReady}
                  </td>
                  <td>{count(pool, "leased")}</td>
                  <td>
                    {pool.members.length} / {pool.maxSize}
                  </td>
                  <td>{pool.leaseTtlSeconds}s</td>
                  <td>
                    <button
                      type="button"
                      className="btn"
                      disabled={busyId === pool.id || pool.deleting}
                      onClick={(event) => {
                        event.stopPropagation();
                        select(pool);
                        void run(pool.id, () => acquirePoolLease(pool.id, crypto.randomUUID()));
                      }}
                    >
                      {t("Acquire", "Lease 받기")}
                    </button>{" "}
                    <button
                      type="button"
                      className="btn"
                      disabled={busyId === pool.id || pool.deleting}
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

      {selected && (
        <div style={{ marginTop: "1rem" }}>
          <h3 className="panel-title">{selected.name}</h3>
          {detailError && <div className="field-error">{detailError}</div>}
          {selected.lastError && (
            <div className="field-error">
              {t("Last member failure: ", "최근 멤버 실패: ")}
              {selected.lastError}
            </div>
          )}
          <dl className="detail-fields mono">
            <dt>id</dt>
            <dd>{selected.id}</dd>
            <dt>{t("image", "이미지")}</dt>
            <dd>
              {selected.template} ({selected.templateVersion})
            </dd>
            <dt>spec</dt>
            <dd>
              {selected.cpu} vCPU · {selected.ram} MiB · {selected.diskGb} GiB
            </dd>
            <dt>{t("members", "멤버")}</dt>
            <dd>
              {(["provisioning", "ready", "leased", "draining"] as const)
                .map((state) => `${state} ${count(selected, state)}`)
                .join(" · ")}
            </dd>
          </dl>

          {!selected.deleting && (
            <form
              className="create-grid"
              onSubmit={(event) => {
                event.preventDefault();
                void handleSave(selected);
              }}
            >
              <div className="field">
                <label htmlFor="pool-edit-min">minReady</label>
                <input id="pool-edit-min" type="number" min={0} value={editMin} onChange={(e) => setEditMin(e.target.value)} />
              </div>
              <div className="field">
                <label htmlFor="pool-edit-max">maxSize</label>
                <input id="pool-edit-max" type="number" min={1} max={32} value={editMax} onChange={(e) => setEditMax(e.target.value)} />
              </div>
              <div className="field">
                <label htmlFor="pool-edit-ttl">{t("lease TTL (s)", "lease TTL (초)")}</label>
                <input id="pool-edit-ttl" type="number" min={60} value={editTtl} onChange={(e) => setEditTtl(e.target.value)} />
              </div>
              <div className="field">
                <label>&nbsp;</label>
                <button className="btn" type="submit" disabled={busyId === selected.id}>
                  {t("Save", "저장")}
                </button>
              </div>
            </form>
          )}

          <h3 className="panel-title" style={{ marginTop: "1rem" }}>
            {t("Leases", "Lease")}
          </h3>
          {leases.length === 0 ? (
            <div className="empty">{t("No leases.", "Lease가 없습니다.")}</div>
          ) : (
            <div className="table-scroll">
              <table className="vm-table">
                <thead>
                  <tr>
                    <th>{t("state", "상태")}</th>
                    <th>VM</th>
                    <th>ipv4</th>
                    <th>{t("expires", "만료")}</th>
                    <th></th>
                  </tr>
                </thead>
                <tbody>
                  {leases.map((lease) => (
                    <tr key={lease.id}>
                      <td>
                        <span className={`state-badge ${lease.state === "active" ? "running" : "stopped"}`}>{lease.state}</span>
                      </td>
                      <td className="mono">{lease.vm?.name ?? lease.vmId}</td>
                      <td className="mono">{lease.vm?.ipv4 ?? "—"}</td>
                      <td>{formatTime(lease.expiresAtMs)}</td>
                      <td>
                        {lease.state === "active" && (
                          <>
                            <a className="btn" href={consolePageUrl(lease.vmId)} target="_blank" rel="noreferrer">
                              {t("Terminal", "터미널")}
                            </a>{" "}
                            <button
                              type="button"
                              className="btn"
                              disabled={busyId === lease.id}
                              onClick={() => void run(lease.id, () => releasePoolLease(selected.id, lease.id))}
                            >
                              {t("Release", "반납")}
                            </button>
                          </>
                        )}
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}
        </div>
      )}
    </section>
  );
}
