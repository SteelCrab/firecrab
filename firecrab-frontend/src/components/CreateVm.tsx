import { useEffect, useMemo, useState } from "react";
import type { FormEvent } from "react";
import {
  ApiClientError,
  createVm,
  fetchJson,
  listSecurityGroups,
  listImages,
  listMicroNetworks,
  listShells,
  listStorageRoots,
} from "../api/client";
import type {
  CreateVmRequest,
  EgressPolicy,
  ImageResponse,
  MicroNetworkResponse,
  PortForward,
  PortProtocol,
  ShellResponse,
  StorageRootResponse,
  VmResponse,
} from "../bindings";
import type { SecurityGroupResponse } from "../bindings/SecurityGroupResponse";
type AvailableVolume = {
  id: string;
  name: string;
  state: string;
  attachedVmId: string | null;
};
import { isValidPort } from "../lib/portForward";
import { useHostStatus } from "../hostStatusContext";
import ShellCheckboxList from "./ShellCheckboxList";
import MicroNetworkSelect from "./MicroNetworkSelect";
import { useI18n } from "../i18n";

const FIELDS_WITH_OWN_ERROR = [
  "name",
  "cpu",
  "ram",
  "template",
  "diskGb",
  "microNetworkId",
  "storageRoot",
  "securityGroupId",
  "blockStorages",
  "shellIds",
  "portForwards",
  "env",
] as const;

type EnvRow = { key: string; value: string };

/** Empty select value until the user picks a MicroNetwork (required). */
const NO_NETWORK = "";

interface CreateVmProps {
  onCreated: (vm: VmResponse) => void;
  onError: (message: string) => void;
  onCancel?: () => void;
  initialResources?: { cpu: number; ram: number; diskGb: number };
}

function storageLabel(root: StorageRootResponse): string {
  const free = root.availableGib > 0 ? ` · ${root.availableGib} GiB free` : "";
  const label = root.name && root.name !== root.id ? `${root.name}` : root.id;
  return `${label} (${root.path})${free}`;
}

/** VM disk floor in whole GiB, derived only from image disk bytes. */
function diskFloorGb(image: ImageResponse): number {
  const bytes = image.rootfsSizeBytes;
  if (typeof bytes === "number" && Number.isFinite(bytes) && bytes > 0) {
    return Math.max(1, Math.ceil(bytes / 1024 ** 3));
  }
  // Fallback if an older API omits rootfsSizeBytes.
  return image.minDiskGb > 0 ? image.minDiskGb : 2;
}

function templateLabel(image: ImageResponse): string {
  // `alias` is the user-facing image name and pinned version (`ubuntu-26.04`).
  // Disk size is configured in its own field, so keep this select unambiguous.
  return image.alias;
}

