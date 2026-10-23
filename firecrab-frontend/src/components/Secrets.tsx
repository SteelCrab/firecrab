import { useEffect, useState } from "react";
import { useI18n } from "../i18n";
import {
  applySecretTransfer, deleteSecret, deleteSecretConnection, listSecretConnections, listSecrets,
  previewSecretTransfer, providerLabel, revealSecret,
} from "../api/secrets";
import type { SecretConnection, SecretSummary, TransferDirection, TransferPreview } from "../api/secrets";
import { SecretEditor, SecretConnectionEditor } from "./SecretsEditors";

export default function Secrets({ onUnsavedChange }: { onUnsavedChange: (dirty: boolean) => void }) {
  const { t, locale } = useI18n();
  const [tab, setTab] = useState<"internal" | "external">("internal");
  const [secrets, setSecrets] = useState<SecretSummary[] | null>(null);
  const [connections, setConnections] = useState<SecretConnection[]>([]);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [destination, setDestination] = useState("");
  const [query, setQuery] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [message, setMessage] = useState("");
  const [editor, setEditor] = useState<SecretSummary | null | undefined>();
  const [connectionEditor, setConnectionEditor] = useState<SecretConnection | null | undefined>();
  const [revealed, setRevealed] = useState<Record<string, string>>({});
  const [preview, setPreview] = useState<TransferPreview | null>(null);
  const editing = editor !== undefined || connectionEditor !== undefined;

  useEffect(() => {
    onUnsavedChange(editing || busy);
    return () => onUnsavedChange(false);
  }, [editing, busy, onUnsavedChange]);

  useEffect(() => {
    let active = true;
    Promise.all([listSecrets(), listSecretConnections()]).then(([rows, links]) => {
      if (active) { setSecrets(rows); setConnections(links); }
    }).catch((failure: Error) => { if (active) setError(failure.message); });
    return () => { active = false; };
  }, []);

  useEffect(() => {
    if (Object.keys(revealed).length === 0) return;
    const hide = () => setRevealed({});
    const timer = window.setTimeout(hide, 30_000);
    window.addEventListener("blur", hide);
    document.addEventListener("visibilitychange", hide);
    return () => { clearTimeout(timer); window.removeEventListener("blur", hide); document.removeEventListener("visibilitychange", hide); };
  }, [revealed]);

  async function refresh() {
    const [rows, links] = await Promise.all([listSecrets(), listSecretConnections()]);
    setSecrets(rows); setConnections(links); setRevealed({});
    setSelected((previous) => new Set([...previous].filter((id) => rows.some((row) => row.id === id))));
    setDestination((previous) => links.some((link) => link.id === previous) ? previous : "");
  }

  async function run(operation: () => Promise<void>) {
    if (busy) return;
    setBusy(true); setError(""); setMessage("");
    try { await operation(); } catch (failure) { setError((failure as Error).message); }
    finally { setBusy(false); }
  }

  async function transfer(connection: SecretConnection, direction: TransferDirection) {
    await run(async () => {
      setPreview(null); setRevealed({});
      setPreview(await previewSecretTransfer(connection.id, direction, [...selected]));
    });
  }

  async function apply(names: string[]) {
    if (!preview) return;
    await run(async () => {
      const result = await applySecretTransfer(preview.id, names);
      setPreview(null);
      setMessage(t(`${result.applied.length} secret(s) applied.`, `${result.applied.length}개 시크릿을 적용했다.`));
      if (result.failed.length) setError(t(`Upload failed: ${result.failed.map((item) => item.name).join(", ")}. Preview again to retry.`, `업로드 실패: ${result.failed.map((item) => item.name).join(", ")}. 다시 미리보기한 뒤 재시도한다.`));
      await refresh();
    });
  }

  const toggle = (id: string) => setSelected((previous) => {
    const next = new Set(previous);
    if (next.has(id)) next.delete(id); else next.add(id);
    return next;
  });
  const rows = (secrets ?? []).filter((secret) => `${secret.name} ${secret.description} ${secret.source?.connectionName ?? ""}`.toLowerCase().includes(query.toLowerCase()));
  const links = connections.filter((connection) => `${connection.name} ${providerLabel(connection.provider)}`.toLowerCase().includes(query.toLowerCase()));
  const date = (milliseconds: number) => new Date(milliseconds).toLocaleString(locale === "ko" ? "ko-KR" : "en-US");

  return <>
    <section className="panel secrets-panel" aria-labelledby="secrets-heading">
      <h2 className="panel-title" id="secrets-heading"><span>Secrets</span><span className="secrets-badge">PipeSecrets</span></h2>
      <p className="poll-note">{t("Store secrets in Firecrab. Import from or upload to external stores.", "Firecrab에 시크릿을 저장한다. 외부 저장소에서 가져오거나 업로드한다.")}</p>
      <div className="secrets-tabs" role="tablist" aria-label={t("Secret management", "시크릿 관리")}>
        <button id="secrets-internal-tab" role="tab" aria-selected={tab === "internal"} aria-controls="secrets-content" className={`btn ${tab === "internal" ? "primary" : ""}`} disabled={busy || editing} onClick={() => { setTab("internal"); setQuery(""); setRevealed({}); setPreview(null); }}>{t("Internal", "내부 관리")} <span>{secrets?.length ?? 0}</span></button>
        <button id="secrets-external-tab" role="tab" aria-selected={tab === "external"} aria-controls="secrets-content" className={`btn ${tab === "external" ? "primary" : ""}`} disabled={busy || editing} onClick={() => { setTab("external"); setQuery(""); setRevealed({}); setPreview(null); }}>{t("External connections", "외부 연결")} <span>{connections.length}</span></button>
      </div>
      {error && <p className="field-error" role="alert">{error}</p>}
      {message && <p className="secrets-notice" role="status">{message}</p>}
      {busy && <p className="poll-note" role="status">{t("Working…", "처리 중…")}</p>}
      <div id="secrets-content" role="tabpanel" aria-labelledby={`secrets-${tab}-tab`}>
        <div className="secrets-toolbar">
          <input className="secrets-search" type="search" aria-label={t("Search secrets and connections", "시크릿과 연결 검색")} placeholder={t("Search by name…", "이름으로 검색…")} value={query} onChange={(event) => setQuery(event.target.value)} />
          <button className="btn" disabled={busy || editing} onClick={() => run(refresh)}>{t("Refresh", "새로 고침")}</button>
          <button className="btn primary" disabled={busy || editing || secrets === null} onClick={() => tab === "internal" ? setEditor(null) : setConnectionEditor(null)}>{tab === "internal" ? t("New secret", "시크릿 추가") : t("Add connection", "외부 연결 추가")}</button>
        </div>
        {secrets === null ? <p className="empty">{error ? t("Unable to load Secrets. Refresh to retry.", "Secrets를 불러오지 못했다. 새로 고침으로 재시도한다.") : t("Loading…", "불러오는 중…")}</p> : tab === "internal" ? <>
          {secrets.length > 0 && <div className="secrets-upload">
            <span>{t(`${selected.size} selected`, `${selected.size}개 선택`)}</span>
            <label className="sr-only" htmlFor="secrets-destination">{t("Upload destination", "업로드 대상")}</label>
            <select id="secrets-destination" value={destination} disabled={busy || editing} onChange={(event) => setDestination(event.target.value)}><option value="">{t("Choose external connection", "외부 연결 선택")}</option>{connections.map((connection) => <option key={connection.id} value={connection.id}>{connection.name} · {providerLabel(connection.provider)}</option>)}</select>
            <button className="btn" disabled={busy || editing || !selected.size || !destination} onClick={() => { const connection = connections.find((item) => item.id === destination); if (connection) void transfer(connection, "export"); }}>{t("Preview upload", "업로드 미리보기")}</button>
          </div>}
          {!rows.length ? <p className="empty">{secrets.length ? t("No matching secrets.", "검색 결과가 없다.") : t("No secrets yet. Add a secret or import from an external connection.", "저장된 시크릿이 없다. 시크릿을 추가하거나 외부 연결에서 가져온다.")}</p> : <div className="table-scroll"><table className="vm-table secrets-table">
            <thead><tr><th scope="col"><input type="checkbox" aria-label={t("Select visible secrets", "표시된 시크릿 선택")} disabled={busy || editing} checked={rows.every((row) => selected.has(row.id))} onChange={(event) => setSelected((previous) => { const next = new Set(previous); for (const row of rows) { if (event.target.checked) next.add(row.id); else next.delete(row.id); } return next; })} /></th>{[t("Name", "이름"), t("Value", "값"), t("Source", "출처"), t("Updated", "수정 시각"), t("Actions", "작업")].map((label) => <th scope="col" key={label}>{label}</th>)}</tr></thead>
            <tbody>{rows.map((secret) => <tr key={secret.id}>
              <td><input type="checkbox" aria-label={t(`Select ${secret.name}`, `${secret.name} 선택`)} checked={selected.has(secret.id)} disabled={busy || editing} onChange={() => toggle(secret.id)} /></td>
              <td className="name"><strong>{secret.name}</strong>{secret.description && <small className="secrets-description">{secret.description}</small>}<small className="secrets-description mono" title={secret.id}>{secret.id}</small></td>
              <td><div className="secrets-value"><span aria-label={t(`${secret.name} value`, `${secret.name} 값`)} className="mono secrets-value-text">{revealed[secret.id] ?? "••••••••"}</span><button className="btn" disabled={busy || editing} aria-label={t(`${secret.id in revealed ? "Hide" : "Reveal"} ${secret.name}`, `${secret.name} ${secret.id in revealed ? "숨기기" : "보기"}`)} onClick={() => {
                if (secret.id in revealed) { setRevealed((previous) => { const next = { ...previous }; delete next[secret.id]; return next; }); return; }
                void run(async () => { const result = await revealSecret(secret.id); if (result.version !== secret.version) { await refresh(); throw new Error(t("The secret changed. Reveal it again.", "시크릿이 변경되었다. 다시 보기를 선택한다.")); } setRevealed((previous) => ({ ...previous, [secret.id]: result.value })); });
              }}>{secret.id in revealed ? t("Hide", "숨기기") : t("Reveal", "보기")}</button></div></td>
              <td>{secret.source?.connectionName ?? "PipeSecrets"}{secret.source && <small className="secrets-description" title={date(secret.source.importedAt)}>{providerLabel(secret.source.provider)}</small>}</td>
              <td><span>{date(secret.updatedAt)}</span><small className="secrets-description">v{secret.version}</small></td>
              <td><div className="secrets-actions"><button className="btn" disabled={busy || editing} aria-label={t(`Edit ${secret.name}`, `${secret.name} 수정`)} onClick={() => { setRevealed({}); setEditor(secret); }}>{t("Edit", "수정")}</button><button className="btn danger" disabled={busy || editing} aria-label={t(`Delete ${secret.name}`, `${secret.name} 삭제`)} onClick={() => {
                if (window.confirm(t(`Delete secret "${secret.name}" from PipeSecrets?`, `PipeSecrets에서 "${secret.name}" 시크릿을 삭제할까요?`))) void run(async () => { await deleteSecret(secret.id); setPreview(null); await refresh(); });
              }}>{t("Delete", "삭제")}</button></div></td>
            </tr>)}</tbody>
          </table></div>}
        </> : <>
          <p className="poll-note">{t("Imports create local copies in PipeSecrets. External changes appear after another import. Upload selected local secrets from the Internal tab.", "가져온 값은 PipeSecrets에 복사한다. 외부 변경은 다시 가져올 때 반영한다. 내부 관리에서 시크릿을 선택해 업로드한다.")}</p>
          {!links.length ? <p className="empty">{connections.length ? t("No matching connections.", "검색 결과가 없다.") : t("Connect Infisical, AWS Secrets Manager or another Firecrab instance.", "Infisical, AWS Secrets Manager 또는 다른 Firecrab 인스턴스를 연결한다.")}</p> : <div className="secrets-connections">{links.map((connection) => <article className="secrets-connection" key={connection.id}>
            <div><span className="secrets-badge">{providerLabel(connection.provider)}</span><h3>{connection.name}</h3><p className="mono">{connection.provider === "awsSecretsManager" ? `${connection.settings.region} · ${connection.settings.prefix || "*"}` : connection.settings.baseUrl}</p>{connection.provider === "infisical" && <p>{connection.settings.projectId} · {connection.settings.environment} · {connection.settings.secretPath}</p>}</div>
            <div className="secrets-actions"><button className="btn primary" disabled={busy || editing} aria-label={t(`Import from ${connection.name}`, `${connection.name}에서 가져오기`)} onClick={() => transfer(connection, "import")}>{t("Preview import", "가져오기 미리보기")}</button><button className="btn" disabled={busy || editing} aria-label={t(`Edit connection ${connection.name}`, `${connection.name} 연결 수정`)} onClick={() => setConnectionEditor(connection)}>{t("Edit", "수정")}</button><button className="btn danger" disabled={busy || editing} aria-label={t(`Delete connection ${connection.name}`, `${connection.name} 연결 삭제`)} onClick={() => {
              if (window.confirm(t(`Remove connection "${connection.name}"? Imported PipeSecrets values remain available.`, `"${connection.name}" 연결을 삭제할까요? 가져온 시크릿은 PipeSecrets에 유지한다.`))) void run(async () => { await deleteSecretConnection(connection.id); setPreview(null); await refresh(); });
            }}>{t("Remove", "연결 삭제")}</button></div>
          </article>)}</div>}
        </>}
      </div>
    </section>
    {editor !== undefined && <SecretEditor key={editor?.id ?? "new"} secret={editor} onCancel={() => setEditor(undefined)} onSaved={async () => { setEditor(undefined); setPreview(null); await run(refresh); }} />}
    {connectionEditor !== undefined && <SecretConnectionEditor key={connectionEditor?.id ?? "new-connection"} connection={connectionEditor} onCancel={() => setConnectionEditor(undefined)} onSaved={async () => { setConnectionEditor(undefined); setPreview(null); await run(refresh); }} />}
    {preview && <TransferPanel key={preview.id} preview={preview} connection={connections.find((connection) => connection.id === preview.connectionId)} busy={busy} onApply={apply} onCancel={() => setPreview(null)} />}
  </>;
}

