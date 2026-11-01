import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type FormEvent,
} from "react";
import { HardDrive } from "lucide-react";
import { fetchJson } from "../api/client";
import type { StorageRootResponse, VmResponse } from "../bindings";
import { useI18n } from "../i18n";
import { vmDetailsHash } from "../navigation";
import { useScreenStatus } from "./layout/screenStatus";
import Screen from "./layout/Screen";

type Volume = {
  id: string;
  name: string;
  sizeGib: number;
  storageRoot: string;
  state: string;
  attachedVmId: string | null;
  mountPath: string | null;
};
const base = "/api/microblockstorage";
const listVolumes = () => fetchJson<Volume[]>(base);
const send = (path: string, method: string, body?: object) =>
  fetchJson<Volume>(path, {
    method,
    ...(body
      ? {
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify(body),
        }
      : {}),
  });
const available = (volume: Volume) =>
  volume.state === "available" && !volume.attachedVmId;

function useVolumes() {
  const [volumes, setVolumes] = useState<Volume[] | null>(null);
  const [error, setError] = useState("");
  const reload = useCallback(async () => {
    const rows = await listVolumes();
    setVolumes(rows);
    setError("");
  }, []);
  useEffect(() => {
    let active = true,
      timer = 0;
    const tick = async () => {
      try {
        const rows = await listVolumes();
        if (active) {
          setVolumes(rows);
          setError("");
        }
      } catch (cause) {
        if (active)
          setError(cause instanceof Error ? cause.message : String(cause));
      }
      if (active) timer = window.setTimeout(() => void tick(), 3000);
    };
    void tick();
    return () => {
      active = false;
      window.clearTimeout(timer);
    };
  }, []);
  return { volumes, error, reload };
}

