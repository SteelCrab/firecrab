import { useEffect, useState, type FormEvent } from "react";
import { getRegistryImages, getRegistryRepositories, getRegistryTags, type RegistryImage, type RegistryRepositories, type RegistryTags } from "../api/registries";
import { useI18n } from "../i18n";

/** Local pull record: it does not tell whether the remote tag moved since. */
function LocalImageStatus({ pulled, unavailable }: { pulled: boolean | undefined; unavailable: boolean }) {
  const { t } = useI18n();
  const label = unavailable ? t("Status unavailable", "상태 확인 불가")
    : pulled === undefined ? t("Checking…", "확인 중…")
    : pulled ? t("Pulled", "가져옴") : t("Not pulled", "가져오지 않음");
  return <span className={`state-badge${pulled && !unavailable ? " running" : ""}`}>{label}</span>;
}

function ImageTags({ registry, repository, images, imagesUnavailable, disabled, pulling, onPull }: {
  registry: string;
  repository: string;
  images: RegistryImage[] | null;
  imagesUnavailable: boolean;
  disabled: boolean;
  pulling: boolean;
  onPull: (reference: string) => Promise<void>;
}) {
  const { t } = useI18n();
  const [page, setPage] = useState<RegistryTags | null>(null);
  const [selectedTag, setSelectedTag] = useState("");
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [refresh, setRefresh] = useState(0);

  useEffect(() => {
    let cancelled = false;
    void getRegistryTags(`${registry}/${repository}:latest`).then(page => {
      if (cancelled) return;
      setPage(page); setSelectedTag(page.tags[0] ?? "");
    }).catch(failure => {
      if (!cancelled) setError((failure as Error).message);
    }).finally(() => {
      if (!cancelled) setLoading(false);
    });
    return () => { cancelled = true; };
  }, [registry, repository, refresh]);

  function reload() {
    setPage(null); setSelectedTag(""); setError(null); setLoading(true);
    setRefresh(value => value + 1);
  }

  async function moreTags() {
    if (!page?.nextLast || loading) return;
    setLoading(true); setError(null);
    try {
      const next = await getRegistryTags(`${registry}/${repository}:latest`, page.nextLast);
      setPage({ ...next, tags: [...new Set([...page.tags, ...next.tags])] });
    } catch (failure) {
      setError((failure as Error).message);
    } finally {
      setLoading(false);
    }
  }

  const reference = `${registry}/${repository}:${selectedTag}`;
  const pulled = (tag: string) => !imagesUnavailable && !!images?.some(image => image.reference === `${registry}/${repository}:${tag}`);
  const canPull = !disabled && !pulling && !loading && !error && !!selectedTag && !!page?.tags.includes(selectedTag);
  function submit(event: FormEvent) {
    event.preventDefault();
    if (canPull) void onPull(reference);
  }
  return <div className="subpanel registry-pull" role="group" aria-label={t("Selected repository", "선택한 저장소")}>
    {error && <div className="banner error" role="alert">{error}</div>}
    {loading && <p className="poll-note" role="status">{t("Loading tags…", "태그 조회 중…")}</p>}
    {page && !page.tags.length && <div className="empty">{t("No tags.", "태그가 없다.")}</div>}
    {(!!page?.tags.length || !!error) && <form className="pipe-controls registry-pull-form" onSubmit={submit}>
      {!!page?.tags.length && <div className="field"><label htmlFor="registry-image-tag">{t("Tag", "태그")}</label>
        <select id="registry-image-tag" value={selectedTag} disabled={loading || !!error} onChange={event => setSelectedTag(event.target.value)}>
          {page.tags.map(tag => <option key={tag} value={tag}>{tag}{pulled(tag) ? t(" · Pulled", " · 가져옴") : ""}</option>)}
        </select>
      </div>}
      {page?.nextLast && <button className="btn" type="button" disabled={loading} onClick={() => void moreTags()}>{t("More tags", "태그 더 보기")}</button>}
      {error && <button className="btn" type="button" disabled={loading} onClick={reload}>{t("Retry tags", "태그 다시 조회")}</button>}
      <button className="btn primary" type="submit" disabled={!canPull}>{t("Pull image", "이미지 가져오기")}</button>
    </form>}
    {selectedTag && <dl className="detail-fields mono registry-selection">
      <dt>{t("Image", "이미지")}</dt><dd>{reference}</dd>
      <dt>{t("Status", "상태")}</dt><dd><LocalImageStatus pulled={images ? pulled(selectedTag) : undefined} unavailable={imagesUnavailable} /></dd>
    </dl>}
  </div>;
}

