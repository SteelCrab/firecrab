import { useEffect, useState } from "react";
import {
  Check,
  Circle,
  Clock3,
  Download,
  LoaderCircle,
  ShieldCheck,
  Terminal,
  X,
  Archive,
} from "lucide-react";
import type { KernelInstallResponse } from "../bindings";
import { useI18n } from "../i18n";
import {
  kernelSteps,
  registryInstall,
  kernelBytes,
  kernelDownloadPercent,
  kernelLastActivity,
  kernelStage,
} from "../lib/kernelInstall";

const descriptions = [
  [
    "Resolving the source and checking the local kernel cache.",
    "소스 정보와 로컬 커널 캐시를 확인한다.",
  ],
  [
    "Downloading and checking the Linux source archive from kernel.org.",
    "kernel.org에서 Linux 소스 파일을 내려받고 확인한다.",
  ],
  [
    "Waiting for the shared host builder. Kernel builds run one at a time.",
    "호스트의 빌드 차례를 기다린다. 커널은 한 번에 하나씩 빌드한다.",
  ],
  [
    "Compiling Linux for this host's architecture. Downloading is complete.",
    "다운로드를 마쳤다. 호스트 아키텍처에 맞게 Linux 커널을 컴파일한다.",
  ],
  [
    "Checking the built kernel and saving the verified image.",
    "빌드한 커널을 검증하고 이미지로 저장한다.",
  ],
] as const;

const registryDescriptions = [
  [
    "Checking the published package and local cache.",
    "공개된 커널 패키지와 로컬 캐시를 확인한다.",
  ],
  [
    "Downloading the prebuilt kernel from the registry. No local compilation is required.",
    "레지스트리에서 빌드된 커널을 내려받는다. 로컬 빌드는 하지 않는다.",
  ],
  [
    "Extracting the kernel image and runtime config.",
    "커널 이미지와 실행 설정의 압축을 해제한다.",
  ],
  [
    "Checking architecture, checksum, and built-in VM terminal drivers.",
    "아키텍처, 체크섬, VM 터미널 드라이버를 검증한다.",
  ],
  [
    "Saving the verified kernel for your VMs.",
    "검증한 커널을 VM에서 사용할 수 있도록 저장한다.",
  ],
] as const;

export function KernelElapsed({ job }: { job: KernelInstallResponse }) {
  const { t } = useI18n();
  const [now, setNow] = useState(Date.now);
  useEffect(() => {
    if (job.status !== "running") return;
    const timer = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(timer);
  }, [job.status]);
  if (!job.startedAtMs) return null;
  const seconds = Math.max(
    0,
    Math.floor(((job.endedAtMs ?? now) - job.startedAtMs) / 1000),
  );
  const minutes = Math.floor(seconds / 60);
  return (
    <span className="kernel-elapsed">
      {minutes > 0
        ? t(`${minutes}m ${seconds % 60}s`, `${minutes}분 ${seconds % 60}초`)
        : t(`${seconds}s`, `${seconds}초`)}
    </span>
  );
}

