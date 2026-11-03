import { useCallback, useEffect, useState } from "react";
import { LockKeyhole, Trash2 } from "lucide-react";
import { useI18n } from "../i18n";
import { base } from "../canvas/types";
import { request, message, WorkspaceError } from "../canvas/request";
import {
  cleanupKey,
  readCleanupIntent,
  type BackupOperation,
  type BackupRetention,
  type CleanupIntent,
  type CleanupOperation,
  type CleanupPreview,
} from "../canvas/backups";

type Props = {
  project: string;
  generation?: number;
  rows: BackupOperation[];
  cleanups: CleanupOperation[];
  busy: boolean;
  disabled: boolean;
  run: (work: () => Promise<void>) => Promise<void>;
  reload: () => Promise<void>;
};
export default function ProjectBackupRetention({
  project,
  generation,
  rows,
  cleanups,
  busy,
  disabled,
  run,
  reload,
}: Props) {
  const { t } = useI18n();
  const path = base(project) + "/backups";
  const [policy, setPolicy] = useState<BackupRetention>();
  const [preview, setPreview] = useState<CleanupPreview>();
  const [selected, setSelected] = useState<string[]>([]);
  const [confirm, setConfirm] = useState(false);
  const [retainedIntent, setPending] = useState<CleanupIntent>();
  const pending =
    retainedIntent &&
    !cleanups.some(
      (r) =>
        r.id === retainedIntent.id &&
        r.phase === "complete" &&
        !r.pendingReconciliation,
    )
      ? retainedIntent
      : undefined;
  const [error, setError] = useState("");
  const fetchData = useCallback(
    () =>
      Promise.all([
        request<BackupRetention>("GET", path + "/retention"),
        request<CleanupPreview>("GET", path + "/retention/preview"),
      ]),
    [path],
  );
  const applyData = useCallback(
    ([p, plan]: [BackupRetention, CleanupPreview]) => {
      setPolicy(p);
      setPreview(plan);
      setSelected((ids) =>
        ids.filter((id) =>
          plan.candidates.some((c) => c.id === id && c.eligible),
        ),
      );
      setPending(readCleanupIntent(localStorage, project));
      setError("");
    },
    [project],
  );
  useEffect(() => {
    let current = true;
    void fetchData()
      .then((data) => {
        if (current) applyData(data);
      })
      .catch((e) => {
        if (current) setError(message(e));
      });
    return () => {
      current = false;
    };
  }, [fetchData, applyData, rows, cleanups]);
  useEffect(() => {
    if (
      retainedIntent &&
      !pending &&
      readCleanupIntent(localStorage, project)?.id === retainedIntent.id
    )
      localStorage.removeItem(cleanupKey(project));
  }, [retainedIntent, pending, project]);
  async function load() {
    applyData(await fetchData());
  }
  async function refresh() {
    await reload();
    await load();
  }
  async function send(intent: CleanupIntent) {
    await request(
      "PUT",
      `${path}/cleanups/${intent.id}`,
      {
        expectedGeneration: intent.expectedGeneration,
        targetIds: intent.targetIds,
      },
      false,
    );
    await refresh();
  }
  async function remove() {
    if (!generation || !selected.length) return;
    const intent = {
      id: crypto.randomUUID(),
      expectedGeneration: generation,
      targetIds: [...selected].sort(),
    };
    localStorage.setItem(cleanupKey(project), JSON.stringify(intent));
    setPending(intent);
    setConfirm(false);
    try {
      await send(intent);
      setSelected([]);
    } catch (e) {
      if (
        e instanceof WorkspaceError &&
        e.status &&
        e.status >= 400 &&
        e.status < 500
      ) {
        try {
          await request(
            "GET",
            `${path}/cleanups/${intent.id}`,
            undefined,
            false,
          );
        } catch (proof) {
          if (
            proof instanceof WorkspaceError &&
            proof.status === 404 &&
            readCleanupIntent(localStorage, project)?.id === intent.id
          ) {
            localStorage.removeItem(cleanupKey(project));
            setPending(undefined);
          }
        }
      }
      throw e;
    } finally {
      await refresh();
    }
  }
  const unavailable = busy || disabled || !generation || !!pending || !!error;
  return (
    <section aria-label={t("Backup retention and cleanup", "백업 보관·정리")}>
      <h3>{t("Retention & cleanup", "보관·정리·삭제")}</h3>
      <p>
        {t(
          "Keep a verified checkpoint. Protected or active backups cannot be deleted. Cleanup is permanent and requires a stopped, idle VM. Receipts and integrity metadata remain.",
          "정상 백업 하나를 남긴다. 보호 중이거나 사용 중인 백업은 삭제할 수 없다. VM이 정지하고 작업이 끝나야 정리할 수 있다. 파일 삭제는 취소할 수 없다. 작업 기록과 검사 정보는 유지한다.",
        )}
      </p>
      {error && <p role="alert">{error}</p>}
      {policy && (
        <>
          <label>
            <input
              type="checkbox"
              checked={policy.enabled}
              disabled={busy}
              onChange={(e) =>
                setPolicy({ ...policy, enabled: e.target.checked })
              }
            />
            {t("Enable automatic cleanup", "자동 정리 사용")}
          </label>
          <label>
            {t("Keep recent checkpoints", "최근 백업 보존 개수")}
            <input
              type="number"
              min={1}
              max={63}
              value={policy.keepLatest}
              disabled={busy}
              onChange={(e) =>
                setPolicy({ ...policy, keepLatest: Number(e.target.value) })
              }
            />
          </label>
          <label>
            {t("Minimum age (days)", "정리 대상 최소 나이(일)")}
            <input
              type="number"
              min={1}
              max={3650}
              placeholder={t("Count only", "개수 기준만 사용")}
              value={policy.maxAgeDays ?? ""}
              disabled={busy}
              onChange={(e) =>
                setPolicy({
                  ...policy,
                  maxAgeDays:
                    e.target.value === "" ? null : Number(e.target.value),
                })
              }
            />
          </label>
          <label>
            <input
              type="checkbox"
              checked={policy.includeManual}
              disabled={busy}
              onChange={(e) =>
                setPolicy({ ...policy, includeManual: e.target.checked })
              }
            />
            {t(
              "Include manual backups in automatic cleanup",
              "수동 백업도 자동 정리에 포함",
            )}
          </label>
          <p>
            {t(
              "Recent and protected backups stay. An age limit also keeps younger backups. Disabling cleanup prevents new admissions; an admitted cleanup still finishes. Preserved restore originals and aborted partial copies require explicit selection.",
              "최근 백업과 보호한 백업은 유지한다. 기간을 설정하면 그보다 새 백업도 유지한다. 자동 정리를 꺼도 이미 시작한 정리는 마무리한다. 복구 전 원본과 취소된 임시 파일은 직접 선택해야 정리한다.",
            )}
          </p>
          <button
            type="button"
            disabled={
              busy ||
              !generation ||
              !Number.isInteger(policy.keepLatest) ||
              policy.keepLatest < 1 ||
              policy.keepLatest > 63 ||
              (policy.maxAgeDays !== null &&
                (!Number.isInteger(policy.maxAgeDays) ||
                  policy.maxAgeDays < 1 ||
                  policy.maxAgeDays > 3650))
            }
            onClick={() =>
              void run(async () => {
                await request(
                  "PUT",
                  path + "/retention",
                  {
                    expectedGeneration: generation,
                    enabled: policy.enabled,
                    keepLatest: policy.keepLatest,
                    maxAgeDays: policy.maxAgeDays,
                    includeManual: policy.includeManual,
                  },
                  false,
                );
                await refresh();
              })
            }
          >
            {t("Save retention policy", "보관 정책 저장")}
          </button>
          {policy.error && <p role="status">{policy.error}</p>}
        </>
      )}
      <div className="cw-backup-actions">
        <button type="button" disabled={busy} onClick={() => void run(refresh)}>
          {t("Refresh cleanup preview", "정리 미리보기 갱신")}
        </button>
        <button
          type="button"
          disabled={unavailable || !preview?.automaticTargetIds.length}
          onClick={() => {
            setSelected(preview?.automaticTargetIds ?? []);
            setConfirm(false);
          }}
        >
          {t("Select policy candidates", "정책 대상 선택")}
        </button>
      </div>
      <ul className="cw-backup-records">
        {preview?.candidates.map((candidate) => (
          <li key={candidate.id}>
            <label>
              <input
                type="checkbox"
                aria-label={
                  t("Select cleanup", "정리 대상 선택") + " " + candidate.id
                }
                checked={selected.includes(candidate.id)}
                disabled={unavailable || !candidate.eligible}
                onChange={(e) => {
                  setConfirm(false);
                  setSelected(
                    e.target.checked
                      ? [...selected, candidate.id]
                      : selected.filter((id) => id !== candidate.id),
                  );
                }}
              />
              {candidate.kind === "checkpoint"
                ? t("Checkpoint", "백업")
                : candidate.kind === "partial"
                  ? t("Aborted partial files", "취소된 임시 파일")
                  : t("Preserved restore originals", "복구 전 보존 원본")}
            </label>
            <code>{candidate.id}</code>
            {candidate.reason && <small>{candidate.reason}</small>}
            {candidate.kind === "checkpoint" &&
              rows.some(
                (r) =>
                  r.id === candidate.id &&
                  r.phase === "complete" &&
                  !r.cleanupId,
              ) && (
                <button
                  type="button"
                  disabled={unavailable}
                  onClick={() =>
                    void run(async () => {
                      await request(
                        "PUT",
                        `${path}/operations/${candidate.id}/protection`,
                        {
                          expectedGeneration: generation,
                          protected: !candidate.protected,
                        },
                        false,
                      );
                      await refresh();
                    })
                  }
                >
                  <LockKeyhole size={13} />
                  {candidate.protected
                    ? t("Unprotect checkpoint", "백업 보호 해제")
                    : t("Protect checkpoint", "백업 보호")}
                </button>
              )}
          </li>
        ))}
      </ul>
      <button
        type="button"
        disabled={unavailable || !selected.length || selected.length > 64}
        onClick={() => setConfirm(true)}
      >
        <Trash2 size={14} />
        {t("Delete selected artifacts", "선택한 파일 정리")}
      </button>
      {confirm && (
        <div
          className="cw-backup-confirm"
          role="region"
          aria-label={t("Confirm permanent cleanup", "영구 삭제 확인")}
        >
          <h4>
            {t(
              "Permanently delete these artifacts?",
              "선택한 파일을 영구 삭제하는가?",
            )}
          </h4>
          <p>
            {t(
              "Checkpoint files, aborted temporary copies or preserved pre-restore originals for the selected operations will be removed. Current project files remain. The server rechecks protection, in-use references and a verified surviving checkpoint before deleting.",
              "선택한 작업의 백업 파일, 취소된 임시 파일 또는 복구 전 원본을 지운다. 현재 프로젝트 파일은 유지한다. 서버에서 보호 상태, 사용 중인 작업, 남겨 둘 정상 백업을 다시 확인한다.",
            )}
          </p>
          {selected.map((id) => (
            <code key={id}>{id}</code>
          ))}
          <button
            type="button"
            disabled={unavailable}
            onClick={() => void run(remove)}
          >
            {t("Permanently delete selected files", "선택한 파일 영구 삭제")}
          </button>
          <button
            type="button"
            disabled={busy}
            onClick={() => setConfirm(false)}
          >
            {t("Cancel cleanup", "정리 취소")}
          </button>
        </div>
      )}
      {pending && (
        <div className="cw-backup-confirm">
          <h4>{t("Retained cleanup request", "보존된 정리 요청")}</h4>
          <code>{pending.id}</code>
          <p>
            {t(
              "The outcome is uncertain. Retry this exact identity and target list; do not create another deletion request.",
              "결과를 확인하지 못했다. 같은 작업 ID와 대상 목록으로 재시도한다.",
            )}
          </p>
          <button
            type="button"
            disabled={busy}
            onClick={() =>
              void run(async () => {
                try {
                  await send(pending);
                } finally {
                  await refresh();
                }
              })
            }
          >
            {t("Retry same cleanup request", "같은 정리 요청 재시도")}
          </button>
        </div>
      )}
      <h4>{t("Cleanup history", "정리 작업 기록")}</h4>
      <ol className="cw-backup-records">
        {cleanups.slice(0, 100).map((r) => (
          <li key={r.id}>
            <strong>
              {r.pendingReconciliation
                ? t("Cleanup reconciliation pending", "정리 상태 확인 필요")
                : t("Cleanup complete", "정리 완료")}
            </strong>
            <code>{r.id}</code>
            <small>
              {r.removedFiles} {t("files", "개 파일")} ·{" "}
              {(r.logicalBytes / 1024 ** 3).toFixed(2)} GiB{" "}
              {t("logical bytes", "논리 크기")}
            </small>
            {r.error && <p role="status">{r.error}</p>}
            {r.pendingReconciliation && (
              <button
                type="button"
                disabled={busy}
                onClick={() =>
                  void run(async () => {
                    try {
                      await request(
                        "POST",
                        `${path}/cleanups/${r.id}/retry`,
                        {},
                        false,
                      );
                    } finally {
                      await refresh();
                    }
                  })
                }
              >
                {t("Resume cleanup", "정리 재개")}
              </button>
            )}
          </li>
        ))}
      </ol>
    </section>
  );
}
