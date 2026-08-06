import { useCallback, useEffect, useRef, useState } from "react";
import type { BuildResponse, ImageInstallResponse, ImageResponse, VmResponse } from "../bindings";
import {
  ApiClientError,
  buildPackages,
  cancelBuild,
  deleteImage,
  deleteVm,
  finalizeBuild,
  getBuild,
  getImageInstall,
  getImagePackage,
  listImages,
  listVms,
  startBuild,
  startImageInstall,
  startImagePackage,
  stopVm,
} from "../api/client";
import { logDownloadFilename } from "../lib/textExport";
import LogExportActions from "./LogExportActions";

const KNOWN_TEMPLATES = [
  { alias: "alpine-3.24", label: "Alpine Linux", logoSrc: "https://www.alpinelinux.org/alpinelinux-logo.svg" },
  { alias: "ubuntu-26.04", label: "Ubuntu", logoSrc: "https://assets.ubuntu.com/v1/ff6a9a38-ubuntu-logo-2022.svg" },
  { alias: "rocky-9", label: "Rocky Linux", logoSrc: "https://raw.githubusercontent.com/rocky-linux/branding/main/logo/src/icon-primary.svg" },
] as const;

/** Last path segment of an official package URL for the table cell. */
function packageBasename(url: string): string {
  try {
    const path = new URL(url).pathname;
    const seg = path.split("/").filter(Boolean).pop();
    return seg || url;
  } catch {
    return url;
  }
}

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
 * A poll may have left the browser before the POST that starts a newer job.
 * Do not let that older `idle`/`running` response erase the state returned by
 * the POST (or a terminal state for the same job).
 */
function keepNewestJobSnapshot(
  current: ImageInstallResponse | null | undefined,
  incoming: ImageInstallResponse,
): ImageInstallResponse {
  if (!current) return incoming;
  if (incoming.status === "idle" && current.status !== "idle") return current;
  const currentStarted = current.startedAtMs;
  const incomingStarted = incoming.startedAtMs;
  if (currentStarted !== undefined && incomingStarted !== undefined && incomingStarted < currentStarted) {
    return current;
  }
  const currentIsTerminal = current.status === "succeeded" || current.status === "failed";
  if (currentStarted !== undefined && incomingStarted === currentStarted && currentIsTerminal && incoming.status === "running") {
    return current;
  }
  return incoming;
}

/**
 * Build modal: boot a builder VM off `sourceAlias`, install/remove packages
 * on its console, then save the result as a new or updated template.
 */