export default function KernelInstallation({
  job,
  builderVersion,
  registry = false,
}: {
  job: KernelInstallResponse;
  builderVersion?: string;
  registry?: boolean;
}) {
  const { t } = useI18n();
  const stage = kernelStage(job);
  const isRegistry = registry || registryInstall(job);
  const steps = kernelSteps(job, isRegistry);
  const failed = job.status === "failed";
  const cancelling = !!job.cancelling;
  const currentStep = steps[stage];
  const description = (isRegistry ? registryDescriptions : descriptions)[stage];
  const percent = stage === 1 ? kernelDownloadPercent(job) : null;
  const ActivityIcon = cancelling
    ? LoaderCircle
    : failed
      ? X
      : (isRegistry
          ? [Clock3, Download, Archive, ShieldCheck, Check]
          : [Clock3, Download, Clock3, Terminal, ShieldCheck])[stage];
  const activity = kernelLastActivity(job.log);
  return (
    <section
      className="kernel-installation"
      aria-label={t("Installation progress", "설치 진행 상황")}
    >
      <div className={`kernel-current${failed ? " failed" : ""}`}>
        <span className="kernel-current-icon">
          <ActivityIcon aria-hidden="true" />
        </span>
        <div>
          <p className="kernel-eyebrow">
            {cancelling
              ? t("Stopping installation", "설치 중단 중")
              : failed
                ? t("Installation stopped", "설치 중단")
                : t(
                    `Step ${stage + 1} of ${steps.length}`,
                    `${steps.length}단계 중 ${stage + 1}단계`,
                  )}
          </p>
          <h3 aria-live="polite">
            {cancelling
              ? t("Cancelling installation…", "설치 취소 중…")
              : failed
                ? t("Installation failed", "설치 실패")
                : t(currentStep[0], currentStep[1])}
          </h3>
        </div>
        <KernelElapsed job={job} />
      </div>
      <p className="kernel-stage-description">
        {cancelling
          ? t(
              isRegistry
                ? "Stopping the download or extraction and removing temporary files."
                : "Stopping the download or compiler and removing temporary files.",
              isRegistry
                ? "다운로드나 압축 해제를 중단하고 임시 파일을 정리한다."
                : "다운로드나 빌드를 중단하고 임시 파일을 정리한다.",
            )
          : failed
            ? t(
                `Stopped during ${currentStep[0].toLowerCase()}. Review the error below before retrying.`,
                `${currentStep[1]} 단계에서 중단됐다. 아래 오류를 확인한 뒤 다시 시도한다.`,
              )
            : t(description[0], description[1])}
      </p>
      {!isRegistry && !cancelling && stage === 2 && builderVersion && (
        <p className="kernel-builder-note">
          {t(
            `Linux ${builderVersion} is using the builder.`,
            `Linux ${builderVersion}이 빌드 중이다.`,
          )}
        </p>
      )}
      {!failed && !cancelling && (
        <>
          <div
            className={`kernel-progress${percent === null ? " indeterminate" : ""}`}
            role="progressbar"
            aria-label={t("Kernel installation", "커널 설치")}
            aria-valuemin={0}
            aria-valuemax={100}
            aria-valuenow={percent ?? undefined}
            aria-valuetext={
              percent === null
                ? t(currentStep[0], currentStep[1])
                : `${percent}%`
            }
          >
            <span
              style={percent === null ? undefined : { width: `${percent}%` }}
            />
          </div>
          <div className="kernel-progress-note">
            {stage === 1 && job.downloadedBytes !== undefined ? (
              <span>
                {kernelBytes(job.downloadedBytes)}
                {job.totalBytes
                  ? ` / ${kernelBytes(job.totalBytes)}`
                  : t(" downloaded", " 다운로드됨")}
              </span>
            ) : (
              <span>
                {!isRegistry && stage === 3
                  ? t(
                      "Build percentage is not reported by the compiler.",
                      "컴파일러는 전체 빌드 진행률을 제공하지 않는다.",
                    )
                  : t(
                      "Waiting for the next milestone…",
                      "다음 단계의 상태를 기다리는 중…",
                    )}
              </span>
            )}
            {percent !== null && <strong>{percent}%</strong>}
          </div>
        </>
      )}
      <ol
        className="kernel-timeline"
        aria-label={t("Installation steps", "설치 단계")}
      >
        {steps.map((step, index) => {
          const state =
            index < stage
              ? "complete"
              : index === stage
                ? failed
                  ? "failed"
                  : "current"
                : "pending";
          const Icon =
            state === "complete"
              ? Check
              : state === "failed"
                ? X
                : state === "current"
                  ? !isRegistry && stage === 2
                    ? Clock3
                    : LoaderCircle
                  : Circle;
          const stateLabel =
            state === "complete"
              ? t("Completed", "완료")
              : state === "failed"
                ? t("Failed", "실패")
                : state === "current"
                  ? cancelling
                    ? t("Stopping", "중단 중")
                    : !isRegistry && index === 2
                      ? t("Waiting", "대기 중")
                      : t("In progress", "진행 중")
                  : t("Pending", "진행 전");
          return (
            <li
              key={step[0]}
              data-state={state}
              aria-current={state === "current" ? "step" : undefined}
            >
              <span className="kernel-step-icon">
                <Icon aria-hidden="true" />
              </span>
              <span className="kernel-step-name">{t(step[0], step[1])}</span>
              <span className="kernel-step-state">{stateLabel}</span>
            </li>
          );
        })}
      </ol>
      {activity && (
        <div className={`kernel-latest${failed ? " failed" : ""}`}>
          <span>
            {failed ? t("Error", "오류") : t("Latest activity", "최근 작업")}
          </span>
          <p title={activity}>{activity}</p>
        </div>
      )}
    </section>
  );
}
