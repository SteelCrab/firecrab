import { useCallback, useEffect, useState } from "react";
import type { FormEvent } from "react";
import type { MicroBlockStorageResponse, StorageRootResponse } from "../bindings";
import { createMicroBlockStorage, deleteMicroBlockStorage, listMicroBlockStorages, listStorageRoots } from "../api/client";
import { useI18n } from "../i18n";
import { vmDetailsHash } from "../navigation";
import Screen from "./layout/Screen";

export default function MicroBlockStorages() {
  const { t } = useI18n();
  const [volumes, setVolumes] = useState<MicroBlockStorageResponse[]>([]);
  const [roots, setRoots] = useState<StorageRootResponse[]>([]);
  const [name, setName] = useState("");
  const [size, setSize] = useState(10);
  const [root, setRoot] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [loaded, setLoaded] = useState(false);
  const [busy, setBusy] = useState(false);
  const refresh = useCallback(async () => {
    try { setVolumes(await listMicroBlockStorages()); setLoaded(true); setLoadError(null); }
    catch (cause) { setLoadError((cause as Error).message); }
  }, []);
  useEffect(() => {
    listMicroBlockStorages().then((items) => { setVolumes(items); setLoaded(true); })
      .catch((cause: Error) => setLoadError(cause.message));
    listStorageRoots().then(setRoots).catch((cause: Error) => setError(cause.message));
    const timer = window.setInterval(() => void refresh(), 3000);
    return () => window.clearInterval(timer);
  }, [refresh]);

  const create = async (event: FormEvent) => {
    event.preventDefault();
    if (busy) return;
    setBusy(true);
    setError(null);
    try {
      await createMicroBlockStorage({ name: name.trim(), sizeGib: size, storageRoot: root || null });
      setName("");
      await refresh();
    } catch (cause) { setError((cause as Error).message); }
    finally { setBusy(false); }
  };
  const remove = async (volume: MicroBlockStorageResponse) => {
    if (busy || !window.confirm(t(
      `Permanently delete volume "${volume.name}" and all its data?`,
      `볼륨 "${volume.name}"과 모든 데이터를 영구 삭제할까요?`,
    ))) return;
    setBusy(true);
    setError(null);
    try { await deleteMicroBlockStorage(volume.id); await refresh(); }
    catch (cause) { setError((cause as Error).message); }
    finally { setBusy(false); }
  };
  return (
    <Screen view="volumes">
      <section className="panel">
        <h2 className="panel-title">MicroBlockStorage</h2>
        <p className="poll-note">{t(
          "Data volumes survive VM deletion. Attach them in the Volumes tab of a stopped MicroVM.",
          "VM을 삭제해도 데이터 볼륨은 유지됩니다. 중지한 MicroVM 상세의 볼륨 탭에서 연결합니다.",
        )}</p>
        <form className="create-grid" onSubmit={create}>
          <div className="field"><label htmlFor="volume-name">{t("Name", "이름")}</label>
            <input id="volume-name" value={name} onChange={(event) => setName(event.target.value)}
              required maxLength={64} pattern="[A-Za-z0-9][A-Za-z0-9._-]*" disabled={busy} />
          </div>
          <div className="field"><label htmlFor="volume-size">{t("Size (GiB)", "크기 (GiB)")}</label>
            <input id="volume-size" type="number" min={1} max={65535} required value={size}
              onChange={(event) => setSize(Number(event.target.value))} disabled={busy} />
          </div>
          <div className="field"><label htmlFor="volume-root">MicroStorage</label>
            <select id="volume-root" value={root} onChange={(event) => setRoot(event.target.value)} disabled={busy}>
              <option value="">{t("Default storage", "기본 저장소")}</option>
              {roots.map((item) => <option key={item.id} value={item.id}>{item.name} · {item.availableGib} GiB</option>)}
            </select>
          </div>
          <div className="field"><label>&nbsp;</label>
            <button className="btn primary" type="submit" disabled={busy}>
              {busy ? t("Working…", "처리 중…") : t("Create volume", "볼륨 생성")}
            </button>
          </div>
        </form>
        {(error || loadError) && <p className="field-error" role="alert">{error || loadError}</p>}
        {!loaded && !error && <p>{t("Loading volumes…", "볼륨을 불러오는 중…")}</p>}
        {loaded && volumes.length === 0 && <p className="empty-state">{t("No data volumes yet.", "데이터 볼륨이 없습니다.")}</p>}
        {volumes.length > 0 && <div className="table-scroll"><table className="vm-table">
          <thead><tr><th>{t("Name", "이름")}</th><th>GiB</th><th>{t("State", "상태")}</th><th>MicroStorage</th><th>MicroVM</th><th>{t("Mount path", "마운트 경로")}</th><th>{t("Actions", "작업")}</th></tr></thead>
          <tbody>{volumes.map((volume) => <tr key={volume.id}>
            <td><strong>{volume.name}</strong><div className="mono">{volume.id}</div></td>
            <td>{volume.sizeGib}</td>
            <td>{t(volume.state, ({ available: "연결 가능", attached: "연결됨", creating: "생성 중", deleting: "삭제 중" })[volume.state])}</td>
            <td>{roots.find((item) => item.id === volume.storageRoot)?.name ?? volume.storageRoot}</td>
            <td>{volume.attachedVmId ? <a href={vmDetailsHash(volume.attachedVmId)}>{volume.attachedVmId}</a> : "—"}</td>
            <td className="mono">{volume.mountPath ?? "—"}</td>
            <td><button className="btn danger" type="button" onClick={() => void remove(volume)}
              disabled={busy || volume.attachedVmId !== null || volume.state === "creating"}>
              {t("Delete", "삭제")}
            </button></td>
          </tr>)}</tbody>
        </table></div>}
      </section>
    </Screen>
  );
}