function BuildModal({
  sourceAlias,
  installedAliases,
  onClose,
  onFinalized,
}: {
  sourceAlias: string;
  installedAliases: string[];
  onClose: () => void;
  onFinalized: () => void;
}) {
  const [build, setBuild] = useState<BuildResponse | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [installInput, setInstallInput] = useState("");
  const [removeInput, setRemoveInput] = useState("");
  const [busy, setBusy] = useState(false);
  const [saveMode, setSaveMode] = useState<"update" | "derive">("update");
  const [newAlias, setNewAlias] = useState("");

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const started = await startBuild(sourceAlias);
        if (!cancelled) setBuild(started);
      } catch (err) {
        if (!cancelled) setError((err as Error).message);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [sourceAlias]);

  useEffect(() => {
    if (!build || build.status === "succeeded" || build.status === "failed") return;
    let cancelled = false;
    const timer = setTimeout(async () => {
      try {
        const snapshot = await getBuild(build.buildId);
        if (!cancelled) setBuild(snapshot);
      } catch {
        /* keep last snapshot */
      }
    }, 1000);
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, [build]);

  const runPackages = async (action: "install" | "remove", input: string) => {
    if (!build) return;
    const packages = input.split(/\s+/).filter(Boolean);
    if (packages.length === 0) return;
    setBusy(true);
    setError(null);
    try {
      const updated = await buildPackages(build.buildId, action, packages);
      setBuild(updated);
      if (action === "install") setInstallInput("");
      if (action === "remove") setRemoveInput("");
    } catch (err) {
      setError((err as Error).message);
    } finally {
      setBusy(false);
    }
  };

  const handleFinalize = async () => {
    if (!build) return;
    if (saveMode === "derive" && !newAlias.trim()) {
      setError("새 이미지 이름을 입력하세요.");
      return;
    }
    setBusy(true);
    setError(null);
    try {
      await finalizeBuild(build.buildId, saveMode === "derive" ? newAlias.trim() : undefined);
      onFinalized();
      onClose();
    } catch (err) {
      setError((err as Error).message);
    } finally {
      setBusy(false);
    }
  };

  const handleCancel = async () => {
    if (build) {
      try {
        await cancelBuild(build.buildId);
      } catch {
        /* best-effort */
      }
    }
    onClose();
  };

  const ready = build?.status === "ready";
  const aliasTaken = newAlias.trim().length > 0 && installedAliases.includes(newAlias.trim());

  return (
    <div className="modal-overlay" onClick={handleCancel}>
      <div className="modal" onClick={(event) => event.stopPropagation()}>
        <h2 className="panel-title">M2Image-builder — {sourceAlias}</h2>
        {error && <div className="field-error">{error}</div>}
        <div className="state-badge">{build?.status ?? "시작 중…"}</div>
        <pre className="detail-log">{build?.log ?? ""}</pre>
        <div className="package-row">
          <input
            type="text"
            placeholder="설치할 패키지 (공백으로 구분)"
            value={installInput}
            onChange={(event) => setInstallInput(event.target.value)}
            disabled={!ready || busy}
          />
          <button type="button" className="btn" disabled={!ready || busy || !installInput.trim()} onClick={() => void runPackages("install", installInput)}>
            설치
          </button>
        </div>
        <div className="package-row">
          <input
            type="text"
            placeholder="삭제할 패키지 (공백으로 구분)"
            value={removeInput}
            onChange={(event) => setRemoveInput(event.target.value)}
            disabled={!ready || busy}
          />
          <button type="button" className="btn danger" disabled={!ready || busy || !removeInput.trim()} onClick={() => void runPackages("remove", removeInput)}>
            삭제
          </button>
        </div>
        <fieldset className="package-row">
          <label>
            <input type="radio" checked={saveMode === "update"} onChange={() => setSaveMode("update")} />
            같은 이미지 갱신 ({sourceAlias})
          </label>
          <label>
            <input type="radio" checked={saveMode === "derive"} onChange={() => setSaveMode("derive")} />
            새 이미지로 저장
          </label>
          {saveMode === "derive" && (
            <input
              type="text"
              placeholder="새 이미지 이름"
              value={newAlias}
              onChange={(event) => setNewAlias(event.target.value)}
            />
          )}
        </fieldset>
        {aliasTaken && <div className="field-error">이미 사용 중인 이름입니다.</div>}
        <div className="package-row">
          <button type="button" className="btn" onClick={() => void handleCancel()}>
            취소
          </button>
          <button type="button" className="btn primary" disabled={!ready || busy || aliasTaken} onClick={() => void handleFinalize()}>
            {busy ? "저장 중…" : "이미지로 저장"}
          </button>
        </div>
      </div>
    </div>
  );
}

/**
 * Single M2Image inventory table plus an on-demand build modal. Package
 * download/install ("가져오기") is a per-row action here rather than a
 * separate panel — the two-stage Packer/Store split moved into BuildModal.
 */
