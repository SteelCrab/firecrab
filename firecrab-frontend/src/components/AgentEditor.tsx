import { useCallback, useEffect, useState } from "react";
import { ArrowLeft, CodeXml, RefreshCw } from "lucide-react";
import { agentRequest } from "../api/agents";
import { useHostStatus } from "../hostStatusContext";
import { useI18n } from "../i18n";
import Screen from "./layout/Screen";
import "../styles/agent-editor.css";

type Status = {
  vmId: string;
  agentId: string;
  projectPath: string;
  state: string;
  phase?: string;
  error?: string | null;
  url?: string;
};
export default function AgentEditor({
  vmId,
  agentId,
  controllerId,
}: {
  vmId: string;
  agentId: string;
  controllerId: string;
}) {
  const { t } = useI18n();
  const host = useHostStatus();
  const [status, setStatus] = useState<Status | null>(null);
  const [error, setError] = useState("");
  const [retry, setRetry] = useState(0);
  const endpoint = `/${encodeURIComponent(agentId)}/editor`;
  const hostMatches =
    !!controllerId && host?.platform?.controllerId === controllerId;
  const go = useCallback(
    (value: Status) => {
      if (
        value.vmId !== vmId ||
        value.agentId !== agentId ||
        ![
          `/workspace/firecrab/${agentId}`,
          "/workspace/firecrab/project",
        ].includes(value.projectPath)
      )
        throw Error(
          t(
            "The editor workspace identity changed.",
            "편집기의 작업 공간이 변경되었습니다.",
          ),
        );
      if (value.state !== "ready" || !value.url) return false;
      const url = new URL(value.url);
      if (
        url.protocol !== "http:" ||
        url.hostname !== "127.0.0.1" ||
        !url.port ||
        url.pathname !== `/${agentId}/` ||
        url.searchParams.get("folder") !== value.projectPath ||
        !/^[a-f0-9]{64}$/i.test(url.searchParams.get("tkn") || "") ||
        url.hash ||
        url.username ||
        url.password
      )
        throw Error(
          t(
            "The private editor address could not be verified.",
            "편집기의 로컬 접속 주소를 확인할 수 없습니다.",
          ),
        );
      window.location.replace(url.toString());
      return true;
    },
    [agentId, vmId, t],
  );
  useEffect(() => {
    if (!hostMatches) return;
    let cancelled = false,
      timer = 0;
    const start = async () => {
      try {
        const next = await agentRequest<Status>(
          endpoint,
          {},
          controllerId,
          vmId,
        );
        if (cancelled) return;
        setStatus(next);
        setError("");
        if (go(next)) return;
        timer = window.setTimeout(() => void poll(), 2000);
      } catch (e) {
        if (!cancelled) setError((e as Error).message);
      }
    };
    const poll = async () => {
      try {
        const next = await agentRequest<Status>(
          endpoint,
          undefined,
          undefined,
          vmId,
        );
        if (cancelled) return;
        setStatus(next);
        if (next.vmId !== vmId || next.agentId !== agentId)
          throw Error(
            t(
              "The editor workspace identity changed.",
              "편집기의 작업 공간이 변경되었습니다.",
            ),
          );
        if (next.state === "failed" || next.state === "interrupted") {
          setError(
            next.error ||
              t(
                "Editor installation was interrupted. Retry after checking the VM.",
                "편집기 설치가 중단되었습니다. VM 상태를 확인한 뒤 다시 시도하세요.",
              ),
          );
          return;
        }
        if (next.state === "installed" || next.state === "running") {
          await start();
          return;
        }
        timer = window.setTimeout(() => void poll(), 2000);
      } catch (e) {
        if (!cancelled) setError((e as Error).message);
      }
    };
    timer = window.setTimeout(() => void start(), 0);
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, [hostMatches, controllerId, vmId, agentId, endpoint, retry, go, t]);
  const phase = (
    {
      preparing: t("Preparing", "준비"),
      dependencies: t("Runtime dependencies", "실행 도구 준비"),
      download: t("Downloading editor", "편집기 다운로드"),
      verify: t("Verifying checksum", "체크섬 확인"),
      extract: t("Installing editor", "편집기 설치"),
      ready: t("Opening workspace", "작업 폴더 열기"),
    } as Record<string, string>
  )[status?.phase || "preparing"];
  return (
    <Screen view="ai-sandbox" heading="Web VS Code">
      <div className="ae-page">
        <CodeXml className="ae-icon" />
        <h2>{t("Open this agent’s files", "에이전트 작업 파일 열기")}</h2>
        <p>
          {t(
            "Web VS Code runs inside the same MicroVM and edits the same files as the agent.",
            "같은 MicroVM 안에서 Web VS Code를 실행합니다. 파일을 저장하면 에이전트의 작업 파일에 바로 반영됩니다.",
          )}
        </p>
        {status?.projectPath && <code>{status.projectPath}</code>}
        {error ? (
          <>
            <p className="ae-error" role="alert">
              {error}
            </p>
            <button
              className="btn primary"
              onClick={() => {
                setError("");
                setRetry((v) => v + 1);
              }}
            >
              <RefreshCw />
              {t("Retry editor connection", "편집기 연결 다시 시도")}
            </button>
          </>
        ) : host?.platform?.controllerId && !hostMatches ? (
          <p className="ae-error" role="alert">
            {t(
              "This link belongs to another host. Open the editor from the matching AI Sandbox workspace.",
              "다른 호스트에 연결된 편집기입니다. 해당 호스트의 AI Sandbox 작업에서 편집기를 여세요.",
            )}
          </p>
        ) : (
          <div role="status" className="ae-progress">
            <progress />
            {hostMatches
              ? phase
              : t("Checking host connection…", "호스트 연결 확인 중…")}
          </div>
        )}
        <a className="btn" href="#/ai-sandbox">
          <ArrowLeft />
          {t("Back to AI Sandbox", "AI Sandbox로 이동")}
        </a>
      </div>
    </Screen>
  );
}
