import { useCallback, useEffect, useState } from "react";
import type { ImageInstallResponse, ImageResponse, VmResponse } from "../bindings";
import {
  ApiClientError,
  deleteImage,
  deleteVm,
  getImageInstall,
  getImagePackage,
  listImages,
  listVms,
  startImageInstall,
  startImagePackage,
  stopVm,
} from "../api/client";
import { logDownloadFilename } from "../lib/textExport";
import LogExportActions from "./LogExportActions";

const PACKER_TEMPLATES = [
  {
    alias: "alpine-3.24",
    label: "Alpine Linux",
    detail: "3.24 · virt kernel",
    logoSrc: "https://www.alpinelinux.org/alpinelinux-logo.svg",
  },
  {
    alias: "ubuntu-26.04",
    label: "Ubuntu",
    detail: "26.04 · generic kernel",
    logoSrc: "https://assets.ubuntu.com/v1/ff6a9a38-ubuntu-logo-2022.svg",
  },
  {
    alias: "rocky-9",
    label: "Rocky Linux",
    detail: "9 · generic kernel",
    logoSrc: "https://raw.githubusercontent.com/rocky-linux/branding/main/logo/src/icon-primary.svg",
  },
] as const;
type PackerTarget = (typeof PACKER_TEMPLATES)[number]["alias"];
type PackageJobs = Partial<Record<PackerTarget, ImageInstallResponse>>;

interface PackerStage {
  id: "source" | "rootfs" | "kernel" | "package";
  label: string;
}

const PACKER_STAGES: PackerStage[] = [
  {
    id: "source",
    label: "소스 확인",
  },
  {
    id: "rootfs",
    label: "rootfs 확인",
  },
  {
    id: "kernel",
    label: "커널 확인",
  },
  {
    id: "package",
    label: "패키지 검증",
  },
];

type PackerStageState = "pending" | "running" | "succeeded" | "failed";

function stageLog(job: ImageInstallResponse | null, stage: PackerStage["id"]): string {
  if (!job || job.status === "idle") return "";
  return job.log
    .split("\n")
    .filter((line) => line.includes(`[packer:${stage}]`))
    .join("\n");
}

function stageState(job: ImageInstallResponse | null, stage: PackerStage["id"]): PackerStageState {
  const log = stageLog(job, stage);
  if (!job || job.status === "idle") return "pending";
  // Package archives survive an API restart while the in-memory stage log
  // does not. A succeeded cache is still a completed pipeline; its generic
  // cache message is shown in the log panel below.
  if (!log && job.status === "succeeded") return "succeeded";
  if (!log) return "pending";
  if (job.status === "failed" && !log.includes("complete")) return "failed";
  if (log.includes("complete")) return "succeeded";
  return "running";
}

function stageStatusText(state: PackerStageState): string {
  switch (state) {
    case "running":
      return "진행 중";
    case "succeeded":
      return "완료";
    case "failed":
      return "실패";
    default:
      return "대기";
  }
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
  if (
    currentStarted !== undefined &&
    incomingStarted !== undefined &&
    incomingStarted < currentStarted
  ) {
    return current;
  }

  const currentIsTerminal = current.status === "succeeded" || current.status === "failed";
  if (
    currentStarted !== undefined &&
    incomingStarted === currentStarted &&
    currentIsTerminal &&
    incoming.status === "running"
  ) {
    return current;
  }
  return incoming;
}

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

