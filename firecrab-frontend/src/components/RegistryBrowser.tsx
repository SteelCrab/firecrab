import { useEffect, useState } from "react";
import { getRegistryRepositories, getRegistryTags, type RegistryRepositories, type RegistryTags } from "../api/registries";
import { useI18n } from "../i18n";

function ImageTags({ registry, repository, disabled, onPull }: {
  registry: string;
  repository: string;
  disabled: boolean;
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
  const canPull = !disabled && !loading && !!selectedTag && !!page?.tags.includes(selectedTag);
  return <div className="registry-image-tags" aria-label={t("Repository tags", "저장소 태그")}>
    <h4>{t("Image tags", "이미지 태그")}</h4>
    {loading && <p className="registry-help" role="status">{t("Loading image tags…", "이미지 태그 조회 중…")}</p>}
    {error && <div className="banner error" role="alert">{error}</div>}
    {page && page.tags.length === 0 && <p className="registry-help" role="status">{t("This repository has no tags.", "이 저장소에 태그가 없다.")}</p>}
    {!!page?.tags.length && <div className="registry-tag-list" role="group" aria-label={t("Image tags", "이미지 태그")}>
      {page.tags.map(tag => <button className="btn" key={tag} disabled={disabled || loading} aria-pressed={selectedTag === tag} onClick={() => setSelectedTag(tag)}>{tag}</button>)}
    </div>}
    <div className="pipe-row-actions registry-browser-actions">
      {page?.nextLast && <button className="btn" disabled={disabled || loading} onClick={() => void moreTags()}>{t("More tags", "태그 더 보기")}</button>}
      {error && <button className="btn" disabled={disabled || loading} onClick={reload}>{t("Retry tags", "태그 다시 조회")}</button>}
      <button className="btn primary" disabled={!canPull} onClick={() => { if (canPull) void onPull(reference); }}>{t("Pull image", "이미지 가져오기")}</button>
    </div>
    {selectedTag && <p className="registry-help registry-selected" role="status">{t("Selected image", "선택한 이미지")}: <code>{reference}</code></p>}
  </div>;
}

export default function RegistryBrowser({ registry, disabled, onPull }: {
  registry: string;
  disabled: boolean;
  onPull: (reference: string) => Promise<void>;
}) {
  const { t } = useI18n();
  const [catalog, setCatalog] = useState<RegistryRepositories | null>(null);
  const [repository, setRepository] = useState("");
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [refresh, setRefresh] = useState(0);

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

  return <div className="registry-browser">
    {error && <div className="banner error" role="alert">{error}</div>}
    {loading && <p className="registry-help" role="status">{t("Loading registry images…", "레지스트리 이미지 조회 중…")}</p>}
    {catalog && !catalog.listingSupported && <p className="registry-help" role="status">{t("Repository lookup requires AWS IAM.", "저장소 조회에는 AWS IAM 연동이 필요하다.")}</p>}
    {catalog?.listingSupported && catalog.repositories.length === 0 && <p className="registry-help" role="status">{t("No repositories.", "저장소가 없다.")}</p>}
    {!!catalog?.repositories.length && <div className="table-scroll registry-catalog-scroll">
      <table className="vm-table registry-catalog" aria-label={t("Registry images", "레지스트리 이미지")}>
        <thead><tr><th>{t("Image repository", "이미지 저장소")}</th><th className="actions">{t("Actions", "작업")}</th></tr></thead>
        <tbody>{catalog.repositories.map(value => <tr key={value} aria-selected={repository === value}>
          <td className="registry-reference">{registry}/{value}</td>
          <td className="actions"><button className="btn" disabled={disabled} aria-pressed={repository === value} onClick={() => setRepository(value)}>{repository === value ? t("Selected", "선택됨") : t("Select", "선택")}</button></td>
        </tr>)}</tbody>
      </table>
    </div>}
    <div className="pipe-row-actions registry-browser-actions">
      {catalog?.nextPage && <button className="btn" disabled={disabled || loading} onClick={() => void moreRepositories()}>{t("More images", "이미지 더 보기")}</button>}
      {catalog?.listingSupported !== false && <button className="btn" disabled={disabled || loading} onClick={reload}>{t("Refresh repositories", "저장소 새로 고침")}</button>}
    </div>
    {repository && <ImageTags key={repository} registry={registry} repository={repository} disabled={disabled} onPull={onPull} />}
  </div>;
}
