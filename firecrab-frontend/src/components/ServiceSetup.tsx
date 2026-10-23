import { useCallback, useEffect, useRef, useState } from "react";
import type { FormEvent, ReactNode } from "react";
import { ArrowLeft, ArrowRight, Box, Check, CheckCircle2, Info, SlidersHorizontal } from "lucide-react";
import type { MicroNetworkResponse, MicroServiceResponse, MicroServiceSpec, SecurityGroupResponse } from "../bindings";
import { ApiClientError, createMicroService, listMicroNetworks, listSecurityGroups, updateMicroService } from "../api/client";
import { getPipeAuth } from "../api/pipeAuth";
import ServiceAppPicker, { type ServiceAppSelection } from "./ServiceAppPicker";
import { useI18n } from "../i18n";

const DEFAULT_SPEC: MicroServiceSpec = {
  name: "", image: "", runtime: "vm", port: 8080, healthPath: "/health", domain: null,
  ram: 512, cpu: 1, replicas: 1, startupTimeout: 300, drainTimeout: 30, env: {}, secrets: {},
};
const FIELD_STEP: Record<string, number> = {
  name: 0, image: 0, runtime: 0, port: 1, healthPath: 1, domain: 1, securityGroupId: 1,
  ram: 2, cpu: 2, replicas: 2, startupTimeout: 2, drainTimeout: 2, env: 2, secrets: 2,
};
type Pair = { key: string; value: string };

