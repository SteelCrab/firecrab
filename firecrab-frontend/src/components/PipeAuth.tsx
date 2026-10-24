import { useEffect, useRef, useState, type FormEvent } from "react";
import { ArrowLeft, KeyRound } from "lucide-react";
import { connectPipeAuth, disconnectPipeAuth, getPipeAuth, hasImageAuth, type AuthConnection, type AuthLogin, type AuthProvider } from "../api/pipeAuth";
import { useI18n } from "../i18n";
import { pipeCardHash, viewHash } from "../navigation";
import { readServiceDeploymentDraft, resumeServiceDeployment } from "../lib/serviceDeploymentResume";
import Screen from "./layout/Screen";
import { useScreenStatus } from "./layout/screenStatus";
import { DetailHead, DetailProp, DetailSection } from "./ui/DetailSection";
import PipeServiceMenu from "./PipeServiceMenu";

interface Editor { provider: AuthProvider; token: string; accessKeyId: string; secretAccessKey: string; sessionToken: string; region: string }
const PROVIDERS: { id: AuthProvider; name: string; service: string; href: string; external?: boolean }[] = [
  { id: "github", name: "GitHub", service: "GHCR", href: pipeCardHash("registry") },
  { id: "aws", name: "AWS", service: "Amazon ECR", href: pipeCardHash("registry") },
  { id: "firecrab", name: "Firecrab", service: "Firecrab Console", href: "https://console.firecrab.dev/#account", external: true },
];

