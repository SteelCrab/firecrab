import { useEffect, useMemo, useState } from "react";
import type { ImageResponse, MicroNetworkResponse } from "../bindings";
import {
  cancelBenchmarkJob,
  listBenchmarkJobs,
  listImages,
  listMicroNetworks,
  startBenchmarkJob,
} from "../api/client";
import type { BenchmarkJob, BenchmarkJobCommand, StartBenchmarkJobRequest } from "../benchmark";
import { useI18n } from "../i18n";

const COMMANDS = [
  { id: "boot", en: "Boot", ko: "부팅", detailEn: "Sequential boot latency", detailKo: "순차 부팅 지연 시간" },
  { id: "create", en: "Concurrent Create", ko: "동시 생성", detailEn: "Parallel VM creation", detailKo: "병렬 VM 생성 성능" },
  { id: "density", en: "Maximum Density", ko: "최대 밀도", detailEn: "Stable running VM limit", detailKo: "안정적 실행 VM 한계" },
  { id: "lifecycle", en: "Lifecycle Stress", ko: "Lifecycle 부하", detailEn: "Create/start/stop/delete", detailKo: "생성·시작·중지·삭제 반복" },
] as const;

interface Props { onResultPublished: () => void; }

/** Launch controls and live process state for host-local benchmark jobs. */
export default function BenchmarkControls({ onResultPublished }: Props) {
  const { t } = useI18n();
  const [command, setCommand] = useState<BenchmarkJobCommand>("boot");
  const [images, setImages] = useState<ImageResponse[]>([]);
  const [networks, setNetworks] = useState<MicroNetworkResponse[]>([]);
  const [template, setTemplate] = useState("");
  const [networkId, setNetworkId] = useState("");
  const [ram, setRam] = useState("512");
  const [cpu, setCpu] = useState("1");
  const [diskGb, setDiskGb] = useState("8");
  const [amount, setAmount] = useState("5");
  const [densityStep, setDensityStep] = useState("10");
  const [confirmDensity, setConfirmDensity] = useState(false);
  const [jobs, setJobs] = useState<BenchmarkJob[]>([]);
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    Promise.all([listImages(), listMicroNetworks()])
      .then(([nextImages, nextNetworks]) => {
        if (cancelled) return;
        const installed = nextImages.filter((image) => image.installed);
        setImages(installed);
        setNetworks(nextNetworks);
        setTemplate((current) => current || installed[0]?.alias || "");
        setNetworkId((current) => current || nextNetworks[0]?.id || "");
      })
      .catch((cause) => { if (!cancelled) setError(errorText(cause)); });
    return () => { cancelled = true; };
  }, []);

  useEffect(() => {
    let cancelled = false;
    let previousTerminal = "";
    let initialized = false;
    const refresh = () => listBenchmarkJobs().then((next) => {
      if (cancelled) return;
      setJobs(next);
      const terminal = next
        .filter((job) => ["succeeded", "failed", "cancelled"].includes(job.status))
        .map((job) => `${job.id}:${job.status}`).join(",");
      if (initialized && terminal !== previousTerminal) onResultPublished();
      previousTerminal = terminal;
      initialized = true;
    }).catch(() => undefined);
    refresh();
    const timer = window.setInterval(refresh, 2_000);
    return () => { cancelled = true; window.clearInterval(timer); };
  }, [onResultPublished]);

  const active = useMemo(
    () => jobs.find((job) => job.status === "running" || job.status === "cancelling"),
    [jobs],
  );
  const selected = COMMANDS.find((item) => item.id === command)!;

  const chooseCommand = (next: BenchmarkJobCommand) => {
    setCommand(next);
    setConfirmDensity(false);
    setAmount(next === "lifecycle" ? "100" : next === "density" ? "20" : next === "create" ? "10" : "5");
  };

  const start = async () => {
    setSubmitting(true);
    setError(null);
    try {
      const job = await startBenchmarkJob(buildRequest(command, {
        template, networkId, ram: Number(ram), cpu: Number(cpu), diskGb: Number(diskGb),
        amount: Number(amount), densityStep: Number(densityStep), confirmDensity,
      }));
      setJobs((current) => [job, ...current.filter((item) => item.id !== job.id)]);
    } catch (cause) {
      setError(errorText(cause));
    } finally {
      setSubmitting(false);
    }
  };

  const cancel = async (id: string) => {
    setError(null);
    try {
      const job = await cancelBenchmarkJob(id);
      setJobs((current) => current.map((item) => item.id === job.id ? job : item));
    } catch (cause) {
      setError(errorText(cause));
    }
  };

  const cannotStart = Boolean(active) || submitting || !template || !networkId;
  return (
    <section className="benchmark-section benchmark-controls" aria-labelledby="benchmark-control-title">
      <div className="benchmark-section-heading">
        <h3 id="benchmark-control-title">{t("Benchmark Control Center", "Benchmark 실행 도구")}</h3>
        <span className="poll-note">{t("One job at a time", "한 번에 한 작업")}</span>
      </div>
      <div className="benchmark-command-grid">
        {COMMANDS.map((item) => <button
          className={`benchmark-command-card ${command === item.id ? "selected" : ""}`}
          key={item.id} type="button" onClick={() => chooseCommand(item.id)}
        ><strong>{t(item.en, item.ko)}</strong><span>{t(item.detailEn, item.detailKo)}</span></button>)}
      </div>
      <div className="benchmark-run-form">
        <label>{t("Image", "이미지")}<select value={template} onChange={(event) => setTemplate(event.target.value)}>
          {images.map((image) => <option key={image.alias} value={image.alias}>{image.alias}</option>)}
        </select></label>
        <label>MicroNetwork<select value={networkId} onChange={(event) => setNetworkId(event.target.value)}>
          {networks.map((network) => <option key={network.id} value={network.id}>{network.name} · {network.subnetCidr}</option>)}
        </select></label>
        <NumberField label="RAM (MiB)" value={ram} setValue={setRam} min={128} max={8192} />
        <NumberField label="vCPU" value={cpu} setValue={setCpu} min={1} max={32} />
        <NumberField label="Disk (GiB)" value={diskGb} setValue={setDiskGb} min={1} max={256} />
        <NumberField label={amountLabel(command, t)} value={amount} setValue={setAmount} min={1} max={command === "lifecycle" ? 1000 : 100} />
        {command === "density" && <NumberField label="Step" value={densityStep} setValue={setDensityStep} min={1} max={100} />}
      </div>
      {command === "density" && <label className="benchmark-density-confirm"><input
        type="checkbox" checked={confirmDensity} onChange={(event) => setConfirmDensity(event.target.checked)}
      /><span>{t("I understand this can saturate host CPU and memory (maximum 100 VMs).", "Host CPU와 메모리가 포화될 수 있음을 확인합니다(최대 100 VM).")}</span></label>}
      {error && <div className="form-error" role="alert">{error}</div>}
      {!images.length && <div className="form-error">{t("Install an image before running a benchmark.", "Benchmark 실행 전 이미지를 설치해 주세요.")}</div>}
      {!networks.length && <div className="form-error">{t("Create a MicroNetwork before running a benchmark.", "Benchmark 실행 전 MicroNetwork를 생성해 주세요.")}</div>}
      <div className="benchmark-run-actions">
        <button className="btn primary" type="button" disabled={cannotStart || (command === "density" && !confirmDensity)} onClick={start}>
          {submitting ? t("Starting…", "시작 중…") : t(`Run ${selected.en}`, `${selected.ko} 실행`)}
        </button>
        {active && <button className="btn danger" type="button" disabled={active.status === "cancelling"} onClick={() => cancel(active.id)}>
          {active.status === "cancelling" ? t("Cancelling…", "취소 중…") : t("Cancel job", "작업 취소")}
        </button>}
      </div>
      {jobs.length > 0 && <JobTable jobs={jobs} />}
    </section>
  );
}

