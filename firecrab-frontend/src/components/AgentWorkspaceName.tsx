import { useEffect, useState } from "react";
import { Pencil } from "lucide-react";
import { ApiClientError } from "../api/client";
import {
  listWorkspaces,
  renameWorkspace,
  validWorkspaceName,
  type WorkspaceSetup,
} from "../api/workspaces";
import type { VmResponse } from "../bindings";
import { useI18n } from "../i18n";

/**
 * The workspace title with an inline Rename action. Only a VM prepared by
 * AI Sandbox has a workspace record; other VMs show the name without the action.
 */
export default function AgentWorkspaceName({
  vm,
  onRenamed,
}: {
  vm: VmResponse;
  onRenamed: (name: string) => void;
}) {
  const { t } = useI18n();
  const [setup, setSetup] = useState<WorkspaceSetup | null>(null);
  const [draft, setDraft] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState("");
  useEffect(() => {
    let cancelled = false;
    listWorkspaces()
      .then((setups) => {
        if (!cancelled) setSetup(setups.find((s) => s.vmId === vm.id) ?? null);
      })
      .catch(() => {
        if (!cancelled) setSetup(null);
      });
    return () => {
      cancelled = true;
    };
  }, [vm.id]);
  const busy = setup?.state === "queued" || setup?.state === "running";
  const name = draft?.trim() ?? "";
  const invalid =
    draft !== null && !validWorkspaceName(name)
      ? t(
          "Use 1–64 letters, numbers, '.', '_' or '-', starting with a letter or number.",
          "영문자·숫자·'.'·'_'·'-'로 1~64자를 입력해 주세요. 첫 글자는 영문자나 숫자여야 해요.",
        )
      : "";
  const cancel = () => {
    setDraft(null);
    setError("");
  };
  const save = async () => {
    if (!setup || saving || invalid) return;
    if (name === vm.name) return cancel();
    setSaving(true);
    setError("");
    try {
      const renamed = await renameWorkspace(setup.id, name);
      setSetup(renamed);
      setDraft(null);
      onRenamed(renamed.name);
    } catch (e) {
      setError(renameError(e, t));
    } finally {
      setSaving(false);
    }
  };
  if (draft === null)
    return (
      <div className="awc-name">
        <h2>{vm.name}</h2>
        {setup && (
          <button
            type="button"
            className="btn"
            disabled={busy}
            title={
              busy
                ? t(
                    "Wait for the workspace to finish preparing.",
                    "작업 공간 준비가 끝난 뒤 바꿀 수 있어요.",
                  )
                : undefined
            }
            onClick={() => {
              setError("");
              setDraft(vm.name);
            }}
          >
            <Pencil aria-hidden="true" />
            {t("Rename", "이름 변경")}
          </button>
        )}
      </div>
    );
  return (
    <div className="awc-rename">
      <div className="awc-name">
        <input
          aria-label={t("Workspace name", "작업 공간 이름")}
          aria-invalid={!!invalid}
          value={draft}
          maxLength={64}
          autoFocus
          disabled={saving}
          onFocus={(e) => e.currentTarget.select()}
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Escape") {
              e.preventDefault();
              e.stopPropagation();
              cancel();
            }
            if (e.key === "Enter" && !e.nativeEvent.isComposing) {
              e.preventDefault();
              void save();
            }
          }}
        />
        <button
          type="button"
          className="btn primary"
          disabled={saving || !!invalid}
          onClick={() => void save()}
        >
          {saving ? t("Saving…", "저장 중…") : t("Save", "저장")}
        </button>
        <button
          type="button"
          className="btn"
          disabled={saving}
          onClick={cancel}
        >
          {t("Cancel", "취소")}
        </button>
      </div>
      {invalid ? (
        <p className="awc-error" role="alert">
          {invalid}
        </p>
      ) : (
        <p>
          {t(
            "The VM gets the same name. You can rename it while it runs; its hostname, files and AI sessions stay the same.",
            "VM 이름도 같이 바뀌어요. 실행 중에도 바꿀 수 있고, VM 안의 호스트 이름과 파일, AI 세션은 그대로예요.",
          )}
        </p>
      )}
      {error && (
        <p className="awc-error" role="alert">
          {error}
        </p>
      )}
    </div>
  );
}

function renameError(
  error: unknown,
  t: (en: string, ko: string) => string,
): string {
  if (!(error instanceof ApiClientError))
    return error instanceof Error ? error.message : String(error);
  switch (error.apiError?.code) {
    case "invalid_state":
      return t(
        "The VM is starting, stopping or paused. Try again when it is running or stopped.",
        "VM이 시작·중지·일시 정지 중이에요. 실행 중이거나 중지된 뒤 다시 시도해 주세요.",
      );
    case "workspace_busy":
      return t(
        "The workspace is still preparing. Try again when it is ready.",
        "작업 공간을 아직 준비하고 있어요. 준비가 끝난 뒤 다시 시도해 주세요.",
      );
    case "vm_managed":
      return t(
        "Another feature manages this VM, so it cannot be renamed here.",
        "다른 기능이 관리하는 VM이라 여기서 이름을 바꿀 수 없어요.",
      );
    default:
      return error.fieldError("name") ?? error.message;
  }
}
