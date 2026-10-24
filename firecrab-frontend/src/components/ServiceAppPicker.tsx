import { useEffect, useState } from "react";
import { KeyRound, RefreshCw } from "lucide-react";
import { getPipeAuth, type AuthConnection } from "../api/pipeAuth";
import { getRegistryRepositories, getRegistryTags, type RegistryRepositories, type RegistryTags } from "../api/registries";
import { useI18n } from "../i18n";
import { pipeCardHash } from "../navigation";

export interface ServiceAppSelection { connection: AuthConnection; reference: string }
type ImageAuth = AuthConnection & { registry: string };

/** Only provider accounts connected through Pipe Auth can supply an app. */
export default function ServiceAppPicker({ value, disabled, onChange }: {
  value: string; disabled: boolean; onChange: (selection: ServiceAppSelection | null) => void;
}) {
  const { t } = useI18n();
  const [initialReference] = useState(value);
  const [connections, setConnections] = useState<ImageAuth[] | null>(null);
  const [account, setAccount] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [version, setVersion] = useState(0);
  useEffect(() => {
    const controller = new AbortController();
    onChange(null);
    getPipeAuth(controller.signal).then((rows) => {
      if (!controller.signal.aborted) setConnections(rows.filter((row): row is ImageAuth => row.registry !== null && row.services.includes("registry")));
    }).catch((failure) => { if (!controller.signal.aborted) setError((failure as Error).message); });
    return () => controller.abort();
  }, [version, onChange]);
  const selected = connections?.find((row) => row.provider === account)
    ?? connections?.find((row) => initialReference.startsWith(`${row.registry}/`))
    ?? (connections?.length === 1 ? connections[0] : undefined);
  const reload = () => { onChange(null); setConnections(null); setAccount(""); setError(null); setVersion((previous) => previous + 1); };

  return <div className="service-app-picker">
    <div className="service-auth-note"><KeyRound aria-hidden="true" /><p>{t("Choose an app from an account connected in Pipe Auth. Only that account's repositories and versions are available.", "Pipe Auth에 연결된 계정에서 앱을 선택한다. 해당 계정의 저장소와 버전만 사용할 수 있다.")}</p><a href={pipeCardHash("auth")}>{t("Manage AUTH", "AUTH 관리")}</a></div>
    {error ? <div className="service-picker-error"><p className="field-error" role="alert">{error}</p><button className="btn" type="button" disabled={disabled} onClick={reload}>{t("Retry accounts", "계정 다시 조회")}</button></div>
      : connections === null ? <p className="service-hint" role="status">{t("Loading connected accounts…", "연결된 계정 조회 중…")}</p>
      : !connections.length ? <div className="service-auth-empty"><strong>{t("Connect an account first", "먼저 계정을 연결한다")}</strong><p>{t("Connect GitHub or AWS in Pipe Auth, then return here to select your app.", "Pipe Auth에서 GitHub 또는 AWS 계정을 연결한다. 연결한 뒤 이 화면에서 앱을 선택한다.")}</p><a className="btn primary" href={pipeCardHash("auth")}>{t("Connect in AUTH", "AUTH에서 계정 연결")}</a><button type="button" className="btn" disabled={disabled} onClick={reload}><RefreshCw aria-hidden="true" />{t("Check connections", "연결 다시 확인")}</button></div>
      : <>
        <div className="field"><label htmlFor="service-account">{t("AUTH account", "AUTH 계정")}</label><select id="service-account" value={selected?.provider ?? ""} disabled={disabled} onChange={(event) => { onChange(null); setAccount(event.target.value); }} required aria-describedby="service-account-hint"><option value="" disabled>{t("Select a connected account", "연결된 계정 선택")}</option>{connections.map((row) => <option key={row.provider} value={row.provider}>{row.provider === "github" ? "GitHub" : "AWS"} · {row.account}{row.region ? ` · ${row.region}` : ""}</option>)}</select><span className="service-hint" id="service-account-hint">{selected?.registry ?? t("Only accounts connected in AUTH are listed.", "AUTH에 연결된 계정만 표시한다.")}</span></div>
        {selected && <AccountApps key={`${version}:${selected.provider}:${selected.account}:${selected.registry}`} connection={selected} initialReference={initialReference} disabled={disabled} onChange={onChange} />}
      </>}
  </div>;
}