/** M2Image-Packer owns package acquisition only. */
function PackerBuildPanel({
  images,
  target,
  packageJob,
  imageInstall,
  busyAlias,
  onTargetChange,
  onPackage,
}: {
  images: ImageResponse[];
  target: PackerTarget;
  packageJob: ImageInstallResponse | null;
  imageInstall: ImageInstallResponse | null;
  busyAlias: string | null;
  onTargetChange: (alias: PackerTarget) => void;
  onPackage: (alias: PackerTarget) => void;
}) {
  const [selectedStageId, setSelectedStageId] = useState<PackerStage["id"]>("source");
  const selectedStage =
    PACKER_STAGES.find((stage) => stage.id === selectedStageId) ?? PACKER_STAGES[0];
  const image = images.find((candidate) => candidate.alias === target);
  const packageForTarget = packageJob?.alias === target ? packageJob : null;
  const imageInstallForTarget = imageInstall?.alias === target ? imageInstall : null;
  const packageRunning = packageForTarget?.status === "running";
  const imageInstalling = imageInstallForTarget?.status === "running";
  const busy = busyAlias === target || packageRunning || imageInstalling;
  const packageReady = packageForTarget?.status === "succeeded";
  const packageDisabled = !image || busy || !image.packageUrl;
  const packageTitle = !image
    ? `${target} template을 현재 실행 중인 API가 인식하지 못합니다. API를 최신 버전으로 다시 시작하세요.`
    : packageReady
      ? "패키지를 다시 내려받아 로컬 캐시를 교체합니다"
      : !image.packageUrl
        ? "FIRECRAB_IMAGE_BASE_URL 미설정 — 원격 설치 불가"
        : `패키지 내려받기 및 구조 검증: ${image.packageUrl}`;
  const stageStates = PACKER_STAGES.map((stage) => ({
    ...stage,
    state: stageState(packageForTarget, stage.id),
  }));
  const activeStage = stageStates.find((stage) => stage.state === "running");
  const activeStageId = activeStage?.id;
  const selectedLog = stageLog(packageForTarget, selectedStage.id);
  const log = packageForTarget?.status === "failed"
    ? packageForTarget.log
    : selectedLog ||
      (packageReady
        ? packageForTarget?.log
        : "패키지 설치를 시작하면 이 단계의 실제 로그가 여기에 표시됩니다.");

  useEffect(() => {
    if (activeStageId) setSelectedStageId(activeStageId);
  }, [activeStageId]);

  return (
    <section className="panel">
      <h2 className="panel-title">
        M2Image-Packer <span className="poll-note">패키지 설치</span>
      </h2>
      <div className="packer-toolbar">
        <fieldset className="packer-template-picker">
          <legend>template</legend>
          <div className="packer-template-options" role="radiogroup" aria-label="Packer template">
            {PACKER_TEMPLATES.map((template) => {
              const selected = template.alias === target;
              return (
                <button
                  key={template.alias}
                  type="button"
                  role="radio"
                  aria-checked={selected}
                  className={`packer-template-option${selected ? " is-selected" : ""}`}
                  onClick={() => onTargetChange(template.alias)}
                >
                  <img className="packer-template-logo" src={template.logoSrc} alt="" />
                  <span>
                    <strong>{template.label}</strong>
                    <small>{template.detail}</small>
                  </span>
                </button>
              );
            })}
          </div>
        </fieldset>
        <div className="packer-install-action">
          <span className={`state-badge${packageReady ? " running" : packageRunning ? " starting" : ""}`}>
            {packageRunning ? "패키지 설치 중" : packageReady ? "패키지 준비됨" : "패키지 미준비"}
          </span>
          <button
            type="button"
            className="btn"
            disabled={packageDisabled}
            title={packageTitle}
            onClick={() => onPackage(target)}
          >
            {packageRunning ? "패키지 설치 중…" : packageReady ? "패키지 다시 설치" : "패키지 설치"}
          </button>
        </div>
      </div>
      <code className="packer-command">package: dist/m2images/{target}.tar.zst → M2Image-Store 가져오기</code>
      <ol className="pipeline packer-pipeline" aria-label={`${target} package installation stages`}>
        {stageStates.map((stage) => {
          const selected = stage.id === selectedStage.id;
          return (
            <li key={stage.id} className={`pipeline-step ${stage.state}`}>
              <span className="step-label">{stage.label}</span>
              <button
                type="button"
                className={`step-bar packer-stage-button${selected ? " is-active" : ""}`}
                aria-pressed={selected}
                onClick={() => setSelectedStageId(stage.id)}
              >
                <span className="step-time">{stageStatusText(stage.state)}</span>
                <span className="step-mark">
                  {stage.state === "succeeded" ? "✓" : stage.state === "failed" ? "✕" : "—"}
                </span>
              </button>
              <span className="step-started">
                {stage.state === "pending" ? "패키지 설치 대기" : "로그 보기"}
              </span>
            </li>
          );
        })}
      </ol>
      <div className="log-export-bar">
        <span className="log-export-bar-label">
          패키지 설치 로그 — {target} · {selectedStage.label}
        </span>
        <LogExportActions
          text={log}
          filename={logDownloadFilename("m2image-packer", `${target}-${selectedStage.id}`)}
          buttonClassName="btn console-bar-btn"
        />
      </div>
      <pre className="detail-log packer-log">{log}</pre>
      <p className="packer-note">
        원본 빌드는 build host/CI에서 수행합니다. 여기서는 게시된 아카이브를 검증해
        <strong>패키지 설치</strong>만 합니다. 준비된 패키지는 <strong>M2Image-Store</strong>에서
        이미지로 가져옵니다.
      </p>
    </section>
  );
}

/**
 * M2Image Store + Packer: download a known package first, then separately
 * install it into the local template registry with observable logs.
 */
