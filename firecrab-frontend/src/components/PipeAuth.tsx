import { useEffect, useRef, useState, type FormEvent } from "react";
import { connectPipeAuth, disconnectPipeAuth, getPipeAuth, type AuthConnection, type AuthProvider } from "../api/pipeAuth";
import { useI18n } from "../i18n";
import { pipeCardHash } from "../navigation";
import PipeServiceMenu from "./PipeServiceMenu";

interface Editor { provider: AuthProvider; token: string; accessKeyId: string; secretAccessKey: string; sessionToken: string; region: string }
const PROVIDERS: { id: AuthProvider; name: string; service: string }[] = [
  { id: "github", name: "GitHub", service: "GHCR" },
  { id: "aws", name: "AWS", service: "Amazon ECR" },
];

/** Provider credentials are entered once and reused by supported Pipe services. */
export default function PipeAuth({ onUnsavedChange }: { onUnsavedChange: (dirty: boolean) => void }) {
  const { t } = useI18n();
  const [connections, setConnections] = useState<AuthConnection[]>([]);
  const [loading, setLoading] = useState(true);
  const [loadError, setLoadError] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [editor, setEditor] = useState<Editor | null>(null);
  const [version, setVersion] = useState(0);
  const inFlight = useRef(false);
  const original = connections.find(connection => connection.provider === editor?.provider);
  const dirty = !!editor && (!!editor.token || !!editor.accessKeyId || !!editor.secretAccessKey || !!editor.sessionToken || editor.region !== (original?.region ?? "ap-northeast-2"));

  useEffect(() => { onUnsavedChange(dirty); return () => onUnsavedChange(false); }, [dirty, onUnsavedChange]);
  useEffect(() => {
    const controller = new AbortController();
    void getPipeAuth(controller.signal).then(rows => { if (!controller.signal.aborted) setConnections(rows); })
      .catch(failure => { if (!controller.signal.aborted) { setError((failure as Error).message); setLoadError(true); } })
      .finally(() => { if (!controller.signal.aborted) setLoading(false); });
    return () => controller.abort();
  }, [version]);

  function open(provider: AuthProvider) {
    const connection = connections.find(connection => connection.provider === provider);
    setEditor({ provider, token: "", accessKeyId: "", secretAccessKey: "", sessionToken: "", region: connection?.region ?? "ap-northeast-2" });
    setError(null); setNotice(null);
  }
  async function perform(action: () => Promise<void>) {
    if (inFlight.current) return;
    inFlight.current = true; setBusy(true); setError(null); setNotice(null);
    try { await action(); } catch (failure) { setError((failure as Error).message); }
    finally { inFlight.current = false; setBusy(false); }
  }
  async function connect(event: FormEvent) {
    event.preventDefault();
    if (!editor) return;
    const login = editor.provider === "github" ? { provider: "github" as const, token: editor.token.trim() }
      : { provider: "aws" as const, accessKeyId: editor.accessKeyId.trim(), secretAccessKey: editor.secretAccessKey.trim(), sessionToken: editor.sessionToken.trim(), region: editor.region.trim() };
    await perform(async () => {
      const connection = await connectPipeAuth(login);
      setConnections(rows => [...rows.filter(row => row.provider !== connection.provider), connection]);
      setEditor(null);
      setNotice(t("Connected. The registry is available in Pipe Registry.", "인증을 연결했다. Pipe Registry에서 레지스트리를 사용할 수 있다."));
    });
  }
  function disconnect(provider: AuthProvider) {
    const name = PROVIDERS.find(value => value.id === provider)!.name;
    if (!window.confirm(t(`Disconnect ${name}? Pipe services using this connection will lose access.`, `${name} 인증 연결을 해제할까요? 이 인증을 사용하는 Pipe 서비스의 접근 권한이 해제된다.`))) return;
    void perform(async () => {
      await disconnectPipeAuth(provider);
      setConnections(rows => rows.filter(row => row.provider !== provider));
      setNotice(t("Disconnected.", "인증 연결을 해제했다."));
    });
  }

  return <section className="panel pipe-auth" aria-label="Pipe Auth">
    <h2 className="panel-title"><span>Pipe Auth</span></h2>
    <p className="pipe-description">{t("Connect an account to use its Pipe services.", "계정을 인증하여 관련 Pipe 서비스를 사용한다.")}</p>
    {error && <div className="banner error" role="alert">{error}</div>}
    {notice && <div className="banner info" role="status">{notice}</div>}
    {loadError && <button className="btn" type="button" onClick={() => { setLoading(true); setLoadError(false); setError(null); setVersion(value => value + 1); }}>{t("Retry", "다시 시도")}</button>}
    {editor && <AuthForm editor={editor} busy={busy} connected={!!original} onChange={setEditor} onSubmit={connect} onCancel={() => setEditor(null)} />}
    {loading ? <p className="poll-note" role="status">{t("Loading connections…", "인증 조회 중…")}</p> : !loadError && <div className="table-scroll"><table className="vm-table pipe-auth-connections" aria-label={t("Provider connections", "공급자 인증")}>
      <thead><tr><th scope="col">{t("Provider", "공급자")}</th><th scope="col">{t("Account", "계정")}</th><th scope="col">{t("Status", "상태")}</th><th scope="col">{t("Pipe services", "Pipe 서비스")}</th><th scope="col" className="actions">{t("Actions", "작업")}</th></tr></thead>
      <tbody>{PROVIDERS.map(provider => {
        const connection = connections.find(row => row.provider === provider.id);
        return <tr key={provider.id}>
          <td>{provider.name}</td>
          <td data-label={t("Account", "계정")}>{connection ? <>{connection.account}{connection.region && <span className="pipe-auth-region">{connection.region}</span>}</> : "—"}</td>
          <td><span className={`state-badge${connection ? " running" : ""}`}>{connection ? t("Connected", "연결됨") : t("Not connected", "미연결")}</span></td>
          <td data-label={t("Pipe services", "Pipe 서비스")}>{connection ? <a href={pipeCardHash("registry")}>{provider.service}</a> : <span className="poll-note">{provider.service}</span>}</td>
          <td className="actions">{connection ? <PipeServiceMenu label={t(`${provider.name} actions`, `${provider.name} 작업`)} disabled={busy || !!editor} items={[
            { label: t("Update credentials", "인증 정보 갱신"), onSelect: () => open(provider.id) },
            { label: t("Disconnect", "연결 해제"), danger: true, onSelect: () => disconnect(provider.id) },
          ]} /> : <button className="btn" type="button" disabled={busy || !!editor} onClick={() => open(provider.id)}>{t(`Connect ${provider.name}`, `${provider.name} 연결`)}</button>}</td>
        </tr>;
      })}</tbody>
    </table></div>}
  </section>;
}