/** Provider connections use the dashboard's shared list and detail-panel layout. */
export default function PipeAuth({ onUnsavedChange }: { onUnsavedChange: (dirty: boolean) => void }) {
  const { t } = useI18n();
  const { setDetailOpen } = useScreenStatus();
  const [connections, setConnections] = useState<AuthConnection[]>([]);
  const [resumeDraft] = useState(readServiceDeploymentDraft);
  const [loading, setLoading] = useState(true);
  const [loadError, setLoadError] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [selectedProvider, setSelectedProvider] = useState<AuthProvider | null>(null);
  const [editor, setEditor] = useState<Editor | null>(null);
  const [version, setVersion] = useState(0);
  const inFlight = useRef(false);
  const original = connections.find(connection => connection.provider === editor?.provider);
  const selected = PROVIDERS.find(provider => provider.id === selectedProvider);
  const selectedConnection = connections.find(connection => connection.provider === selectedProvider);
  const dirty = !!editor && (!!editor.token || !!editor.accessKeyId || !!editor.secretAccessKey || !!editor.sessionToken || editor.region !== (original?.region ?? "ap-northeast-2"));

  useEffect(() => { onUnsavedChange(dirty); return () => onUnsavedChange(false); }, [dirty, onUnsavedChange]);
  useEffect(() => {
    const controller = new AbortController();
    void getPipeAuth(controller.signal).then(rows => { if (!controller.signal.aborted) setConnections(rows); })
      .catch(failure => { if (!controller.signal.aborted) { setError((failure as Error).message); setLoadError(true); } })
      .finally(() => { if (!controller.signal.aborted) setLoading(false); });
    return () => controller.abort();
  }, [version]);

  function discardDraft() {
    return !dirty || window.confirm(t("Pipe changes have not been saved. Leave without saving?", "Pipe 변경을 저장하지 않았다. 저장하지 않고 이동할까요?"));
  }
  function select(provider: AuthProvider) {
    if (busy || (provider !== selectedProvider && !discardDraft())) return;
    if (provider !== selectedProvider) { setEditor(null); setError(null); setNotice(null); }
    setSelectedProvider(provider);
    setDetailOpen(true);
  }
  function closeDetail() {
    if (busy || !discardDraft()) { setDetailOpen(true); return; }
    setEditor(null);
    setSelectedProvider(null);
  }
  function open(provider: AuthProvider) {
    const connection = connections.find(connection => connection.provider === provider);
    setSelectedProvider(provider);
    setDetailOpen(true);
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
    const login: AuthLogin = editor.provider === "aws"
      ? { provider: "aws", accessKeyId: editor.accessKeyId.trim(), secretAccessKey: editor.secretAccessKey.trim(), sessionToken: editor.sessionToken.trim(), region: editor.region.trim() }
      : { provider: editor.provider, token: editor.token.trim() };
    await perform(async () => {
      const connection = await connectPipeAuth(login);
      setConnections(rows => [...rows.filter(row => row.provider !== connection.provider), connection]);
      setEditor(null);
      setNotice(connection.provider === "firecrab"
        ? t("Connected to the Firecrab account.", "Firecrab 계정을 연결했다.")
        : t("Connected. The registry is available in Pipe Registry.", "인증을 연결했다. Pipe Registry에서 레지스트리를 사용할 수 있다."));
      if (!resumeDraft) return;
      if (hasImageAuth(resumeDraft.image, [connection])) {
        onUnsavedChange(false);
        resumeServiceDeployment(resumeDraft);
        return;
      }
      setNotice(null);
      setError(t("This account does not match the deployment image. Connect its GitHub account or the matching AWS account and region.", "이 계정은 배포할 이미지와 일치하지 않는다. 이미지 소유자의 GitHub 계정 또는 같은 AWS 계정과 리전을 연결한다."));
    });
  }
  function disconnect(provider: AuthProvider) {
    const name = PROVIDERS.find(value => value.id === provider)!.name;
    const message = provider === "firecrab"
      ? t("Disconnect the Firecrab account from this host? To revoke its account integration token, open console.firecrab.dev.", "이 호스트에서 Firecrab 계정 연결을 해제할까요? 계정 연동 토큰을 폐기하려면 console.firecrab.dev를 연다.")
      : t(`Disconnect ${name}? Pipe services using this connection will lose access.`, `${name} 인증 연결을 해제할까요? 이 인증을 사용하는 Pipe 서비스의 접근 권한이 해제된다.`);
    if (!window.confirm(message)) return;
    void perform(async () => {
      await disconnectPipeAuth(provider);
      setConnections(rows => rows.filter(row => row.provider !== provider));
      setNotice(t("Disconnected.", "인증 연결을 해제했다."));
    });
  }

  const feedback = <>
    {error && <div className="banner error" role="alert">{error}</div>}
    {notice && <div className="banner info" role="status">{notice}</div>}
  </>;
  const status = (connected: boolean) => <span className={`badge${connected ? " running" : ""}`}>{connected ? t("Connected", "연결됨") : t("Not connected", "미연결")}</span>;

  return <section className="pipe-auth-screen" aria-label="Auth">
    <Screen view="pipe" flush
      actions={<nav aria-label={t("Pipe navigation", "Pipe 이동")}><a className="btn sm ghost" href={viewHash("pipe")}><ArrowLeft aria-hidden="true" />Pipe</a></nav>}
      detailLabel={t("Authentication detail", "인증 상세")}
      detailKind={<><KeyRound aria-hidden="true" />Auth</>}
      onCloseDetail={closeDetail}
      detail={selected ? <>
        <DetailHead title={selected.name} meta={status(!!selectedConnection)} />
        <div className="detail-body pipe-auth-detail">
          {feedback}
          {editor ? <AuthForm editor={editor} busy={busy} connected={!!original} onChange={setEditor} onSubmit={connect} onCancel={() => setEditor(null)} /> : <>
            <DetailSection title={t("Connection", "연결 정보")}>
              <dl className="props">
                <DetailProp label={t("Account", "계정")} wrap>{selectedConnection?.account ?? "—"}</DetailProp>
                {selectedConnection?.accountId && <DetailProp label={t("Account ID", "계정 ID")} wrap><span className="mono">{selectedConnection.accountId}</span></DetailProp>}
                {selectedConnection?.expiresAt && <DetailProp label={t("Token expires", "토큰 만료")} wrap>{new Date(selectedConnection.expiresAt).toLocaleString()}</DetailProp>}
                {selectedConnection?.region && <DetailProp label={t("Region", "리전")}>{selectedConnection.region}</DetailProp>}
                {selectedConnection?.registry && <DetailProp label={t("Registry", "레지스트리")} wrap><span className="mono">{selectedConnection.registry}</span></DetailProp>}
                {selected.id === "firecrab" && <DetailProp label={t("Console", "콘솔")} wrap><a href="https://console.firecrab.dev/#account" target="_blank" rel="noreferrer">console.firecrab.dev</a></DetailProp>}
              </dl>
              <div className="detail-sec-buttons">
                <button className="btn sm" type="button" disabled={busy} onClick={() => open(selected.id)}>{selectedConnection ? t("Update credentials", "인증 정보 갱신") : t("Connect account", "계정 연결")}</button>
                {selectedConnection && <button className="btn sm danger" type="button" disabled={busy} onClick={() => disconnect(selected.id)}>{t("Disconnect", "연결 해제")}</button>}
              </div>
            </DetailSection>
            <DetailSection title={selected.id === "firecrab" ? t("Account integration", "계정 연동") : t("Pipe services", "Pipe 서비스")}>
              <p className="detail-note">{selectedConnection ? <a href={selected.href} target={selected.external ? "_blank" : undefined} rel={selected.external ? "noreferrer" : undefined}>{t(`Open ${selected.service}`, `${selected.service} 열기`)}</a> : t(`Connect ${selected.name} to use ${selected.service}.`, `${selected.name} 계정을 연결하면 ${selected.service}를 사용할 수 있다.`)}</p>
            </DetailSection>
          </>}
        </div>
      </> : null}
    >
      <div className="pipe-auth-heading">
        <h2>Auth</h2>
        <p className="detail-note">{t("Connect an account to use its Pipe services.", "계정을 인증하여 관련 Pipe 서비스를 사용한다.")}</p>
      </div>
      {resumeDraft && <div className="banner info">
        <span>{t(`Reconnect AUTH to continue deploying ${resumeDraft.serviceName}. You will return to the deployment screen with your network and port choices.`, `${resumeDraft.serviceName} 배포를 계속하려면 AUTH를 다시 연결한다. 연결이 완료되면 선택한 네트워크와 포트를 유지한 배포 화면으로 돌아간다.`)}<br /><code>{resumeDraft.image}</code></span>
      </div>}
      {!selected && feedback}
      {loadError && <div className="pipe-auth-retry"><button className="btn" type="button" onClick={() => { setLoading(true); setLoadError(false); setError(null); setVersion(value => value + 1); }}>{t("Retry", "다시 시도")}</button></div>}
      {loading ? <p className="empty" role="status">{t("Loading connections…", "인증 조회 중…")}</p> : !loadError && <div className="table-scroll"><table className="vm-table pipe-auth-connections" aria-label={t("Provider connections", "공급자 인증")}>
        <thead><tr><th scope="col">{t("Provider", "공급자")}</th><th scope="col" className="pipe-auth-account">{t("Account", "계정")}</th><th scope="col">{t("Status", "상태")}</th><th scope="col" className="pipe-auth-services">{t("Pipe services", "Pipe 서비스")}</th><th scope="col" className="actions">{t("Actions", "작업")}</th></tr></thead>
        <tbody>{PROVIDERS.map(provider => {
          const connection = connections.find(row => row.provider === provider.id);
          return <tr key={provider.id} className={selectedProvider === provider.id ? "selected" : undefined} aria-selected={selectedProvider === provider.id} onClick={() => select(provider.id)}>
            <td className="name"><button className="link-button" type="button" onClick={event => { event.stopPropagation(); select(provider.id); }}>{provider.name}</button></td>
            <td className="pipe-auth-account">{connection ? <>{connection.account}{connection.region && <span className="pipe-auth-region">{connection.region}</span>}</> : "—"}</td>
            <td>{status(!!connection)}</td>
            <td className="pipe-auth-services" onClick={event => event.stopPropagation()}>{connection ? <><a href={provider.href} target={provider.external ? "_blank" : undefined} rel={provider.external ? "noreferrer" : undefined}>{provider.service}</a>{connection.services.includes("storage") && <> · <a href={pipeCardHash("storage")}>S3</a></>}</> : <span className="muted">{provider.service}</span>}</td>
            <td className="actions" onClick={event => event.stopPropagation()}>{connection ? <PipeServiceMenu label={t(`${provider.name} actions`, `${provider.name} 작업`)} disabled={busy || !!editor} items={[
              { label: t("Update credentials", "인증 정보 갱신"), onSelect: () => open(provider.id) },
              { label: t("Disconnect", "연결 해제"), danger: true, onSelect: () => disconnect(provider.id) },
            ]} /> : <button className="btn sm" type="button" disabled={busy || !!editor} onClick={() => open(provider.id)}>{t(`Connect ${provider.name}`, `${provider.name} 연결`)}</button>}</td>
          </tr>;
        })}</tbody>
      </table></div>}
    </Screen>
  </section>;
}

