import { useCallback, useEffect, useState } from "react";
import type { ImageInstallResponse, ImageResponse, VmResponse } from "../bindings";
import {
  ApiClientError,
  deleteImage,
  deleteVm,
  getImageInstall,
  listImages,
  listVms,
  startImageInstall,
  stopVm,
} from "../api/client";
import { logDownloadFilename } from "../lib/textExport";
import LogExportActions from "./LogExportActions";

const POLL_MS = 1_500;

/** Human size for the real rootfs artifact (not the ceiled min-disk floor). */
function formatRootfsSize(bytes: number | undefined | null): string {
  const n = typeof bytes === "number" ? bytes : Number(bytes);
  if (!Number.isFinite(n) || n <= 0) return "—";
  const gib = n / 1024 ** 3;
  if (gib >= 1) {
    const rounded = gib >= 10 || Number.isInteger(gib) ? gib.toFixed(0) : gib.toFixed(2);
    return `${rounded} GiB`;
  }
  const mib = n / 1024 ** 2;
  const rounded = mib >= 10 || Number.isInteger(mib) ? mib.toFixed(0) : mib.toFixed(1);
  return `${rounded} MiB`;
}

/**
 * M2Image catalog page — list known templates, install missing ones from
 * `FIRECRAB_IMAGE_BASE_URL`, and show the install progress log with copy/download.
 */
