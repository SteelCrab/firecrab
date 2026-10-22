import { useCallback, useEffect, useRef, useState } from "react";
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
  createPool,
  deletePool,
  listImages,
  listMicroNetworks,
  listPoolLeases,
  listPools,
  releasePoolLease,
  updatePool,
} from "../api/client";
import { ArrowRight, Plus, SquareTerminal } from "lucide-react";
import "../styles/pools.css";
import { consolePageUrl } from "../navigation";
import { useI18n } from "../i18n";
import { acquireWithRetryKey } from "../lib/poolAcquire";

const POLL_MS = 3000;

function count(pool: PoolResponse, state: PoolMemberState): number {
  return pool.members.filter((member) => member.state === state).length;
}

/**
 * Warm MicroVM pools (#291): booted, never-used VMs kept ready to lease. A
 * released or expired member is deleted and replaced, never handed out again.
 */
export default function Pools() {
  const { locale, t } = useI18n();
  const [createOpen, setCreateOpen] = useState(false);
  const [configLoading, setConfigLoading] = useState(true);
  const [configError, setConfigError] = useState<string | null>(null);
  const advanced = useRef<HTMLDetailsElement>(null);
  const selection = useRef<string | null>(null);
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
  const [actionError, setActionError] = useState<string | null>(null);

  const [name, setName] = useState("");
  const [template, setTemplate] = useState("");
  const [networkId, setNetworkId] = useState("");
  const [cpu, setCpu] = useState("1");
  const [ram, setRam] = useState("512");
  const [diskGb, setDiskGb] = useState("2");
  const [minReady, setMinReady] = useState("2");
  const [maxSize, setMaxSize] = useState("4");
  const [leaseTtl, setLeaseTtl] = useState("10");

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
    const poolId = selection.current;
    if (poolId) {
      try {
        const next = await listPoolLeases(poolId);
        if (selection.current === poolId) { setLeases(next); setDetailError(null); }
      } catch (error) {
        if (selection.current === poolId) setDetailError((error as Error).message);
      }
    }
  }, []);

  useEffect(() => {
    const first = setTimeout(() => void refresh(), 0);
    const interval = setInterval(() => void refresh(), POLL_MS);
    return () => {
      clearTimeout(first);
      clearInterval(interval);
    };
  }, [refresh, selectedId]);

  useEffect(() => {
    let current = true;
    void Promise.all([listImages(), listMicroNetworks()]).then(([nextImages, nextNetworks]) => {
      if (!current) return;
      const installed = nextImages.filter((image) => image.installed && image.alias !== "__microboot");
      setImages(installed);
      setTemplate(installed[0]?.alias ?? "");
      setDiskGb(String(Math.max(2, installed[0]?.minDiskGb ?? 1)));
      setNetworks(nextNetworks);
      setNetworkId(nextNetworks.find((network) => network.id === "default")?.id ?? nextNetworks[0]?.id ?? "");
    }).catch((error: Error) => {
      if (current) setConfigError(error.message);
    }).finally(() => { if (current) setConfigLoading(false); });
    return () => { current = false; };
  }, []);

  const select = (pool: PoolResponse) => {
    selection.current = pool.id;
    setSelectedId(pool.id);
    if (selectedId !== pool.id) { setLeases([]); setDetailError(null); }
    setEditMin(String(pool.minReady));
    setEditMax(String(pool.maxSize));
    setEditTtl(String(pool.leaseTtlSeconds / 60));
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
        leaseTtlSeconds: Math.round(Number(leaseTtl) * 60),
      });
      setName("");
      setCreateOpen(false);
      await refresh();
      select(created);
    } catch (error) {
      setFormError(error as ApiClientError);
      if (advanced.current) advanced.current.open = true;
    } finally {
      setSubmitting(false);
    }
  };

  const run = async (id: string, action: () => Promise<unknown>) => {
    if (busyId) return;
    setBusyId(id);
    setActionError(null);
    try {
      await action();
      await refresh();
    } catch (error) {
      setActionError((error as Error).message);
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
        leaseTtlSeconds: Math.round(Number(editTtl) * 60),
      }),
    );

  const fieldError = (field: string) => {
    const message = formError?.fieldError(field);
    return message ? <span className="field-error">{message}</span> : null;
  };
  const numericField = (id: string, label: string, value: string, change: (value: string) => void, min: number, max: number, field?: string, step: number | "any" = 1) => (
    <div className="field">
      <label htmlFor={id}>{label}</label>
      <input id={id} type="number" min={min} max={max} step={step} value={value} onChange={(event) => change(event.target.value)} required />
      {field && fieldError(field)}
    </div>
  );
  const active = leases.filter((lease) => lease.state === "active");
  const history = leases.filter((lease) => lease.state !== "active");
  const showCreate = createOpen || pools?.length === 0;
  const minimumDisk = Math.max(1, images.find((image) => image.alias === template)?.minDiskGb ?? 1);
  const formatTime = (ms: number) => new Date(ms).toLocaleString(locale === "ko" ? "ko-KR" : "en-US");

  return (
    <div className="pools-screen">
      <div className="pool-heading">
        <div>
          <h2>{t("Fresh VMs, ready to use", "필요할 때 바로 쓰는 VM")}</h2>
          <p>{t("Choose an image once. Startup, cleanup, and replacement happen automatically.", "이미지를 한 번 선택하면 준비, 정리, 교체는 자동으로 처리합니다.")}</p>
        </div>
        {!showCreate && <button className="btn" onClick={() => setCreateOpen(true)}><Plus aria-hidden="true" />{t("New pool", "새 풀")}</button>}
      </div>

      {showCreate && (
        <section className="panel pool-create" aria-labelledby="pool-create-title">
          <h3 id="pool-create-title" className="panel-title">{t("Create a pool", "풀 만들기")}</h3>
          <form onSubmit={handleCreate}>
            <div className="pool-form-grid">
              <div className="field">
                <label htmlFor="pool-name">{t("Name", "이름")}</label>
                <input id="pool-name" placeholder="build-runners" value={name} onChange={(e) => setName(e.target.value)} required maxLength={40} />
                {fieldError("name")}
              </div>
              <div className="field">
                <label htmlFor="pool-image">{t("Image", "이미지")}</label>
                <select id="pool-image" value={template} onChange={(e) => {
                  setTemplate(e.target.value);
                  setDiskGb(String(Math.max(Number(diskGb), images.find((image) => image.alias === e.target.value)?.minDiskGb ?? 1)));
                }} required disabled={configLoading || images.length === 0}>
                  {images.map((image) => <option key={image.alias} value={image.alias}>{image.alias}</option>)}
                </select>
                {fieldError("template")}
              </div>
            </div>
            {configLoading ? <p className="pool-note">{t("Loading available images…", "사용할 이미지를 불러오는 중…")}</p>
              : configError ? <p className="field-error" role="alert">{configError} <button className="btn sm" type="button" onClick={() => window.location.reload()}>{t("Reload", "다시 불러오기")}</button></p>
              : images.length === 0 ? <p className="pool-note">{t("Install an image to get started.", "시작하려면 이미지를 설치하세요.")} <a href="#/images">{t("Open Images", "이미지 열기")}</a></p>
              : networks.length === 0 ? <p className="pool-note">{t("Create a network to get started.", "시작하려면 네트워크를 만드세요.")} <a href="#/networks">{t("Open Networks", "네트워크 열기")}</a></p>
              : null}
            <details ref={advanced} className="pool-disclosure" onInvalidCapture={() => { if (advanced.current) advanced.current.open = true; }}>
              <summary>{t("Advanced settings", "고급 설정")}</summary>
              <div className="pool-form-grid pool-advanced-grid">
                {numericField("pool-min", t("Keep ready", "미리 준비할 VM 수"), minReady, setMinReady, 0, Number(maxSize), "minReady")}
                {numericField("pool-max", t("Maximum VMs", "최대 VM 수"), maxSize, setMaxSize, Math.max(1, Number(minReady)), 32, "maxSize")}
                {numericField("pool-ttl", t("Session duration (minutes)", "사용 시간 (분)"), leaseTtl, setLeaseTtl, 1, 10080, "leaseTtlSeconds", "any")}
                {numericField("pool-cpu", "vCPU", cpu, setCpu, 1, 32, "cpu")}
                <div className="field">
                  <label htmlFor="pool-ram">{t("Memory", "메모리")}</label>
                  <select id="pool-ram" value={ram} onChange={(event) => setRam(event.target.value)}>
                    {[128, 256, 512, 1024, 2048, 4096, 8192, 16384, 32768].map((size) => <option key={size} value={size}>{size < 1024 ? `${size} MiB` : `${size / 1024} GiB`}</option>)}
                  </select>
                  {fieldError("ram")}
                </div>
                {numericField("pool-disk", t("Disk (GiB)", "디스크 (GiB)"), diskGb, setDiskGb, minimumDisk, 500, "diskGb")}
                <div className="field">
                  <label htmlFor="pool-network">{t("Network", "네트워크")}</label>
                  <select id="pool-network" value={networkId} onChange={(e) => setNetworkId(e.target.value)} required disabled={configLoading || networks.length === 0}>
                    {networks.map((network) => <option key={network.id} value={network.id}>{network.name}</option>)}
                  </select>
                  {fieldError("microNetworkId")}
                </div>
              </div>
            </details>
            <p className="pool-note">{t(`${minReady} kept ready · Up to ${maxSize} VMs · ${leaseTtl} minutes per use`, `${minReady}대 미리 준비 · 최대 ${maxSize}대 · 한 번에 ${leaseTtl}분 사용`)}</p>
            {formError && <p className="field-error" role="alert">{formError.message}</p>}
            <div className="pool-form-actions">
              <button className="btn primary" type="submit" disabled={submitting || configLoading || !!configError || !template || !networkId}>
                {submitting ? t("Creating…", "만드는 중…") : t("Create pool", "풀 만들기")}
              </button>
              {!!pools?.length && <button className="btn ghost" type="button" disabled={submitting} onClick={() => setCreateOpen(false)}>{t("Cancel", "취소")}</button>}
            </div>
          </form>
        </section>
      )}

      {listError && <div className="field-error" role="alert">{listError}</div>}
      {actionError && <div className="field-error" role="alert">{actionError}</div>}
      {pools === null ? <p className="pool-note">{t("Loading pools…", "풀을 불러오는 중…")}</p> : pools.length > 0 && (
        <section className="pool-list" aria-label={t("Pools", "풀 목록")}>
          {pools.map((pool) => (
            <article key={pool.id} className={`pool-row${selectedId === pool.id ? " selected" : ""}`}>
              <div className="pool-row-name">
                <button className="pool-select" disabled={!!busyId} aria-expanded={selectedId === pool.id} onClick={() => select(pool)}>{pool.name}</button>
                <span>{pool.template}</span>
              </div>
              <div className="pool-row-status">
                <span className={`badge ${pool.deleting ? "stopping" : count(pool, "ready") > 0 ? "running" : pool.lastError ? "error" : count(pool, "provisioning") > 0 ? "starting" : ""}`}>
                  {pool.deleting ? t("Deleting", "삭제 중") : count(pool, "ready") > 0 ? t(`${count(pool, "ready")} ready`, `${count(pool, "ready")}대 준비됨`) : pool.lastError ? t("Needs attention", "확인 필요") : count(pool, "provisioning") > 0 ? t("Preparing", "준비 중") : t("No VMs ready", "준비된 VM 없음")}
                </span>
                <span>{t(`${count(pool, "leased")} in use`, `${count(pool, "leased")}대 사용 중`)}</span>
              </div>
              <button className="btn primary" disabled={!!busyId || pool.deleting} onClick={() => {
                select(pool);
                void run(pool.id, async () => {
                  const lease = await acquireWithRetryKey(pool.id);
                  setLeases((current) => [lease, ...current.filter((item) => item.id !== lease.id)]);
                });
              }}>
                {busyId === pool.id ? t("Working…", "처리 중…") : t("Use VM", "VM 사용")}<ArrowRight aria-hidden="true" />
              </button>
            </article>
          ))}
        </section>
      )}

      {selected && (
        <section className="panel pool-detail" aria-label={t(`${selected.name} details`, `${selected.name} 상세`)}>
          <h3 className="panel-title">{selected.name}<span className="pool-note">{t("Automatically managed", "자동 관리")}</span></h3>
          <p className="pool-note">{t("End a session when you are done. Its VM is deleted and a fresh one is prepared automatically.", "사용을 종료하면 VM을 삭제하고 새 VM을 자동으로 준비합니다.")}</p>
          {selected.lastError && <p className="field-error" role="alert">{selected.lastError}</p>}
          {detailError && <p className="field-error" role="alert">{detailError}</p>}
          <h4 className="pool-section-title">{t("In use", "사용 중")}</h4>
          {active.length === 0 ? <p className="pool-note">{t("Nothing in use. Choose Use VM to get started.", "사용 중인 VM이 없습니다. ‘VM 사용’을 누르면 시작합니다.")}</p> : active.map((lease) => (
            <div className="pool-session" key={lease.id}>
              <div className="pool-session-info">
                <strong className="mono">{lease.vm?.ipv4 ?? lease.vm?.name ?? lease.vmId}</strong>
                <span>{t("Ends ", "자동 종료: ")}{formatTime(lease.expiresAtMs)}</span>
              </div>
              <div className="pool-session-actions">
                <a className="btn" href={consolePageUrl(lease.vmId)} target="_blank" rel="noreferrer"><SquareTerminal aria-hidden="true" />{t("Open terminal", "터미널 열기")}</a>
                <button className="btn ghost" disabled={!!busyId || selected.deleting} onClick={() => void run(lease.id, () => releasePoolLease(selected.id, lease.id))}>{t("End session", "사용 종료")}</button>
              </div>
            </div>
          ))}
          <details className="pool-disclosure" key={selected.id}>
            <summary>{t("Pool settings", "풀 설정")}</summary>
            <p className="pool-note">{selected.template} · {selected.cpu} vCPU · {selected.ram} MiB · {selected.diskGb} GiB</p>
            <p className="pool-note">{t("Pinned image", "고정 이미지")}: <span className="mono">{selected.templateVersion}</span></p>
            {!selected.deleting && <form onSubmit={(event) => { event.preventDefault(); void handleSave(selected); }}>
              <div className="pool-form-grid pool-advanced-grid">
                {numericField("pool-edit-min", t("Keep ready", "미리 준비할 VM 수"), editMin, setEditMin, 0, Number(editMax))}
                {numericField("pool-edit-max", t("Maximum VMs", "최대 VM 수"), editMax, setEditMax, Math.max(1, Number(editMin)), 32)}
                {numericField("pool-edit-ttl", t("Session duration (minutes)", "사용 시간 (분)"), editTtl, setEditTtl, 1, 10080, undefined, "any")}
              </div>
              <div className="pool-form-actions"><button className="btn" type="submit" disabled={!!busyId}>{t("Save settings", "설정 저장")}</button></div>
            </form>}
            <p className="pool-note">{t(`${selected.members.length} of ${selected.maxSize} VMs · ${count(selected, "provisioning")} preparing · ${count(selected, "draining")} cleaning up`, `${selected.maxSize}대 중 ${selected.members.length}대 · ${count(selected, "provisioning")}대 준비 중 · ${count(selected, "draining")}대 정리 중`)}</p>
            <p className="pool-note mono">ID: {selected.id}</p>
            <button className="btn danger" disabled={!!busyId || selected.deleting || active.length > 0} onClick={() => handleDelete(selected)}>{t("Delete pool", "풀 삭제")}</button>
            {active.length > 0 && <p className="pool-note">{t("End all sessions before deleting the pool.", "풀을 삭제하려면 사용 중인 VM을 모두 종료하세요.")}</p>}
          </details>
          {history.length > 0 && <details className="pool-disclosure">
            <summary>{t("Past sessions", "지난 사용 내역")} ({history.length})</summary>
            {history.map((lease) => <p className="pool-history" key={lease.id}><span>{lease.vm?.name ?? lease.vmId}</span><span>{lease.state === "released" ? t("Ended", "종료됨") : t("Expired", "시간 만료")}</span><span>{formatTime(lease.expiresAtMs)}</span></p>)}
          </details>}
        </section>
      )}
    </div>
  );
}