export default function Images() {
  const [images, setImages] = useState<ImageResponse[] | null>(null);
  const [listError, setListError] = useState<string | null>(null);
  const [busyAlias, setBusyAlias] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  const [packageJobs, setPackageJobs] = useState<Record<string, ImageInstallResponse>>({});
  const [install, setInstall] = useState<ImageInstallResponse | null>(null);
  const [buildSourceAlias, setBuildSourceAlias] = useState<string | null>(null);
  const [newBuildSource, setNewBuildSource] = useState<string>(KNOWN_TEMPLATES[0].alias);

  const refreshList = useCallback(async () => {
    try {
      const next = await listImages();
      setImages(next);
      setListError(null);
    } catch (error) {
      setListError((error as Error).message);
    }
  }, []);

  useEffect(() => {
    void refreshList();
  }, [refreshList]);

  // `Images` is conditionally mounted by the App shell (only while the
  // "images" tab is active), so a poll started here can easily outlive the
  // component if the user navigates away mid-install. Every tick must check
  // this before touching state.
  const mountedRef = useRef(true);
  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
    };
  }, []);

  // `startImageInstall` only kicks the install off — the backend always
  // answers with a single "install started" snapshot and does the real
  // extract/register work in the background, so the result must be polled
  // to a terminal status before the table can show "설치됨".
  const pollInstall = (alias: string) => {
    const tick = async () => {
      if (!mountedRef.current) return;
      try {
        const latest = await getImageInstall(alias);
        if (!mountedRef.current) return;
        setInstall((current) => keepNewestJobSnapshot(current, latest));
        if (latest.status === "running") {
          setTimeout(() => void tick(), 300);
        } else if (latest.status === "succeeded") {
          await refreshList();
        }
        // "failed" is a confirmed terminal state too — stop without retrying.
      } catch {
        // A fetch failure is not positive confirmation the job reached a
        // terminal state, so keep polling rather than freezing the log on a
        // one-off network blip (unless the component is already gone).
        if (mountedRef.current) setTimeout(() => void tick(), 300);
      }
    };
    void tick();
  };

  const handleFetchPackage = async (alias: string) => {
    setBusyAlias(alias);
    setActionError(null);
    try {
      const snap = await startImagePackage(alias);
      setPackageJobs((current) => ({ ...current, [alias]: snap }));
      const poll = async () => {
        const latest = await getImagePackage(alias);
        setPackageJobs((current) => ({ ...current, [alias]: keepNewestJobSnapshot(current[alias], latest) }));
        if (latest.status === "running") setTimeout(() => void poll(), 500);
        else if (latest.status === "succeeded") {
          const installed = await startImageInstall(alias);
          setInstall(installed);
          pollInstall(alias);
        }
      };
      void poll();
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
          if (latest.state === "stopped" || latest.state === "error" || latest.state === "created") break;
        }
      }
      const latest = (await listVms()).find((entry) => entry.id === vm.id);
      if (!latest) continue;
      if (latest.state === "stopping" || latest.state === "starting") {
        throw new Error(`VM ${latest.name}이(가) 아직 ${latest.state} 상태입니다. 잠시 후 다시 시도하세요.`);
      }
      await deleteVm(latest.id);
    }
  };

  const handleDelete = async (alias: string) => {
    if (!window.confirm(`'${alias}' 이미지를 삭제할까요?\n레지스트리에서 제거하고 디스크 파일을 지웁니다.`)) return;
    setBusyAlias(alias);
    setActionError(null);
    try {
      try {
        await deleteImage(alias);
      } catch (error) {
        const apiError = error instanceof ApiClientError ? error : null;
        if (apiError?.apiError?.code !== "in_use") throw error;
        const users = (await listVms()).filter((vm) => vm.template === alias);
        if (users.length === 0) throw error;
        const lines = users.map((vm) => `· ${vm.name} [${vm.state}]`).join("\n");
        if (!window.confirm(`'${alias}' 이미지를 쓰는 VM ${users.length}개가 있습니다.\n웹에서 해당 VM을 지운 뒤 이미지를 삭제할까요?\n\n${lines}`)) {
          setActionError(`이미지 삭제 취소됨 — 사용 중인 VM: ${users.map((vm) => vm.name).join(", ")}`);
          return;
        }
        await removeVmsUsingImage(users);
        await deleteImage(alias);
      }
      await refreshList();
      if (install?.alias === alias) setInstall(null);
    } catch (error) {
      setActionError((error as Error).message);
    } finally {
      setBusyAlias(null);
    }
  };

  if (images === null && !listError) {
    return <div className="empty">이미지 목록 불러오는 중…</div>;
  }

  const installedAliases = (images ?? []).filter((image) => image.installed).map((image) => image.alias);

  return (
    <div className="stack">
      <section className="panel">
        <h2 className="panel-title">M2Image</h2>
        {listError && <div className="field-error">{listError}</div>}
        {actionError && <div className="field-error">{actionError}</div>}
        <table className="vm-table image-table">
          <thead>
            <tr>
              <th>이미지</th>
              <th>크기</th>
              <th>상태</th>
              <th />
            </tr>
          </thead>
          <tbody>
            {(images ?? []).map((image) => {
              const job = packageJobs[image.alias];
              const fetching = job?.status === "running";
              const statusLabel = image.installed ? "설치됨" : job?.status === "succeeded" ? "패키지 준비됨" : "미설치";
              // Derived/web-built templates won't have a KNOWN_TEMPLATES entry —
              // fall back to plain alias text with no logo for those.
              const known = KNOWN_TEMPLATES.find((template) => template.alias === image.alias);
              return (
                <tr key={image.alias}>
                  <td className="mono">
                    {known && <img className="packer-template-logo" src={known.logoSrc} alt="" />}
                    {image.alias}
                  </td>
                  <td className="mono">{formatRootfsSize(image.rootfsSizeBytes)}</td>
                  <td>
                    <span className={`state-badge${image.installed ? " running" : ""}`}>{statusLabel}</span>
                  </td>
                  <td className="actions">
                    {image.installed ? (
                      <>
                        <button type="button" className="btn" disabled={busyAlias === image.alias} onClick={() => setBuildSourceAlias(image.alias)}>
                          빌드
                        </button>
                        <button type="button" className="btn danger" disabled={busyAlias === image.alias} onClick={() => void handleDelete(image.alias)}>
                          {busyAlias === image.alias ? "삭제 중…" : "삭제"}
                        </button>
                      </>
                    ) : image.packageUrl ? (
                      <button type="button" className="btn primary" disabled={fetching || busyAlias === image.alias} onClick={() => void handleFetchPackage(image.alias)} title={image.packageUrl}>
                        {fetching ? "가져오는 중…" : `가져오기 (${packageBasename(image.packageUrl)})`}
                      </button>
                    ) : (
                      <span className="poll-note">패키지 URL 없음</span>
                    )}
                  </td>
                </tr>
              );
            })}
          </tbody>
        </table>
        {install && install.status !== "idle" && (
          <>
            <div className="log-export-bar">
              <span className="log-export-bar-label">이미지 가져오기 로그 — {install.alias}</span>
              <LogExportActions text={install.log} filename={logDownloadFilename("m2image-import", install.alias)} buttonClassName="btn console-bar-btn" disabled={!install.log} />
            </div>
            <pre className="detail-log image-install-log">{install.log}</pre>
          </>
        )}
        <div className="package-row">
          <select value={newBuildSource} onChange={(event) => setNewBuildSource(event.target.value)}>
            {installedAliases.map((alias) => (
              <option key={alias} value={alias}>{alias}</option>
            ))}
          </select>
          <button type="button" className="btn primary" disabled={installedAliases.length === 0} onClick={() => setBuildSourceAlias(newBuildSource)}>
            + 새 이미지 빌드
          </button>
        </div>
      </section>

      {buildSourceAlias && (
        <BuildModal
          key={buildSourceAlias}
          sourceAlias={buildSourceAlias}
          installedAliases={installedAliases}
          onClose={() => setBuildSourceAlias(null)}
          onFinalized={() => void refreshList()}
        />
      )}
    </div>
  );
}
