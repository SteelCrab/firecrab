import { useEffect, useRef, useState, type FormEvent } from "react";
import { fetchJson } from "../api/client";
import { useI18n } from "../i18n";

type Policy = {
  policy: { reference: string; intervalSeconds: number } | null;
  pause: string | null;
  observedTag: string | null;
  observedDigest: string | null;
  error: string | null;
};
type Token = {
  id: string;
  name: string;
  repository: string;
  expiresAt: number;
};
const write = (method: string, body?: unknown): RequestInit => ({
  method,
  ...(body !== undefined
    ? {
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(body),
      }
    : {}),
});
export default function ServiceAutomation({
  serviceId,
  domain,
  disabled,
}: {
  serviceId: string;
  domain: string;
  disabled: boolean;
}) {
  const { t } = useI18n();
  const base = `/api/micro-services/${encodeURIComponent(serviceId)}`,
    [autoOpen, setAutoOpen] = useState(false),
    [ciOpen, setCiOpen] = useState(false),
    [status, setStatus] = useState<Policy | null>(null),
    [reference, setReference] = useState(""),
    [interval, setInterval] = useState(300),
    [tokens, setTokens] = useState<Token[]>([]),
    [name, setName] = useState(""),
    [days, setDays] = useState(30),
    [issued, setIssued] = useState(""),
    [revealed, setRevealed] = useState(false),
    [busy, setBusy] = useState(false),
    [error, setError] = useState("");
  const mounted = useRef(false),
    ciGeneration = useRef(0);
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);
  const refreshAuto = async () => {
    const value = await fetchJson<Policy>(`${base}/auto-deploy`);
    setStatus(value);
    setReference(value.policy?.reference || "");
    setInterval(value.policy?.intervalSeconds || 300);
  };
  const refreshTokens = async () =>
    setTokens(await fetchJson<Token[]>(`${base}/deploy-tokens`));
  const run = async (action: () => Promise<void>) => {
    if (busy) return;
    setBusy(true);
    setError("");
    try {
      await action();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };
  const toggleAuto = () => {
    if (autoOpen) {
      setAutoOpen(false);
      return;
    }
    setAutoOpen(true);
    void run(refreshAuto);
  };
  const toggleCi = () => {
    if (ciOpen) {
      ciGeneration.current++;
      setCiOpen(false);
      setIssued("");
      setRevealed(false);
      return;
    }
    setCiOpen(true);
    void run(refreshTokens);
  };
  const save = (event: FormEvent) => {
    event.preventDefault();
    void run(async () => {
      const value = await fetchJson<Policy>(
        `${base}/auto-deploy`,
        write("PUT", {
          reference: reference.trim(),
          intervalSeconds: interval,
        }),
      );
      setStatus(value);
    });
  };
  const issue = (event: FormEvent) => {
    event.preventDefault();
    if (issued) return;
    const generation = ciGeneration.current;
    void run(async () => {
      const result = await fetchJson<{ token: string; credential: Token }>(
        `${base}/deploy-tokens`,
        write("POST", { name: name.trim(), expiresInSeconds: days * 86400 }),
      );
      if (mounted.current && generation === ciGeneration.current) {
        setIssued(result.token);
        setTokens((rows) => [result.credential, ...rows]);
      }
    });
  };
  return (
    <section className="panel">
      <h3>{t("Automation", "자동 배포")}</h3>
      {error && <p role="alert">{error}</p>}
      <button className="btn" disabled={busy} onClick={toggleAuto}>
        {t("Registry monitoring", "레지스트리 자동 배포")}
      </button>
      <button className="btn" disabled={busy && !ciOpen} onClick={toggleCi}>
        {t("Deployment tokens", "배포 토큰")}
      </button>
      {autoOpen && (
        <form onSubmit={save}>
          <label>
            {t("Registry source", "레지스트리 소스")}{" "}
            <input
              required
              maxLength={2048}
              disabled={busy || disabled}
              value={reference}
              onChange={(e) => setReference(e.target.value)}
              placeholder="ghcr.io/team/app:v1.*"
            />
          </label>
          <label>
            {t("Interval (seconds)", "확인 간격(초)")}{" "}
            <input
              type="number"
              min="60"
              max="3600"
              disabled={busy || disabled}
              value={interval}
              onChange={(e) => setInterval(Number(e.target.value))}
            />
          </label>
          <p>
            {t(
              "Fixed tags and stable version patterns are supported. Rollback pauses the observed digest.",
              "고정 태그와 안정 버전 규칙을 사용할 수 있습니다. 롤백한 이미지의 다이제스트는 자동 배포에서 제외됩니다.",
            )}
          </p>
          <button className="btn primary" disabled={busy || disabled}>
            {t("Save", "저장")}
          </button>
          <button
            type="button"
            className="btn"
            disabled={busy || disabled || !status?.policy}
            onClick={() =>
              void run(async () => {
                setStatus(
                  await fetchJson(`${base}/auto-deploy`, write("PUT", null)),
                );
              })
            }
          >
            {t("Disable", "해제")}
          </button>
          <button
            type="button"
            className="btn"
            disabled={busy || !status?.policy}
            onClick={() =>
              void run(async () => {
                setStatus(
                  await fetchJson(
                    `${base}/auto-deploy/${status?.pause ? "resume" : "pause"}`,
                    write("POST"),
                  ),
                );
              })
            }
          >
            {status?.pause ? t("Resume", "재개") : t("Pause", "일시 중지")}
          </button>
          <p role="status" className="wrap">
            {status?.policy
              ? status.pause || t("Monitoring", "확인 중")
              : t("Disabled", "해제")}{" "}
            · {status?.observedTag} {status?.observedDigest}
          </p>
          {status?.error && <p role="alert">{status.error}</p>}
        </form>
      )}
      {ciOpen && (
        <div>
          <p>
            {t("Endpoint", "엔드포인트")}:{" "}
            <code>{`https://${domain}/__firecrab/deploy`}</code>
          </p>
          <form onSubmit={issue}>
            <label>
              {t("Name", "이름")}{" "}
              <input
                required
                maxLength={64}
                value={name}
                onChange={(e) => setName(e.target.value)}
              />
            </label>
            <label>
              {t("Lifetime", "유효 기간")}{" "}
              <select
                value={days}
                onChange={(e) => setDays(Number(e.target.value))}
              >
                <option value="7">7 {t("days", "일")}</option>
                <option value="30">30 {t("days", "일")}</option>
                <option value="90">90 {t("days", "일")}</option>
              </select>
            </label>
            <button
              className="btn primary"
              disabled={busy || disabled || !!issued}
            >
              {t("Create token", "토큰 발급")}
            </button>
          </form>
          {issued && (
            <div>
              <p>
                {t(
                  "Copy this token now. It is removed when this panel closes.",
                  "지금 토큰을 복사하세요. 이 화면을 닫으면 토큰 값을 다시 확인할 수 없습니다.",
                )}
              </p>
              <label>
                {t("Token", "토큰")}{" "}
                <input
                  id="deploy-ci-issued"
                  readOnly
                  type={revealed ? "text" : "password"}
                  value={issued}
                />
              </label>
              <button className="btn" onClick={() => setRevealed((v) => !v)}>
                {revealed ? t("Hide", "숨김") : t("Reveal", "표시")}
              </button>
              <button
                className="btn"
                onClick={() => void navigator.clipboard.writeText(issued)}
              >
                {t("Copy", "복사")}
              </button>
              <button
                className="btn"
                onClick={() => {
                  setIssued("");
                  setRevealed(false);
                }}
              >
                {t("Dismiss", "닫기")}
              </button>
            </div>
          )}
          {tokens.map((token) => (
            <div key={token.id}>
              <strong>{token.name}</strong>
              <p>
                {token.repository} ·{" "}
                {new Date(token.expiresAt * 1000).toLocaleString()}
              </p>
              <button
                className="btn danger"
                disabled={busy}
                onClick={() => {
                  if (
                    window.confirm(
                      t("Revoke this token?", "이 토큰을 폐기할까요?"),
                    )
                  )
                    void run(async () => {
                      await fetchJson(
                        `${base}/deploy-tokens/${encodeURIComponent(token.id)}`,
                        write("DELETE"),
                      );
                      await refreshTokens();
                    });
                }}
              >
                {t("Revoke", "폐기")}
              </button>
            </div>
          ))}
        </div>
      )}
    </section>
  );
}
