import { useEffect, useRef, useState } from "react";
import { getPipeAuth, type AuthConnection } from "../api/pipeAuth";
import {
  getRegistryRepositories,
  getRegistryTags,
  type RegistryRepositories,
  type RegistryTags,
} from "../api/registries";
import { useI18n } from "../i18n";

/** The API requires a connected account that owns the image namespace. */
export function hasServiceImageAuth(
  image: string,
  accounts: AuthConnection[],
): boolean {
  const [registry, owner, ...repository] = image.split("/");
  if (!owner) return false;
  return accounts.some(
    (account) =>
      account.services.includes("registry") &&
      account.registry === registry &&
      (account.provider === "aws" ||
        (account.provider === "github" &&
          registry === "ghcr.io" &&
          repository.length > 0 &&
          owner.toLowerCase() === account.account.toLowerCase())),
  );
}

export default function ServiceAppPicker({
  image,
  onChange,
  onAuthReady,
  disabled,
  manualRevision = 0,
}: {
  manualRevision?: number;
  image: string;
  onChange: (image: string) => void;
  onAuthReady: (ready: boolean) => void;
  disabled: boolean;
}) {
  const { t } = useI18n();
  const [accounts, setAccounts] = useState<AuthConnection[] | null>(null);
  useEffect(
    () => onAuthReady(hasServiceImageAuth(image, accounts ?? [])),
    [image, accounts, onAuthReady],
  );
  const [accountId, setAccountId] = useState("");
  const [catalog, setCatalog] = useState<RegistryRepositories | null>(null);
  const [repository, setRepository] = useState("");
  const [tags, setTags] = useState<RegistryTags | null>(null);
  const [tag, setTag] = useState("");
  const [error, setError] = useState("");
  const [loading, setLoading] = useState(false);
  const [refresh, setRefresh] = useState(0);
  const imageRef = useRef(image);
  imageRef.current = image;
  const choose = useRef(!image);
  const manual = useRef(manualRevision);
  manual.current = manualRevision;
  useEffect(() => {
    if (manualRevision > 0) choose.current = false;
  }, [manualRevision]);
  const repoVersion = useRef(0),
    tagVersion = useRef(0);
  const changed = useRef(onChange);
  changed.current = onChange;
  const account = accounts?.find((value) => value.provider === accountId);
  const registry = account?.registry ?? "";
  useEffect(() => {
    let live = true;
    setLoading(true);
    setError("");
    getPipeAuth()
      .then((rows) => {
        if (!live) return;
        const usable = rows.filter(
          (value) =>
            (value.provider === "aws" || value.provider === "github") &&
            value.services.includes("registry") &&
            value.registry,
        );
        setAccounts(usable);
        setAccountId((current) =>
          usable.some((value) => value.provider === current)
            ? current
            : (usable.find((value) =>
                imageRef.current.startsWith(value.registry + "/"),
              )?.provider ??
              usable[0]?.provider ??
              ""),
        );
      })
      .catch((cause) => {
        if (live) {
          setError(String(cause));
          setAccounts(null);
        }
      })
      .finally(() => {
        if (live) setLoading(false);
      });
    return () => {
      live = false;
    };
  }, [refresh]);
  useEffect(() => {
    repoVersion.current++;
    setCatalog(null);
    setRepository("");
    setTags(null);
    setTag("");
    if (!registry) return;
    let live = true;
    setLoading(true);
    setError("");
    getRegistryRepositories(registry)
      .then((page) => {
        if (!live) return;
        setCatalog(page);
        const initial = imageRef.current
          .slice(registry.length + 1)
          .split("@")[0]
          .replace(/:[^/]+$/, "");
        setRepository(
          page.repositories.includes(initial)
            ? initial
            : (page.repositories[0] ?? ""),
        );
      })
      .catch((cause) => {
        if (live) setError(String(cause));
      })
      .finally(() => {
        if (live) setLoading(false);
      });
    return () => {
      live = false;
    };
  }, [registry, refresh]);
  useEffect(() => {
    tagVersion.current++;
    setTags(null);
    setTag("");
    if (!registry || !repository) return;
    let live = true;
    const revision = manual.current;
    setLoading(true);
    setError("");
    getRegistryTags(`${registry}/${repository}:latest`)
      .then((page) => {
        if (!live) return;
        setTags(page);
        const initial = imageRef.current.startsWith(
          `${registry}/${repository}:`,
        )
          ? imageRef.current.slice(`${registry}/${repository}:`.length)
          : "";
        const next = page.tags.includes(initial)
          ? initial
          : (page.tags[0] ?? "");
        setTag(next);
        if (next && choose.current && revision === manual.current)
          changed.current(`${registry}/${repository}:${next}`);
      })
      .catch((cause) => {
        if (live) setError(String(cause));
      })
      .finally(() => {
        if (live) setLoading(false);
      });
    return () => {
      live = false;
    };
  }, [registry, repository, refresh]);
  async function moreRepositories() {
    if (!catalog?.nextPage || loading) return;
    setLoading(true);
    const current = repoVersion.current;
    try {
      const next = await getRegistryRepositories(
        registry,
        catalog.nextPage,
        catalog.nextCursor,
      );
      if (current === repoVersion.current)
        setCatalog({
          ...next,
          repositories: [
            ...new Set([...catalog.repositories, ...next.repositories]),
          ],
        });
    } catch (cause) {
      setError(String(cause));
    } finally {
      setLoading(false);
    }
  }
  async function moreTags() {
    if (!tags?.nextLast || loading) return;
    setLoading(true);
    const current = tagVersion.current;
    try {
      const next = await getRegistryTags(
        `${registry}/${repository}:latest`,
        tags.nextLast,
      );
      if (current === tagVersion.current)
        setTags({ ...next, tags: [...new Set([...tags.tags, ...next.tags])] });
    } catch (cause) {
      setError(String(cause));
    } finally {
      setLoading(false);
    }
  }
  return (
    <section id="service-app-picker" className="service-full">
      <p>
        {t(
          "Choose an app from an account connected in Pipe Auth.",
          "Pipe Auth에 연결된 계정에서 앱을 선택하세요.",
        )}{" "}
        <a href="#/pipe/auth">{t("Manage AUTH", "AUTH 관리")}</a>
      </p>
      {error && <p role="alert">{error}</p>}
      {accounts?.length === 0 && (
        <p>{t("Connect an account first", "먼저 계정을 연결하세요")}</p>
      )}
      {accounts && accounts.length > 0 && (
        <div className="service-picker-fields">
          <div className="field">
            <label htmlFor="service-account">
              {t("AUTH account", "AUTH 계정")}
            </label>
            <select
              id="service-account"
              value={accountId}
              disabled={disabled}
              onChange={(event) => {
                choose.current = true;
                changed.current("");
                setAccountId(event.target.value);
              }}
            >
              {accounts.map((value) => (
                <option key={value.provider} value={value.provider}>
                  {value.provider} · {value.account} · {value.registry}
                </option>
              ))}
            </select>
          </div>
          <div className="field">
            <label htmlFor="service-repository">
              {t("Repository", "저장소")}
            </label>
            <select
              id="service-repository"
              value={repository}
              disabled={disabled || !catalog?.repositories.length}
              onChange={(event) => {
                choose.current = true;
                changed.current("");
                setRepository(event.target.value);
              }}
            >
              {catalog?.repositories.map((value) => (
                <option key={value} value={value}>
                  {value}
                </option>
              ))}
            </select>
          </div>
          <div className="field">
            <label htmlFor="service-tag">{t("Tag", "태그")}</label>
            <select
              id="service-tag"
              value={tag}
              disabled={disabled || loading || !tags?.tags.length}
              onChange={(event) => {
                setTag(event.target.value);
                choose.current = true;
                changed.current(
                  `${registry}/${repository}:${event.target.value}`,
                );
              }}
            >
              {tags?.tags.map((value) => (
                <option key={value} value={value}>
                  {value}
                </option>
              ))}
            </select>
          </div>
          {catalog?.listingSupported === false && (
            <p>
              {t(
                "Repository discovery is unavailable. Enter the complete OCI reference below.",
                "저장소 목록을 자동으로 불러올 수 없습니다. 아래에 OCI 이미지 주소를 입력하세요.",
              )}
            </p>
          )}
          {catalog?.nextPage && (
            <button
              type="button"
              className="btn"
              disabled={loading || disabled}
              onClick={() => void moreRepositories()}
            >
              {t("More repositories", "저장소 더 보기")}
            </button>
          )}
          {tags?.nextLast && (
            <button
              type="button"
              className="btn"
              disabled={loading || disabled}
              onClick={() => void moreTags()}
            >
              {t("More tags", "태그 더 보기")}
            </button>
          )}
        </div>
      )}
      <button
        type="button"
        className="btn"
        disabled={loading || disabled}
        onClick={() => setRefresh((value) => value + 1)}
      >
        {t(
          accounts === null ? "Retry accounts" : "Check connections",
          accounts === null ? "계정 다시 조회" : "연결 확인",
        )}
      </button>
    </section>
  );
}
