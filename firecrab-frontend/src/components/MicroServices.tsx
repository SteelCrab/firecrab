import { useEffect, useState } from "react";
import type { FormEvent } from "react";
import type { MicroServiceResponse, MicroServiceSpec } from "../bindings";
import {
  ApiClientError, createMicroService, deleteMicroService, getMicroService,
  listMicroServices, updateMicroService,
} from "../api/client";
import { useI18n } from "../i18n";
import ServiceDeployment from "./ServiceDeployment";

const DEFAULT_SPEC: MicroServiceSpec = {
  name: "", image: "", runtime: "vm", port: 8080, healthPath: "/health", domain: null,
  ram: 512, cpu: 1, replicas: 1, startupTimeout: 300, drainTimeout: 30, env: {}, secrets: {},
};

export default function MicroServices() {
  const { t } = useI18n();
  const [services, setServices] = useState<MicroServiceResponse[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  // undefined closes the editor; null creates a new configuration.
  const [editor, setEditor] = useState<MicroServiceResponse | null | undefined>(undefined);
  const [runtime, setRuntime] = useState<MicroServiceResponse | null>(null);

  const refresh = async () => {
    try {
      setServices(await listMicroServices());
      setError(null);
    } catch (failure) {
      setError((failure as Error).message);
    }
  };

  useEffect(() => {
    let active = true;
    listMicroServices().then((rows) => { if (active) setServices(rows); })
      .catch((failure) => { if (active) setError((failure as Error).message); });
    return () => { active = false; };
  }, []);

  const edit = async (id: string) => {
    if (busy) return;
    setBusy(true);
    try {
      setEditor(await getMicroService(id));
    } catch (failure) {
      setError((failure as Error).message);
    } finally {
      setBusy(false);
    }
  };

  const remove = async (service: MicroServiceResponse) => {
    if (busy || !window.confirm(t(`Delete MicroService "${service.name}"?`, `MicroService "${service.name}" 설정을 삭제할까요?`))) return;
    setBusy(true);
    try {
      await deleteMicroService(service.id);
      if (editor?.id === service.id) setEditor(undefined);
      if (runtime?.id === service.id) setRuntime(null);
      await refresh();
    } catch (failure) {
      setError((failure as Error).message);
    } finally {
      setBusy(false);
    }
  };

  return (
    <>
      <section className="panel" aria-labelledby="microservice-heading">
        <h2 className="panel-title" id="microservice-heading">MicroService</h2>
        <p className="poll-note">{t("Save a configuration, then deploy its image to MicroVMs and check app health.", "설정을 저장한 뒤 이미지를 MicroVM에 배포한다. 앱의 상태 검사 결과를 확인한다.")}</p>
        <div className="service-actions">
          <button className="btn primary" disabled={busy || editor !== undefined} onClick={() => setEditor(null)}>{t("New MicroService", "MicroService 추가")}</button>
          <button className="btn" disabled={busy} onClick={async () => { setBusy(true); await refresh(); setBusy(false); }}>{t("Refresh", "새로 고침")}</button>
        </div>
        {error && <p className="field-error" role="alert">{error}</p>}
        {services === null ? (
          !error && <p className="empty">{t("Loading…", "불러오는 중…")}</p>
        ) : services.length === 0 ? (
          <p className="empty">{t("No service configurations yet.", "저장된 서비스 설정이 없다.")}</p>
        ) : (
          <div className="table-scroll">
            <table className="vm-table service-table">
              <thead><tr>{[t("Name", "이름"), t("Image", "이미지"), t("Port", "포트"), t("Replicas", "복제 수"), "RAM / CPU", t("Domain", "도메인"), t("Actions", "작업")].map((label, index) => <th key={label} id={`service-column-${index}`} scope="col">{label}</th>)}</tr></thead>
              <tbody>{services.map((service) => (
                <tr key={service.id}>
                  <td headers="service-column-0" className="name" data-label={t("Name", "이름")}>{service.name}</td><td headers="service-column-1" className="mono service-image" data-label={t("Image", "이미지")}>{service.image}</td>
                  <td headers="service-column-2" data-label={t("Port", "포트")}>{service.port}</td><td headers="service-column-3" data-label={t("Replicas", "복제 수")}>{service.replicas}</td><td headers="service-column-4" data-label="RAM / CPU">{service.ram} MiB / {service.cpu}</td>
                  <td headers="service-column-5" className="mono" data-label={t("Domain", "도메인")}>{service.domain ?? "—"}</td>
                  <td headers="service-column-6" className="actions">
                    <button className="btn primary" disabled={busy || editor !== undefined} onClick={() => setRuntime(service)} aria-label={t(`Run ${service.name}`, `${service.name} 실행 관리`)}>{t("Run", "실행 관리")}</button>
                    <button className="btn" disabled={busy || editor !== undefined} onClick={() => edit(service.id)} aria-label={t(`Edit ${service.name}`, `${service.name} 수정`)}>{t("Edit", "수정")}</button>
                    <button className="btn danger" disabled={busy || editor !== undefined} onClick={() => remove(service)} aria-label={t(`Delete ${service.name}`, `${service.name} 삭제`)}>{t("Delete", "삭제")}</button>
                  </td>
                </tr>
              ))}</tbody>
            </table>
          </div>
        )}
      </section>
      {runtime && <ServiceDeployment key={`runtime-${runtime.id}`} service={runtime} onClose={() => setRuntime(null)} />}
      {editor !== undefined && <ServiceForm key={`editor-${editor?.id ?? "new"}`} service={editor} onCancel={() => setEditor(undefined)} onSaved={async () => { setEditor(undefined); setRuntime(null); await refresh(); }} />}
    </>
  );
}

type Pair = { key: string; value: string };

function ServiceForm({ service, onCancel, onSaved }: {
  service: MicroServiceResponse | null; onCancel: () => void; onSaved: () => Promise<void>;
}) {
  const { t } = useI18n();
  const [spec, setSpec] = useState<MicroServiceSpec>(service ?? DEFAULT_SPEC);
  const [env, setEnv] = useState<Pair[]>(Object.entries(spec.env).map(([key, value]) => ({ key, value })));
  const [secrets, setSecrets] = useState<Pair[]>(Object.entries(spec.secrets).map(([key, value]) => ({ key, value })));
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [fields, setFields] = useState<Record<string, string>>({});

  const change = <K extends keyof MicroServiceSpec>(key: K, value: MicroServiceSpec[K]) => setSpec((previous) => ({ ...previous, [key]: value }));
  const fieldError = (key: string) => fields[key] && <span className="field-error" role="alert">{fields[key]}</span>;

  const submit = async (event: FormEvent) => {
    event.preventDefault();
    if (saving) return;
    setError(null); setFields({});
    const maps: Record<string, Record<string, string>> = {};
    for (const [field, rows] of [["env", env], ["secrets", secrets]] as const) {
      const map: Record<string, string> = Object.create(null);
      for (const row of rows) {
        const key = row.key.trim();
        if (!key || Object.hasOwn(map, key)) {
          setFields({ [field]: t("Each row needs a unique variable name.", "각 행에 서로 다른 변수 이름이 필요하다.") });
          return;
        }
        map[key] = row.value;
      }
      maps[field] = map;
    }
    const request: MicroServiceSpec = {
      name: spec.name.trim(), image: spec.image.trim(), runtime: spec.runtime, port: spec.port,
      healthPath: spec.healthPath, domain: spec.domain?.trim() || null,
      ram: spec.ram, cpu: spec.cpu, replicas: spec.replicas,
      startupTimeout: spec.startupTimeout, drainTimeout: spec.drainTimeout,
      env: maps.env!, secrets: maps.secrets!,
    };
    setSaving(true);
    try {
      if (service) await updateMicroService(service.id, request);
      else await createMicroService(request);
      await onSaved();
    } catch (failure) {
      setError((failure as Error).message);
      if (failure instanceof ApiClientError) setFields(failure.apiError?.fields ?? {});
    } finally {
      setSaving(false);
    }
  };

  return (
    <section className="panel" aria-labelledby="service-editor-heading">
      <h2 className="panel-title" id="service-editor-heading">{service ? t(`Edit: ${service.name}`, `수정: ${service.name}`) : t("New MicroService", "MicroService 추가")}</h2>
      {error && <p className="field-error" role="alert">{error}</p>}
      <form className="create-grid service-form" onSubmit={submit}>
        <div className="field"><label htmlFor="service-name">{t("Name", "이름")}</label><input id="service-name" value={spec.name} onChange={(event) => change("name", event.target.value)} placeholder="orders" required maxLength={63} pattern={"[a-z0-9]([a-z0-9\\-]*[a-z0-9])?"} disabled={saving} />{fieldError("name")}</div>
        <div className="field service-wide"><label htmlFor="service-image">{t("OCI image", "OCI 이미지")}</label><input id="service-image" value={spec.image} onChange={(event) => change("image", event.target.value)} placeholder="ghcr.io/team/orders:1.0" required maxLength={2048} disabled={saving} />{fieldError("image")}</div>
        <div className="field"><label htmlFor="service-runtime">{t("Runtime", "실행 환경")}</label><select id="service-runtime" defaultValue="vm" disabled><option value="vm">MicroVM</option></select>{fieldError("runtime")}</div>
        <div className="field"><label htmlFor="service-port">{t("App port", "앱 포트")}</label><input id="service-port" type="number" min={1} max={65535} value={spec.port} onChange={(event) => change("port", event.target.valueAsNumber)} required disabled={saving} />{fieldError("port")}</div>
        <div className="field"><label htmlFor="service-health">{t("Health path", "상태 검사 경로")}</label><input id="service-health" value={spec.healthPath} onChange={(event) => change("healthPath", event.target.value)} placeholder="/actuator/health" required disabled={saving} />{fieldError("healthPath")}</div>
        <div className="field service-wide"><label htmlFor="service-domain">{t("Domain (optional)", "도메인 (선택)")}</label><input id="service-domain" value={spec.domain ?? ""} onChange={(event) => change("domain", event.target.value)} placeholder="orders.example.com" maxLength={253} disabled={saving} />{fieldError("domain")}</div>
        <div className="field"><label htmlFor="service-ram">RAM (MiB)</label><select id="service-ram" value={spec.ram} onChange={(event) => change("ram", Number(event.target.value))} disabled={saving}>{[128, 256, 512, 1024, 2048, 4096, 8192, 16384, 32768].map((size) => <option key={size} value={size}>{size}</option>)}</select>{fieldError("ram")}</div>
        <div className="field"><label htmlFor="service-cpu">CPU</label><input id="service-cpu" type="number" min={1} max={32} value={spec.cpu} onChange={(event) => change("cpu", event.target.valueAsNumber)} required disabled={saving} />{fieldError("cpu")}</div>
        <div className="field"><label htmlFor="service-replicas">{t("Replicas", "복제 수")}</label><input id="service-replicas" type="number" min={1} max={100} value={spec.replicas} onChange={(event) => change("replicas", event.target.valueAsNumber)} required disabled={saving} />{fieldError("replicas")}</div>
        <div className="field"><label htmlFor="service-startup">{t("Startup timeout (s)", "시작 제한 시간 (초)")}</label><input id="service-startup" type="number" min={1} max={3600} value={spec.startupTimeout} onChange={(event) => change("startupTimeout", event.target.valueAsNumber)} required disabled={saving} />{fieldError("startupTimeout")}</div>
        <div className="field"><label htmlFor="service-drain">{t("Drain timeout (s)", "요청 종료 대기 (초)")}</label><input id="service-drain" type="number" min={0} max={3600} value={spec.drainTimeout} onChange={(event) => change("drainTimeout", event.target.valueAsNumber)} required disabled={saving} />{fieldError("drainTimeout")}</div>
        <VariableRows kind="env" rows={env} setRows={setEnv} disabled={saving} />
        <div className="service-full">{fieldError("env")}</div>
        <VariableRows kind="secrets" rows={secrets} setRows={setSecrets} disabled={saving} />
        <div className="service-full">{fieldError("secrets")}</div>
        <div className="service-actions service-full">
          <button type="submit" className="btn primary" disabled={saving}>{saving ? t("Saving…", "저장 중…") : t("Save configuration", "설정 저장")}</button>
          <button type="button" className="btn" disabled={saving} onClick={onCancel}>{t("Cancel", "취소")}</button>
        </div>
      </form>
    </section>
  );
}

function VariableRows({ kind, rows, setRows, disabled }: {
  kind: "env" | "secrets"; rows: Pair[]; setRows: (rows: Pair[]) => void; disabled: boolean;
}) {
  const { t } = useI18n();
  const secret = kind === "secrets";
  return (
    <fieldset className="service-variables service-full" disabled={disabled}>
      <legend>{secret ? t("Secret references", "시크릿 참조") : t("Environment variables", "환경 변수")}</legend>
      {secret && <p className="poll-note">{t("Enter a CrabSecrets ID for each variable. Store sensitive values in CrabSecrets.", "변수마다 CrabSecrets ID를 입력한다. 민감한 값은 CrabSecrets에 저장한다.")}</p>}
      {rows.map((row, index) => (
        <div className="service-variable-row" key={index}>
          <div className="field"><label htmlFor={`service-${kind}-key-${index}`}>{t("Variable", "변수")}</label><input id={`service-${kind}-key-${index}`} value={row.key} maxLength={256} autoComplete="off" onChange={(event) => setRows(rows.map((pair, i) => i === index ? { ...pair, key: event.target.value } : pair))} required /></div>
          <div className="field"><label htmlFor={`service-${kind}-value-${index}`}>{secret ? "CrabSecrets ID" : t("Value", "값")}</label><input id={`service-${kind}-value-${index}`} value={row.value} maxLength={secret ? 36 : 4096} autoComplete="off" onChange={(event) => setRows(rows.map((pair, i) => i === index ? { ...pair, value: event.target.value } : pair))} required={secret} /></div>
          <button type="button" className="btn danger" aria-label={t(`Remove ${kind} row ${index + 1}`, `${kind} ${index + 1}행 삭제`)} onClick={() => setRows(rows.filter((_, i) => i !== index))}>{t("Remove", "삭제")}</button>
        </div>
      ))}
      <button type="button" className="btn" disabled={rows.length >= 64} onClick={() => setRows([...rows, { key: "", value: "" }])}>{secret ? t("Add secret reference", "시크릿 참조 추가") : t("Add variable", "변수 추가")}</button>
    </fieldset>
  );
}