export default function ServiceSetup({ service, onCancel, onSaved }: {
  service: MicroServiceResponse | null;
  onCancel: () => void;
  onSaved: (service: MicroServiceResponse) => Promise<void>;
}) {
  const { t } = useI18n();
  const [app, setApp] = useState<ServiceAppSelection | null>(null);
  const [step, setStep] = useState(0);
  const [spec, setSpec] = useState<MicroServiceSpec>(service ?? DEFAULT_SPEC);
  const [env, setEnv] = useState<Pair[]>(Object.entries(spec.env).map(([key, value]) => ({ key, value })));
  const [secrets, setSecrets] = useState<Pair[]>(Object.entries(spec.secrets).map(([key, value]) => ({ key, value })));
  const [advanced, setAdvanced] = useState(false);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [fields, setFields] = useState<Record<string, string>>({});
  const [groups, setGroups] = useState<SecurityGroupResponse[] | null>(null);
  const [networks, setNetworks] = useState<MicroNetworkResponse[]>([]);
  const [groupError, setGroupError] = useState<string | null>(null);
  const [groupVersion, setGroupVersion] = useState(0);
  useEffect(() => {
    let active = true;
    Promise.all([listSecurityGroups(), listMicroNetworks()])
      .then(([rows, nets]) => { if (active) { setGroups(rows); setNetworks(nets); setGroupError(null); } })
      .catch((failure) => { if (active) setGroupError((failure as Error).message); });
    return () => { active = false; };
  }, [groupVersion]);
  const heading = useRef<HTMLHeadingElement>(null);
  const steps = [t("App", "앱 선택"), t("Connection", "접속 설정"), t("Resources", "실행 설정"), t("Review", "확인")];
  const titles = [t("What would you like to run?", "어떤 앱을 실행할까요?"), t("How does your app respond?", "앱에 어떻게 접속할까요?"), t("Start with the recommended settings", "권장 설정으로 시작한다"), t("Ready to save your app?", "마지막으로 설정을 확인한다")];
  const descriptions = [
    t("Choose an app and version from an account connected in AUTH.", "AUTH에 연결된 계정에서 앱과 버전을 선택한다."),
    t("Use the port and health path that your app actually serves.", "앱이 실제로 사용하는 포트와 상태 검사 경로를 입력한다."),
    t("These defaults are a starting point. Adjust them to your app and host capacity.", "기본값으로 시작할 수 있다. 앱과 호스트 용량에 맞게 조정한다."),
    t("Saving creates a configuration. You will choose a host port and deploy in the next screen.", "저장하면 설정이 만들어진다. 다음 화면에서 호스트 포트를 정하고 배포한다."),
  ];
  useEffect(() => { heading.current?.focus(); }, [step]);

  const change = <K extends keyof MicroServiceSpec>(key: K, value: MicroServiceSpec[K]) => {
    setSpec((previous) => ({ ...previous, [key]: value }));
    setFields((previous) => { const next = { ...previous }; delete next[key]; return next; });
  };
  const control = (key: string) => ({
    id: `service-${key}`, "aria-describedby": `service-${key}-hint${fields[key] ? ` service-${key}-error` : ""}`,
    "aria-invalid": !!fields[key], disabled: saving,
  });
  const showErrors = (errors: Record<string, string>) => {
    setFields(errors);
    const target = FIELD_STEP[Object.keys(errors)[0] ?? ""];
    if (target !== undefined) setStep(target);
    if (["startupTimeout", "drainTimeout", "env", "secrets"].some((field) => field in errors)) setAdvanced(true);
  };
  const variables = () => {
    const maps = { env: Object.create(null) as Record<string, string>, secrets: Object.create(null) as Record<string, string> };
    for (const [kind, rows] of [["env", env], ["secrets", secrets]] as const) {
      for (const row of rows) {
        const key = row.key.trim();
        if (!/^[A-Za-z_][A-Za-z0-9_]*$/.test(key) || Object.hasOwn(maps[kind], key)) {
          showErrors({ [kind]: t("Use a unique variable name with letters, digits and underscores. Start with a letter or underscore.", "서로 다른 변수 이름을 입력한다. 영문자나 밑줄로 시작하고 영문자, 숫자, 밑줄만 사용한다.") });
          return null;
        }
        maps[kind][key] = row.value;
      }
    }
    if (env.length + secrets.length > 64) {
      showErrors({ env: t("Use at most 64 variables and secret references in total.", "환경 변수와 시크릿 참조는 합계 64개까지 입력한다.") });
      return null;
    }
    if (Object.keys(maps.secrets).some((key) => Object.hasOwn(maps.env, key))) {
      showErrors({ secrets: t("A name cannot appear in both environment variables and secret references.", "환경 변수와 시크릿 참조에 같은 이름을 사용할 수 없다.") });
      return null;
    }
    return maps;
  };
  const selectApp = useCallback((selection: ServiceAppSelection | null) => {
    setApp(selection);
    setSpec((previous) => ({ ...previous, image: selection?.reference ?? "" }));
    if (selection) setFields((previous) => { const next = { ...previous }; delete next.image; return next; });
  }, []);
  const verifyAccount = async () => {
    if (!app || app.reference !== spec.image) {
      showErrors({ image: t("Select an app and version from a connected AUTH account.", "AUTH에 연결된 계정에서 앱과 버전을 선택한다.") });
      return false;
    }
    try {
      const rows = await getPipeAuth();
      const previous = app.connection;
      if (!rows.some((row) => row.provider === previous.provider && row.account === previous.account && row.registry === previous.registry && row.region === previous.region && row.services.includes("registry"))) {
        setApp(null);
        showErrors({ image: t("This AUTH account is no longer connected. Select a connected account again.", "이 AUTH 계정의 연결이 변경되었다. 연결된 계정에서 앱을 다시 선택한다.") });
        return false;
      }
      return true;
    } catch (failure) {
      setError((failure as Error).message);
      setStep(0);
      return false;
    }
  };
  const submit = async (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    if (saving || groups === null || groupError) return;
    if (!event.currentTarget.reportValidity()) return;
    if (step === 1 && (!spec.healthPath.startsWith("/") || spec.healthPath.startsWith("//") || /[^\x21-\x7e]|[?#\\]/.test(spec.healthPath))) {
      showErrors({ healthPath: t("Start with one / and omit spaces, query strings and fragments.", "/로 시작하는 경로를 입력한다. 공백, 쿼리 문자열, #은 제외한다.") });
      return;
    }
    const maps = variables();
    if (!maps) return;
    if (step === 0 || step === 3) {
      setSaving(true);
      const connected = await verifyAccount();
      setSaving(false);
      if (!connected) return;
    }
    if (step < 3) { setError(null); setFields({}); setStep(step + 1); return; }
    setError(null); setFields({}); setSaving(true);
    const request: MicroServiceSpec = {
      ...spec, name: spec.name.trim(), image: spec.image.trim(), domain: normalizedDomain(spec.domain), ...maps,
    };
    // Copy only specification fields; an edited response also contains its ID.
    const { name, image, runtime, port, healthPath, domain, ram, cpu, replicas, startupTimeout, drainTimeout, securityGroupId } = request;
    try {
      const body = { name, image, runtime, port, healthPath, domain, ram, cpu, replicas, startupTimeout, drainTimeout, securityGroupId, ...maps };
      const saved = service ? await updateMicroService(service.id, body) : await createMicroService(body);
      await onSaved(saved);
    } catch (failure) {
      setError((failure as Error).message);
      if (failure instanceof ApiClientError) showErrors(failure.apiError?.fields ?? {});
    } finally { setSaving(false); }
  };
  const unsupported = secrets.length > 0;
  const reviewSections: { title: string; step: number; values: [string, string | number][] }[] = [
    { title: steps[0]!, step: 0, values: [[t("Name", "이름"), spec.name], [t("Image", "이미지"), spec.image]] },
    { title: steps[1]!, step: 1, values: [[t("App port", "앱 포트"), spec.port], [t("Health path", "상태 검사 경로"), spec.healthPath], [t("Domain", "도메인"), spec.domain || t("None", "없음")]] },
    { title: steps[2]!, step: 2, values: [[t("Per instance", "앱 1개당"), `${spec.ram} MiB / ${spec.cpu} CPU`], [t("Replicas", "복제 수"), spec.replicas], [t("Variables / secret references", "환경 변수 / 시크릿 참조"), `${env.length} / ${secrets.length}`]] },
  ];

  return (
    <section className="service-setup" aria-labelledby="service-editor-heading" data-editing>
      <button type="button" className="btn ghost service-back" disabled={saving} onClick={onCancel}><ArrowLeft aria-hidden="true" />{t("Back to services", "서비스 목록으로")}</button>
      <div className="service-setup-heading"><span className="service-eyebrow">{t("GUIDED SETUP", "단계별 설정")}</span><h2 id="service-editor-heading">{service ? t(`Edit: ${service.name}`, `수정: ${service.name}`) : t("New MicroService", "MicroService 추가")}</h2><p>{t("One step at a time. You can go back without losing your settings.", "한 단계씩 설정한다. 이전 단계로 돌아가도 입력값을 유지한다.")}</p></div>
      <nav aria-label={t("Setup steps", "설정 단계")}><ol className="service-steps">{steps.map((label, index) => <li key={index} className={index === step ? "current" : index < step ? "complete" : ""}><button type="button" disabled={saving || index > step} aria-current={index === step ? "step" : undefined} onClick={() => setStep(index)}><span className="service-step-number">{index < step ? <Check aria-hidden="true" /> : index + 1}</span><span>{label}</span></button></li>)}</ol></nav>
      <div className="service-setup-layout">
        <form className="service-setup-card" onSubmit={submit} onInvalidCapture={(event) => { const details = (event.target as HTMLElement).closest("details"); if (details) { details.open = true; setAdvanced(true); } }} noValidate>
          <div className="service-step-heading"><span className="service-eyebrow">{t(`STEP ${step + 1} OF 4`, `4단계 중 ${step + 1}단계`)}</span><h3 ref={heading} tabIndex={-1}>{titles[step]}</h3><p>{descriptions[step]}</p></div>
          {error && <p className="service-notice service-notice-error" role="alert">{error}</p>}
          <div className="service-step-fields">
            {step === 0 && <>
              <Field name="name" label={t("Name", "이름")} hint={t("Use lowercase letters, digits and hyphens. Example: my-app.", "영문 소문자, 숫자, 하이픈을 사용한다. 예: my-app.")} error={fields.name}><input {...control("name")} value={spec.name} onChange={(event) => change("name", event.target.value)} placeholder="my-app" required maxLength={63} pattern="[a-z0-9]([a-z0-9\-]*[a-z0-9])?" autoComplete="off" /></Field>
              <ServiceAppPicker value={spec.image} disabled={saving} onChange={selectApp} />
              {fields.image && <p className="field-error" role="alert">{fields.image}</p>}
              <p className="service-hint"><CheckCircle2 aria-hidden="true" />{t("Runs in an isolated MicroVM. No manual VM setup is needed.", "격리된 MicroVM에서 실행한다. VM을 직접 만들 필요가 없다.")}</p>
            </>}
            {step === 1 && <>
              <Field name="port" label={t("App port", "앱 포트")} hint={t("The port inside your app. nginx uses 80. Many web apps use 8080.", "앱 내부에서 사용하는 포트이다. nginx는 80번을 사용한다. 웹 앱은 보통 8080번을 사용한다.")} error={fields.port}><input {...control("port")} type="number" min={1} max={65535} value={Number.isFinite(spec.port) ? spec.port : ""} onChange={(event) => change("port", event.target.valueAsNumber)} required /></Field>
              <Field name="healthPath" label={t("Health path", "상태 검사 경로")} hint={t("Firecrab requests this path to check your app. It must return HTTP 2xx. Use / for the nginx example.", "Firecrab이 이 경로에 요청을 보낸다. HTTP 2xx 응답이 와야 정상으로 판단한다. nginx 예제는 /를 사용한다.")} error={fields.healthPath}><input {...control("healthPath")} value={spec.healthPath} onChange={(event) => change("healthPath", event.target.value)} placeholder="/health" required maxLength={2048} spellCheck={false} /></Field>
              <div className="service-notice"><Info aria-hidden="true" /><p>{t("You will choose the external host port after saving. You can start without a domain.", "저장한 뒤 외부 접속용 호스트 포트를 정한다. 도메인 없이 시작할 수 있다.")}</p></div>
              <Field name="securityGroupId" label={t("Security group", "보안그룹")} hint={t("New replicas inherit this group's rules and MicroNetwork.", "새 복제 VM은 이 그룹의 규칙과 MicroNetwork를 상속한다.")} error={fields.securityGroupId}>
                <select {...control("securityGroupId")} value={spec.securityGroupId ?? ""} disabled={saving || groups === null || !!groupError} onChange={(event) => change("securityGroupId", event.target.value || null)}>
                  <option value="">{groups === null ? t("Loading…", "불러오는 중…") : t("None — use network defaults", "없음 — 네트워크 기본 설정 사용")}</option>
                  {spec.securityGroupId && !groups?.some((group) => group.id === spec.securityGroupId) && <option value={spec.securityGroupId}>{t("Unavailable group", "그룹을 확인할 수 없다")} · {spec.securityGroupId}</option>}
                  {groups?.map((group) => <option key={group.id} value={group.id}>{group.name} · {networks.find((network) => network.id === group.microNetworkId)?.name ?? group.microNetworkId}</option>)}
                </select>
                {groupError && <div><p className="field-error" role="alert">{groupError}</p><button type="button" className="btn" onClick={() => setGroupVersion((value) => value + 1)}>{t("Retry loading security groups", "보안그룹 다시 불러오기")}</button></div>}
              </Field>
              <details className="service-advanced" open={!!fields.domain || undefined}><summary>{t("Domain configuration", "도메인 설정")}</summary><Field name="domain" label={t("Domain (optional)", "도메인 (선택)")} hint={t("Healthy replicas connect automatically. Public access uses Pipe Tunnel and your DNS.", "정상 인스턴스를 자동 연결한다. 외부 접속에는 Pipe Tunnel과 DNS 연결이 필요하다.")} error={fields.domain}><input {...control("domain")} value={spec.domain ?? ""} onChange={(event) => change("domain", event.target.value)} placeholder="app.example.com" maxLength={253} /></Field></details>
            </>}
            {step === 2 && <>
              <div className="service-resource-grid">
                <Field name="ram" label={t("Memory per instance", "앱 1개당 메모리")} hint={t("512 MiB is the default starting size.", "기본값은 512 MiB이다.")} error={fields.ram}><select {...control("ram")} value={spec.ram} onChange={(event) => change("ram", Number(event.target.value))}>{[128, 256, 512, 1024, 2048, 4096, 8192, 16384, 32768].map((size) => <option key={size} value={size}>{size >= 1024 ? `${size / 1024} GiB` : `${size} MiB`}{size === 512 ? t(" · default", " · 기본값") : ""}</option>)}</select></Field>
                <Field name="cpu" label={t("CPU per instance", "앱 1개당 CPU")} hint={t("1 CPU is the default starting size.", "기본값은 CPU 1개이다.")} error={fields.cpu}><input {...control("cpu")} type="number" min={1} max={32} value={Number.isFinite(spec.cpu) ? spec.cpu : ""} onChange={(event) => change("cpu", event.target.valueAsNumber)} required /></Field>
                <Field name="replicas" label={t("Replicas", "복제 수")} hint={t("How many copies of your app run at once. Start with 1.", "동시에 실행할 앱의 개수이다. 1개로 시작한다.")} error={fields.replicas}><input {...control("replicas")} type="number" min={1} max={100} value={Number.isFinite(spec.replicas) ? spec.replicas : ""} onChange={(event) => change("replicas", event.target.valueAsNumber)} required /></Field>
              </div>
              <details className="service-advanced" open={advanced} onToggle={(event) => setAdvanced(event.currentTarget.open)}><summary><SlidersHorizontal aria-hidden="true" />{t("Advanced settings", "고급 설정")}<span>{t("Optional", "선택")}</span></summary><p className="service-hint">{t("Keep the defaults unless your app needs different timeouts or variables.", "시간 제한이나 변수가 필요할 때만 변경한다.")}</p>
                <div className="service-resource-grid">
                  <Field name="startupTimeout" label={t("Startup timeout (s)", "시작 제한 시간 (초)")} hint={t("How long to wait for the app to become healthy after boot.", "부팅 후 앱이 정상 응답할 때까지 기다리는 시간이다.")} error={fields.startupTimeout}><input {...control("startupTimeout")} type="number" min={1} max={3600} value={Number.isFinite(spec.startupTimeout) ? spec.startupTimeout : ""} onChange={(event) => change("startupTimeout", event.target.valueAsNumber)} required /></Field>
                  <Field name="drainTimeout" label={t("Drain timeout (s)", "요청 종료 대기 (초)")} hint={t("Wait for existing requests to finish before removing service instances.", "서비스 인스턴스를 삭제하기 전에 기존 요청이 끝나도록 기다리는 시간이다.")} error={fields.drainTimeout}><input {...control("drainTimeout")} type="number" min={0} max={3600} value={Number.isFinite(spec.drainTimeout) ? spec.drainTimeout : ""} onChange={(event) => change("drainTimeout", event.target.valueAsNumber)} required /></Field>
                </div>
                <VariableRows kind="env" rows={env} setRows={setEnv} disabled={saving} error={fields.env} />
                <VariableRows kind="secrets" rows={secrets} setRows={setSecrets} disabled={saving} error={fields.secrets} />
              </details>
            </>}
            {step === 3 && <>
              <div className="service-review">{reviewSections.map(({ title, step: index, values }) => <section key={index}><div className="service-review-heading"><h4>{title}</h4><button type="button" className="btn ghost sm" disabled={saving} onClick={() => setStep(index)} aria-label={t(`Edit ${title}`, `${title} 수정`)}>{t("Edit", "수정")}</button></div><dl>{values.map(([label, value]) => <div key={label}><dt>{label}</dt><dd>{value}</dd></div>)}</dl></section>)}</div>
              {unsupported && <div className="service-notice service-notice-warning"><Info aria-hidden="true" /><p>{t("You can save this configuration, but deployment requires no secret references. Edit Resources to clear them.", "이 설정은 저장할 수 있다. 배포하려면 시크릿 참조를 제거해야 한다. 실행 설정에서 수정한다.")}</p></div>}
              <div className="service-notice"><CheckCircle2 aria-hidden="true" /><p>{t("Your app starts only when you select Deploy and run on the next screen.", "다음 화면에서 배포 및 실행을 선택해야 앱이 시작된다.")}</p></div>
            </>}
          </div>
          <footer className="service-wizard-footer"><button type="button" className="btn ghost" disabled={saving} onClick={onCancel}>{t("Cancel", "취소")}</button><div>{step > 0 && <button type="button" className="btn" disabled={saving} onClick={() => setStep(step - 1)}><ArrowLeft aria-hidden="true" />{t("Previous", "이전")}</button>}<button type="submit" className="btn primary" disabled={saving || (step === 0 && !app)}>{saving ? t("Checking…", "확인 중…") : step === 3 ? t("Save configuration", "설정 저장") : t("Continue", "다음")}<ArrowRight aria-hidden="true" /></button></div></footer>
        </form>
        <aside className="service-setup-summary" aria-label={t("Configuration summary", "설정 요약")}><div className="service-summary-icon"><Box aria-hidden="true" /></div><h3>{spec.name || t("Your new app", "새로운 앱")}</h3><p className="service-summary-image">{spec.image || t("Choose an image to get started.", "이미지를 선택하면 여기에 표시한다.")}</p><dl><div><dt>{t("Runtime", "실행 환경")}</dt><dd>MicroVM</dd></div><div><dt>{t("App port", "앱 포트")}</dt><dd>{Number.isFinite(spec.port) ? spec.port : "—"}</dd></div><div><dt>{t("Memory per instance", "앱 1개당 메모리")}</dt><dd>{spec.ram} MiB</dd></div><div><dt>CPU × {t("replicas", "복제 수")}</dt><dd>{Number.isFinite(spec.cpu) ? spec.cpu : "—"} × {Number.isFinite(spec.replicas) ? spec.replicas : "—"}</dd></div></dl><div className="service-summary-note"><Info aria-hidden="true" /><p>{t("Each replica runs in its own MicroVM and uses host resources until stopped.", "복제본마다 MicroVM 1개를 사용한다. 중지할 때까지 호스트 자원을 사용한다.")}</p></div></aside>
      </div>
    </section>
  );
}

function Field({ name, label, hint, error, children }: { name: string; label: string; hint: string; error?: string; children: ReactNode }) {
  return <div className="field"><label htmlFor={`service-${name}`}>{label}</label>{children}<span className="service-hint" id={`service-${name}-hint`}>{hint}</span>{error && <span className="field-error" id={`service-${name}-error`} role="alert">{error}</span>}</div>;
}

function VariableRows({ kind, rows, setRows, disabled, error }: { kind: "env" | "secrets"; rows: Pair[]; setRows: (rows: Pair[]) => void; disabled: boolean; error?: string }) {
  const { t } = useI18n();
  const secret = kind === "secrets";
  return <fieldset className="service-variables" disabled={disabled} aria-describedby={error ? `service-${kind}-error` : undefined}>
    <legend>{secret ? t("Secret references · save only", "시크릿 참조 · 저장만 지원") : t("Environment variables", "환경 변수")}</legend>
    <p className="service-hint">{secret ? t("PipeSecrets injection is not supported by the current deployer. Save references here for later; remove them before deploying.", "현재 배포 기능은 PipeSecrets 주입을 지원하지 않는다. 참조를 저장할 수 있지만 배포하려면 제거해야 한다.") : t("Extra settings passed to your app, such as NODE_ENV=production. Do not put passwords here.", "앱에 전달할 추가 설정이다. 예: NODE_ENV=production. 비밀번호는 입력하지 않는다.")}</p>
    {error && <p className="field-error" id={`service-${kind}-error`} role="alert">{error}</p>}
    {rows.map((row, index) => <div className="service-variable-row" key={index}>
      <div className="field"><label htmlFor={`service-${kind}-key-${index}`}>{t("Variable", "변수")}</label><input id={`service-${kind}-key-${index}`} value={row.key} maxLength={256} autoComplete="off" onChange={(event) => setRows(rows.map((pair, i) => i === index ? { ...pair, key: event.target.value } : pair))} required /></div>
      <div className="field"><label htmlFor={`service-${kind}-value-${index}`}>{secret ? "PipeSecrets ID" : t("Value", "값")}</label><input id={`service-${kind}-value-${index}`} value={row.value} maxLength={secret ? 36 : 4096} autoComplete="off" pattern={secret ? "[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}" : undefined} onChange={(event) => setRows(rows.map((pair, i) => i === index ? { ...pair, value: event.target.value } : pair))} required={secret} /></div>
      <button type="button" className="btn danger" aria-label={t(`Remove ${kind} row ${index + 1}`, `${kind} ${index + 1}행 삭제`)} onClick={() => setRows(rows.filter((_, i) => i !== index))}>{t("Remove", "삭제")}</button>
    </div>)}
    <button type="button" className="btn" disabled={rows.length >= 64} onClick={() => setRows([...rows, { key: "", value: "" }])}>{secret ? t("Add secret reference", "시크릿 참조 추가") : t("Add variable", "변수 추가")}</button>
  </fieldset>;
}

/** Accept a pasted site address without silently accepting a path or query. */
function normalizedDomain(value: string | null): string | null {
  const input = value?.trim().toLowerCase();
  if (!input) return null;
  if (/^https?:\/\//.test(input)) {
    try {
      const url = new URL(input);
      if (url.pathname === "/" && !url.search && !url.hash && !url.port && !url.username && !url.password) return url.hostname.replace(/\.$/, "");
    } catch { /* Keep invalid input for the API's domain field error. */ }
  }
  return input.replace(/\.$/, "");
}
