import { useEffect, useMemo, useState } from "react";
import type { FormEvent } from "react";
import {
  ApiClientError,
  createVm,
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
import RamStepper from "./RamStepper";
import ShellCheckboxList from "./ShellCheckboxList";
import { useI18n } from "../i18n";

const FIELDS_WITH_OWN_ERROR = [
  "name",
  "cpu",
  "ram",
  "template",
  "diskGb",
  "microNetworkId",
  "storageRoot",
  "shellIds",
  "portForwards",
] as const;

/** Empty select value until the user picks a MicroNetwork (required). */
const NO_NETWORK = "";

interface CreateVmProps {
  onCreated: (vm: VmResponse) => void;
  onError: (message: string) => void;
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

export default function CreateVm({ onCreated, onError }: CreateVmProps) {
  const { t } = useI18n();
  const [name, setName] = useState("");
  const [template, setTemplate] = useState<string>("");
  const [images, setImages] = useState<ImageResponse[]>([]);
  const [imagesLoading, setImagesLoading] = useState(true);
  const [imagesError, setImagesError] = useState<string | null>(null);
  const [cpu, setCpu] = useState("1");
  const [ram, setRam] = useState("512");
  const [diskGb, setDiskGb] = useState("2");
  const [egressPolicy, setEgressPolicy] = useState<EgressPolicy>("internet");
  const [microNetworkId, setMicroNetworkId] = useState<string>(NO_NETWORK);
  const [microNetworks, setMicroNetworks] = useState<MicroNetworkResponse[]>([]);
  const [storageRoots, setStorageRoots] = useState<StorageRootResponse[]>([]);
  const [storageRoot, setStorageRoot] = useState<string>("");
  const [shells, setShells] = useState<ShellResponse[]>([]);
  const [shellIds, setShellIds] = useState<string[]>([]);
  const [portForwards, setPortForwards] = useState<PortForward[]>([]);
  /** Bumped after a successful create so Shell checkboxes remount cleared. */
  const [formEpoch, setFormEpoch] = useState(0);
  const [submitting, setSubmitting] = useState(false);
  const [fieldErrors, setFieldErrors] = useState<ApiClientError | null>(null);

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
  const noTemplates = !imagesLoading && images.length === 0;
  const canSubmit =
    !submitting && !imagesLoading && !noTemplates && Boolean(template) && !imagesError;

  const onTemplateChange = (alias: string) => {
    setTemplate(alias);
    const image = images.find((entry) => entry.alias === alias);
    if (image) {
      const floor = diskFloorGb(image);
      if (Number(diskGb) < floor) setDiskGb(String(floor));
    }
  };

  const addPortForward = () => {
    setPortForwards((current) => [...current, { hostPort: 8080, guestPort: 80, protocol: "tcp" }]);
  };

  const removePortForward = (index: number) => {
    setPortForwards((current) => current.filter((_, i) => i !== index));
  };

  const updatePortForward = (index: number, field: keyof PortForward, value: any) => {
    setPortForwards((current) => {
      const next = [...current];
      next[index] = { ...next[index], [field]: value };
      return next;
    });
  };

  const handleSubmit = async (event: FormEvent) => {
    event.preventDefault();
    if (!canSubmit) return;

    const validPortForwards = portForwards.filter((pf) => pf.hostPort > 0 && pf.guestPort > 0);

    const request: CreateVmRequest = {
      name: name.trim(),
      template,
      cpu: parseInt(cpu, 10) || 0,
      ram: parseInt(ram, 10) || 0,
      diskGb: parseInt(diskGb, 10) || 0,
      egressPolicy,
      microNetworkId: microNetworkId,
      storageRoot: storageRoot || null,
      shellIds: shellIds.length > 0 ? shellIds : undefined,
      portForwards: validPortForwards.length > 0 ? validPortForwards : undefined,
    };

    setSubmitting(true);
    setFieldErrors(null);
    try {
      const vm = await createVm(request);
      // Clear identity + shell pins + port forwards so the next create does not re-send
      setName("");
      setShellIds([]);
      setPortForwards([]);
      setFieldErrors(null);
      setFormEpoch((epoch) => epoch + 1);
      listShells()
        .then(setShells)
        .catch(() => setShells([]));
      onCreated(vm);
    } catch (error) {
      const apiError = error as ApiClientError;
      if (FIELDS_WITH_OWN_ERROR.every((field) => apiError.fieldError(field) === undefined)) {
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
      ? t("Unable to load the image catalog", "이미지 목록을 불러오지 못했습니다")
      : noTemplates
        ? t("No installed images. Run install.sh or install an image.", "설치된 이미지가 없습니다 (install.sh 또는 이미지 설치 필요)")
        : null;

  return (
    <form className="create-grid" onSubmit={handleSubmit}>
      <div className="field">
        <label htmlFor="vm-name">name</label>
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
        <label htmlFor="vm-image">image</label>
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
        {(imagesError || noTemplates) && !fieldErrors?.fieldError("template") && (
          <span className="field-error">{imagesError ?? t("Install an image before creating a VM.", "생성하려면 먼저 이미지를 설치하세요")}</span>
        )}
      </div>
      <div className="field">
        <label htmlFor="vm-cpu">cpu</label>
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
        <label htmlFor="vm-ram">ram (MiB)</label>
        <RamStepper id="vm-ram" value={ram} onChange={setRam} />
        {fieldError("ram")}
      </div>
      <div className="field">
        <label htmlFor="vm-disk">disk (GiB)</label>
        <input
          id="vm-disk"
          type="number"
          min={minDiskGb}
          max={500}
          value={diskGb}
          onChange={(event) => setDiskGb(event.target.value)}
        />
        {fieldError("diskGb")}
      </div>
      {/* Always render storage so the 5-column grid stays aligned even
          when the host has no extra storage roots yet. */}
      <div className="field">
        <label htmlFor="vm-storage">{t("Storage location", "저장 위치")}</label>
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
      <div className="field">
        <label htmlFor="vm-micro-network">MicroNetwork</label>
        <select
          id="vm-micro-network"
          value={microNetworkId}
          onChange={(event) => setMicroNetworkId(event.target.value)}
        >
          <option value={NO_NETWORK} disabled>
            {microNetworks.length === 0
              ? t("Create a MicroNetwork first", "먼저 MicroNetwork를 만드세요")
              : t("Select a MicroNetwork", "MicroNetwork 선택")}
          </option>
          {microNetworks.map((network) => (
            <option key={network.id} value={network.id}>
              {network.name} ({network.subnetCidr})
            </option>
          ))}
        </select>
        {fieldError("microNetworkId")}
      </div>
      <div className="field">
        <label htmlFor="vm-egress-policy">{t("Egress", "외부 통신")}</label>
        <select
          id="vm-egress-policy"
          value={egressPolicy}
          onChange={(event) => setEgressPolicy(event.target.value as EgressPolicy)}
        >
          {(["internet", "isolated"] as EgressPolicy[]).map((policy) => (
            <option key={policy} value={policy}>
              {policy === "internet"
                ? t("Internet access", "인터넷 허용")
                : t("Isolated (gateway only)", "격리(게이트웨이만 허용)")}
            </option>
          ))}
        </select>
        <span className="field-error" aria-hidden />
      </div>
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
        <div className="port-forwards-list" style={{ display: "flex", flexDirection: "column", gap: "0.5rem" }}>
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
                border: "1px solid var(--border-color, rgba(255, 255, 255, 0.1))",
              }}
            >
              <div style={{ display: "flex", flexDirection: "column", gap: "0.25rem" }}>
                <span style={{ fontSize: "0.75rem", color: "var(--muted, #888)", fontWeight: 500 }}>
                  {t("VM Guest Port", "VM 내부 포트")}
                </span>
                <input
                  type="number"
                  placeholder="80"
                  value={pf.guestPort || ""}
                  onChange={(e) => updatePortForward(idx, "guestPort", Number(e.target.value))}
                  style={{ width: "130px" }}
                />
              </div>
              <span style={{ fontSize: "1rem", color: "var(--muted, #888)", paddingBottom: "0.4rem" }}>➔</span>
              <div style={{ display: "flex", flexDirection: "column", gap: "0.25rem" }}>
                <span style={{ fontSize: "0.75rem", color: "var(--muted, #888)", fontWeight: 500 }}>
                  {t("Host Port", "호스트 포트")}
                </span>
                <input
                  type="number"
                  placeholder="8080"
                  value={pf.hostPort || ""}
                  onChange={(e) => updatePortForward(idx, "hostPort", Number(e.target.value))}
                  style={{ width: "130px" }}
                />
              </div>
              <div style={{ display: "flex", flexDirection: "column", gap: "0.25rem" }}>
                <span style={{ fontSize: "0.75rem", color: "var(--muted, #888)", fontWeight: 500 }}>
                  {t("Protocol", "프로토콜")}
                </span>
                <select
                  value={pf.protocol}
                  onChange={(e) => updatePortForward(idx, "protocol", e.target.value as PortProtocol)}
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
        {fieldError("portForwards")}
      </div>
      <div className="field field-submit">
        <label htmlFor="vm-create-submit">&nbsp;</label>
        <button
          id="vm-create-submit"
          className="btn primary"
          type="submit"
          disabled={!canSubmit}
        >
          {submitting ? t("Creating…", "생성 중…") : t("Create", "생성")}
        </button>
        <span className="field-error" aria-hidden />
      </div>
    </form>
  );
}
