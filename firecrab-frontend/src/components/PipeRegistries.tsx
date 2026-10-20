import { useEffect, useRef, useState, type FormEvent } from "react";
import { useI18n } from "../i18n";
import { getOciImport, startOciImport } from "../api/client";
import { deleteRegistry, getRegistries, getRegistryImages, getRegistryTags, getRegistryTransfer, saveRegistry, startRegistryTransfer, type RegistryConnection, type RegistryImage, type RegistryTransfer } from "../api/registries";
import type { ImageInstallResponse } from "../bindings";

type ActiveJob = { kind: "transfer" | "import"; id: string };
const JOB_KEY = "firecrab.registry-job";
function restoredJob(): ActiveJob | null {
  try {
    const value = JSON.parse(localStorage.getItem(JOB_KEY) ?? "null");
    return value && ["transfer", "import"].includes(value.kind) && typeof value.id === "string" ? value : null;
  } catch { return null; }
}
function pinnedReference(image: RegistryImage): string {
  const name = image.reference.split("@")[0];
  const colon = name.lastIndexOf(":");
  return `${colon > name.lastIndexOf("/") ? name.slice(0, colon) : name}@${image.digest}`;
}

export default function PipeRegistries({ onUnsavedChange }: { onUnsavedChange: (dirty: boolean) => void }) {
  const { t } = useI18n();
  const [connections, setConnections] = useState<RegistryConnection[]>([]);
  const [images, setImages] = useState<RegistryImage[]>([]);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [editor, setEditor] = useState(false);
  const [provider, setProvider] = useState("ghcr");
  const [host, setHost] = useState("");
  const [username, setUsername] = useState("");
  const [secret, setSecret] = useState("");
  const [registry, setRegistry] = useState("ghcr.io");
  const [repository, setRepository] = useState("");
  const [tag, setTag] = useState("latest");
  const [tags, setTags] = useState<string[]>([]);
  const [nextLast, setNextLast] = useState<string | null>(null);
  const [activeJob, setActiveJob] = useState<ActiveJob | null>(restoredJob);
  const [job, setJob] = useState<ImageInstallResponse | null>(null);
  const inFlight = useRef(false);
  const disabled = loading || busy || activeJob !== null;
  const [initial, setInitial] = useState({ host: "", username: "" });
  const dirty = editor && (!!secret || host !== initial.host || username !== initial.username);

  useEffect(() => { onUnsavedChange(dirty); return () => onUnsavedChange(false); }, [dirty, onUnsavedChange]);
  useEffect(() => {
    let cancelled = false;
    void Promise.all([getRegistries(), getRegistryImages()]).then(([connections, images]) => {
      if (!cancelled) { setConnections(connections); setImages(images); }
    }).catch(failure => { if (!cancelled) setError((failure as Error).message); })
      .finally(() => { if (!cancelled) setLoading(false); });
    return () => { cancelled = true; };
  }, []);

  useEffect(() => {
    try {
      if (activeJob) localStorage.setItem(JOB_KEY, JSON.stringify(activeJob));
      else localStorage.removeItem(JOB_KEY);
    } catch { /* Job recovery is optional when browser storage is unavailable. */ }
    if (!activeJob) return;
    let cancelled = false;
    let timer: ReturnType<typeof setTimeout>;
    async function poll() {
      try {
        const next = await (activeJob!.kind === "transfer" ? getRegistryTransfer(activeJob!.id) : getOciImport(activeJob!.id));
        if (cancelled) return;
        setJob(next);
        if (next.status === "running") { timer = setTimeout(poll, 1_000); return; }
        if (next.status === "succeeded") {
          setNotice(activeJob!.kind === "import" ? t("M2Image import completed.", "M2Image 가져오기를 완료했다.") : t("Image pull completed.", "이미지 가져오기를 완료했다."));
          const images = await getRegistryImages();
          if (!cancelled) setImages(images);
        } else {
          setError(next.status === "idle" ? t("The job is no longer available. Refresh the local image list before retrying.", "작업 상태가 없다. 로컬 이미지 목록을 새로 고친 뒤 다시 시도한다.") : next.log.split("\n").filter(Boolean).at(-1) ?? t("Image pull failed.", "이미지 가져오기에 실패했다."));
        }
        if (!cancelled) setActiveJob(null);
      } catch (failure) {
        if (!cancelled) { setError((failure as Error).message); timer = setTimeout(poll, 5_000); }
      }
    }
    void poll();
    return () => { cancelled = true; clearTimeout(timer); };
  }, [activeJob, t]);

  async function perform(action: () => Promise<void>) {
    if (inFlight.current) return;
    inFlight.current = true; setBusy(true); setError(null); setNotice(null);
    try { await action(); } catch (failure) { setError((failure as Error).message); }
    finally { inFlight.current = false; setBusy(false); }
  }
  function resetEditor() { setEditor(false); setHost(""); setUsername(""); setSecret(""); }
  function edit(connection?: RegistryConnection) {
    const host = connection?.registry ?? "";
    const username = connection?.username ?? "";
    setProvider(connection?.provider ?? "ghcr"); setHost(host); setUsername(username); setInitial({ host, username }); setSecret(""); setEditor(true);
  }
  async function login(event: FormEvent) {
    event.preventDefault();
    await perform(async () => {
      const loginRegistry = provider === "ecr" ? host.trim() : provider === "dockerHub" ? "docker.io" : "ghcr.io";
      const saved = await saveRegistry(loginRegistry, provider === "ecr" ? "AWS" : username.trim(), secret);
      setConnections(current => [...current.filter(value => value.registry !== saved.registry), saved].sort((a,b) => a.registry.localeCompare(b.registry)));
      setRegistry(saved.registry); setTags([]); setNextLast(null); resetEditor(); setNotice(t("Registry login saved.", "레지스트리 인증을 저장했다."));
    });
  }
  async function browse(more = false) {
    await perform(async () => {
      const page = await getRegistryTags(`${registry}/${repository.trim()}:latest`, more ? nextLast ?? undefined : undefined);
      setTags(current => more ? [...new Set([...current, ...page.tags])] : page.tags); setNextLast(page.nextLast);
      if (!more && page.tags.length) setTag(page.tags[0]);
      if (!page.tags.length) setNotice(t("This repository has no tags.", "이 저장소에 태그가 없다."));
    });
  }
  async function transfer(body: RegistryTransfer) {
    await perform(async () => {
      const next = await startRegistryTransfer(body);
      setJob(next); setActiveJob({ kind: "transfer", id: next.alias });
    });
  }
  const registries = [...new Set(["docker.io", "ghcr.io", "public.ecr.aws", ...connections.map(value => value.registry)])];
  return <section className="panel pipe-registries" aria-label="Pipe Registry">
    <h2 className="panel-title"><span>Pipe Registry</span><button className="btn" disabled={disabled || editor} onClick={() => edit()}>{t("Add registry", "레지스트리 추가")}</button></h2>
    <p className="pipe-description">{t("Connect Docker Hub, GHCR or Amazon ECR and pull OCI images to this host. Import a pulled image as M2Image to run it in a VM.", "Docker Hub, GHCR 또는 Amazon ECR에 연결해 OCI 이미지를 이 호스트로 가져온다. VM에서 실행하려면 가져온 이미지를 M2Image로 변환한다.")}</p>
    {error && <div className="banner error" role="alert">{error}</div>}
    {notice && <div className="banner info" role="status">{notice}</div>}
    {loading && <p role="status">{t("Loading registries…", "레지스트리 조회 중…")}</p>}
    {editor && <form className="registry-form" onSubmit={login} aria-label={t("Registry login", "레지스트리 인증")}>
      <label className="field"><span>{t("Provider", "서비스")}</span><select value={provider} disabled={disabled} onChange={event => { setProvider(event.target.value); setHost(""); setUsername(""); setSecret(""); }}><option value="dockerHub">Docker Hub</option><option value="ghcr">GitHub Container Registry</option><option value="ecr">Amazon ECR</option></select></label>
      {provider === "ecr" && <label className="field"><span>{t("Registry hostname", "레지스트리 호스트")}</span><input value={host} onChange={event => setHost(event.target.value)} placeholder="123456789012.dkr.ecr.ap-northeast-2.amazonaws.com" disabled={disabled} required /></label>}
      {provider !== "ecr" && <label className="field"><span>{provider === "dockerHub" ? t("Docker Hub username", "Docker Hub 사용자명") : t("GitHub username", "GitHub 사용자명")}</span><input autoComplete="username" value={username} onChange={event => setUsername(event.target.value)} disabled={disabled} required /></label>}
      <label className="field"><span>{provider === "ghcr" ? t("Personal access token (classic)", "개인 액세스 토큰 (classic)") : provider === "dockerHub" ? t("Personal access token (PAT)", "개인 액세스 토큰 (PAT)") : t("ECR login password", "ECR 로그인 비밀번호")}</span><input type="password" autoComplete="new-password" value={secret} onChange={event => setSecret(event.target.value)} disabled={disabled} required /></label>
      <p className="registry-help registry-wide">{provider === "dockerHub" ? t("Use a personal access token with repository read access. This login is shared with existing Docker Hub settings.", "저장소 읽기 권한이 있는 개인 액세스 토큰을 사용한다. 기존 Docker Hub 설정과 같은 인증을 사용한다.") : provider === "ghcr" ? t("Use read:packages to pull. Authorize the token for your organization if SSO is required.", "가져오기에는 read:packages 권한을 사용한다. 조직에서 SSO를 사용하면 토큰을 승인한다.") : t("Paste the output of aws ecr get-login-password --region REGION. The username is AWS. Tokens expire after 12 hours; update the login to renew. For public ECR, use aws ecr-public get-login-password --region us-east-1.", "aws ecr get-login-password --region REGION의 결과를 입력한다. 사용자명은 AWS이다. 토큰은 12시간 뒤 만료되므로 인증을 갱신한다. 공개 ECR은 aws ecr-public get-login-password --region us-east-1을 사용한다.")}</p>
      <div className="pipe-row-actions registry-wide"><button className="btn primary" disabled={disabled} type="submit">{t("Save login", "인증 저장")}</button><button className="btn" type="button" disabled={busy} onClick={resetEditor}>{t("Cancel", "취소")}</button></div>
    </form>}
    {!loading && connections.length > 0 && <div className="table-scroll"><table className="vm-table registry-table"><thead><tr><th>{t("Registry", "레지스트리")}</th><th>{t("Account", "계정")}</th><th className="actions">{t("Actions", "작업")}</th></tr></thead><tbody>{connections.map(connection => <tr key={connection.registry}><td className="registry-reference">{connection.registry}</td><td>{connection.username}</td><td className="actions"><div className="pipe-row-actions"><button className="btn" disabled={disabled || editor} onClick={() => edit(connection)}>{t("Update token", "토큰 갱신")}</button><button className="btn danger" disabled={disabled} onClick={() => void perform(async () => { await deleteRegistry(connection.registry); setConnections(current => current.filter(value => value.registry !== connection.registry)); if (registry === connection.registry) setRegistry("ghcr.io"); setNotice(t("Registry login removed.", "레지스트리 인증을 삭제했다.")); })}>{t("Remove", "삭제")}</button></div></td></tr>)}</tbody></table></div>}
    {!loading && connections.length === 0 && <p className="registry-help">{t("No saved logins. Public images can be pulled without a login. Save a login to pull private images.", "저장한 인증이 없다. 공개 이미지는 인증 없이 가져올 수 있다. 비공개 이미지를 가져오려면 인증을 저장한다.")}</p>}
    <h3>{t("Pull an image", "이미지 가져오기")}</h3>
    <form className="registry-form" onSubmit={event => { event.preventDefault(); const version = tag.trim(); void transfer({ action: "pull", reference: `${registry}/${repository.trim()}${version.startsWith("sha256:") ? "@" : ":"}${version}` }); }}>
      <label className="field"><span>{t("Source registry", "원본 레지스트리")}</span><select value={registry} disabled={disabled} onChange={event => { setRegistry(event.target.value); setTags([]); setNextLast(null); }}>{registries.map(host => <option key={host} value={host}>{host === "docker.io" ? "Docker Hub (docker.io)" : host}</option>)}</select></label>
      <label className="field"><span>{t("Repository", "저장소")}</span><input placeholder={registry === "docker.io" ? "nginx or owner/image" : "owner/image"} value={repository} disabled={disabled} onChange={event => { setRepository(event.target.value); setTags([]); setNextLast(null); }} required /></label>
      <label className="field"><span>{t("Tag or digest", "태그 또는 다이제스트")}</span><input list="registry-tags" value={tag} disabled={disabled} onChange={event => setTag(event.target.value)} required /><datalist id="registry-tags">{tags.map(tag => <option key={tag} value={tag} />)}</datalist></label>
      <div className="pipe-row-actions registry-wide"><button className="btn" type="button" disabled={disabled || !repository.trim()} onClick={() => void browse()}>{t("List tags", "태그 조회")}</button>{nextLast && <button className="btn" type="button" disabled={disabled} onClick={() => void browse(true)}>{t("More tags", "태그 더 보기")}</button>}<button className="btn primary" type="submit" disabled={disabled || !repository.trim() || !tag.trim()}>{t("Pull image", "이미지 가져오기")}</button></div>
    </form>
    {tags.length > 0 && <p className="registry-help">{t(`${tags.length} tags loaded. Select a tag in the tag field.`, `태그 ${tags.length}개를 조회했다. 태그 입력란에서 선택한다.`)}</p>}
    <h3>{t("Local OCI images", "로컬 OCI 이미지")}</h3>
    <p className="registry-help">{t("A pull keeps the selected platform's config and layers on this host. Other platforms, signatures, and attestations are not included.", "가져오기는 선택한 플랫폼의 설정과 레이어를 이 호스트에 보관한다. 다른 플랫폼, 서명과 증명 자료는 포함하지 않는다.")}</p>
    <button className="btn" disabled={disabled} onClick={() => void perform(async () => { setImages(await getRegistryImages()); })}>{t("Refresh images", "이미지 새로 고침")}</button>
    {!loading && images.length === 0 && <p className="registry-help">{t("No local OCI images. Pull an image to enable M2Image import.", "로컬 OCI 이미지가 없다. 이미지를 가져오면 M2Image 변환을 사용할 수 있다.")}</p>}
    {images.length > 0 && <div className="table-scroll"><table className="vm-table registry-table"><thead><tr><th>{t("Image", "이미지")}</th><th>{t("Architecture", "아키텍처")}</th><th>{t("Size", "크기")}</th><th className="actions">{t("Actions", "작업")}</th></tr></thead><tbody>{images.map(image => <tr key={image.id}><td className="registry-reference">{image.reference}<small>{image.digest}</small></td><td>{image.architecture}</td><td>{(image.sizeBytes / 1024 / 1024).toFixed(1)} MiB</td><td className="actions"><div className="pipe-row-actions"><button className="btn" disabled={disabled} onClick={() => void perform(async () => { const next = await startOciImport({ reference: pinnedReference(image) }); setJob(next); setActiveJob({ kind: "import", id: next.alias }); })}>{t("Import as M2Image", "M2Image로 변환")}</button></div></td></tr>)}</tbody></table></div>}
    {job && <div aria-live="polite"><p className="registry-help">{job.status === "running" ? t("Image operation in progress…", "이미지 작업 진행 중…") : t("Last image operation", "마지막 이미지 작업")}</p><pre className="registry-log">{job.log}</pre></div>}
  </section>;
}
