import { cloneElement, useId, useState } from "react";
import type { FormEvent, ReactElement } from "react";
import { useI18n } from "../i18n";
import { providerLabel, saveSecret, saveSecretConnection } from "../api/secrets";
import type { ConnectionSettings, SecretConnection, SecretProvider, SecretSummary } from "../api/secrets";

export function SecretEditor({ secret, onSaved, onCancel }: {
  secret: SecretSummary | null; onSaved: () => Promise<void>; onCancel: () => void;
}) {
  const { t } = useI18n();
  const [name, setName] = useState(secret?.name ?? "");
  const [description, setDescription] = useState(secret?.description ?? "");
  const [value, setValue] = useState("");
  const [replace, setReplace] = useState(!secret);
  const [visible, setVisible] = useState(false);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState("");

  async function submit(event: FormEvent) {
    event.preventDefault();
    if (saving) return;
    setSaving(true); setError("");
    try {
      await saveSecret({ name, description, ...(replace ? { value } : {}), ...(secret ? { version: secret.version } : {}) }, secret?.id);
      setValue("");
      await onSaved();
    } catch (failure) { setError((failure as Error).message); }
    finally { setSaving(false); }
  }

  return <section className="panel secrets-panel" aria-labelledby="secret-editor-heading">
    <h2 className="panel-title" id="secret-editor-heading">{secret ? t(`Edit secret: ${secret.name}`, `시크릿 수정: ${secret.name}`) : t("New secret", "시크릿 추가")}</h2>
    <form className="secrets-form" onSubmit={submit}>
      <fieldset disabled={saving}>
        <Field label={t("Secret name", "시크릿 이름")}><input autoFocus required maxLength={512} autoComplete="off" value={name} onChange={(event) => setName(event.target.value)} placeholder="DB_PASSWORD" /></Field>
        <Field label={t("Description", "설명")}><input maxLength={1000} value={description} onChange={(event) => setDescription(event.target.value)} /></Field>
        {secret && <label className="secrets-checkbox"><input type="checkbox" checked={replace} onChange={(event) => { setReplace(event.target.checked); setValue(""); }} />{t("Replace secret value", "시크릿 값 교체")}</label>}
        {replace && <>
          <Field label={t("Secret value", "시크릿 값")}><textarea rows={4} autoComplete="off" autoCapitalize="off" spellCheck={false} maxLength={16384} className={`mono ${visible ? "" : "secrets-masked-input"}`} value={value} onChange={(event) => setValue(event.target.value)} /></Field>
          <label className="secrets-checkbox"><input type="checkbox" checked={visible} onChange={(event) => setVisible(event.target.checked)} />{t("Show entered value", "입력한 값 표시")}</label>
          <p className="poll-note">{t("An empty value is allowed. Existing values are never loaded into this form.", "빈 값을 저장할 수 있다. 기존 값은 이 입력란에 불러오지 않는다.")}</p>
        </>}
        {error && <p className="field-error" role="alert">{error}</p>}
        <div className="secrets-actions"><button className="btn primary" type="submit">{saving ? t("Saving…", "저장 중…") : t("Save secret", "시크릿 저장")}</button><button className="btn" type="button" onClick={onCancel}>{t("Cancel", "취소")}</button></div>
      </fieldset>
    </form>
  </section>;
}

const EMPTY_SETTINGS: ConnectionSettings = { baseUrl: "", projectId: "", environment: "", secretPath: "/", region: "ap-northeast-2", prefix: "" };