export default function CreateVm({
  onCreated,
  onError,
  onCancel,
  initialResources,
}: CreateVmProps) {
  const { t } = useI18n();
  const host = useHostStatus();
  const [name, setName] = useState("");
  const [template, setTemplate] = useState<string>("");
  const [images, setImages] = useState<ImageResponse[]>([]);
  const [imagesLoading, setImagesLoading] = useState(true);
  const [imagesError, setImagesError] = useState<string | null>(null);
  const [cpu, setCpu] = useState(String(initialResources?.cpu ?? 2));
  const [ram, setRam] = useState(String(initialResources?.ram ?? 1024));
  const [diskGb, setDiskGb] = useState(String(initialResources?.diskGb ?? 8));
  const [egressPolicy, setEgressPolicy] = useState<EgressPolicy>("internet");
  const [microNetworkId, setMicroNetworkId] = useState<string>(NO_NETWORK);
  const [microNetworks, setMicroNetworks] = useState<MicroNetworkResponse[]>(
    [],
  );
  const [storageRoots, setStorageRoots] = useState<StorageRootResponse[]>([]);
  const [storageRoot, setStorageRoot] = useState<string>("");
  const [groups, setGroups] = useState<SecurityGroupResponse[]>([]);
  const [securityGroupId, setSecurityGroupId] = useState("");
  const [volumes, setVolumes] = useState<AvailableVolume[]>([]);
  const [mounts, setMounts] = useState<Record<string, string>>({});
  const [resourceError, setResourceError] = useState("");
  const [shells, setShells] = useState<ShellResponse[]>([]);
  const [shellIds, setShellIds] = useState<string[]>([]);
  const [portForwards, setPortForwards] = useState<PortForward[]>([]);
  const [envRows, setEnvRows] = useState<EnvRow[]>([]);
  /** Bumped after a successful create so Shell checkboxes remount cleared. */
  const [formEpoch, setFormEpoch] = useState(0);
  const [submitting, setSubmitting] = useState(false);
  const [fieldErrors, setFieldErrors] = useState<ApiClientError | null>(null);
  const [portForwardsError, setPortForwardsError] = useState<string | null>(
    null,
  );
  const [envError, setEnvError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    setImagesLoading(true);
    listImages()
      .then((next) => {
        if (cancelled) return;
        // Create form only offers installed templates (uninstalled come later
        // with the install API).
        const installed = next.filter((image) => image.installed);
        setImages(installed);
        setImagesError(null);
        setTemplate((current) => {
          if (current && installed.some((image) => image.alias === current)) {
            return current;
          }
          return installed[0]?.alias ?? "";
        });
        if (installed[0] && Number(diskGb) < diskFloorGb(installed[0])) {
          setDiskGb(String(diskFloorGb(installed[0])));
        }
      })
      .catch((error) => {
        if (cancelled) return;
        setImages([]);
        setTemplate("");
        setImagesError(error instanceof Error ? error.message : String(error));
      })
      .finally(() => {
        if (!cancelled) setImagesLoading(false);
      });

    listMicroNetworks()
      .then((networks) => {
        if (cancelled) return;
        setMicroNetworks(networks);
        if (networks.length > 0) {
          setMicroNetworkId((current) => current || networks[0].id);
        }
      })
      .catch(() => {
        if (!cancelled) setMicroNetworks([]);
      });
    listStorageRoots()
      .then((roots) => {
        if (cancelled) return;
        setStorageRoots(roots);
        if (roots.length > 0) {
          setStorageRoot((current) => current || roots[0].id);
        }
      })
      .catch(() => {
        if (!cancelled) setStorageRoots([]);
      });

    Promise.all([
      listSecurityGroups(),
      fetchJson<AvailableVolume[]>("/api/microblockstorage"),
    ])
      .then(([nextGroups, nextVolumes]) => {
        if (!cancelled) {
          setGroups(nextGroups);
          setVolumes(
            nextVolumes.filter(
              (volume) => volume.state === "available" && !volume.attachedVmId,
            ),
          );
        }
      })
      .catch(() => {
        if (!cancelled)
          setResourceError(
            t(
              "Could not load security groups or data volumes.",
              "보안그룹 또는 데이터 볼륨을 불러오지 못했다.",
            ),
          );
      });

    listShells()
      .then((next) => {
        if (!cancelled) setShells(next);
      })
      .catch(() => {
        if (!cancelled) setShells([]);
      });

    return () => {
      cancelled = true;
    };
    // diskGb intentionally omitted — only seed floor on first catalog load.
    // eslint-disable-next-line react-hooks/exhaustive-deps -- mount-only fetch
  }, []);

  const selectedImage = useMemo(
    () => images.find((image) => image.alias === template) ?? null,
    [images, template],
  );
  const minDiskGb = selectedImage ? diskFloorGb(selectedImage) : 2;
  const freeMemory =
    host &&
    host.memoryTotalMib > 0 &&
    host.memoryAvailableMib >= 0 &&
    host.memoryAvailableMib <= host.memoryTotalMib
      ? `${t("Free host memory", "남은 호스트 메모리")} ${(host.memoryAvailableMib / 1024).toFixed(1)} GiB`
      : "";
  const noTemplates = !imagesLoading && images.length === 0;
  const canSubmit =
    !submitting &&
    !imagesLoading &&
    !noTemplates &&
    Boolean(template) &&
    !imagesError;

  const onTemplateChange = (alias: string) => {
    setTemplate(alias);
    const image = images.find((entry) => entry.alias === alias);
    if (image) {
      const floor = diskFloorGb(image);
      if (Number(diskGb) < floor) setDiskGb(String(floor));
    }
  };

  const addPortForward = () => {
    setPortForwards((current) => [
      ...current,
      { hostPort: 8080, guestPort: 80, protocol: "tcp" },
    ]);
  };

  const removePortForward = (index: number) => {
    setPortForwards((current) => current.filter((_, i) => i !== index));
  };

  const updatePortForward = (
    index: number,
    field: keyof PortForward,
    value: any,
  ) => {
    setPortForwards((current) => {
      const next = [...current];
      next[index] = { ...next[index], [field]: value };
      return next;
    });
  };

  const addEnvRow = () => {
    setEnvRows((current) => [...current, { key: "", value: "" }]);
  };

  const removeEnvRow = (index: number) => {
    setEnvRows((current) => current.filter((_, i) => i !== index));
  };

  const updateEnvRow = (index: number, field: keyof EnvRow, value: string) => {
    setEnvRows((current) => {
      const next = [...current];
      next[index] = { ...next[index], [field]: value };
      return next;
    });
  };

  const handleSubmit = async (event: FormEvent) => {
    event.preventDefault();
    if (!canSubmit) return;

    // Incomplete rows must block submission, not be silently dropped — a
    // dropped row would leave the user believing a rule was created when it
    // never left the browser. The row itself stays in the editor so they
    // can fix or explicitly remove it.
    const incompleteIndex = portForwards.findIndex(
      (pf) => !isValidPort(pf.hostPort) || !isValidPort(pf.guestPort),
    );
    if (incompleteIndex !== -1) {
      setPortForwardsError(
        t(
          "Every port forward needs a host and guest port (1-65535).",
          "모든 포트 포워딩 규칙에는 호스트/게스트 포트(1-65535)가 필요합니다.",
        ),
      );
      return;
    }
    setPortForwardsError(null);

    const env: { [key: string]: string } = {};
    for (const row of envRows) {
      const key = row.key.trim();
      if (!key) {
        setEnvError(
          t(
            "Every environment variable needs a key.",
            "모든 환경 변수에는 키가 필요합니다.",
          ),
        );
        return;
      }
      if (key in env) {
        setEnvError(
          t(
            "Environment keys must be unique.",
            "환경 변수 키는 중복될 수 없습니다.",
          ),
        );
        return;
      }
      env[key] = row.value;
    }
    setEnvError(null);

    const request: CreateVmRequest = {
      name: name.trim(),
      template,
      cpu: parseInt(cpu, 10) || 0,
      ram: parseInt(ram, 10) || 0,
      diskGb: parseInt(diskGb, 10) || 0,
      egressPolicy,
      microNetworkId: microNetworkId,
      securityGroupId: securityGroupId || null,
      blockStorages: Object.entries(mounts).map(([volumeId, mountPath]) => ({
        volumeId,
        mountPath: mountPath.trim(),
      })),
      storageRoot: storageRoot || null,
      shellIds: shellIds.length > 0 ? shellIds : undefined,
      portForwards: portForwards.length > 0 ? portForwards : undefined,
      env,
    };

    setSubmitting(true);
    setFieldErrors(null);
    try {
      const vm = await createVm(request);
      // Clear identity + shell pins + port forwards so the next create does not re-send
      setName("");
      setShellIds([]);
      setPortForwards([]);
      setPortForwardsError(null);
      setEnvRows([]);
      setEnvError(null);
      setFieldErrors(null);
      setFormEpoch((epoch) => epoch + 1);
      listShells()
        .then(setShells)
        .catch(() => setShells([]));
      onCreated(vm);
    } catch (error) {
      const apiError = error as ApiClientError;
      if (
        FIELDS_WITH_OWN_ERROR.every(
          (field) => apiError.fieldError(field) === undefined,
        )
      ) {
        onError(apiError.message);
      }
      setFieldErrors(apiError);
    } finally {
      setSubmitting(false);
    }
  };

  const fieldError = (field: string) => (
    <span className="field-error">{fieldErrors?.fieldError(field) ?? ""}</span>
  );

  const imageHint = imagesLoading
    ? t("Loading images…", "이미지 불러오는 중…")
    : imagesError
      ? t(
          "Unable to load the image catalog",
          "이미지 목록을 불러오지 못했습니다",
        )
      : noTemplates
        ? t(
            "No installed images. Run install.sh or install an image.",
            "설치된 이미지가 없습니다 (install.sh 또는 이미지 설치 필요)",
          )
        : null;

  return (
    <form id="editor" className="vm-create-form" onSubmit={handleSubmit}>
      <section className="form-section">
        <div className="form-section-head">
          <h3>{t("Basics", "기본 정보")}</h3>
          <p>
            {t(
              "Name the VM and choose an installed image.",
              "이름과 설치된 이미지를 선택한다.",
            )}
          </p>
        </div>
        <div className="form-section-fields">
          <div className="field">
            <label htmlFor="vm-name">{t("Name", "이름")}</label>
            <input
              id="vm-name"
              placeholder="my-vm"
              value={name}
              onChange={(event) => setName(event.target.value)}
              required
              minLength={1}
              maxLength={64}
            />
            {fieldError("name")}
          </div>
          <div className="field">
            <label htmlFor="vm-image">{t("Image", "이미지")}</label>
            <select
              id="vm-image"
              value={template}
              onChange={(event) => onTemplateChange(event.target.value)}
              disabled={imagesLoading || noTemplates || Boolean(imagesError)}
            >
              {imageHint ? (
                <option value="">{imageHint}</option>
              ) : (
                images.map((image) => (
                  <option key={image.alias} value={image.alias}>
                    {templateLabel(image)}
                  </option>
                ))
              )}
            </select>
            {fieldError("template")}
            {(imagesError || noTemplates) &&
              !fieldErrors?.fieldError("template") && (
                <span className="field-error">
                  {imagesError ??
                    t(
                      "Install an image before creating a VM.",
                      "생성하려면 먼저 이미지를 설치하세요",
                    )}
                </span>
              )}
            {selectedImage && !selectedImage.hasGuestService && (
              <span style={{ fontSize: "0.75rem", color: "var(--text-3)" }}>
                {t(
                  "Runtime env is ignored: this image has no guest service.",
                  "게스트 서비스가 없는 이미지라 런타임 환경 변수는 적용되지 않습니다.",
                )}
              </span>
            )}
          </div>
        </div>
      </section>
      <section className="form-section">
        <div className="form-section-head">
          <h3>{t("Resources", "리소스")}</h3>
          <p>
            {t(
              "Allocate CPU, memory and disk space.",
              "CPU, 메모리와 디스크 용량을 지정한다.",
            )}
          </p>
        </div>
        <div className="form-section-fields">
          <div className="field">
            <label htmlFor="vm-cpu">vCPU</label>
            <input
              id="vm-cpu"
              type="number"
              min={1}
              max={32}
              value={cpu}
              onChange={(event) => setCpu(event.target.value)}
            />
            {fieldError("cpu")}
          </div>
          <div className="field">
            <label htmlFor="vm-disk">
              {t("Disk", "디스크")}{" "}
              <span className="unit" aria-hidden="true">
                GiB
              </span>
            </label>
            <input
              id="vm-disk"
              aria-label={t("Disk", "디스크")}
              type="number"
              min={minDiskGb}
              max={500}
              value={diskGb}
              onChange={(event) => setDiskGb(event.target.value)}
            />
            {fieldError("diskGb")}
          </div>
          <div className="field field-span-2">
            <fieldset
              className="memory-picker"
              role="radiogroup"
              aria-label={t("Memory", "메모리")}
              aria-describedby={freeMemory ? "hint-ram" : undefined}
            >
              <legend>{t("Memory", "메모리")}</legend>
              <div className="memory-options">
                {[128, 256, 512, 1024, 2048, 4096, 8192, 16384, 32768].map(
                  (value) => (
                    <label
                      key={value}
                      className={
                        Number(ram) === value
                          ? "memory-chip selected"
                          : "memory-chip"
                      }
                    >
                      <input
                        type="radio"
                        name="memory"
                        value={value}
                        checked={Number(ram) === value}
                        onChange={() => setRam(String(value))}
                      />
                      <span>
                        {value < 1024 ? `${value} MiB` : `${value / 1024} GiB`}
                      </span>
                    </label>
                  ),
                )}
              </div>
            </fieldset>
            <span id="hint-ram" className="poll-note" hidden={!freeMemory}>
              {freeMemory}
            </span>
            {fieldError("ram")}
          </div>
          <div className="field">
            <label htmlFor="vm-storage">
              {t("Storage location", "저장 위치")}
            </label>
            <select
              id="vm-storage"
              value={storageRoot}
              onChange={(event) => setStorageRoot(event.target.value)}
              disabled={storageRoots.length === 0}
            >
              {storageRoots.length === 0 ? (
                <option value="">default</option>
              ) : (
                storageRoots.map((root) => (
                  <option key={root.id} value={root.id}>
                    {storageLabel(root)}
                  </option>
                ))
              )}
            </select>
            {fieldError("storageRoot")}
          </div>
        </div>
      </section>
      <section className="form-section">
        <div className="form-section-head">
          <h3>{t("Network", "네트워크")}</h3>
          <p>
            {t(
              "Choose a network and outbound access.",
              "네트워크와 외부 통신 범위를 선택한다.",
            )}
          </p>
        </div>
        <div className="form-section-fields">
          <div className="field">
            <label htmlFor="vm-micro-network">MicroNetwork</label>
            <MicroNetworkSelect
              id="vm-micro-network"
              value={microNetworkId}
              networks={microNetworks}
              onChange={(id) => {
                setMicroNetworkId(id);
                setSecurityGroupId("");
              }}
            />
            {fieldError("microNetworkId")}
          </div>
          <div className="field">
            <label htmlFor="vm-security-group">
              {t("Security group", "보안그룹")}
            </label>
            <select
              id="vm-security-group"
              value={securityGroupId}
              onChange={(event) => setSecurityGroupId(event.target.value)}
            >
              <option value="">{t("None", "없음")}</option>
              {groups
                .filter((group) => group.microNetworkId === microNetworkId)
                .map((group) => (
                  <option key={group.id} value={group.id}>
                    {group.name}
                  </option>
                ))}
            </select>
            {fieldError("securityGroupId")}
          </div>
          {resourceError && (
            <p className="field-error" role="alert">
              {resourceError}
            </p>
          )}
          <fieldset
            className="field field-span-2 internet-picker"
            role="radiogroup"
            aria-label={t("Internet access", "인터넷 접근")}
          >
            <legend>{t("Internet access", "인터넷 접근")}</legend>
            {(["internet", "isolated"] as EgressPolicy[]).map((policy) => (
              <label key={policy}>
                <input
                  type="radio"
                  name="egress"
                  checked={egressPolicy === policy}
                  onChange={() => setEgressPolicy(policy)}
                />
                {policy === "internet"
                  ? t("Allow internet", "인터넷 허용")
                  : t("Isolated", "격리")}
              </label>
            ))}
          </fieldset>
        </div>
      </section>
      <details className="form-advanced">
        <summary>{t("Advanced options", "고급 설정")}</summary>
        <div className="advanced-fields">
          {volumes.length > 0 && (
            <fieldset className="field field-span-2">
              <legend>{t("Data volumes", "데이터 볼륨")}</legend>
              {volumes.map((volume) => (
                <div key={volume.id}>
                  <label>
                    <input
                      type="checkbox"
                      checked={Object.hasOwn(mounts, volume.id)}
                      onChange={(event) =>
                        setMounts((current) => {
                          const next = { ...current };
                          if (event.target.checked) next[volume.id] = "";
                          else delete next[volume.id];
                          return next;
                        })
                      }
                    />{" "}
                    {volume.name}
                  </label>
                  {Object.hasOwn(mounts, volume.id) && (
                    <label>
                      {t("Mount path", "마운트 경로")}{" "}
                      <input
                        required
                        pattern="/.*"
                        placeholder="/data"
                        value={mounts[volume.id]}
                        onChange={(event) =>
                          setMounts((current) => ({
                            ...current,
                            [volume.id]: event.target.value,
                          }))
                        }
                      />
                    </label>
                  )}
                </div>
              ))}
              {fieldError("blockStorages")}
            </fieldset>
          )}
          <div className="field">
            <label id="vm-shells-label">
              {t("Shells (optional)", "Shell (선택)")}
            </label>
            <ShellCheckboxList
              key={`vm-create-shells-${formEpoch}`}
              shells={shells}
              selectedIds={shellIds}
              onChange={setShellIds}
              disabled={submitting}
              idPrefix={`vm-create-shell-${formEpoch}`}
              emptyLabel={t(
                "No shells yet — create one under Shells, then come back.",
                "등록된 Shell이 없습니다. Shell 메뉴에서 만든 뒤 다시 오세요.",
              )}
            />
            {fieldError("shellIds")}
          </div>
          <div className="field" style={{ gridColumn: "1 / -1" }}>
            <label>{t("Port Forwarding", "포트 포워딩")}</label>
            <div
              className="port-forwards-list"
              style={{
                display: "flex",
                flexDirection: "column",
                gap: "0.5rem",
              }}
            >
              {portForwards.map((pf, idx) => (
                <div
                  key={idx}
                  style={{
                    display: "flex",
                    gap: "0.75rem",
                    alignItems: "flex-end",
                    background: "var(--bg-subtle, rgba(255, 255, 255, 0.03))",
                    padding: "0.6rem 0.8rem",
                    borderRadius: "6px",
                    border: "1px solid var(--border)",
                  }}
                >
                  <div
                    style={{
                      display: "flex",
                      flexDirection: "column",
                      gap: "0.25rem",
                    }}
                  >
                    <span
                      style={{
                        fontSize: "0.75rem",
                        color: "var(--text-3)",
                        fontWeight: 500,
                      }}
                    >
                      {t("VM Guest Port", "VM 내부 포트")}
                    </span>
                    <input
                      type="number"
                      placeholder="80"
                      aria-label={`${t("Guest port", "내부 포트")} ${idx + 1}`}
                      value={pf.guestPort || ""}
                      onChange={(e) =>
                        updatePortForward(
                          idx,
                          "guestPort",
                          Number(e.target.value),
                        )
                      }
                      style={{
                        width: "130px",
                        borderColor: isValidPort(pf.guestPort)
                          ? undefined
                          : "var(--red)",
                      }}
                    />
                  </div>
                  <span
                    style={{
                      fontSize: "1rem",
                      color: "var(--text-3)",
                      paddingBottom: "0.4rem",
                    }}
                  >
                    ➔
                  </span>
                  <div
                    style={{
                      display: "flex",
                      flexDirection: "column",
                      gap: "0.25rem",
                    }}
                  >
                    <span
                      style={{
                        fontSize: "0.75rem",
                        color: "var(--text-3)",
                        fontWeight: 500,
                      }}
                    >
                      {t("Host Port", "호스트 포트")}
                    </span>
                    <input
                      type="number"
                      placeholder="8080"
                      aria-label={`${t("Host port", "호스트 포트")} ${idx + 1}`}
                      value={pf.hostPort || ""}
                      onChange={(e) =>
                        updatePortForward(
                          idx,
                          "hostPort",
                          Number(e.target.value),
                        )
                      }
                      style={{
                        width: "130px",
                        borderColor: isValidPort(pf.hostPort)
                          ? undefined
                          : "var(--red)",
                      }}
                    />
                  </div>
                  <div
                    style={{
                      display: "flex",
                      flexDirection: "column",
                      gap: "0.25rem",
                    }}
                  >
                    <span
                      style={{
                        fontSize: "0.75rem",
                        color: "var(--text-3)",
                        fontWeight: 500,
                      }}
                    >
                      {t("Protocol", "프로토콜")}
                    </span>
                    <select
                      value={pf.protocol}
                      onChange={(e) =>
                        updatePortForward(
                          idx,
                          "protocol",
                          e.target.value as PortProtocol,
                        )
                      }
                      style={{ width: "85px" }}
                    >
                      <option value="tcp">TCP</option>
                      <option value="udp">UDP</option>
                    </select>
                  </div>
                  <button
                    type="button"
                    className="btn small danger"
                    onClick={() => removePortForward(idx)}
                    style={{ padding: "0.4rem 0.75rem" }}
                  >
                    {t("Remove", "삭제")}
                  </button>
                </div>
              ))}
              <button
                type="button"
                className="btn small secondary"
                onClick={addPortForward}
                style={{ width: "fit-content", marginTop: "0.2rem" }}
              >
                + {t("Add Port Forward Rule", "포트 포워딩 규칙 추가")}
              </button>
            </div>
            {portForwardsError && (
              <span className="field-error">{portForwardsError}</span>
            )}
            {fieldError("portForwards")}
          </div>
          <div
            className="field"
            role="group"
            aria-label={t("Environment", "환경 변수")}
            style={{ gridColumn: "1 / -1" }}
          >
            <label>{t("Environment", "환경 변수")}</label>
            <div
              style={{
                display: "flex",
                flexDirection: "column",
                gap: "0.5rem",
              }}
            >
              {envRows.map((row, idx) => (
                <div
                  key={idx}
                  style={{
                    display: "flex",
                    gap: "0.75rem",
                    alignItems: "flex-end",
                    background: "var(--bg-subtle, rgba(255, 255, 255, 0.03))",
                    padding: "0.6rem 0.8rem",
                    borderRadius: "6px",
                    border: "1px solid var(--border)",
                  }}
                >
                  <div
                    style={{
                      display: "flex",
                      flexDirection: "column",
                      gap: "0.25rem",
                      flex: 1,
                    }}
                  >
                    <span
                      style={{
                        fontSize: "0.75rem",
                        color: "var(--text-3)",
                        fontWeight: 500,
                      }}
                    >
                      {t("Key", "키")}
                    </span>
                    <input
                      id={`vm-env-key-${idx}`}
                      aria-label={`${t("Variable name", "변수 이름")} ${idx + 1}`}
                      type="text"
                      placeholder="APP_NAME"
                      value={row.key}
                      maxLength={256}
                      autoComplete="off"
                      spellCheck={false}
                      onChange={(event) =>
                        updateEnvRow(idx, "key", event.target.value)
                      }
                    />
                  </div>
                  <div
                    style={{
                      display: "flex",
                      flexDirection: "column",
                      gap: "0.25rem",
                      flex: 2,
                    }}
                  >
                    <span
                      style={{
                        fontSize: "0.75rem",
                        color: "var(--text-3)",
                        fontWeight: 500,
                      }}
                    >
                      {t("Value", "값")}
                    </span>
                    <input
                      id={`vm-env-value-${idx}`}
                      aria-label={`${t("Variable value", "변수 값")} ${idx + 1}`}
                      type="text"
                      placeholder="web"
                      value={row.value}
                      maxLength={4096}
                      autoComplete="off"
                      spellCheck={false}
                      onChange={(event) =>
                        updateEnvRow(idx, "value", event.target.value)
                      }
                    />
                  </div>
                  <button
                    type="button"
                    className="btn small danger"
                    onClick={() => removeEnvRow(idx)}
                    style={{ padding: "0.4rem 0.75rem" }}
                  >
                    {t("Remove", "삭제")}
                  </button>
                </div>
              ))}
              <button
                type="button"
                id="vm-env-add"
                className="btn small secondary"
                onClick={addEnvRow}
                disabled={envRows.length >= 64}
                style={{ width: "fit-content", marginTop: "0.2rem" }}
              >
                + {t("Add variable", "변수 추가")}
              </button>
            </div>
            {envError && <span className="field-error">{envError}</span>}
            {fieldError("env")}
          </div>
        </div>
      </details>
      <div className="form-footer">
        <span id="form-summary">
          {cpu || "–"} vCPU ·{" "}
          {Number(ram) >= 1024 ? `${Number(ram) / 1024} GiB` : `${ram} MiB`} ·{" "}
          {diskGb} GiB{template ? ` · ${template}` : ""}
        </span>
        <div className="form-footer-actions">
          {onCancel ? (
            <button type="button" className="btn" onClick={onCancel}>
              {t("Cancel", "취소")}
            </button>
          ) : (
            <a className="btn" href="#/vms">
              {t("Cancel", "취소")}
            </a>
          )}
          <button
            id="vm-create-submit"
            className="btn primary"
            type="submit"
            disabled={!canSubmit}
          >
            {submitting
              ? t("Creating…", "생성 중…")
              : t("Create MicroVM", "MicroVM 생성")}
          </button>
        </div>
      </div>
    </form>
  );
}
