import { useCallback, useEffect, useState } from "react";
import type { MicroServiceResponse } from "../bindings";
import {
  ApiClientError,
  deleteMicroService,
  getMicroService,
  listMicroServices,
} from "../api/client";
import { Boxes, Rocket } from "lucide-react";
import { useI18n } from "../i18n";
import {
  readServiceDeployDraft,
  clearServiceDeployDraft,
} from "../lib/serviceDeployDraft";
import ServiceDeployment from "./ServiceDeployment";
import ServiceForm from "./ServiceForm";
import Screen from "./layout/Screen";
import PipeServiceMenu from "./PipeServiceMenu";
import { useScreenStatus } from "./layout/screenStatus";

export default function MicroServices({
  onUnsavedChange,
  creating = false,
}: {
  onUnsavedChange?: (dirty: boolean) => void;
  creating?: boolean;
}) {
  const { t } = useI18n();
  const { setDetailOpen } = useScreenStatus();
  const [services, setServices] = useState<MicroServiceResponse[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  // undefined closes the editor; null creates a new configuration.
  const [editor, setEditor] = useState<MicroServiceResponse | null | undefined>(
    undefined,
  );
  const [formDirty, setFormDirty] = useState(false);
  const formChange = useCallback(
    (dirty: boolean) => {
      setFormDirty(dirty);
      onUnsavedChange?.(dirty);
    },
    [onUnsavedChange],
  );
  const closeForm = () => {
    if (
      formDirty &&
      !window.confirm(
        t("Discard unsaved changes?", "저장하지 않은 변경을 버릴까요?"),
      )
    )
      return false;
    formChange(false);
    if (creating) window.location.hash = "#/services";
    else setEditor(undefined);
    return true;
  };
  const [startDeploy, setStartDeploy] = useState(false);
  const [runtime, setRuntime] = useState<MicroServiceResponse | null>(null);

  const refresh = async () => {
    try {
      setServices(await listMicroServices());
      setError(null);
    } catch (failure) {
      setError((failure as Error).message);
    }
  };

  useEffect(() => {
    let active = true;
    listMicroServices()
      .then((rows) => {
        if (active) setServices(rows);
      })
      .catch((failure) => {
        if (active) setError((failure as Error).message);
      });
    return () => {
      active = false;
    };
  }, []);

  useEffect(() => {
    const draft = readServiceDeployDraft();
    if (!draft) return;
    let active = true;
    getMicroService(draft.serviceId)
      .then((value) => {
        if (active) {
          setRuntime(value);
          setStartDeploy(true);
          setDetailOpen(true);
        }
      })
      .catch((cause) => {
        if (active) {
          setError(String(cause));
          if (cause instanceof ApiClientError && cause.status === 404)
            clearServiceDeployDraft();
        }
      });
    return () => {
      active = false;
    };
  }, [setDetailOpen]);
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
    if (
      busy ||
      !window.confirm(
        t(
          `Delete MicroService "${service.name}"?`,
          `MicroService "${service.name}" 설정을 삭제할까요?`,
        ),
      )
    )
      return;
    setBusy(true);
    try {
      await deleteMicroService(service.id);
      if (editor?.id === service.id) setEditor(undefined);
      if (runtime?.id === service.id) setRuntime(null);
      await refresh();
    } catch (failure) {
      setError((failure as Error).message);
    } finally {
      setBusy(false);
    }
  };

  const openRuntime = async (service: MicroServiceResponse, deploy = false) => {
    if (busy) return;
    setBusy(true);
    try {
      setRuntime(await getMicroService(service.id));
      setStartDeploy(deploy);
      setDetailOpen(true);
    } catch (cause) {
      setError(String(cause));
    } finally {
      setBusy(false);
    }
  };
  // The form wins over the runtime view; both close together after a save.
  const detail = creating ? undefined : editor !== undefined ? (
    <ServiceForm
      key={`editor-${editor?.id ?? "new"}`}
      service={editor}
      onCancel={closeForm}
      onUnsavedChange={formChange}
      onSaved={async (saved) => {
        formChange(false);
        setEditor(undefined);
        setRuntime(saved ?? null);
        setStartDeploy(!!saved);
        setDetailOpen(true);
        await refresh();
      }}
    />
  ) : runtime ? (
    <ServiceDeployment
      key={`runtime-${runtime.id}-${startDeploy}`}
      initialDeploy={startDeploy}
      onUnsavedChange={formChange}
      service={runtime}
      onEdit={() => void edit(runtime.id)}
      onDelete={() => void remove(runtime)}
    />
  ) : null;

  return (
    <Screen
      view="services"
      actions={
        <>
          {!creating && (
            <a className="btn primary" href="#/services/new">
              {t("New MicroService", "새 MicroService")}
            </a>
          )}
        </>
      }
      detail={
        detail && <div className="detail-body detail-embed">{detail}</div>
      }
      detailLabel={t("MicroService detail", "MicroService 상세")}
      detailKind={
        <>
          <Boxes aria-hidden="true" />
          MicroService
        </>
      }
      onCloseDetail={() => {
        if ((editor !== undefined || formDirty) && !closeForm()) return false;
        clearServiceDeployDraft();
        setRuntime(null);
        return true;
      }}
    >
      {creating ? (
        <section className="panel service-create-page">
          <ServiceForm
            service={null}
            onCancel={closeForm}
            onUnsavedChange={formChange}
            onSaved={async () => {
              formChange(false);
              await refresh();
              window.location.hash = "#/services";
            }}
          />
        </section>
      ) : (
        <>
          <section className="panel" aria-labelledby="microservice-heading">
            <h2 className="sr-only" id="microservice-heading">
              MicroService
            </h2>
            <p className="poll-note">
              {t(
                "Save a configuration, then deploy its image to MicroVMs and check app health.",
                "설정을 저장한 뒤 이미지를 MicroVM에 배포한다. 앱의 상태 검사 결과를 확인한다.",
              )}
            </p>
            <div className="service-actions">
              <button
                className="btn"
                disabled={busy}
                onClick={async () => {
                  setBusy(true);
                  await refresh();
                  setBusy(false);
                }}
              >
                {t("Refresh", "새로 고침")}
              </button>
            </div>
            {error && (
              <p className="field-error" role="alert">
                {error}
              </p>
            )}
            {services === null ? (
              !error && <p className="empty">{t("Loading…", "불러오는 중…")}</p>
            ) : services.length === 0 ? (
              <div className="empty-state">
                <div className="empty-state-icon">
                  <Boxes aria-hidden="true" />
                </div>
                <div className="empty-state-title">
                  {t("No MicroServices yet", "아직 MicroService가 없다")}
                </div>
                <p>
                  {t(
                    "Run a container image on MicroVMs and deploy without downtime.",
                    "컨테이너 이미지를 MicroVM에서 실행하고 무중단으로 배포한다.",
                  )}
                </p>
                <a className="btn primary" href="#/services/new">
                  {t("New MicroService", "새 MicroService")}
                </a>
              </div>
            ) : (
              <div className="table-scroll">
                <table className="vm-table service-table">
                  <thead>
                    <tr>
                      {[
                        t("Service", "서비스"),
                        t("Port", "포트"),
                        t("Replicas", "복제 수"),
                        t("Domain", "도메인"),
                        t("Actions", "작업"),
                      ].map((label) => (
                        <th scope="col" key={label}>
                          {label}
                        </th>
                      ))}
                    </tr>
                  </thead>
                  <tbody>
                    {services.map((service) => (
                      <tr
                        key={service.id}
                        onClick={() => {
                          if (editor === undefined && !formDirty)
                            void openRuntime(service);
                        }}
                      >
                        <td
                          className="name"
                          aria-label={`${service.name} ${service.image}`}
                        >
                          <button
                            className="link-button service-name"
                            title={service.name}
                            disabled={busy || editor !== undefined || formDirty}
                            aria-label={t(
                              `Open ${service.name}`,
                              `${service.name} 상세`,
                            )}
                            onClick={(event) => {
                              event.stopPropagation();
                              void openRuntime(service);
                            }}
                          >
                            {service.name}
                          </button>
                          <span
                            className="cell-sub service-image"
                            title={service.image}
                          >
                            {service.image.split("/").at(-1)}
                          </span>
                        </td>
                        <td>{service.port}</td>
                        <td>{service.replicas}</td>
                        <td
                          className="service-domain"
                          title={service.domain ?? undefined}
                        >
                          {service.domain || <span className="none">—</span>}
                        </td>
                        <td
                          className="actions"
                          onClick={(event) => event.stopPropagation()}
                        >
                          <button
                            className="icon-btn"
                            aria-label={t(
                              `Deploy ${service.name}`,
                              `${service.name} 배포`,
                            )}
                            disabled={busy || editor !== undefined || formDirty}
                            onClick={() => void openRuntime(service, true)}
                          >
                            <Rocket aria-hidden="true" />
                          </button>
                          <PipeServiceMenu
                            label={t(
                              `Actions for ${service.name}`,
                              `${service.name} 작업`,
                            )}
                            disabled={busy || editor !== undefined || formDirty}
                            items={[
                              {
                                label: t(
                                  `Edit ${service.name}`,
                                  `${service.name} 수정`,
                                ),
                                onSelect: () => void edit(service.id),
                              },
                              {
                                label: t(
                                  `Delete ${service.name}`,
                                  `${service.name} 삭제`,
                                ),
                                danger: true,
                                onSelect: () => void remove(service),
                              },
                            ]}
                          />
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            )}
          </section>
        </>
      )}
    </Screen>
  );
}