export function SecretConnectionEditor({ connection, onSaved, onCancel }: {
  connection: SecretConnection | null; onSaved: () => Promise<void>; onCancel: () => void;
}) {
  const { t } = useI18n();
  const [name, setName] = useState(connection?.name ?? "");
  const [provider, setProvider] = useState<SecretProvider>(connection?.provider ?? "infisical");
  const [settings, setSettings] = useState<ConnectionSettings>(connection?.settings ?? { ...EMPTY_SETTINGS, baseUrl: "https://us.infisical.com" });
  const [credentials, setCredentials] = useState<Record<string, string>>({});
  const [cleared, setCleared] = useState<Set<string>>(new Set());
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState("");
  const updateSetting = (key: keyof ConnectionSettings, value: string) => setSettings((previous) => ({ ...previous, [key]: value }));

  async function submit(event: FormEvent) {
    event.preventDefault();
    if (saving) return;
    setSaving(true); setError("");
    const supplied = Object.fromEntries(Object.entries(credentials).filter(([, value]) => value !== ""));
    for (const key of cleared) supplied[key] = "";
    try {
      await saveSecretConnection({ name, provider, settings, credentials: supplied, ...(connection ? { version: connection.version } : {}) }, connection?.id);
      setCredentials({});
      await onSaved();
    } catch (failure) { setError((failure as Error).message); }
    finally { setSaving(false); }
  }

  function credential(key: string, label: string, optional = false) {
    return <div key={key}>
      <Field label={label}><input type="password" autoComplete="new-password" maxLength={8192} required={!connection && !optional} disabled={cleared.has(key)} value={credentials[key] ?? ""} onChange={(event) => setCredentials((previous) => ({ ...previous, [key]: event.target.value }))} placeholder={connection ? t("Leave blank to keep saved credential", "비워 두면 저장한 인증 정보를 유지한다") : ""} /></Field>
      {connection && optional && <label className="secrets-checkbox"><input type="checkbox" checked={cleared.has(key)} onChange={(event) => setCleared((previous) => { const next = new Set(previous); if (event.target.checked) next.add(key); else next.delete(key); return next; })} />{t(`Clear saved ${label}`, `저장된 ${label} 삭제`)}</label>}
    </div>;
  }

  return <section className="panel secrets-panel" aria-labelledby="connection-editor-heading">
    <h2 className="panel-title" id="connection-editor-heading">{connection ? t(`Edit connection: ${connection.name}`, `외부 연결 수정: ${connection.name}`) : t("Add external connection", "외부 연결 추가")}</h2>
    <form className="secrets-form" onSubmit={submit}><fieldset disabled={saving}>
      <Field label={t("Connection name", "연결 이름")}><input autoFocus required maxLength={100} value={name} onChange={(event) => setName(event.target.value)} placeholder={t("Production secrets", "운영 시크릿")} /></Field>
      <Field label={t("Provider", "저장소")}><select value={provider} disabled={!!connection} onChange={(event) => { const next = event.target.value as SecretProvider; setProvider(next); setCredentials({}); setCleared(new Set()); setSettings({ ...EMPTY_SETTINGS, baseUrl: next === "infisical" ? "https://us.infisical.com" : "" }); }}>
        {(["infisical", "awsSecretsManager", "firecrab"] as const).map((item) => <option value={item} key={item}>{providerLabel(item)}</option>)}
      </select></Field>
      {provider !== "awsSecretsManager" && <Field label={t("Base URL", "서버 URL")}><input type="url" required maxLength={2048} value={settings.baseUrl} onChange={(event) => updateSetting("baseUrl", event.target.value)} placeholder={provider === "firecrab" ? "https://firecrab.example.com" : "https://us.infisical.com"} /></Field>}
      {provider === "infisical" && <>
        <Field label={t("Project ID", "프로젝트 ID")}><input required value={settings.projectId} onChange={(event) => updateSetting("projectId", event.target.value)} /></Field>
        <Field label={t("Environment", "환경")}><input required value={settings.environment} onChange={(event) => updateSetting("environment", event.target.value)} placeholder="prod" /></Field>
        <Field label={t("Secret path", "시크릿 경로")}><input required value={settings.secretPath} onChange={(event) => updateSetting("secretPath", event.target.value)} placeholder="/" /></Field>
        {credential("token", t("Access token", "액세스 토큰"))}
      </>}
      {provider === "awsSecretsManager" && <>
        <Field label={t("AWS region", "AWS 리전")}><input required maxLength={40} value={settings.region} onChange={(event) => updateSetting("region", event.target.value)} placeholder="ap-northeast-2" /></Field>
        <Field label={t("Name prefix (optional)", "이름 접두사 (선택)")}><input value={settings.prefix} onChange={(event) => updateSetting("prefix", event.target.value)} placeholder="production/" /></Field>
        {credential("accessKeyId", t("Access key ID", "액세스 키 ID"))}
        {credential("secretAccessKey", t("Secret access key", "시크릿 액세스 키"))}
        {credential("sessionToken", t("Session token (optional)", "세션 토큰 (선택)"), true)}
      </>}
      {provider === "firecrab" && credential("token", t("Bearer token (optional)", "Bearer 토큰 (선택)"), true)}
      <p className="poll-note">{t("Credentials are encrypted on the server. Import preview checks access to the external store.", "인증 정보는 서버에 암호화해 저장한다. 가져오기 미리보기에서 외부 저장소 접근을 확인한다.")}</p>
      {error && <p className="field-error" role="alert">{error}</p>}
      <div className="secrets-actions"><button className="btn primary" type="submit">{saving ? t("Saving…", "저장 중…") : t("Save connection", "연결 저장")}</button><button className="btn" type="button" onClick={onCancel}>{t("Cancel", "취소")}</button></div>
    </fieldset></form>
  </section>;
}

function Field({ label, children }: { label: string; children: ReactElement<{ id?: string }> }) {
  const id = useId();
  return <div className="field secrets-field"><label htmlFor={id}>{label}</label>{cloneElement(children, { id })}</div>;
}