function AccountApps({ connection, initialReference, disabled, onChange }: {
  connection: ImageAuth; initialReference: string; disabled: boolean; onChange: (selection: ServiceAppSelection | null) => void;
}) {
  const { t } = useI18n();
  const prefix = `${connection.registry}/`;
  const [initialRepository] = useState(() => initialReference.startsWith(prefix) ? initialReference.slice(prefix.length).split(/[:@]/)[0] ?? "" : "");
  const [repository, setRepository] = useState(initialRepository);
  const [catalog, setCatalog] = useState<RegistryRepositories | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [version, setVersion] = useState(0);
  const [pageNumber, setPageNumber] = useState(1);
  const [cursor, setCursor] = useState<string | null>(null);
  useEffect(() => {
    let active = true;
    getRegistryRepositories(connection.registry, pageNumber, cursor).then((next) => {
      if (active) setCatalog((previous) => ({ ...next, repositories: [...new Set([...(previous?.repositories ?? []), ...next.repositories])] }));
    }).catch((failure) => { if (active) setError((failure as Error).message); })
      .finally(() => { if (active) setLoading(false); });
    return () => { active = false; };
  }, [connection.registry, pageNumber, cursor, version]);
  const selected = catalog?.repositories.includes(repository) ? repository : "";
  return <>
    <div className="field"><label htmlFor="service-repository">{t("App repository", "앱 저장소")}</label><select id="service-repository" value={selected} disabled={disabled || !catalog?.listingSupported || loading} onChange={(event) => { onChange(null); setRepository(event.target.value); }} required aria-describedby="service-repository-hint"><option value="" disabled>{t("Select an app repository", "앱 저장소 선택")}</option>{catalog?.repositories.map((name) => <option key={name} value={name}>{name}</option>)}</select><span id="service-repository-hint" className="service-hint">{t("Choose a repository returned by the connected account.", "연결된 계정에서 조회한 저장소를 선택한다.")}</span></div>
    {loading && <p role="status" className="service-hint">{t("Loading app repositories…", "앱 저장소 조회 중…")}</p>}
    {error && <div><p role="alert" className="field-error">{error}</p><button type="button" className="btn" disabled={disabled || loading} onClick={() => { setLoading(true); setError(null); setVersion((previous) => previous + 1); }}>{t("Retry repositories", "저장소 다시 조회")}</button></div>}
    {catalog && !catalog.listingSupported && <p className="field-error" role="alert">{t("The connected account cannot list repositories. Update its credentials in AUTH.", "연결된 계정으로 저장소 목록을 조회할 수 없다. AUTH에서 인증 정보를 갱신한다.")}</p>}
    {catalog?.listingSupported && !catalog.repositories.length && !loading && <p className="service-hint">{t("This account has no app repositories. Publish an image to the account, then refresh.", "이 계정에 앱 저장소가 없다. 계정에 이미지를 게시한 뒤 다시 조회한다.")}</p>}
    {catalog?.nextPage && <button className="btn" type="button" disabled={disabled || loading} onClick={() => { setLoading(true); setError(null); setPageNumber(catalog.nextPage!); setCursor(catalog.nextCursor ?? null); }}>{t("More repositories", "저장소 더 보기")}</button>}
    {!catalog?.repositories.length && !loading && !error && <button className="btn" type="button" disabled={disabled} onClick={() => { setLoading(true); setVersion((previous) => previous + 1); }}>{t("Refresh repositories", "저장소 새로 고침")}</button>}
    {selected && <AppVersions key={selected} connection={connection} repository={selected} initialReference={initialReference} disabled={disabled} onChange={onChange} />}
  </>;
}

function AppVersions({ connection, repository, initialReference, disabled, onChange }: {
  connection: ImageAuth; repository: string; initialReference: string; disabled: boolean; onChange: (selection: ServiceAppSelection | null) => void;
}) {
  const { t } = useI18n();
  const reference = `${connection.registry}/${repository}`;
  const [initialTag] = useState(() => initialReference.startsWith(`${reference}:`) ? initialReference.slice(reference.length + 1) : "");
  const [tag, setTag] = useState(initialTag);
  const [catalog, setCatalog] = useState<RegistryTags | null>(null);
  const [last, setLast] = useState<string | undefined>();
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [version, setVersion] = useState(0);
  useEffect(() => {
    let active = true;
    getRegistryTags(`${reference}:latest`, last).then((next) => {
      if (active) setCatalog((previous) => ({ ...next, tags: [...new Set([...(previous?.tags ?? []), ...next.tags])] }));
    }).catch((failure) => { if (active) setError((failure as Error).message); })
      .finally(() => { if (active) setLoading(false); });
    return () => { active = false; };
  }, [reference, last, version]);
  const selected = !loading && !error && catalog?.tags.includes(tag) ? tag : "";
  useEffect(() => {
    onChange(selected ? { connection, reference: `${reference}:${selected}` } : null);
  }, [connection, reference, selected, onChange]);
  return <>
    <div className="field"><label htmlFor="service-tag">{t("App version", "앱 버전")}</label><select id="service-tag" value={selected} disabled={disabled || loading || !!error} required onChange={(event) => setTag(event.target.value)} aria-describedby="service-tag-hint"><option value="" disabled>{t("Select a version", "버전 선택")}</option>{catalog?.tags.map((name) => <option key={name} value={name}>{name}</option>)}</select><span className="service-hint" id="service-tag-hint">{t("Versions are the image tags published in this repository.", "버전은 이 저장소에 게시된 이미지 태그이다.")}</span></div>
    {loading && <p role="status" className="service-hint">{t("Loading app versions…", "앱 버전 조회 중…")}</p>}
    {error && <div><p role="alert" className="field-error">{error}</p><button className="btn" type="button" disabled={disabled || loading} onClick={() => { setLoading(true); setError(null); setVersion((previous) => previous + 1); }}>{t("Retry versions", "버전 다시 조회")}</button></div>}
    {catalog && !catalog.tags.length && !loading && <p className="service-hint">{t("This repository has no versions. Publish an image tag, then refresh.", "이 저장소에 버전이 없다. 이미지 태그를 게시한 뒤 다시 조회한다.")}</p>}
    {catalog && !catalog.tags.length && !loading && !error && <button className="btn" type="button" disabled={disabled} onClick={() => { setLoading(true); setVersion((previous) => previous + 1); }}>{t("Refresh versions", "버전 새로 고침")}</button>}
    {catalog?.nextLast && <button className="btn" type="button" disabled={disabled || loading} onClick={() => { onChange(null); setLoading(true); setError(null); setLast(catalog.nextLast!); }}>{t("More versions", "버전 더 보기")}</button>}
    {selected && <div className="service-selected-image"><span>{t("Selected image", "선택한 이미지")}</span><code>{reference}:{selected}</code></div>}
  </>;
}
