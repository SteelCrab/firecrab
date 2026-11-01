import { useState } from "react";
import { Box, ChevronDown, Trash2 } from "lucide-react";
import type { ImageResponse, KernelResponse, VmResponse } from "../bindings";
import { useI18n } from "../i18n";
import { vmDetailsHash } from "../navigation";
import { DetailHead, DetailProp, DetailSection } from "./ui/DetailSection";
import StatusBadge from "./ui/StatusBadge";
import "../styles/image-detail.css";

function formatSize(bytes: number | undefined): string {
  if (!bytes || bytes <= 0) return "—";
  const gib = bytes / 1024 ** 3;
  return gib >= 1
    ? `${Number(gib.toFixed(2))} GiB`
    : `${Number((bytes / 1024 ** 2).toFixed(1))} MiB`;
}

export default function ImageDetail({
  image,
  kernels,
  busy,
  onUpdateKernel,
  onDelete,
  usedByVms,
  usedByError,
}: {
  image: ImageResponse;
  kernels: KernelResponse[];
  busy: boolean;
  onUpdateKernel: (version: string) => Promise<void>;
  onDelete: () => Promise<void>;
  usedByVms: VmResponse[] | null;
  usedByError: string | null;
}) {
  const { t } = useI18n();
  const [targetKernel, setTargetKernel] = useState(image.kernelVersion ?? "");
  const installedKernels = kernels.filter((kernel) => kernel.installed);
  const unused = usedByVms !== null && usedByVms.length === 0 && !usedByError;
  const canUpdate =
    unused && !!targetKernel && targetKernel !== image.kernelVersion && !busy;

  return (
    <div className="image-detail">
      <DetailHead
        title={image.alias}
        meta={
          <span className={`state-badge${image.installed ? " running" : ""}`}>
            {image.installed
              ? t("Installed", "설치됨")
              : t("Not installed", "설치 안 됨")}
          </span>
        }
        actions={
          image.installed && (
            <button
              type="button"
              className="btn danger"
              disabled={busy || !unused}
              onClick={() => void onDelete()}
            >
              <Trash2 aria-hidden="true" />
              {t("Delete image", "이미지 삭제")}
            </button>
          )
        }
      />
      <div className="detail-body">
        {image.description?.trim() && (
          <p className="image-detail-description">{image.description}</p>
        )}
        <DetailSection title={t("Overview", "기본 정보")}>
          <dl className="props">
            <DetailProp label={t("Version", "버전")}>
              {image.version || "—"}
            </DetailProp>
            <DetailProp label={t("Kernel", "커널")}>
              {image.kernelVersion || t("Distro-provided", "배포판 제공")}
            </DetailProp>
            <DetailProp label={t("Image size", "이미지 크기")}>
              {formatSize(image.rootfsSizeBytes)}
            </DetailProp>
            <DetailProp label={t("Minimum disk", "최소 디스크")}>
              {image.minDiskGb > 0 ? `${image.minDiskGb} GiB` : "—"}
            </DetailProp>
            {image.installed && image.hasGuestService !== undefined && (
              <DetailProp label={t("Startup service", "시작 서비스")}>
                {image.hasGuestService
                  ? t("Included", "포함")
                  : t("None", "없음")}
              </DetailProp>
            )}
          </dl>
        </DetailSection>
        <DetailSection title={t("VMs using this image", "사용 중인 VM")}>
          {usedByError ? (
            <p className="detail-note" role="alert">
              {usedByError}
            </p>
          ) : usedByVms === null ? (
            <p className="detail-note">
              {t("Checking usage…", "사용 여부 확인 중…")}
            </p>
          ) : usedByVms.length === 0 ? (
            <p className="detail-note">
              {t("No VMs use this image.", "이 이미지를 사용하는 VM이 없다.")}
            </p>
          ) : (
            <ul className="detail-list">
              {usedByVms.map((vm) => (
                <li key={vm.id}>
                  <Box aria-hidden="true" />
                  <a className="detail-list-main" href={vmDetailsHash(vm.id)}>
                    {vm.name}
                  </a>
                  <StatusBadge state={vm.state} />
                </li>
              ))}
            </ul>
          )}
        </DetailSection>
        {image.installed && (
          <details className="image-detail-disclosure">
            <summary>
              <span>{t("Change kernel", "커널 변경")}</span>
              <ChevronDown aria-hidden="true" />
            </summary>
            <div className="image-detail-disclosure-body">
              <form
                onSubmit={(event) => {
                  event.preventDefault();
                  if (canUpdate) void onUpdateKernel(targetKernel);
                }}
              >
                <label htmlFor="image-target-kernel">
                  {t("Installed kernel", "설치된 커널")}
                </label>
                <select
                  id="image-target-kernel"
                  value={targetKernel}
                  disabled={busy || !unused || installedKernels.length === 0}
                  onChange={(event) => setTargetKernel(event.target.value)}
                >
                  <option value="">{t("Select kernel", "커널 선택")}</option>
                  {installedKernels.map((kernel) => (
                    <option key={kernel.version} value={kernel.version}>
                      {kernel.version}
                      {kernel.version === image.kernelVersion
                        ? t(" · current", " · 현재")
                        : ""}
                    </option>
                  ))}
                </select>
                <button type="submit" className="btn" disabled={!canUpdate}>
                  {busy
                    ? t("Updating…", "변경 중…")
                    : t("Apply kernel", "커널 적용")}
                </button>
              </form>
              {!unused && (
                <p className="detail-note">
                  {t(
                    "Only images with no VM references can change their kernel.",
                    "사용 중인 VM이 없는 이미지만 커널을 변경할 수 있다.",
                  )}
                </p>
              )}
              {installedKernels.length === 0 && (
                <p className="detail-note">
                  {t("Install a kernel first.", "먼저 커널을 설치한다.")}
                </p>
              )}
            </div>
          </details>
        )}
        <details className="image-detail-disclosure">
          <summary>
            <span>{t("Technical details", "기술 정보")}</span>
            <ChevronDown aria-hidden="true" />
          </summary>
          <dl className="props image-detail-technical">
            {image.kernelImage && (
              <DetailProp label={t("Kernel image", "커널 이미지")} wrap>
                <span className="mono">{image.kernelImage}</span>
              </DetailProp>
            )}
            {image.kernelSha256 && (
              <DetailProp
                label={t("Kernel SHA-256", "커널 SHA-256")}
                copy={image.kernelSha256}
                wrap
              >
                <span className="mono">{image.kernelSha256}</span>
              </DetailProp>
            )}
            {image.rootfsSha256 && (
              <DetailProp label="Rootfs SHA-256" copy={image.rootfsSha256} wrap>
                <span className="mono">{image.rootfsSha256}</span>
              </DetailProp>
            )}
            {image.initrdSha256 && (
              <DetailProp label="Initrd SHA-256" copy={image.initrdSha256} wrap>
                <span className="mono">{image.initrdSha256}</span>
              </DetailProp>
            )}
            {image.packageUrl && !image.installed && !image.packageStaged && (
              <DetailProp
                label={t(
                  "Installation file download URL",
                  "설치 파일 다운로드 주소",
                )}
                copy={image.packageUrl}
                wrap
              >
                <span className="mono">{image.packageUrl}</span>
              </DetailProp>
            )}
            <DetailProp label={t("Installation file", "설치 파일")} wrap>
              <span>
                <span>
                  {image.packageStaged
                    ? t("Stored", "보관 중")
                    : t("Not stored", "보관된 파일 없음")}
                </span>
                <small className="image-file-help">
                  {t(
                    "A compressed archive for installing this image.",
                    "이미지 설치에 쓰는 압축 파일이다.",
                  )}
                  {image.installed && !image.packageStaged && (
                    <>
                      {" "}
                      {t(
                        "The installed image can still be used.",
                        "설치된 이미지는 그대로 사용할 수 있다.",
                      )}
                    </>
                  )}
                </small>
              </span>
            </DetailProp>
          </dl>
        </details>
      </div>
    </div>
  );
}
