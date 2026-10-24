import { useEffect, useState } from "react";
import type { MicroBlockStorageResponse, VmBlockStorageMount } from "../bindings";
import { listMicroBlockStorages } from "../api/client";
import { useI18n } from "../i18n";

export default function VolumeSelection({ mounts, onChange, disabled }: {
  mounts: VmBlockStorageMount[];
  onChange: (mounts: VmBlockStorageMount[]) => void;
  disabled: boolean;
}) {
  const { t } = useI18n();
  const [volumes, setVolumes] = useState<MicroBlockStorageResponse[]>([]);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    let active = true;
    listMicroBlockStorages().then((items) => { if (active) setVolumes(items); })
      .catch((cause: Error) => { if (active) setError(cause.message); });
    return () => { active = false; };
  }, []);
  return (
    <div className="field" style={{ gridColumn: "1 / -1" }}>
      <label>{t("Data volumes (optional)", "데이터 볼륨 (선택)")}</label>
      <p className="poll-note">{t("Each volume belongs to one VM. Files at its mount path are hidden while mounted.", "한 볼륨은 한 VM에 연결합니다. 마운트 경로의 기존 파일은 연결 중에 가려집니다.")}</p>
      {error && <p className="field-error" role="alert">{error}</p>}
      {mounts.map((mount, index) => (
        <div className="create-grid" key={index}>
          <div className="field"><label htmlFor={`new-volume-${index}`}>{t("Volume", "볼륨")}</label>
            <select id={`new-volume-${index}`} value={mount.volumeId} required disabled={disabled}
              onChange={(event) => onChange(mounts.map((item, slot) => slot === index ? { ...item, volumeId: event.target.value } : item))}>
              <option value="">{t("Select a volume", "볼륨 선택")}</option>
              {volumes.filter((volume) => volume.state === "available" && !mounts.some((other, slot) => slot !== index && other.volumeId === volume.id))
                .map((volume) => <option key={volume.id} value={volume.id}>{volume.name} · {volume.sizeGib} GiB</option>)}
            </select>
          </div>
          <div className="field"><label htmlFor={`new-volume-path-${index}`}>{t("Mount path", "마운트 경로")}</label>
            <input id={`new-volume-path-${index}`} value={mount.mountPath} required placeholder="/data" disabled={disabled}
              onChange={(event) => onChange(mounts.map((item, slot) => slot === index ? { ...item, mountPath: event.target.value } : item))} />
          </div>
          <button className="btn" type="button" disabled={disabled} onClick={() => onChange(mounts.filter((_, slot) => slot !== index))}>{t("Remove selection", "선택 해제")}</button>
        </div>
      ))}
      <div><button className="btn" type="button" disabled={disabled || mounts.length >= 8 || volumes.filter((volume) => volume.state === "available").length <= mounts.length}
        onClick={() => onChange([...mounts, { volumeId: "", mountPath: "/data" }])}>{t("Add volume", "볼륨 추가")}</button></div>
    </div>
  );
}
