import type { FormEvent } from "react";
import type { StorageInput } from "../../api/pipeStorage";
import { useI18n } from "../../i18n";
import { pipeCardHash } from "../../navigation";

export interface StorageEditor extends StorageInput { id?: string; replaceKeys: boolean }

export default function StorageConnectionForm({ editor, busy, onChange, onSubmit, onCancel }: {
  editor: StorageEditor; busy: boolean; onChange: (value: StorageEditor) => void;
  onSubmit: (event: FormEvent) => void; onCancel: () => void;
}) {
  const { t } = useI18n();
  const update = (patch: Partial<StorageEditor>) => onChange({ ...editor, ...patch });
  const changeKey = (field: "accessKeyId" | "secretAccessKey" | "sessionToken", value: string) => update({ credentials: { accessKeyId: "", secretAccessKey: "", sessionToken: "", ...editor.credentials, [field]: value } });
  const enteringKeys = editor.credentialRef === "connection" && (!editor.id || editor.replaceKeys);
  return <form className="s3-form" data-editing onSubmit={onSubmit}>
    <p className="s3-hint">{t("Connect an existing bucket in your AWS account.", "AWS 계정에 있는 버킷을 연결한다.")}</p>
    <label className="field"><span>{t("Bucket", "버킷")}</span><input required maxLength={63} autoComplete="off" spellCheck={false} placeholder="company-files" value={editor.bucket} disabled={busy} onChange={event => update({ bucket: event.target.value })} /></label>
    <div className="s3-field-pair">
      <label className="field"><span>{t("Region", "리전")}</span><input required autoComplete="off" spellCheck={false} value={editor.region} disabled={busy} onChange={event => update({ region: event.target.value })} /></label>
      <label className="field"><span>{t("Folder (optional)", "폴더 (선택)")}</span><input maxLength={1024} placeholder="team/" value={editor.prefix} disabled={busy} onChange={event => update({ prefix: event.target.value })} /></label>
    </div>
    <p className="s3-hint">{t("Leave the folder empty to access the whole bucket.", "폴더를 비워 두면 버킷 전체에 접근한다.")}</p>
    <label className="field"><span>{t("Credentials", "인증")}</span><select value={editor.credentialRef} disabled={busy} onChange={event => update({ credentialRef: event.target.value as StorageEditor["credentialRef"], replaceKeys: event.target.value === "connection", credentials: undefined })}>
      <option value="pipeAuthAws">{t("AWS account from Auth", "Auth의 AWS 계정")}</option><option value="connection">{t("Separate AWS keys", "별도 AWS 키")}</option>
    </select></label>
    {editor.credentialRef === "pipeAuthAws" ? <p className="s3-hint">{t("Uses the AWS account connected in", "AWS 계정 연결 위치:")} <a href={pipeCardHash("auth")}>Auth</a></p> : <>
      {!!editor.id && <label className="s3-check"><input type="checkbox" checked={editor.replaceKeys} disabled={busy} onChange={event => update({ replaceKeys: event.target.checked, credentials: undefined })} />{t("Replace saved keys", "저장된 키 교체")}</label>}
      {enteringKeys && <>
        <label className="field"><span>AWS access key ID</span><input type="password" required maxLength={128} autoComplete="new-password" value={editor.credentials?.accessKeyId ?? ""} disabled={busy} onChange={event => changeKey("accessKeyId", event.target.value)} /></label>
        <label className="field"><span>AWS secret access key</span><input type="password" required maxLength={128} autoComplete="new-password" value={editor.credentials?.secretAccessKey ?? ""} disabled={busy} onChange={event => changeKey("secretAccessKey", event.target.value)} /></label>
      </>}
      <p className="s3-hint">{t("Use keys with access limited to this bucket and folder.", "이 버킷과 폴더로 접근 권한을 제한한 키를 사용한다.")}</p>
    </>}
    <details className="s3-options"><summary>{t("More options", "추가 설정")}</summary>
      <label className="field"><span>{t("Name (optional)", "이름 (선택)")}</span><input maxLength={100} placeholder={editor.bucket || t("Use bucket name", "버킷 이름 사용")} value={editor.name} disabled={busy} onChange={event => update({ name: event.target.value })} /></label>
      {enteringKeys && <label className="field"><span>{t("Session token (optional)", "세션 토큰 (선택)")}</span><input type="password" maxLength={16384} autoComplete="new-password" value={editor.credentials?.sessionToken ?? ""} disabled={busy} onChange={event => changeKey("sessionToken", event.target.value)} /></label>}
    </details>
    <footer className="s3-dialog-actions"><button type="button" className="btn" disabled={busy} onClick={onCancel}>{t("Cancel", "취소")}</button><button type="submit" className="btn primary" disabled={busy}>{busy ? t("Connecting…", "연결 중…") : editor.id ? t("Save changes", "변경 사항 저장") : t("Connect bucket", "버킷 연결")}</button></footer>
  </form>;
}
