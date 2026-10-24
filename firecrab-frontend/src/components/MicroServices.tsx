import { useCallback, useEffect, useRef, useState } from "react";
import type { MicroServiceResponse } from "../bindings";
import {
  deleteMicroService, getMicroService, listMicroServices,
} from "../api/client";
import { ArrowRight, Box, CheckCircle2, Rocket } from "lucide-react";
import { useI18n } from "../i18n";
import { clearServiceDeploymentDraft, readServiceDeploymentDraft, type ServiceDeploymentDraft } from "../lib/serviceDeploymentResume";
import ServiceDeployment from "./ServiceDeployment";
import ServiceSetup from "./ServiceSetup";
import "../styles/services.css";
import Screen from "./layout/Screen";
import { useScreenStatus } from "./layout/screenStatus";

export default function MicroServices() {
  const { t } = useI18n();
  const { setDetailOpen } = useScreenStatus();
  const [services, setServices] = useState<MicroServiceResponse[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  // undefined closes the editor; null creates a new configuration.
  const [editor, setEditor] = useState<MicroServiceResponse | null | undefined>(undefined);
  const [runtime, setRuntime] = useState<{ service: MicroServiceResponse; draft?: ServiceDeploymentDraft } | null>(null);
  const [resumeDraft] = useState(readServiceDeploymentDraft);
  const resumed = useRef(false);

  const applyServices = useCallback((rows: MicroServiceResponse[]) => {
    setServices(rows);
    setError(null);
    if (!resumeDraft || resumed.current) return;
    resumed.current = true;
    clearServiceDeploymentDraft();
    const service = rows.find((row) => row.id === resumeDraft.serviceId);
    if (!service) {
      setError(t("The service to resume no longer exists. Select another service.", "이어서 배포할 서비스가 삭제되었다. 다른 서비스를 선택한다."));
      return;
    }
    setRuntime({ service, draft: resumeDraft });
    setDetailOpen(true);
  }, [resumeDraft, setDetailOpen, t]);

  const refresh = async () => {
    try {
      applyServices(await listMicroServices());
    } catch (failure) {
      setError((failure as Error).message);
    }
  };

  useEffect(() => {
    let active = true;
    listMicroServices().then((rows) => { if (active) applyServices(rows); })
      .catch((failure) => { if (active) setError((failure as Error).message); });
    return () => { active = false; };
  }, [applyServices]);

  const edit = async (id: string) => {
    if (busy) return;
    setBusy(true);
    try {
      setEditor(await getMicroService(id));
      setDetailOpen(true);
    } catch (failure) {
      setError((failure as Error).message);
    } finally {
      setBusy(false);
    }
  };

  const remove = async (service: MicroServiceResponse) => {
    if (busy || !window.confirm(t(`Delete MicroService "${service.name}"?`, `MicroService "${service.name}" 설정을 삭제할까요?`))) return;
    setBusy(true);
    try {
      await deleteMicroService(service.id);
      if (editor?.id === service.id) setEditor(undefined);
      if (runtime?.service.id === service.id) setRuntime(null);
      await refresh();
    } catch (failure) {
      setError((failure as Error).message);
    } finally {
      setBusy(false);
    }
  };

  const openNew = () => {
    setRuntime(null);
    setEditor(null);
    setDetailOpen(true);
  };
  const openRuntime = (service: MicroServiceResponse) => {
    setRuntime({ service });
    setDetailOpen(true);
  };
  const saved = async (service: MicroServiceResponse) => {
    setEditor(undefined);
    setRuntime({ service });
    setDetailOpen(true);
    await refresh();
  };

  if (editor !== undefined) {
    return <Screen view="services"><ServiceSetup key={editor?.id ?? "new"} service={editor} onCancel={() => setEditor(undefined)} onSaved={saved} /></Screen>;
  }

  return (
    <Screen
      view="services"
      detail={runtime ? <div className="detail-body detail-embed"><ServiceDeployment key={runtime.service.id} service={runtime.service} draft={runtime.draft} onClose={() => setRuntime(null)} onEdit={() => edit(runtime.service.id)} /></div> : undefined}
      detailLabel={t("MicroService detail", "MicroService 상세")}
      detailKind={<><Rocket aria-hidden="true" />MicroService</>}
      onCloseDetail={() => {
        setEditor(undefined);
        setRuntime(null);
      }}
    >
      <section className="panel service-home" aria-labelledby="microservice-heading">
        <div className="service-intro">
          <span className="service-eyebrow">MicroService</span>
          <h2 id="microservice-heading">{t("From image to running app", "이미지 하나로 앱을 실행한다")}</h2>
          <p>{t("Choose your app and follow the steps. Firecrab creates the MicroVMs and checks that your app responds.", "앱을 선택하고 단계에 따라 설정한다. Firecrab이 MicroVM을 만들고 앱의 응답을 확인한다.")}</p>
        </div>
        <ol className="service-journey" aria-label={t("How it works", "앱 실행 순서")}>
          <li><Box aria-hidden="true" /><div><strong>{t("1. Configure your app", "1. 앱 설정")}</strong><span>{t("Choose from an AUTH-connected account.", "AUTH에 연결된 계정에서 앱을 선택한다.")}</span></div></li>
          <li><Rocket aria-hidden="true" /><div><strong>{t("2. Deploy", "2. 배포")}</strong><span>{t("Keep the defaults and start your app.", "기본값을 확인하고 앱을 실행한다.")}</span></div></li>
          <li><CheckCircle2 aria-hidden="true" /><div><strong>{t("3. Open your app", "3. 앱 접속")}</strong><span>{t("Check its health and open its address.", "정상 응답을 확인하고 접속한다.")}</span></div></li>
        </ol>
        <div className="service-actions">
          <button className="btn primary" disabled={busy || editor !== undefined} onClick={openNew}><Rocket aria-hidden="true" />{t("New MicroService", "MicroService 추가")}<ArrowRight aria-hidden="true" /></button>
          <button className="btn" disabled={busy} onClick={async () => { setBusy(true); await refresh(); setBusy(false); }}>{t("Refresh", "새로 고침")}</button>
        </div>
        {error && <p className="field-error" role="alert">{error}</p>}
        {services === null ? (
          !error && <p className="empty">{t("Loading…", "불러오는 중…")}</p>
        ) : services.length === 0 ? (
          <div className="service-empty"><Box aria-hidden="true" /><h3>{t("Your first app starts here", "첫 번째 앱을 시작한다")}</h3><p>{t("No service configurations yet.", "저장된 서비스 설정이 없다.")}</p><p>{t("Connect GitHub or AWS in Pipe Auth, then select New MicroService above.", "Pipe Auth에서 GitHub 또는 AWS 계정을 연결한다. 위의 MicroService 추가를 선택해 앱을 고른다.")}</p></div>
        ) : (
          <div className="table-scroll">
            <table className="vm-table service-table">
              <thead><tr>{[t("Name", "이름"), t("Image", "이미지"), t("Port", "포트"), t("Replicas", "복제 수"), "RAM / CPU", t("Domain", "도메인"), t("Actions", "작업")].map((label, index) => <th key={label} id={`service-column-${index}`} scope="col" className={index >= 3 && index <= 5 ? "col-detail" : undefined}>{label}</th>)}</tr></thead>
              <tbody>{services.map((service) => (
                <tr key={service.id}>
                  <td headers="service-column-0" className="name" data-label={t("Name", "이름")}>{service.name}</td><td headers="service-column-1" className="mono service-image" data-label={t("Image", "이미지")}>{service.image}</td>
                  <td headers="service-column-2" data-label={t("Port", "포트")}>{service.port}</td><td headers="service-column-3" className="col-detail" data-label={t("Replicas", "복제 수")}>{service.replicas}</td><td headers="service-column-4" className="col-detail" data-label="RAM / CPU">{service.ram} MiB / {service.cpu}</td>
                  <td headers="service-column-5" className="mono col-detail" data-label={t("Domain", "도메인")}>{service.domain ?? "—"}</td>
                  <td headers="service-column-6" className="actions">
                    <button className="btn primary" disabled={busy || editor !== undefined} onClick={() => openRuntime(service)} aria-label={t(`Run ${service.name}`, `${service.name} 실행 관리`)}>{t("Run", "실행 관리")}</button>
                    <button className="btn" disabled={busy || editor !== undefined} onClick={() => edit(service.id)} aria-label={t(`Edit ${service.name}`, `${service.name} 수정`)}>{t("Edit", "수정")}</button>
                    <button className="btn danger" disabled={busy || editor !== undefined} onClick={() => remove(service)} aria-label={t(`Delete ${service.name}`, `${service.name} 삭제`)}>{t("Delete", "삭제")}</button>
                  </td>
                </tr>
              ))}</tbody>
            </table>
          </div>
        )}
      </section>
    </Screen>
  );
}
