import { useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { DatabaseBackup, RefreshCw, X } from "lucide-react";
import ProjectBackupRetention from "./ProjectBackupRetention";
import { useI18n } from "../i18n";
import { base } from "../canvas/types";
import { request, message, WorkspaceError } from "../canvas/request";
import {
  backupKey,
  readBackupIntent,
  type BackupPolicy,
  type BackupOperation,
  type BackupIntent,
  type CleanupOperation,
} from "../canvas/backups";

export default function ProjectBackups({ project }: { project: string }) {
  const { t } = useI18n();
  const dialog = useRef<HTMLDialogElement>(null);
  const [open, setOpen] = useState(false),
    [busy, setBusy] = useState(false),
    [error, setError] = useState("");
  const [policy, setPolicy] = useState<BackupPolicy>(),
    [rows, setRows] = useState<BackupOperation[]>([]);
  const [generation, setGeneration] = useState<number>(),
    [pending, setPending] = useState<BackupIntent>();
  const [restore, setRestore] = useState("");
  const [cleanups, setCleanups] = useState<CleanupOperation[]>();
  const path = base(project) + "/backups";
  const maintenance =
    !cleanups ||
    rows.some((r) => r.pendingReconciliation) ||
    cleanups.some((r) => r.pendingReconciliation);
  useEffect(() => {
    if (open) dialog.current?.showModal();
    else dialog.current?.close();
  }, [open]);
  async function load() {
    const results = await Promise.allSettled([
      request<BackupPolicy>("GET", path + "/policy"),
      request<BackupOperation[]>("GET", path),
      request<CleanupOperation[]>("GET", path + "/cleanups"),
      request<{ generation: number }>(
        "GET",
        path + "/binding",
        undefined,
        false,
      ),
    ]);
    if (results[0].status === "fulfilled") setPolicy(results[0].value);
    if (results[1].status === "fulfilled") {
      setRows(results[1].value);
      const retained = readBackupIntent(localStorage, project);
      const receipt = results[1].value.find((r) => r.id === retained?.id);
      if (
        receipt &&
        !receipt.pendingReconciliation &&
        ["complete", "aborted"].includes(receipt.phase)
      ) {
        localStorage.removeItem(backupKey(project));
        setPending(undefined);
      } else setPending(retained);
    }
    // Backup controls stay usable when the damaged/fenced project DB cannot be opened.
    if (results[2].status === "fulfilled") setCleanups(results[2].value);
    if (results[3].status === "fulfilled")
      setGeneration(results[3].value.generation);
    const failed = results.slice(0, 3).filter((r) => r.status === "rejected");
    if (failed.length)
      throw new Error(
        failed
          .map((r) => message((r as PromiseRejectedResult).reason))
          .join("; "),
      );
  }
  async function action(work: () => Promise<void>) {
    setBusy(true);
    setError("");
    try {
      await work();
    } catch (e) {
      setError(message(e));
    } finally {
      setBusy(false);
    }
  }
  async function send(intent: BackupIntent) {
    await request<BackupOperation>(
      "PUT",
      `${path}/operations/${intent.id}`,
      {
        expectedGeneration: intent.expectedGeneration,
        restoreCheckpoint: intent.restoreCheckpoint,
      },
      false,
    );
    await load();
  }
  async function create(checkpoint: string | null) {
    if (!generation)
      throw new Error(
        t(
          "Reload project metadata before admitting a new operation.",
          "새 작업을 시작하기 전에 프로젝트 정보를 다시 읽는다.",
        ),
      );
    const intent: BackupIntent = {
      id: crypto.randomUUID(),
      expectedGeneration: generation,
      restoreCheckpoint: checkpoint,
    };
    // Retain the exact operation before sending. An uncertain response must reuse this ID.
    localStorage.setItem(backupKey(project), JSON.stringify(intent));
    setPending(intent);
    setRestore("");
    try {
      await send(intent);
    } catch (error) {
      // Only this fresh request's definitive rejection and an absent receipt release its ID.
      // Earlier uncertain requests retain their identity even when a later retry is rejected.
      if (
        error instanceof WorkspaceError &&
        error.status &&
        error.status >= 400 &&
        error.status < 500
      ) {
        try {
          await request(
            "GET",
            `${path}/operations/${intent.id}`,
            undefined,
            false,
          );
        } catch (proof) {
          if (
            proof instanceof WorkspaceError &&
            proof.status === 404 &&
            readBackupIntent(localStorage, project)?.id === intent.id
          ) {
            localStorage.removeItem(backupKey(project));
            setPending(undefined);
          }
        }
      }
      throw error;
    } finally {
      await load();
    }
  }
  return (
    <>
      <button
        type="button"
        className="cw-icon-label"
        onClick={() => {
          setOpen(true);
          void action(load);
        }}
      >
        <DatabaseBackup size={14} aria-hidden="true" />
        <span>{t("Backups & recovery", "백업·복구")}</span>
      </button>
      {createPortal(
        <dialog
          ref={dialog}
          className="cw-backup-dialog"
          onClose={() => setOpen(false)}
        >
          <header>
            <div>
              <small>PROJECT STORAGE</small>
              <h2>{t("Backups & recovery", "프로젝트 백업·복구")}</h2>
            </div>
            <button
              type="button"
              onClick={() => setOpen(false)}
              aria-label={t("Close", "닫기")}
            >
              <X size={18} />
            </button>
          </header>
          <p>
            {t(
              "A checkpoint pairs the project database with its work volume. Automatic backups wait for a stopped, idle VM. They never stop your workload.",
              "프로젝트 DB와 작업 볼륨을 같은 시점으로 보존한다. 자동 백업은 VM이 정지하고 작업이 끝날 때까지 기다린다.",
            )}
          </p>
          <p className="cw-backup-limit">
            {t(
              "Backups are kept outside the project folder on this host. A lost host needs a separate host backup. Up to 64 retained checkpoints are allowed. Configure explicit retention cleanup below; protected checkpoints stay.",
              "백업은 이 호스트의 프로젝트 폴더 밖에 보관한다. 호스트 손실에는 별도 호스트 백업이 필요하다. 최대 64개를 보존한다. 아래에서 정리 정책을 직접 설정한다. 보호한 백업은 유지한다.",
            )}
          </p>
          <div className="cw-backup-actions">
            <button
              type="button"
              disabled={busy}
              onClick={() => void action(load)}
            >
              <RefreshCw size={14} />
              {t("Refresh", "새로고침")}
            </button>
            <button
              type="button"
              disabled={busy || !!pending || !generation || maintenance}
              onClick={() => void action(() => create(null))}
            >
              {t("Back up now", "지금 백업")}
            </button>
          </div>
          {error && (
            <p role="alert" className="cw-backup-error">
              {error}
            </p>
          )}
          {busy && (
            <p role="status">
              {t(
                "Confirming storage operation. Closing this dialog does not cancel it.",
                "저장소 작업을 확인한다. 이 창을 닫아도 작업은 계속된다.",
              )}
            </p>
          )}
          {policy && (
            <section>
              <h3>{t("Automatic backups", "자동 백업")}</h3>
              <label>
                <input
                  type="checkbox"
                  checked={policy.enabled}
                  disabled={busy}
                  onChange={(e) =>
                    setPolicy({ ...policy, enabled: e.target.checked })
                  }
                />
                {t("Enable scheduled checkpoints", "정기 백업 사용")}
              </label>
              <label>
                {t("Interval", "백업 주기")}
                <select
                  value={policy.intervalSeconds}
                  disabled={busy}
                  onChange={(e) =>
                    setPolicy({
                      ...policy,
                      intervalSeconds: Number(e.target.value),
                    })
                  }
                >
                  <option value={300}>{t("5 minutes", "5분")}</option>
                  <option value={3600}>{t("1 hour", "1시간")}</option>
                  <option value={86400}>{t("24 hours", "24시간")}</option>
                </select>
              </label>
              <button
                type="button"
                disabled={busy || !generation}
                onClick={() =>
                  void action(async () => {
                    setPolicy(
                      await request<BackupPolicy>(
                        "PUT",
                        path + "/policy",
                        {
                          expectedGeneration: generation,
                          enabled: policy.enabled,
                          intervalSeconds: policy.intervalSeconds,
                        },
                        false,
                      ),
                    );
                  })
                }
              >
                {t("Save schedule", "일정 저장")}
              </button>
              {policy.error && <p role="status">{policy.error}</p>}
            </section>
          )}
          {open && (
            <ProjectBackupRetention
              project={project}
              generation={generation}
              rows={rows}
              cleanups={cleanups ?? []}
              busy={busy}
              disabled={!!pending || maintenance}
              run={action}
              reload={load}
            />
          )}
          {pending && (
            <section>
              <h3>{t("Retained request", "보존된 요청")}</h3>
              <code>{pending.id}</code>
              <p>
                {t(
                  "Its result is uncertain. Retry this exact request before starting another operation.",
                  "결과를 확인하지 못한 요청이다. 새 작업을 시작하기 전에 같은 요청으로 다시 확인한다.",
                )}
              </p>
              <button
                type="button"
                disabled={busy}
                onClick={() =>
                  void action(async () => {
                    try {
                      await send(pending);
                    } finally {
                      await load();
                    }
                  })
                }
              >
                {t("Retry same request", "같은 요청 재시도")}
              </button>
            </section>
          )}
          {rows.some((r) => r.restoredGeneration) && (
            <p className="cw-backup-limit">
              {t(
                "Restoration completed. Reload other open pages and reopen Web VS Code before saving. Preserve unsaved text first.",
                "복구가 완료됐다. 미저장 내용을 먼저 보존한다. 다른 페이지를 새로고침하고 Web VS Code를 다시 연다.",
              )}
            </p>
          )}
          <section>
            <h3>{t("Retained operations", "보존된 작업 기록")}</h3>
            {!rows.length && (
              <p>{t("No checkpoints yet.", "아직 백업이 없다.")}</p>
            )}
            <ol
              className="cw-backup-records"
              aria-label={t("Backup operation history", "백업 작업 기록")}
            >
              {rows.slice(0, 100).map((row) => (
                <li key={row.id}>
                  <div>
                    <strong>
                      {row.request.restoreCheckpoint
                        ? t("Restore", "복구")
                        : t("Backup", "백업")}
                    </strong>
                    <span>
                      {row.pendingReconciliation
                        ? t("Reconciliation pending", "상태 확인 필요")
                        : row.phase}
                    </span>
                    <time>
                      {new Date(row.createdAt * 1000).toLocaleString()}
                    </time>
                  </div>
                  <code>{row.id}</code>
                  {row.manifest && (
                    <small>
                      {t("Revision", "리비전")}{" "}
                      {row.manifest.checkpoint.revision} ·{" "}
                      {(row.manifest.volumeBytes / 1024 ** 3).toFixed(1)} GiB
                    </small>
                  )}
                  {row.cleanupId && (
                    <small>
                      {row.cleanedAt
                        ? t(
                            "Artifacts cleaned; receipt retained",
                            "파일 정리 완료 · 작업 기록 유지",
                          )
                        : t(
                            "Artifacts selected for cleanup",
                            "파일 정리 대상으로 확정됨",
                          )}
                    </small>
                  )}
                  {row.error && <p role="status">{row.error}</p>}
                  {row.phase === "complete" &&
                    !row.request.restoreCheckpoint &&
                    !row.cleanupId && (
                      <button
                        type="button"
                        disabled={
                          busy || !!pending || !generation || maintenance
                        }
                        onClick={() => setRestore(row.id)}
                      >
                        {t("Select for restoration", "복구 대상으로 선택")}
                      </button>
                    )}
                  {(row.pendingReconciliation ||
                    row.error ||
                    !["complete", "aborted"].includes(row.phase)) && (
                    <>
                      <button
                        type="button"
                        disabled={busy}
                        onClick={() =>
                          void action(async () => {
                            try {
                              await request(
                                "POST",
                                `${path}/operations/${row.id}/retry`,
                                {},
                                false,
                              );
                            } finally {
                              await load();
                            }
                          })
                        }
                      >
                        {t("Resume", "재개")}
                      </button>
                      {[
                        "pending",
                        "database_frozen",
                        "prepared",
                        "aborted",
                      ].includes(row.phase) && (
                        <button
                          type="button"
                          disabled={busy}
                          onClick={() =>
                            void action(async () => {
                              await request(
                                "DELETE",
                                `${path}/operations/${row.id}`,
                                undefined,
                                false,
                              );
                              await load();
                            })
                          }
                        >
                          {t("Abort before installation", "설치 전 작업 취소")}
                        </button>
                      )}
                    </>
                  )}
                </li>
              ))}
            </ol>
          </section>
          {restore && (
            <section
              className="cw-backup-confirm"
              role="region"
              aria-label={t("Confirm restoration", "복구 확인")}
            >
              <h3>
                {t(
                  "Restore the entire project?",
                  "프로젝트 전체를 복구하는가?",
                )}
              </h3>
              <code>{restore}</code>
              <p>
                {t(
                  "Files, database records and saved layouts return to this checkpoint. Preserve unsaved text outside this workspace before continuing. Current files are preserved privately. All open tabs must reload after restoration. The VM must be stopped first.",
                  "파일, DB 기록, 저장된 화면을 선택한 시점으로 되돌린다. 계속하기 전에 미저장 텍스트를 작업 공간 밖에 복사한다. 현재 파일은 별도로 보존한다. 복구 후에는 모든 탭을 다시 읽어야 한다. 먼저 VM을 정지한다.",
                )}
              </p>
              <button
                type="button"
                disabled={busy || !!pending}
                onClick={() => void action(() => create(restore))}
              >
                {t("Restore this checkpoint", "선택한 백업으로 복구")}
              </button>
              <button
                type="button"
                disabled={busy}
                onClick={() => setRestore("")}
              >
                {t("Cancel", "취소")}
              </button>
            </section>
          )}
        </dialog>,
        document.body,
      )}
    </>
  );
}
