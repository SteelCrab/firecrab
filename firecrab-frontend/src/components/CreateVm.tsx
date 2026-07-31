import { useEffect, useState } from "react";
import type { FormEvent } from "react";
import { ApiClientError, createVm, listMicroNetworks, listStorageRoots } from "../api/client";
import type {
  CreateVmRequest,
  EgressPolicy,
  MicroNetworkResponse,
  StorageRootResponse,
  VmResponse,
} from "../bindings";
import RamStepper from "./RamStepper";

/** The registry aliases the API accepts today; selection only, no free text. */
const TEMPLATES = ["ubuntu-26.04", "alpine-3.24"] as const;

const EGRESS_POLICY_LABEL: Record<EgressPolicy, string> = {
  internet: "인터넷 허용",
  isolated: "격리(게이트웨이만 허용)",
};

const FIELDS_WITH_OWN_ERROR = [
  "name",
  "cpu",
  "ram",
  "template",
  "diskGb",
  "microNetworkId",
  "storageRoot",
] as const;

/** `<select>` value standing in for "no MicroNetwork" — the built-in default
 *  network, which the API represents as a null microNetworkId. */
const DEFAULT_NETWORK = "";

interface CreateVmProps {
  onCreated: (vm: VmResponse) => void;
  onError: (message: string) => void;
}

function storageLabel(root: StorageRootResponse): string {
  const free = root.availableGib > 0 ? ` · ${root.availableGib} GiB free` : "";
  const label = root.name && root.name !== root.id ? `${root.name}` : root.id;
  return `${label} (${root.path})${free}`;
}

export default function CreateVm({ onCreated, onError }: CreateVmProps) {
  const [name, setName] = useState("");
  const [template, setTemplate] = useState<string>(TEMPLATES[0]);
  const [cpu, setCpu] = useState("1");
  const [ram, setRam] = useState("512");
  const [diskGb, setDiskGb] = useState("2");
  const [egressPolicy, setEgressPolicy] = useState<EgressPolicy>("internet");
  const [microNetworkId, setMicroNetworkId] = useState<string>(DEFAULT_NETWORK);
  const [microNetworks, setMicroNetworks] = useState<MicroNetworkResponse[]>([]);
  const [storageRoots, setStorageRoots] = useState<StorageRootResponse[]>([]);
  const [storageRoot, setStorageRoot] = useState<string>("");
  const [submitting, setSubmitting] = useState(false);
  const [fieldErrors, setFieldErrors] = useState<ApiClientError | null>(null);

  // Failing to load the list isn't surfaced: the dropdown then just offers
  // the default network, which is exactly what this form did before
  // MicroNetworks existed.
  useEffect(() => {
    listMicroNetworks().then(setMicroNetworks).catch(() => setMicroNetworks([]));
    listStorageRoots()
      .then((roots) => {
        setStorageRoots(roots);
        if (roots.length > 0) {
          setStorageRoot((current) => current || roots[0].id);
        }
      })
      .catch(() => setStorageRoots([]));
  }, []);

  const handleSubmit = async (event: FormEvent) => {
    event.preventDefault();
    if (submitting) return;

    const request: CreateVmRequest = {
      name: name.trim(),
      template,
      cpu: parseInt(cpu, 10) || 0,
      ram: parseInt(ram, 10) || 0,
      diskGb: parseInt(diskGb, 10) || 0,
      egressPolicy,
      microNetworkId: microNetworkId === DEFAULT_NETWORK ? null : microNetworkId,
      storageRoot: storageRoot || null,
    };

    setSubmitting(true);
    setFieldErrors(null);
    try {
      const vm = await createVm(request);
      setName("");
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
        <label htmlFor="vm-template">template</label>
        <select id="vm-template" value={template} onChange={(event) => setTemplate(event.target.value)}>
          {TEMPLATES.map((alias) => (
            <option key={alias} value={alias}>
              {alias}
            </option>
          ))}
        </select>
        {fieldError("template")}
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
          min={2}
          max={500}
          value={diskGb}
          onChange={(event) => setDiskGb(event.target.value)}
        />
        {fieldError("diskGb")}
      </div>
      {storageRoots.length > 0 && (
        <div className="field">
          <label htmlFor="vm-storage">저장 위치</label>
          <select
            id="vm-storage"
            value={storageRoot}
            onChange={(event) => setStorageRoot(event.target.value)}
          >
            {storageRoots.map((root) => (
              <option key={root.id} value={root.id}>
                {storageLabel(root)}
              </option>
            ))}
          </select>
          {fieldError("storageRoot")}
        </div>
      )}
      <div className="field">
        <label htmlFor="vm-micro-network">MicroNetwork</label>
        <select
          id="vm-micro-network"
          value={microNetworkId}
          onChange={(event) => setMicroNetworkId(event.target.value)}
        >
          <option value={DEFAULT_NETWORK}>기본 네트워크</option>
          {microNetworks.map((network) => (
            <option key={network.id} value={network.id}>
              {network.name} ({network.subnetCidr})
            </option>
          ))}
        </select>
        {fieldError("microNetworkId")}
      </div>
      <div className="field">
        <label htmlFor="vm-egress-policy">외부 통신</label>
        <select
          id="vm-egress-policy"
          value={egressPolicy}
          onChange={(event) => setEgressPolicy(event.target.value as EgressPolicy)}
        >
          {(Object.keys(EGRESS_POLICY_LABEL) as EgressPolicy[]).map((policy) => (
            <option key={policy} value={policy}>
              {EGRESS_POLICY_LABEL[policy]}
            </option>
          ))}
        </select>
      </div>
      <div className="field">
        <label>&nbsp;</label>
        <button className="btn primary" type="submit" disabled={submitting}>
          {submitting ? "생성 중…" : "생성"}
        </button>
        <span className="field-error"></span>
      </div>
    </form>
  );
}