/** Repositories of one saved registry; the selected row's tags open below the table. */
export default function RegistryBrowser({ registry, imageVersion, disabled, pulling, onPull }: {
  registry: string;
  imageVersion: number;
  disabled: boolean;
  pulling: boolean;
  onPull: (reference: string) => Promise<void>;
}) {
  const { t } = useI18n();
  const [catalog, setCatalog] = useState<RegistryRepositories | null>(null);
  const [repository, setRepository] = useState("");
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [refresh, setRefresh] = useState(0);
  const [images, setImages] = useState<RegistryImage[] | null>(null);
  const [imagesUnavailable, setImagesUnavailable] = useState(false);

  useEffect(() => {
    let cancelled = false;
    void getRegistryImages().then(images => {
      if (!cancelled) { setImages(images); setImagesUnavailable(false); }
    }).catch(() => { if (!cancelled) setImagesUnavailable(true); });
    return () => { cancelled = true; };
  }, [imageVersion, refresh]);

  useEffect(() => {
    let cancelled = false;
    void getRegistryRepositories(registry).then(page => {
      if (cancelled) return;
      setCatalog(page); setRepository(page.repositories[0] ?? "");
    }).catch(failure => {
      if (!cancelled) setError((failure as Error).message);
    }).finally(() => {
      if (!cancelled) setLoading(false);
    });
    return () => { cancelled = true; };
  }, [registry, refresh]);

  function reload() {
    setCatalog(null); setRepository(""); setError(null); setLoading(true);
    setRefresh(value => value + 1);
  }

  async function moreRepositories() {
    if (!catalog?.nextPage || loading) return;
    setLoading(true); setError(null);
    try {
      const page = await getRegistryRepositories(registry, catalog.nextPage);
      setCatalog({ ...page, repositories: [...new Set([...catalog.repositories, ...page.repositories])] });
    } catch (failure) {
      setError((failure as Error).message);
    } finally {
      setLoading(false);
    }
  }

  const pulled = (value: string) => images?.some(image => image.reference.startsWith(`${registry}/${value}:`) || image.reference.startsWith(`${registry}/${value}@`));
  return <section className="pipe-subsection registry-browser" aria-labelledby="registry-repositories-title">
    <h3 className="panel-title"><span id="registry-repositories-title">{t("Repositories", "저장소")} <span className="registry-source">{registry}</span></span>
      {catalog?.listingSupported !== false && <button className="btn" type="button" disabled={disabled || loading} onClick={reload}>{t("Refresh", "새로고침")}</button>}
    </h3>
    {error && <div className="banner error" role="alert">{error}</div>}
    {imagesUnavailable && <p className="field-error" role="alert">{t("Local image status unavailable. Refresh to retry.", "로컬 이미지 상태를 확인하지 못했다. 새로고침으로 다시 확인한다.")}</p>}
    {loading && <p className="poll-note" role="status">{t("Loading repositories…", "저장소 조회 중…")}</p>}
    {catalog && !catalog.listingSupported && <div className="empty">{t("Repository browsing is not available for ECR.", "ECR 저장소 목록 조회를 지원하지 않는다.")}</div>}
    {catalog?.listingSupported && !catalog.repositories.length && <div className="empty">{t("No repositories.", "저장소가 없다.")}</div>}
    {!!catalog?.repositories.length && <div className="table-scroll registry-catalog-scroll">
      <table className="vm-table registry-catalog" aria-label={t("Repositories", "저장소")}>
        <thead><tr><th scope="col">{t("Repository", "저장소")}</th><th scope="col">{t("Status", "상태")}</th></tr></thead>
        <tbody>{catalog.repositories.map(value => <tr key={value} className={repository === value ? "selected" : undefined} onClick={() => setRepository(value)}>
          <td className="registry-name"><button className="registry-select" type="button" aria-pressed={repository === value}>{value}</button></td>
          <td className="registry-status"><LocalImageStatus pulled={pulled(value)} unavailable={imagesUnavailable} /></td>
        </tr>)}</tbody>
      </table>
    </div>}
    {catalog?.nextPage && <div className="pipe-controls"><button className="btn" type="button" disabled={disabled || loading} onClick={() => void moreRepositories()}>{t("More repositories", "저장소 더 보기")}</button></div>}
    {repository && <ImageTags key={repository} registry={registry} repository={repository} images={images} imagesUnavailable={imagesUnavailable} disabled={disabled} pulling={pulling} onPull={onPull} />}
  </section>;
}
