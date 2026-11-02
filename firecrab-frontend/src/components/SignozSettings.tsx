import { useEffect, useRef, useState, type FormEvent } from "react";
import { Activity, Box, Check, Info, List, Unplug, X } from "lucide-react";
import { ApiClientError } from "../api/client";
import {
  testSignoz,
  type DeliveryBatch,
  type ObservationSnapshot,
  type SignozConfig,
} from "../api/observability";
import { useI18n } from "../i18n";
function failure(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

export default function SignozSettings({
  snapshot,
  busy,
  onClose,
  onDirty,
  onSave,
  onDisconnect,
}: {
  snapshot: ObservationSnapshot;
  busy: boolean;
  onClose: () => void;
  onDirty: (dirty: boolean) => void;
  onSave: (draft: SignozConfig) => Promise<void>;
  onDisconnect: () => void;
}) {
  const { t } = useI18n();
  const dialog = useRef<HTMLDialogElement>(null),
    form = useRef<HTMLFormElement>(null),
    testRevision = useRef(0),
    controller = useRef<AbortController | null>(null);
  const [endpoint, setEndpoint] = useState(snapshot.config?.endpoint ?? "");
  const [selected, setSelected] = useState(snapshot.config?.serviceIds ?? []);
  const [testing, setTesting] = useState(false),
    [result, setResult] = useState<DeliveryBatch | null>(null),
    [error, setError] = useState("");
  const [initial] = useState(() => ({
    endpoint: snapshot.config?.endpoint ?? "",
    selected: snapshot.config?.serviceIds ?? [],
  }));
  const dirty =
    endpoint !== initial.endpoint ||
    selected.slice().sort().join() !== initial.selected.slice().sort().join();
  useEffect(() => {
    dialog.current?.showModal();
    return () => {
      controller.current?.abort();
      onDirty(false);
    };
  }, [onDirty]);
  useEffect(() => onDirty(dirty), [dirty, onDirty]);
  function changeEndpoint(value: string) {
    setEndpoint(value);
    setResult(null);
    setError("");
    testRevision.current++;
    controller.current?.abort();
    setTesting(false);
  }
  function validate(): boolean {
    if (!form.current?.reportValidity()) return false;
    try {
      const url = new URL(endpoint.trim());
      if (
        !/^https?:\/\//i.test(endpoint.trim()) ||
        !/^https?:$/.test(url.protocol) ||
        url.username ||
        url.password ||
        url.search ||
        url.hash ||
        /\s/.test(endpoint.trim()) ||
        url.pathname.replace(/\/$/, "").endsWith("/v1/logs")
      )
        throw new Error();
    } catch {
      setError(
        t(
          "Enter an HTTP(S) collector base URL without credentials, query, fragment or /v1/logs.",
          "인증 정보, 쿼리, 프래그먼트, /v1/logs 경로가 없는 HTTP(S) 수집기 주소를 입력한다.",
        ),
      );
      return false;
    }
    return true;
  }
  async function test() {
    if (!validate() || testing) return;
    const generation = ++testRevision.current;
    controller.current = new AbortController();
    setTesting(true);
    setError("");
    setResult(null);
    try {
      const value = await testSignoz(
        endpoint.trim(),
        controller.current.signal,
      );
      if (generation === testRevision.current) setResult(value);
    } catch (error) {
      if (
        generation === testRevision.current &&
        !controller.current?.signal.aborted
      )
        setError(failure(error));
    } finally {
      if (generation === testRevision.current) setTesting(false);
    }
  }
  async function submit(event: FormEvent) {
    event.preventDefault();
    if (busy || !validate()) return;
    if (!selected.length) {
      setError(
        t("Select at least one service.", "서비스를 1개 이상 선택한다."),
      );
      return;
    }
    setError("");
    try {
      await onSave({
        endpoint: endpoint.trim(),
        serviceIds: selected,
        paused: snapshot.config?.paused ?? false,
      });
    } catch (error) {
      if (error instanceof ApiClientError && error.apiError?.fields) {
        setError(
          Object.entries(error.apiError.fields)
            .map(
              ([id, message]) =>
                `${snapshot.sources.find((source) => source.id === id)?.name ?? id}: ${message}`,
            )
            .join(" · "),
        );
      } else setError(failure(error));
    }
  }
  return (
    <dialog
      ref={dialog}
      className="obs-settings"
      aria-labelledby="obs-settings-title"
      onCancel={(event) => {
        event.preventDefault();
        if (!busy) onClose();
      }}
    >
      <form ref={form} onSubmit={(event) => void submit(event)}>
        <header>
          <div>
            <span className="obs-eyebrow">SIGNOZ INTEGRATION</span>
            <h2 id="obs-settings-title">
              {t("SigNoz connection settings", "SigNoz 연결 설정")}
            </h2>
          </div>
          <button
            type="button"
            className="icon-btn"
            disabled={busy}
            onClick={onClose}
            aria-label={t("Close settings", "설정 닫기")}
          >
            <X size={17} />
          </button>
        </header>
        <div className="obs-settings-body">
          <p>
            {t(
              "Enter the collector endpoint and select services to export.",
              "수집기 주소를 입력하고 전송할 서비스를 선택한다.",
            )}
          </p>
          <label className="obs-input-label" htmlFor="obs-endpoint">
            {t("OTLP collector endpoint", "OTLP 수집기 주소")} *
          </label>
          <input
            id="obs-endpoint"
            type="url"
            required
            maxLength={2048}
            value={endpoint}
            onChange={(event) => changeEndpoint(event.target.value)}
            disabled={busy}
            placeholder="http://signoz.internal:4318"
            aria-describedby="obs-endpoint-help"
            autoComplete="off"
            spellCheck={false}
          />
          <p className="obs-help" id="obs-endpoint-help">
            {t(
              "Use OTLP/HTTP (default port 4318). /v1/logs is appended automatically. URL credentials and collector authentication are not supported.",
              "OTLP/HTTP 주소를 사용한다. 기본 포트는 4318이다. /v1/logs를 자동으로 추가한다. URL 인증 정보와 수집기 인증은 지원하지 않는다.",
            )}
          </p>
          <fieldset disabled={busy}>
            <legend>
              {t("Services to export", "전송할 서비스")} *{" "}
              <span>
                {selected.length} {t("selected", "개 선택")}
              </span>
            </legend>
            {snapshot.sources.map((source) => (
              <label className="obs-source-option" key={source.id}>
                <input
                  type="checkbox"
                  checked={selected.includes(source.id)}
                  disabled={!source.available && !source.selected}
                  onChange={(event) => {
                    setSelected(
                      event.target.checked
                        ? [...selected, source.id]
                        : selected.filter((id) => id !== source.id),
                    );
                    setError("");
                  }}
                />
                <Box size={16} />
                <span>
                  <strong>{source.name}</strong>
                  <small>
                    {source.available
                      ? t("Logs & service events", "로그·서비스 이벤트")
                      : t(
                          "Managed remotely or has another exporter",
                          "원격 관리 중이거나 다른 전송기가 설정됨",
                        )}
                  </small>
                </span>
              </label>
            ))}
            {!snapshot.sources.length && (
              <p>
                {t(
                  "Create a local MicroService before connecting.",
                  "연결 전에 로컬 MicroService를 생성한다.",
                )}
              </p>
            )}
          </fieldset>
          <div className="obs-signal">
            <List size={18} />
            <div>
              <strong>
                {t("Logs & service events", "로그·서비스 이벤트")}
              </strong>
              <p>
                {t(
                  "Application output, lifecycle events and alert records.",
                  "앱 출력, 배포 이벤트, 알림 기록을 전송한다.",
                )}
              </p>
            </div>
            <Check size={16} />
          </div>
          {result && (
            <p
              className={`obs-notice ${result.status !== "delivered" ? "error" : ""}`}
              role="status"
            >
              {result.status === "delivered"
                ? t(
                    "Collector accepted the empty OTLP test batch.",
                    "수집기가 빈 OTLP 테스트 배치를 수락했다.",
                  )
                : t(
                    "Collector test failed or returned a warning.",
                    "수집기 테스트가 실패하거나 경고를 반환했다.",
                  )}{" "}
              {result.httpStatus
                ? `HTTP ${result.httpStatus}`
                : t("No HTTP response.", "HTTP 응답 없음.")}
            </p>
          )}
          {error && (
            <p className="obs-notice error" role="alert">
              {error}
            </p>
          )}
          <p className="obs-help">
            <Info size={14} />
            {t(
              "Connection testing sends an empty batch. A test log sends one explicit record from a selected service. Saving enables export of new records.",
              "연결 테스트는 빈 배치를 전송한다. 테스트 로그는 선택한 서비스에서 기록 1건을 전송한다. 저장하면 새 기록 전송을 활성화한다.",
            )}
          </p>
        </div>
        <footer>
          {snapshot.config && (
            <button
              type="button"
              className="obs-text-button obs-danger"
              disabled={busy}
              onClick={onDisconnect}
            >
              <Unplug size={14} />
              {t("Disconnect", "연결 해제")}
            </button>
          )}
          <div className="obs-actions">
            <button
              type="button"
              className="btn"
              disabled={busy || testing}
              onClick={() => void test()}
            >
              <Activity size={14} />
              {testing
                ? t("Testing…", "확인 중…")
                : t("Test connection", "연결 테스트")}
            </button>
            <button
              type="submit"
              className="btn primary"
              disabled={busy || testing || !snapshot.sources.length}
            >
              {busy
                ? t("Saving…", "저장 중…")
                : t("Save connection", "연결 저장")}
            </button>
          </div>
        </footer>
      </form>
    </dialog>
  );
}