function NumberField({ label, value, setValue, min, max }: { label: string; value: string; setValue: (value: string) => void; min: number; max: number }) {
  return <label>{label}<input type="number" min={min} max={max} value={value} onChange={(event) => setValue(event.target.value)} /></label>;
}

function JobTable({ jobs }: { jobs: BenchmarkJob[] }) {
  const { t } = useI18n();
  return <div className="table-scroll benchmark-job-list"><table className="vm-table benchmark-table">
    <thead><tr><th>{t("Command", "명령")}</th><th>{t("Status", "상태")}</th><th>{t("Started", "시작")}</th><th>{t("Log", "로그")}</th></tr></thead>
    <tbody>{jobs.map((job) => <tr key={job.id}><td>{commandName(job.request.command)}</td>
      <td><span className={`state-badge ${job.status}`}>{job.status}</span></td>
      <td className="mono">{new Date(job.createdAtMs).toLocaleString()}</td>
      <td>{job.log ? <details><summary>{t("View", "보기")}</summary><pre className="benchmark-job-log">{job.log}</pre></details> : "—"}</td>
    </tr>)}</tbody>
  </table></div>;
}

interface FormValues { template: string; networkId: string; ram: number; cpu: number; diskGb: number; amount: number; densityStep: number; confirmDensity: boolean; }

function buildRequest(command: BenchmarkJobCommand, values: FormValues): StartBenchmarkJobRequest {
  const request: StartBenchmarkJobRequest = { command, template: values.template, microNetworkId: values.networkId, ram: values.ram, cpu: values.cpu, diskGb: values.diskGb };
  if (command === "boot") request.count = values.amount;
  if (command === "create") request.concurrency = values.amount;
  if (command === "density") { request.maxVms = values.amount; request.step = values.densityStep; request.confirmDensity = values.confirmDensity; }
  if (command === "lifecycle") request.iterations = values.amount;
  return request;
}

function amountLabel(command: BenchmarkJobCommand, t: (english: string, korean: string) => string): string {
  if (command === "boot") return t("Boot count", "부팅 횟수");
  if (command === "create") return t("Concurrency", "동시 생성 수");
  if (command === "density") return t("Maximum VMs", "최대 VM 수");
  return t("Iterations", "반복 횟수");
}

function commandName(command: BenchmarkJobCommand): string { return COMMANDS.find((item) => item.id === command)?.en ?? command; }
function errorText(error: unknown): string { return error instanceof Error ? error.message : String(error); }