export default function Volumes({ vms }: { vms: VmResponse[] }) {
  const { setDetailOpen } = useScreenStatus();
  const { t } = useI18n();
  const { volumes, error, reload } = useVolumes();
  const [storage, setStorage] = useState<StorageRootResponse[]>([]);
  const [editor, setEditor] = useState(false);
  const [name, setName] = useState("");
  const [size, setSize] = useState(10);
  const [root, setRoot] = useState("");
  const [actionError, setActionError] = useState("");
  const [busy, setBusy] = useState(false);
  useEffect(() => {
    let active = true;
    void fetchJson<StorageRootResponse[]>("/api/storage")
      .then((rows) => {
        if (active) {
          setStorage(rows);
          setRoot((current) => current || rows[0]?.id || "");
        }
      })
      .catch((cause) => {
        if (active)
          setActionError(
            cause instanceof Error ? cause.message : String(cause),
          );
      });
    return () => {
      active = false;
    };
  }, []);
  const submit = async (event: FormEvent) => {
    event.preventDefault();
    if (busy) return;
    setBusy(true);
    setActionError("");
    try {
      await send(base, "POST", {
        name,
        sizeGib: size,
        storageRoot: root || null,
      });
      await reload();
      setName("");
      setEditor(false);
    } catch (cause) {
      setActionError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setBusy(false);
    }
  };
  const remove = async (volume: Volume) => {
    if (
      busy ||
      !available(volume) ||
      !window.confirm(
        t(
          `Delete volume ${volume.name}?`,
          `"${volume.name}" 볼륨을 삭제할까요?`,
        ),
      )
    )
      return;
    setBusy(true);
    setActionError("");
    try {
      await send(`${base}/${encodeURIComponent(volume.id)}`, "DELETE");
      await reload();
    } catch (cause) {
      setActionError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setBusy(false);
    }
  };
  const roots = [...new Map(storage.map((item) => [item.id, item])).values()];
  return (
    <Screen
      view="volumes"
      live={false}
      detail={
        editor ? (
          <section id="editor" className="panel">
            <h2>{t("Create volume", "볼륨 생성")}</h2>
            {actionError && <p role="alert">{actionError}</p>}
            <form onSubmit={(event) => void submit(event)}>
              <fieldset disabled={busy}>
                <label>
                  {t("Name", "이름")}{" "}
                  <input
                    required
                    maxLength={64}
                    value={name}
                    onChange={(event) => setName(event.target.value)}
                  />
                </label>
                <label>
                  {t("Size (GiB)", "크기 (GiB)")}{" "}
                  <input
                    type="number"
                    required
                    min={1}
                    max={65535}
                    value={size}
                    onChange={(event) => setSize(Number(event.target.value))}
                  />
                </label>
                <div className="field">
                  <label htmlFor="volume-storage">
                    {t("Storage", "스토리지")}
                  </label>
                  <select
                    id="volume-storage"
                    value={root}
                    onChange={(event) => setRoot(event.target.value)}
                  >
                    {roots.length === 0 && (
                      <option value="">{t("Default", "기본")}</option>
                    )}
                    {roots.map((item) => (
                      <option key={item.id} value={item.id}>
                        {item.name}
                      </option>
                    ))}
                  </select>
                </div>
              </fieldset>
              <div className="form-footer">
                <button
                  className="btn ghost"
                  type="button"
                  disabled={busy}
                  onClick={() => setEditor(false)}
                >
                  {t("Cancel", "취소")}
                </button>
                <button className="btn primary" type="submit" disabled={busy}>
                  {t("Create volume", "볼륨 생성")}
                </button>
              </div>
            </form>
          </section>
        ) : undefined
      }
      detailLabel={t("MicroBlockStorage detail", "MicroBlockStorage 상세")}
      detailKind="MicroBlockStorage"
      onCloseDetail={() => setEditor(false)}
      actions={
        <button
          className="btn primary"
          onClick={() => {
            setEditor(true);
            setDetailOpen(true);
          }}
        >
          {t("Create volume", "볼륨 생성")}
        </button>
      }
    >
      <div className="volume-migration">
        {error && <p role="alert">{error}</p>}
        {actionError && !editor && <p role="alert">{actionError}</p>}
        <section className="panel">
          <p>
            {t(
              "Data volumes survive VM deletion. Attach them to a stopped MicroVM.",
              "데이터 볼륨은 VM을 삭제해도 유지됩니다. 중지된 MicroVM에 연결할 수 있습니다.",
            )}
          </p>
          {volumes === null ? (
            <p>{t("Loading…", "불러오는 중…")}</p>
          ) : volumes.length === 0 ? (
            <div className="empty-state">
              <div className="empty-state-icon">
                <HardDrive aria-hidden="true" />
              </div>
              <div className="empty-state-title">
                {t("No data volumes.", "데이터 볼륨이 없습니다.")}
              </div>
            </div>
          ) : (
            <table className="vm-table">
              <thead>
                <tr>
                  <th>{t("Name", "이름")}</th>
                  <th>{t("State", "상태")}</th>
                  <th>MicroVM</th>
                  <th>{t("Storage", "스토리지")}</th>
                  <th>{t("Actions", "작업")}</th>
                </tr>
              </thead>
              <tbody>
                {volumes.map((volume) => (
                  <tr key={volume.id}>
                    <td>
                      <strong>{volume.name}</strong> · {volume.sizeGib} GiB
                    </td>
                    <td>
                      {volume.attachedVmId
                        ? t("Attached", "연결됨")
                        : volume.state}
                    </td>
                    <td>
                      {volume.attachedVmId ? (
                        <a href={vmDetailsHash(volume.attachedVmId)}>
                          {vms.find((vm) => vm.id === volume.attachedVmId)
                            ?.name || volume.attachedVmId}
                        </a>
                      ) : (
                        "—"
                      )}{" "}
                      {volume.mountPath || ""}
                    </td>
                    <td>{volume.storageRoot}</td>
                    <td>
                      <button
                        className="btn danger"
                        disabled={busy || !available(volume)}
                        onClick={() => void remove(volume)}
                      >
                        {t("Delete", "삭제")}
                      </button>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </section>
      </div>
    </Screen>
  );
}

export function VmVolumes({ vm }: { vm: VmResponse }) {
  const { t } = useI18n();
  const { volumes, error, reload } = useVolumes();
  const [selected, setSelected] = useState("");
  const [editing, setEditing] = useState(false);
  const attachButton = useRef<HTMLButtonElement>(null);
  const [mount, setMount] = useState("");
  const [busy, setBusy] = useState(false);
  const [actionError, setActionError] = useState("");
  const attached =
    volumes?.filter((volume) => volume.attachedVmId === vm.id) || [];
  const free = volumes?.filter(available) || [];
  const editable =
    (!vm.purpose || vm.purpose === "instance") &&
    ["created", "stopped", "error"].includes(vm.state);
  const attach = async (event: FormEvent) => {
    event.preventDefault();
    if (
      busy ||
      !editable ||
      attached.length >= 8 ||
      !free.some((volume) => volume.id === selected)
    )
      return;
    setBusy(true);
    setActionError("");
    try {
      await send(`${base}/${encodeURIComponent(selected)}/attach`, "POST", {
        vmId: vm.id,
        mountPath: mount,
      });
      await reload();
      setSelected("");
      setMount("");
      setEditing(false);
    } catch (cause) {
      setActionError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setBusy(false);
    }
  };
  const detach = async (volume: Volume) => {
    if (
      busy ||
      !editable ||
      !window.confirm(
        t(
          "Detach this volume? Its data will be kept.",
          "볼륨을 분리할까요? 데이터는 유지됩니다.",
        ),
      )
    )
      return;
    setBusy(true);
    setActionError("");
    try {
      await send(`${base}/${encodeURIComponent(volume.id)}/detach`, "POST");
      await reload();
    } catch (cause) {
      setActionError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setBusy(false);
    }
  };
  return (
    <section aria-label={t("Data volumes", "데이터 볼륨")}>
      <h3>{t("Attached volumes", "연결된 볼륨")}</h3>
      {(error || actionError) && <p role="alert">{error || actionError}</p>}
      {!editable && (
        <p>
          {t(
            vm.purpose && vm.purpose !== "instance"
              ? "Data volumes can only be attached to instance VMs."
              : "Stop this VM to attach or detach a volume.",
            vm.purpose && vm.purpose !== "instance"
              ? "데이터 볼륨은 일반 VM에만 연결할 수 있습니다."
              : "볼륨을 연결하거나 분리하려면 VM을 중지하세요.",
          )}
        </p>
      )}
      {attached.length === 0 && (
        <p>{t("No attached volumes.", "연결된 볼륨이 없습니다.")}</p>
      )}
      {attached.map((volume) => (
        <div key={volume.id}>
          <strong>{volume.name}</strong> · {volume.sizeGib} GiB ·{" "}
          <code>{volume.mountPath}</code>
          <button
            className="btn"
            disabled={!editable || busy}
            onClick={() => void detach(volume)}
          >
            {t("Detach", "분리")}
          </button>
        </div>
      ))}
      {editable && attached.length < 8 && free.length > 0 && !editing && (
        <button
          ref={attachButton}
          className="btn"
          onClick={() => {
            setSelected(free[0].id);
            setEditing(true);
            setActionError("");
          }}
        >
          {t("Attach volume", "볼륨 연결")}
        </button>
      )}
      {editable && attached.length < 8 && free.length > 0 && editing && (
        <form
          className="volume-attach-form"
          onSubmit={(event) => void attach(event)}
        >
          <div className="field">
            <label htmlFor="vm-volume-id">
              {t("Available volume", "사용 가능한 볼륨")}
            </label>
            <select
              id="vm-volume-id"
              value={selected}
              onChange={(event) => setSelected(event.target.value)}
              required
            >
              <option value="">{t("Select a volume", "볼륨 선택")}</option>
              {free.map((volume) => (
                <option key={volume.id} value={volume.id}>
                  {volume.name} · {volume.sizeGib} GiB
                </option>
              ))}
            </select>
          </div>
          <label>
            {t("Mount path", "마운트 경로")}{" "}
            <input
              required
              pattern="/.*"
              value={mount}
              onChange={(event) => setMount(event.target.value)}
              placeholder="/data"
            />
          </label>
          <button className="btn" disabled={busy} type="submit">
            {t("Attach volume", "볼륨 연결")}
          </button>
          <button
            className="btn"
            type="button"
            disabled={busy}
            onClick={() => {
              setEditing(false);
              setSelected("");
              setMount("");
              setActionError("");
              requestAnimationFrame(() => attachButton.current?.focus());
            }}
          >
            {t("Cancel", "취소")}
          </button>
        </form>
      )}
      {editable && free.length === 0 && (
        <div>
          <p>{t("No available volumes.", "사용 가능한 볼륨이 없습니다.")}</p>{" "}
          <a href="#/volumes">{t("Create volume", "볼륨 생성")}</a>
        </div>
      )}
      <a href="#/volumes">{t("Manage data volumes", "데이터 볼륨 관리")}</a>
    </section>
  );
}