function AuthForm({ editor, busy, connected, onChange, onSubmit, onCancel }: {
  editor: Editor; busy: boolean; connected: boolean; onChange: (value: Editor) => void; onSubmit: (event: FormEvent) => void; onCancel: () => void;
}) {
  const { t } = useI18n();
  const update = (patch: Partial<Editor>) => onChange({ ...editor, ...patch });
  const name = PROVIDERS.find(provider => provider.id === editor.provider)!.name;
  return <form className="registry-form" onSubmit={onSubmit} aria-labelledby="pipe-auth-form-title">
    <h3 className="registry-form-title" id="pipe-auth-form-title">{connected ? t(`Update ${name} credentials`, `${name} 인증 정보 갱신`) : t(`Connect ${name}`, `${name} 연결`)}</h3>
    {editor.provider === "github" ? <div className="field registry-wide"><label htmlFor="pipe-auth-token">{t("Personal access token (classic)", "개인 액세스 토큰 (classic)")}</label>
      <input id="pipe-auth-token" type="password" autoComplete="new-password" autoFocus value={editor.token} onChange={event => update({ token: event.target.value })} disabled={busy} required aria-describedby="pipe-auth-help" />
      <span className="poll-note" id="pipe-auth-help">{t("Permission: read:packages. Authorize SSO for your organization if required.", "권한: read:packages. 조직에서 요구하면 SSO 권한을 승인한다.")}</span>
    </div> : <>
      <div className="field"><label htmlFor="pipe-auth-access-key">AWS access key ID</label><input id="pipe-auth-access-key" type="password" autoComplete="new-password" autoFocus value={editor.accessKeyId} onChange={event => update({ accessKeyId: event.target.value })} disabled={busy} required /></div>
      <div className="field"><label htmlFor="pipe-auth-secret-key">AWS secret access key</label><input id="pipe-auth-secret-key" type="password" autoComplete="new-password" value={editor.secretAccessKey} onChange={event => update({ secretAccessKey: event.target.value })} disabled={busy} required /></div>
      <div className="field"><label htmlFor="pipe-auth-session">{t("AWS session token (optional)", "AWS 세션 토큰 (선택)")}</label><input id="pipe-auth-session" type="password" autoComplete="new-password" value={editor.sessionToken} onChange={event => update({ sessionToken: event.target.value })} disabled={busy} /></div>
      <div className="field"><label htmlFor="pipe-auth-region">{t("AWS region", "AWS 리전")}</label><input id="pipe-auth-region" autoComplete="off" spellCheck={false} value={editor.region} onChange={event => update({ region: event.target.value })} disabled={busy} required /></div>
      <p className="poll-note registry-wide">{t("Needs ecr:GetAuthorizationToken, ecr:DescribeRepositories and permission to pull images. Reconnect when temporary AWS credentials expire.", "ecr:GetAuthorizationToken, ecr:DescribeRepositories 권한과 이미지 가져오기 권한이 필요하다. 임시 AWS 인증 정보가 만료되면 다시 연결한다.")}</p>
    </>}
    <div className="pipe-controls registry-wide"><button className="btn primary" type="submit" disabled={busy}>{busy ? t("Verifying…", "인증 확인 중…") : t("Verify and connect", "인증 확인 후 연결")}</button><button className="btn" type="button" disabled={busy} onClick={onCancel}>{t("Cancel", "취소")}</button></div>
  </form>;
}
