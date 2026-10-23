import { useEffect, useRef, useState, type FormEvent } from "react";
import { ChevronDown, CircleCheck, KeyRound, LogIn, Plus, RefreshCw, ShieldCheck, Sparkles, Unplug, X } from "lucide-react";
import { useI18n } from "../i18n";
import ChatGptIcon from "./icons/ChatGptIcon";
import "../styles/pipe-ai.css";
import { disconnectAi, listAiConnections, saveAiConnection, startAiLogin, getAiLogin, type AiLogin, type AiConnection } from "../api/pipeAi";

const providerName = (provider: AiConnection["provider"]) => provider === "anthropic" ? "Anthropic" : "OpenAI";

export default function PipeAi({ onUnsavedChange }: { onUnsavedChange: (dirty: boolean) => void }) {
  const { t } = useI18n();
  const [connections, setConnections] = useState<AiConnection[] | null>(null);
  const [revision, setRevision] = useState(0);
  const [loading, setLoading] = useState(true);
  const [editing, setEditing] = useState<string | null>(null);
  const [name, setName] = useState("");
  const [apiKey, setApiKey] = useState("");
  const [provider, setProvider] = useState<AiConnection["provider"]>("openai");
  const [authentication, setAuthentication] = useState<"api_key" | "chatgpt">("chatgpt");
  const [login, setLogin] = useState<AiLogin | null>(null);
  const [busy, setBusy] = useState(false);
  const inFlight = useRef(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const waitingLogin = login !== null;
  const hasConnections = !!connections?.length;

  useEffect(() => {
    const controller = new AbortController();
    listAiConnections(controller.signal).then(value => { if (!controller.signal.aborted) { setConnections(value); setError(null); } })
      .catch((failure: Error) => { if (!controller.signal.aborted) { setConnections(null); setError(failure.message); } })
      .finally(() => { if (!controller.signal.aborted) setLoading(false); });
    return () => controller.abort();
  }, [revision]);

  useEffect(() => {
    if (!login) return;
    const controller = new AbortController();
    let timer: ReturnType<typeof setTimeout>;
    const deadline = Date.now() + login.expiresIn * 1000;
    async function poll() {
      try {
        const status = await getAiLogin(login!.loginId, controller.signal);
        if (controller.signal.aborted) return;
        if (status.status === "connected") {
          setLogin(null); setEditing(null); setRevision(value => value + 1);
          setNotice(t("ChatGPT account connected.", "ChatGPT 계정을 연결했다."));
        } else if (status.status === "failed") {
          setLogin(null); setError(status.message ?? t("ChatGPT login failed.", "ChatGPT 로그인에 실패했다."));
        } else if (Date.now() >= deadline) {
          setLogin(null); setError(t("ChatGPT login expired. Start again.", "ChatGPT 로그인이 만료됐다. 다시 시작한다."));
        } else timer = setTimeout(() => void poll(), 1500);
      } catch (failure) {
        if (!controller.signal.aborted) { setLogin(null); setError((failure as Error).message); }
      }
    }
    void poll();
    return () => { controller.abort(); clearTimeout(timer); };
  }, [login, t]);

  useEffect(() => {
    onUnsavedChange(!!editing);
    return () => onUnsavedChange(false);
  }, [editing, onUnsavedChange]);

  async function act(work: () => Promise<void>) {
    if (inFlight.current) return;
    inFlight.current = true; setBusy(true); setError(null); setNotice(null);
    try { await work(); }
    catch (failure) { setError((failure as Error).message); }
    finally { inFlight.current = false; setBusy(false); }
  }

  function edit(connection?: AiConnection, method: "api_key" | "chatgpt" = "chatgpt") {
    const authentication = connection?.authentication ?? method;
    const provider = connection?.provider ?? "openai";
    setEditing(connection?.id ?? crypto.randomUUID());
    setName(connection?.name ?? (authentication === "chatgpt" ? "ChatGPT" : providerName(provider)));
    setProvider(provider); setApiKey(""); setNotice(null); setError(null); setAuthentication(authentication);
  }

  function changeAuthentication(method: "api_key" | "chatgpt") {
    setAuthentication(method); setProvider("openai"); setApiKey("");
    if (["ChatGPT", "OpenAI", "Anthropic"].includes(name)) setName(method === "chatgpt" ? "ChatGPT" : "OpenAI");
  }

  function changeProvider(provider: AiConnection["provider"]) {
    setProvider(provider); setApiKey("");
    if (["OpenAI", "Anthropic"].includes(name)) setName(providerName(provider));
  }

  function disconnect(connection: AiConnection) {
    void act(async () => {
      const removed = await disconnectAi(connection.id);
      setConnections(current => current?.filter(value => value.id !== connection.id) ?? []);
      setNotice(t("Account disconnected.", "계정 연결을 해제했다."));
      if (removed.remoteRevocationConfirmed === false) setNotice(t("Disconnected locally. Remote revocation was not confirmed. Disconnect Firecrab in ChatGPT settings.", "로컬 연결을 해제했다. 원격 토큰 폐기는 확인하지 못했다. ChatGPT 설정에서 Firecrab 연결을 해제한다."));
    });
  }

  function save(event: FormEvent) {
    event.preventDefault();
    if (!editing) return;
    void act(async () => {
      if (authentication === "chatgpt") {
        const started = await startAiLogin(editing, name.trim());
        setLogin(started);
        window.open(started.authorizationUrl, "_blank", "noopener,noreferrer");
        return;
      }
      const saved = await saveAiConnection(editing, { name: name.trim(), provider, apiKey });
      setConnections(current => [...(current ?? []).filter(value => value.id !== saved.id), saved]);
      setApiKey(""); setEditing(null);
      setNotice(t("Account verified and saved.", "계정 연결을 확인하고 저장했다."));
    });
  }

  return <section className="pipe-ai" aria-label="Pipe AI">
    <header className="pipe-ai-heading">
      <div className="pipe-ai-title"><span className="pipe-ai-mark"><Sparkles aria-hidden="true" /></span><div><h2>Pipe AI</h2><p>{t("Connect and manage your AI accounts.", "AI 서비스 계정을 연결하고 관리한다.")}</p></div></div>
      {hasConnections && <button className="btn" disabled={busy || waitingLogin || loading || !!editing} onClick={() => edit()}><Plus aria-hidden="true" />{t("Add connection", "연결 추가")}</button>}
    </header>
    {error && <div className="banner error" role="alert"><span>{error}</span></div>}
    {notice && <div className="banner info" role="status"><CircleCheck aria-hidden="true" /><span>{notice}</span></div>}
    {loading && <p className="pipe-ai-loading" role="status"><RefreshCw aria-hidden="true" />{t("Loading connections…", "연결을 불러오는 중…")}</p>}
    {!loading && connections === null && <button className="btn" onClick={() => { setLoading(true); setRevision(value => value + 1); }}><RefreshCw aria-hidden="true" />{t("Retry", "다시 시도")}</button>}

    {!loading && connections?.length === 0 && !editing && <div className="pipe-ai-welcome">
      <span className="pipe-ai-welcome-icon"><LogIn aria-hidden="true" /></span>
      <h3>{t("Connect your AI account", "AI 계정을 연결한다")}</h3>
      <p>{t("Sign in with ChatGPT, or connect an OpenAI or Anthropic API key.", "ChatGPT 로그인 또는 OpenAI·Anthropic API 키로 연결한다.")}</p>
      <div className="pipe-ai-actions"><button className="btn primary" onClick={() => edit()}><ChatGptIcon />{t("Connect ChatGPT", "ChatGPT 연결")}</button><button className="btn" onClick={() => edit(undefined, "api_key")}><KeyRound aria-hidden="true" />{t("Use API key", "API 키로 연결")}</button></div>
      <p className="pipe-ai-security"><ShieldCheck aria-hidden="true" />{t("Credentials are stored only on the API host.", "인증 정보는 API 호스트에서만 저장한다.")}</p>
    </div>}

    {editing && <form onSubmit={save} className="panel pipe-ai-editor" aria-label={t("Connect AI", "AI 연결")}>
      <div className="pipe-ai-section-head"><h3>{authentication === "chatgpt" ? <ChatGptIcon /> : <KeyRound aria-hidden="true" />}{t("Connect an account", "계정 연결")}</h3><button className="icon-btn" type="button" aria-label={t("Cancel", "취소")} disabled={busy || waitingLogin} onClick={() => { setEditing(null); setApiKey(""); }}><X aria-hidden="true" /></button></div>
      <fieldset className="pipe-ai-authentication" disabled={busy || waitingLogin}>
        <legend>{t("Authentication", "인증 방식")}</legend>
        <div className="pipe-ai-auth-options">
          <label><input type="radio" name="pipe-ai-authentication" value="chatgpt" checked={authentication === "chatgpt"} onChange={() => changeAuthentication("chatgpt")} /><span><ChatGptIcon />ChatGPT</span></label>
          <label><input type="radio" name="pipe-ai-authentication" value="api_key" checked={authentication === "api_key"} onChange={() => changeAuthentication("api_key")} /><span><KeyRound aria-hidden="true" />{t("API key", "API 키")}</span></label>
        </div>
      </fieldset>
      <p className="pipe-ai-hint">{authentication === "chatgpt" ? t("Sign in with your ChatGPT account. No API key needed.", "ChatGPT 계정으로 로그인한다. API 키는 필요 없다.") : t("The server checks your API key before saving it.", "서버에서 API 키의 유효성을 확인한 뒤 저장한다.")}</p>
      {authentication === "api_key" && <div className="field"><label htmlFor="pipe-ai-provider">{t("Provider", "서비스")}</label><select id="pipe-ai-provider" value={provider} disabled={busy} onChange={event => changeProvider(event.target.value as AiConnection["provider"])}><option value="openai">OpenAI</option><option value="anthropic">Anthropic (Claude)</option></select></div>}
      <div className="field"><label htmlFor="pipe-ai-name">{t("Connection name", "연결 이름")}</label><input id="pipe-ai-name" required maxLength={100} value={name} disabled={busy || waitingLogin} onChange={event => setName(event.target.value)} /></div>
      {authentication === "api_key" && <div className="field"><label htmlFor="pipe-ai-key">{providerName(provider)} {t("API key", "API 키")}</label><input id="pipe-ai-key" type="password" required autoComplete="off" spellCheck={false} maxLength={4096} value={apiKey} disabled={busy} onChange={event => setApiKey(event.target.value)} /></div>}
      {login && <p className="pipe-ai-login" role="status"><RefreshCw aria-hidden="true" /><span>{t("Waiting for sign-in…", "로그인을 기다리는 중…")} <a href={login.authorizationUrl} target="_blank" rel="noreferrer">{t("Open ChatGPT sign-in", "ChatGPT 로그인 열기")}</a></span></p>}
      <div className="pipe-ai-actions"><button className="btn primary" disabled={busy || waitingLogin} type="submit">{busy || waitingLogin ? <RefreshCw aria-hidden="true" /> : authentication === "chatgpt" ? <ChatGptIcon /> : <ShieldCheck aria-hidden="true" />}{busy ? t("Verifying…", "연결 확인 중…") : authentication === "chatgpt" ? "Continue with ChatGPT" : t("Verify and save", "검증 후 저장")}</button></div>
      {authentication === "chatgpt" && <details className="pipe-ai-details"><summary><ChevronDown aria-hidden="true" />{t("Sign-in help", "로그인 안내")}</summary><div><p>{t("The browser must reach the local API callback at 127.0.0.1. For a remote API host, forward its port before signing in.", "브라우저에서 127.0.0.1의 로컬 API 콜백에 접근할 수 있어야 한다. 원격 API 호스트는 로그인 전에 포트를 전달한다.")}</p></div></details>}
    </form>}

    {hasConnections && <div className="panel pipe-ai-accounts" aria-label={t("AI connections", "AI 연결 목록")}>
      <div className="pipe-ai-section-head"><h3><KeyRound aria-hidden="true" />{t("Connected accounts", "연결한 계정")}</h3><span className="pipe-ai-count">{connections!.length}</span></div>
      <ul className="pipe-ai-connections">{connections!.map(connection => <li key={connection.id}>
        <span className="pipe-ai-account-icon">{connection.authentication === "chatgpt" ? <ChatGptIcon /> : <KeyRound aria-hidden="true" />}</span>
        <div className="pipe-ai-account-info"><strong>{connection.name}</strong><p className="pipe-ai-hint">{connection.authentication === "chatgpt" ? t("ChatGPT account", "ChatGPT 계정") : `${providerName(connection.provider)} ${t("API key", "API 키")}`}</p></div>
        <span className="badge running"><CircleCheck aria-hidden="true" />{t("Connected", "연결됨")}</span>
        <div className="pipe-ai-manage">
          <button className="btn sm" disabled={busy || waitingLogin || !!editing} onClick={() => edit(connection)}>{connection.authentication === "chatgpt" ? <ChatGptIcon /> : <RefreshCw aria-hidden="true" />}{connection.authentication === "chatgpt" ? t("Sign in again", "다시 로그인") : t("Replace API key", "API 키 교체")}</button>
          <button className="btn sm danger" disabled={busy || waitingLogin || !!editing} onClick={() => disconnect(connection)}><Unplug aria-hidden="true" />{t("Disconnect", "연결 해제")}</button>
        </div>
      </li>)}</ul>
    </div>}
    {hasConnections && !editing && <footer className="pipe-ai-footnote"><p className="pipe-ai-security"><ShieldCheck aria-hidden="true" />{t("Credentials stay on the server.", "인증 정보는 서버에서 관리한다.")}</p>{connections!.some(connection => connection.authentication === "chatgpt") && <a href="https://chatgpt.com/#settings/Usage" target="_blank" rel="noreferrer">{t("Manage ChatGPT usage", "ChatGPT 사용량 관리")}</a>}</footer>}
  </section>;
}