function AuthForm({ editor, busy, connected, onChange, onSubmit, onCancel }: {
  editor: Editor; busy: boolean; connected: boolean; onChange: (value: Editor) => void; onSubmit: (event: FormEvent) => void; onCancel: () => void;
}) {
  const { t } = useI18n();
  const update = (patch: Partial<Editor>) => onChange({ ...editor, ...patch });
  const name = PROVIDERS.find(value => value.id === editor.provider)!.name;
  return <form className="pipe-auth-form detail-sec" data-editing onSubmit={onSubmit} aria-labelledby="pipe-auth-form-title">
    <div className="detail-sec-head"><h3 id="pipe-auth-form-title">{connected ? t(`Update ${name} credentials`, `${name} 인증 정보 갱신`) : t(`Connect ${name}`, `${name} 연결`)}</h3></div>
    {editor.provider !== "aws" ? <div className="field"><label htmlFor="pipe-auth-token">{editor.provider === "firecrab" ? t("Account integration token", "계정 연동 토큰") : t("Personal access token (classic)", "개인 액세스 토큰 (classic)")}</label>
      <input id="pipe-auth-token" type="password" autoComplete="new-password" autoFocus value={editor.token} onChange={event => update({ token: event.target.value })} disabled={busy} required aria-describedby="pipe-auth-help" maxLength={editor.provider === "firecrab" ? 68 : undefined} pattern={editor.provider === "firecrab" ? "fca_[a-f0-9]{64}" : undefined} />
      <p className="detail-note" id="pipe-auth-help">{editor.provider === "firecrab"
        ? <>{t("Issue an account integration token at ", "계정 연동 토큰 발급: ")}<a href="https://console.firecrab.dev/#account" target="_blank" rel="noreferrer">console.firecrab.dev</a>{t(". Open Account integration. The token verifies your account ID and email.", ". 계정 연동 메뉴에서 발급한다. 토큰으로 계정 ID와 이메일을 확인한다.")}</>
        : t("Permission: read:packages. Authorize SSO for your organization if required.", "권한: read:packages. 조직에서 요구하면 SSO 권한을 승인한다.")}</p>
    </div> : <>
      <div className="field"><label htmlFor="pipe-auth-access-key">AWS access key ID</label><input id="pipe-auth-access-key" type="password" autoComplete="new-password" autoFocus value={editor.accessKeyId} onChange={event => update({ accessKeyId: event.target.value })} disabled={busy} required /></div>
      <div className="field"><label htmlFor="pipe-auth-secret-key">AWS secret access key</label><input id="pipe-auth-secret-key" type="password" autoComplete="new-password" value={editor.secretAccessKey} onChange={event => update({ secretAccessKey: event.target.value })} disabled={busy} required /></div>
      <div className="field"><label htmlFor="pipe-auth-session">{t("AWS session token (optional)", "AWS 세션 토큰 (선택)")}</label><input id="pipe-auth-session" type="password" autoComplete="new-password" value={editor.sessionToken} onChange={event => update({ sessionToken: event.target.value })} disabled={busy} /></div>
      <div className="field"><label htmlFor="pipe-auth-region">{t("AWS region", "AWS 리전")}</label><input id="pipe-auth-region" autoComplete="off" spellCheck={false} value={editor.region} onChange={event => update({ region: event.target.value })} disabled={busy} required /></div>
      <p className="detail-note">{t("Needs ecr:GetAuthorizationToken, ecr:DescribeRepositories and permission to pull images. Reconnect when temporary AWS credentials expire.", "ecr:GetAuthorizationToken, ecr:DescribeRepositories 권한과 이미지 가져오기 권한이 필요하다. 임시 AWS 인증 정보가 만료되면 다시 연결한다.")}</p>
    </>}
    <div className="detail-sec-buttons"><button className="btn primary" type="submit" disabled={busy}>{busy ? t("Verifying…", "인증 확인 중…") : t("Verify and connect", "인증 확인 후 연결")}</button><button className="btn" type="button" disabled={busy} onClick={onCancel}>{t("Cancel", "취소")}</button></div>
  </form>;
}