function TransferPanel({ preview, connection, busy, onApply, onCancel }: {
  preview: TransferPreview; connection?: SecretConnection; busy: boolean;
  onApply: (names: string[]) => Promise<void>; onCancel: () => void;
}) {
  const { t } = useI18n();
  const [selected, setSelected] = useState(new Set(preview.changes.filter((item) => item.change === "add").map((item) => item.name)));
  const [expired, setExpired] = useState(() => Date.now() >= preview.expiresAt);
  useEffect(() => { const timer = setTimeout(() => setExpired(true), Math.max(0, preview.expiresAt - Date.now())); return () => clearTimeout(timer); }, [preview.expiresAt]);
  const labels = { add: t("Add", "추가"), update: t("Replace existing value", "기존 값 교체"), unchanged: t("Unchanged", "변경 없음") };
  return <section className="panel secrets-panel" aria-labelledby="transfer-heading">
    <h2 className="panel-title" id="transfer-heading">{preview.direction === "import" ? t("Import preview", "가져오기 미리보기") : t("Upload preview", "업로드 미리보기")}</h2>
    <p>{preview.direction === "import" ? `${connection?.name ?? ""} → PipeSecrets` : `PipeSecrets → ${connection?.name ?? ""}`}</p>
    <p className="poll-note">{t("Select changes to apply. Replacements require selection. Deletes are not transferred.", "적용할 변경을 선택한다. 기존 값 교체는 직접 선택한다. 삭제는 전파하지 않는다.")}</p>
    {expired && <p className="field-error" role="alert">{t("This preview expired. Create a new preview.", "미리보기가 만료되었다. 다시 미리보기한다.")}</p>}
    {!preview.changes.length ? <p className="empty">{t("No secrets in this scope.", "선택한 범위에 시크릿이 없다.")}</p> : <div className="secrets-changes">{preview.changes.map((item) => <label className="secrets-change" key={item.name}><input type="checkbox" aria-label={t(`Apply ${item.name}`, `${item.name} 적용`)} checked={selected.has(item.name)} disabled={busy || expired || item.change === "unchanged"} onChange={() => setSelected((previous) => { const next = new Set(previous); if (next.has(item.name)) next.delete(item.name); else next.add(item.name); return next; })} /><code>{item.name}</code><span className={`secrets-change-${item.change}`}>{labels[item.change]}</span></label>)}</div>}
    <div className="secrets-actions"><button className="btn primary" disabled={busy || expired || selected.size === 0} onClick={() => onApply([...selected])}>{preview.direction === "import" ? t(`Import selected (${selected.size})`, `선택 항목 가져오기 (${selected.size})`) : t(`Upload selected (${selected.size})`, `선택 항목 업로드 (${selected.size})`)}</button><button className="btn" disabled={busy} onClick={onCancel}>{t("Close preview", "미리보기 닫기")}</button></div>
  </section>;
}
