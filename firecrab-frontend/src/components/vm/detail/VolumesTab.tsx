import { useCallback, useEffect, useState } from "react";
import type { FormEvent } from "react";
import type { MicroBlockStorageResponse, VmResponse } from "../../../bindings";
import { attachMicroBlockStorage, detachMicroBlockStorage, listMicroBlockStorages } from "../../../api/client";
import { useI18n } from "../../../i18n";
import { isEditableState } from "../../../model";
import { viewHash } from "../../../navigation";
import { DetailSection } from "../../ui/DetailSection";

export default function VolumesTab({ vm }: { vm: VmResponse }) {
  const { t } = useI18n();
  const [volumes, setVolumes] = useState<MicroBlockStorageResponse[]>([]);
  const [volumeId, setVolumeId] = useState("");
  const [mountPath, setMountPath] = useState("/data");
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
    const timer = window.setInterval(() => void refresh(), 3000);
    return () => window.clearInterval(timer);
  }, [refresh]);
  const attached = volumes.filter((volume) => volume.attachedVmId === vm.id);
  const editable = isEditableState(vm.state, vm.purpose) && loaded && !loadError && !busy;
  const attach = async (event: FormEvent) => {
    event.preventDefault();
    if (!editable) return;
    setBusy(true);
    setError(null);
    try { await attachMicroBlockStorage(volumeId, { vmId: vm.id, mountPath }); setVolumeId(""); await refresh(); }
    catch (cause) { setError((cause as Error).message); }
    finally { setBusy(false); }
  };
  const detach = async (volume: MicroBlockStorageResponse) => {
    if (!editable) return;
    setBusy(true);
    setError(null);
    try { await detachMicroBlockStorage(volume.id); await refresh(); }
    catch (cause) { setError((cause as Error).message); }
    finally { setBusy(false); }
  };
  return (
    <DetailSection title={t("Data volumes", "데이터 볼륨")}>
      <p className="detail-note">{t("Stop this VM to attach or detach a volume. Data stays after VM deletion.", "볼륨 연결·해제 전 VM을 중지합니다. VM을 삭제해도 볼륨 데이터는 유지됩니다.")}</p>
      {(error || loadError) && <p className="field-error" role="alert">{error || loadError}</p>}
      {loaded && attached.length === 0 && <p>{t("No attached volumes.", "연결된 볼륨이 없습니다.")}</p>}
      {attached.map((volume) => <div key={volume.id} className="detail-sec">
        <strong>{volume.name}</strong> · {volume.sizeGib} GiB
        <p className="mono">{volume.mountPath}</p>
        <button type="button" className="btn" disabled={!editable} onClick={() => void detach(volume)}>{t("Detach", "연결 해제")}</button>
      </div>)}
      <form onSubmit={attach}>
        <div className="field"><label htmlFor="vm-volume">{t("Available volume", "연결 가능한 볼륨")}</label>
          <select id="vm-volume" value={volumeId} onChange={(event) => setVolumeId(event.target.value)} required disabled={!editable || attached.length >= 8}>
            <option value="">{t("Select a volume", "볼륨 선택")}</option>
            {volumes.filter((volume) => volume.state === "available").map((volume) => <option key={volume.id} value={volume.id}>{volume.name} · {volume.sizeGib} GiB</option>)}
          </select>
        </div>
        <div className="field"><label htmlFor="vm-volume-path">{t("Mount path", "마운트 경로")}</label>
          <input id="vm-volume-path" value={mountPath} onChange={(event) => setMountPath(event.target.value)} required disabled={!editable} />
        </div>
        <button className="btn primary" type="submit" disabled={!editable || !volumeId}>{t("Attach volume", "볼륨 연결")}</button>
      </form>
      <p><a href={viewHash("volumes")}>{t("Manage data volumes", "데이터 볼륨 관리")}</a></p>
    </DetailSection>
  );
}