export default function Images() {
  const [images, setImages] = useState<ImageResponse[] | null>(null);
  const [listError, setListError] = useState<string | null>(null);
  const [busyAlias, setBusyAlias] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  const [selectedAlias, setSelectedAlias] = useState<string | null>(null);
  const [packerAlias, setPackerAlias] = useState<PackerTarget>("alpine-3.24");
  const [packageJobs, setPackageJobs] = useState<PackageJobs>({});
  const [storeImportAlias, setStoreImportAlias] = useState<PackerTarget | null>(null);
  const [install, setInstall] = useState<ImageInstallResponse | null>(null);

  const refreshList = useCallback(async () => {
    try {
      const next = await listImages();
      setImages(next);
      setListError(null);
      setSelectedAlias((current) => {
        const installed = next.filter((image) => image.installed);
        if (current && installed.some((image) => image.alias === current)) return current;
        return installed[0]?.alias ?? null;
      });
    } catch (error) {
      setListError((error as Error).message);
    }
  }, []);

  useEffect(() => {
    void refreshList();
  }, [refreshList]);

  const packagePollingActive = PACKER_TEMPLATES.some(
    (template) => packageJobs[template.alias]?.status === "running",
  );

  // Package preparation belongs to Packer.  Query each template independently:
  // an older API can legitimately not know a newer template yet, and that 404
  // must not hide the ready state of every other package.
  useEffect(() => {
    let cancelled = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const tick = async () => {
      let continuePolling = packagePollingActive;
      const results = await Promise.allSettled(
        PACKER_TEMPLATES.map((template) => getImagePackage(template.alias)),
      );
      if (cancelled) return;

      const snapshots: PackageJobs = {};
      results.forEach((result, index) => {
        if (result.status !== "fulfilled") return;
        const alias = PACKER_TEMPLATES[index].alias;
        snapshots[alias] = result.value;
        if (result.value.status === "running") continuePolling = true;
      });
      // Leave the last useful result intact for an endpoint that is temporarily
      // unavailable, instead of replacing the whole dashboard state with an
      // empty response because one template returned an error.
      if (Object.keys(snapshots).length > 0) {
        setPackageJobs((current) => {
          const next = { ...current };
          for (const template of PACKER_TEMPLATES) {
            const snapshot = snapshots[template.alias];
            if (snapshot) {
              next[template.alias] = keepNewestJobSnapshot(
                current[template.alias],
                snapshot,
              );
            }
          }
          return next;
        });
      }
      if (!cancelled && continuePolling) {
        timer = setTimeout(() => void tick(), 300);
      }
    };
    void tick();
    return () => {
      cancelled = true;
      if (timer) clearTimeout(timer);
    };
  }, [packagePollingActive]);

  // Store owns the second operation: extract/register the selected prepared
  // package.  Its polling must not follow the template currently selected in
  // Packer.
  useEffect(() => {
    if (!storeImportAlias) return;
    const alias = storeImportAlias;
    let cancelled = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const tick = async () => {
      let continuePolling = install?.alias === alias && install.status === "running";
      try {
        const nextInstall = await getImageInstall(alias);
        if (cancelled) return;
        setInstall((current) => keepNewestJobSnapshot(current, nextInstall));
        if (
          nextInstall.status === "succeeded" &&
          !(install?.alias === alias && install.status === "succeeded")
        ) {
          void refreshList();
        }
        continuePolling = nextInstall.status === "running";
      } catch {
        /* keep last snapshot */
      }
      if (!cancelled && continuePolling) {
        timer = setTimeout(() => void tick(), 300);
      }
    };
    void tick();
    return () => {
      cancelled = true;
      if (timer) clearTimeout(timer);
    };
  }, [storeImportAlias, install?.alias, install?.status, refreshList]);

  const handlePackage = async (alias: PackerTarget) => {
    setBusyAlias(alias);
    setActionError(null);
    setPackerAlias(alias);
    try {
      const snap = await startImagePackage(alias);
      setPackageJobs((current) => ({ ...current, [alias]: snap }));
    } catch (error) {
      setActionError((error as Error).message);
    } finally {
      setBusyAlias(null);
    }
  };

  const handleInstall = async (alias: PackerTarget) => {
    setBusyAlias(alias);
    setActionError(null);
    setStoreImportAlias(alias);
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
    if (PACKER_TEMPLATES.some((template) => template.alias === alias)) {
      setPackerAlias(alias as PackerTarget);
    }
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
      await refreshList();
      if (install?.alias === alias) setInstall(null);
    } catch (error) {
      setActionError((error as Error).message);
    } finally {
      setBusyAlias(null);
    }
  };

  // The Store inventory remains installed-only.  A prepared archive belongs
  // to this separate import queue, not to the image table.
  const readyImportTemplates = PACKER_TEMPLATES.filter((template) => {
    const image = images?.find((candidate) => candidate.alias === template.alias);
    return Boolean(image && !image.installed && packageJobs[template.alias]?.status === "succeeded");
  });
  const readyImportKey = readyImportTemplates.map((template) => template.alias).join(",");

  useEffect(() => {
    const readyAliases = (readyImportKey ? readyImportKey.split(",") : []) as PackerTarget[];
    setStoreImportAlias((current) => {
      if (current && readyAliases.includes(current)) return current;
      return readyAliases[0] ?? null;
    });
  }, [readyImportKey]);

  const importInProgress =
    install?.alias === storeImportAlias && install.status === "running";
  const importCompleted =
    install?.alias === storeImportAlias && install.status === "succeeded";
  const importDisabled =
    !storeImportAlias || !readyImportTemplates.some((template) => template.alias === storeImportAlias) ||
    importInProgress || importCompleted || busyAlias === storeImportAlias;

  if (images === null && !listError) {
    return <div className="empty">이미지 목록 불러오는 중…</div>;
  }

  // The Store is the host's installed inventory. Missing catalog entries must
  // not reappear here after deletion; only their prepared package can be
  // offered through the separate import queue above the table.
  const storeImages = images?.filter((image) => image.installed) ?? [];

  return (
    <div className="stack">
      <section className="panel">
        <h2 className="panel-title">M2Image-Store</h2>
        {listError && <div className="field-error">{listError}</div>}
        {actionError && <div className="field-error">{actionError}</div>}
        <div className="store-import-dock">
          <div className="store-import-copy">
            <strong>이미지 가져오기</strong>
            <small>준비된 패키지를 풀어 검증하고 Store에 등록합니다.</small>
          </div>
          <label className="store-import-picker">
            <span>준비된 패키지</span>
            <select
              value={storeImportAlias ?? ""}
              disabled={readyImportTemplates.length === 0 || importInProgress}
              onChange={(event) => setStoreImportAlias(event.target.value as PackerTarget)}
            >
              {readyImportTemplates.length === 0 && (
                <option value="">Packer에서 패키지를 먼저 설치하세요</option>
              )}
              {readyImportTemplates.map((template) => (
                <option key={template.alias} value={template.alias}>
                  {template.label} · {template.alias}
                </option>
              ))}
            </select>
          </label>
          <button
            type="button"
            className="btn primary"
            disabled={importDisabled}
            title={
              storeImportAlias
                ? "준비된 로컬 패키지를 풀어 검증·등록합니다"
                : "먼저 M2Image-Packer에서 패키지 설치를 완료하세요"
            }
            onClick={() => {
              if (storeImportAlias) void handleInstall(storeImportAlias);
            }}
          >
            {importInProgress ? "이미지 가져오는 중…" : "이미지 가져오기"}
          </button>
        </div>
        {install && install.status !== "idle" && (
          <>
            <div className="log-export-bar">
              <span className="log-export-bar-label">이미지 가져오기 로그 — {install.alias}</span>
              <LogExportActions
                text={install.log}
                filename={logDownloadFilename("m2image-import", install.alias)}
                buttonClassName="btn console-bar-btn"
                disabled={!install.log}
              />
            </div>
            <pre className="detail-log image-install-log">{install.log}</pre>
          </>
        )}
        {!listError && images && storeImages.length === 0 && (
          <div className="empty">설치된 이미지가 없습니다. Packer에서 패키지를 설치한 뒤 여기서 가져오세요.</div>
        )}
        {storeImages.length > 0 && (
          <table className="vm-table image-table">
            <thead>
              <tr>
                <th>image</th>
                <th>disk</th>
                <th>package</th>
                <th />
              </tr>
            </thead>
            <tbody>
              {storeImages.map((image) => {
                const selected = selectedAlias === image.alias;
                return (
                  <tr
                    key={image.alias}
                    className={selected ? "is-selected" : undefined}
                    onClick={() => setSelectedAlias(image.alias)}
                  >
                    <td className="mono">{image.alias}</td>
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
                    <td className="mono image-package-url">
                      {image.packageUrl ? (
                        <a
                          href={image.packageUrl}
                          target="_blank"
                          rel="noreferrer"
                          title={image.packageUrl}
                          onClick={(event) => event.stopPropagation()}
                        >
                          {packageBasename(image.packageUrl)}
                        </a>
                      ) : (
                        "—"
                      )}
                    </td>
                    <td className="actions">
                      <button
                        type="button"
                        className="btn danger"
                        disabled={busyAlias === image.alias}
                        title="레지스트리 해제 및 이미지 파일 삭제"
                        onClick={(event) => {
                          event.stopPropagation();
                          void handleDelete(image.alias);
                        }}
                      >
                        {busyAlias === image.alias ? "삭제 중…" : "삭제"}
                      </button>
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        )}
      </section>

      <PackerBuildPanel
        images={images ?? []}
        target={packerAlias}
        packageJob={packageJobs[packerAlias] ?? null}
        imageInstall={install}
        busyAlias={busyAlias}
        onTargetChange={setPackerAlias}
        onPackage={(alias) => void handlePackage(alias)}
      />
    </div>
  );
}