export default function Images() {
  const [images, setImages] = useState<ImageResponse[] | null>(null);
  const [listError, setListError] = useState<string | null>(null);
  const [busyAlias, setBusyAlias] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  const [selectedAlias, setSelectedAlias] = useState<string | null>(null);
  const [install, setInstall] = useState<ImageInstallResponse | null>(null);

  const refreshList = useCallback(async () => {
    try {
      const next = await listImages();
      setImages(next);
      setListError(null);
      setSelectedAlias((current) => {
        if (current && next.some((image) => image.alias === current)) return current;
        return next[0]?.alias ?? null;
      });
    } catch (error) {
      setListError((error as Error).message);
    }
  }, []);

  useEffect(() => {
    void refreshList();
  }, [refreshList]);

  // Poll install status for the selected alias while running (or once on select).
  useEffect(() => {
    if (!selectedAlias) {
      setInstall(null);
      return;
    }
    let cancelled = false;
    const tick = async () => {
      try {
        const snap = await getImageInstall(selectedAlias);
        if (!cancelled) setInstall(snap);
        if (snap.status === "succeeded") {
          void refreshList();
        }
      } catch {
        /* keep last snapshot */
      }
    };
    void tick();
    const interval = setInterval(() => void tick(), POLL_MS);
    return () => {
      cancelled = true;
      clearInterval(interval);
    };
  }, [selectedAlias, refreshList]);

  const handleInstall = async (alias: string) => {
    setBusyAlias(alias);
    setActionError(null);
    setSelectedAlias(alias);
    try {
      const snap = await startImageInstall(alias);
      setInstall(snap);
    } catch (error) {
      setActionError((error as Error).message);
    } finally {
      setBusyAlias(null);
    }
  };

  /**
   * Stop (if needed) and delete every VM that still pins this image, so the
   * image delete can proceed entirely from the dashboard.
   */
  const removeVmsUsingImage = async (users: VmResponse[]) => {
    for (const vm of users) {
      if (vm.state === "running" || vm.state === "starting") {
        await stopVm(vm.id);
        // Poll until delete-eligible (stopped / error / created).
        for (let attempt = 0; attempt < 40; attempt++) {
          await new Promise((resolve) => setTimeout(resolve, 250));
          const latest = (await listVms()).find((entry) => entry.id === vm.id);
          if (!latest) break;
          if (latest.state === "stopped" || latest.state === "error" || latest.state === "created") {
            break;
          }
        }
      }
      const latest = (await listVms()).find((entry) => entry.id === vm.id);
      if (!latest) continue;
      if (latest.state === "stopping" || latest.state === "starting") {
        throw new Error(
          `VM ${latest.name}이(가) 아직 ${latest.state} 상태입니다. 잠시 후 다시 시도하세요.`,
        );
      }
      await deleteVm(latest.id);
    }
  };

  const handleDelete = async (alias: string) => {
    if (
      !window.confirm(
        `'${alias}' 이미지를 삭제할까요?\n레지스트리에서 제거하고 디스크 파일을 지웁니다.`,
      )
    ) {
      return;
    }
    setBusyAlias(alias);
    setActionError(null);
    setSelectedAlias(alias);
    try {
      try {
        await deleteImage(alias);
      } catch (error) {
        const apiError = error instanceof ApiClientError ? error : null;
        if (apiError?.apiError?.code !== "in_use") throw error;

        const users = (await listVms()).filter((vm) => vm.template === alias);
        if (users.length === 0) throw error;

        const lines = users.map((vm) => `· ${vm.name} [${vm.state}]`).join("\n");
        if (
          !window.confirm(
            `'${alias}' 이미지를 쓰는 VM ${users.length}개가 있습니다.\n` +
              `웹에서 해당 VM을 지운 뒤 이미지를 삭제할까요?\n\n${lines}`,
          )
        ) {
          setActionError(
            `이미지 삭제 취소됨 — 사용 중인 VM: ${users.map((vm) => vm.name).join(", ")}`,
          );
          return;
        }

        await removeVmsUsingImage(users);
        await deleteImage(alias);
      }
      setInstall(null);
      await refreshList();
    } catch (error) {
      setActionError((error as Error).message);
    } finally {
      setBusyAlias(null);
    }
  };

  if (images === null && !listError) {
    return <div className="empty">이미지 목록 불러오는 중…</div>;
  }

  return (
    <div className="stack">
      <section className="panel">
        <h2 className="panel-title">M2Image</h2>
        {listError && <div className="field-error">{listError}</div>}
        {actionError && <div className="field-error">{actionError}</div>}
        {!listError && images && images.length === 0 && (
          <div className="empty">알려진 이미지가 없습니다.</div>
        )}
        {images && images.length > 0 && (
          <table className="vm-table image-table">
            <thead>
              <tr>
                <th>image</th>
                <th>상태</th>
                <th>disk</th>
                <th />
              </tr>
            </thead>
            <tbody>
              {images.map((image) => {
                const selected = selectedAlias === image.alias;
                const installing =
                  selected && install?.status === "running" && install.alias === image.alias;
                return (
                  <tr
                    key={image.alias}
                    className={selected ? "is-selected" : undefined}
                    onClick={() => setSelectedAlias(image.alias)}
                  >
                    <td className="mono">{image.alias}</td>
                    <td>
                      {image.installed ? (
                        <span className="state-badge running">installed</span>
                      ) : installing ? (
                        <span className="state-badge starting">installing</span>
                      ) : (
                        <span className="state-badge">missing</span>
                      )}
                    </td>
                    <td
                      className="mono"
                      title={
                        image.rootfsSizeBytes && image.rootfsSizeBytes > 0
                          ? `${image.rootfsSizeBytes} bytes`
                          : undefined
                      }
                    >
                      {formatRootfsSize(image.rootfsSizeBytes)}
                    </td>
                    <td className="actions">
                      {!image.installed ? (
                        <button
                          type="button"
                          className="btn primary"
                          disabled={busyAlias === image.alias || installing}
                          onClick={(event) => {
                            event.stopPropagation();
                            void handleInstall(image.alias);
                          }}
                        >
                          {busyAlias === image.alias || installing ? "설치 중…" : "설치"}
                        </button>
                      ) : (
                        <button
                          type="button"
                          className="btn danger"
                          disabled={busyAlias === image.alias || installing}
                          title="레지스트리 해제 및 이미지 파일 삭제"
                          onClick={(event) => {
                            event.stopPropagation();
                            void handleDelete(image.alias);
                          }}
                        >
                          {busyAlias === image.alias ? "삭제 중…" : "삭제"}
                        </button>
                      )}
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        )}
      </section>

      {selectedAlias && (
        <section className="panel">
          <div className="log-export-bar">
            <h2 className="panel-title" style={{ margin: 0, flex: 1 }}>
              설치 로그 — {selectedAlias}
              {install?.status ? ` · ${install.status}` : ""}
            </h2>
            <LogExportActions
              text={install?.log ?? ""}
              filename={logDownloadFilename("image-install", selectedAlias)}
              buttonClassName="btn console-bar-btn"
              disabled={!install?.log}
            />
          </div>
          <pre className="detail-log image-install-log">
            {install?.log?.trim()
              ? install.log
              : "아직 설치 기록이 없습니다. missing 이미지에서 설치를 누르세요."}
          </pre>
        </section>
      )}
    </div>
  );
}
