import { Boxes, Check, Cpu, Shield } from "lucide-react";
import type { SecurityGroupResponse, ServiceInstance, VmResponse } from "../bindings";
import type { ServiceConnection } from "../lib/useSecurityGroupConnections";
import { useI18n } from "../i18n";
import StatusBadge from "./ui/StatusBadge";

interface ConnectionsProps {
  group: SecurityGroupResponse;
  services: ServiceConnection[];
  loaded: boolean;
  error: string | null;
  vms: VmResponse[];
  vmsAvailable: boolean;
  onRetry: () => void;
}

export default function SecurityGroupConnections({ group, services, loaded, error, vms, vmsAvailable, onRetry }: ConnectionsProps) {
  const { t } = useI18n();
  const vmsById = new Map(vmsAvailable ? vms.map((vm) => [vm.id, vm]) : []);
  // Saved configuration and running deployment snapshots can select different groups.
  const members = services.filter(({ service, deployment }) => service.securityGroupId === group.id
    || (deployment?.spec.securityGroupId === group.id && deployment.instances.length > 0));
  const replicaCount = members.reduce((count, { deployment }) => count
    + (deployment?.spec.securityGroupId === group.id ? deployment.instances.length : 0), 0);
  const incomplete = services.some((member) => member.error !== null);

  return <figure className="sg-connections" aria-label={t(`Connections for ${group.name}`, `${group.name} 연결 관계`)}>
    <figcaption>
      <span>{t("MicroService connections", "MicroService 연결")}</span>
      {loaded && !error && <span>{t(`${members.length} services`, `서비스 ${members.length}개`)}{!incomplete && <> · {t(`${replicaCount} replica VMs`, `복제 VM ${replicaCount}개`)}</>}</span>}
    </figcaption>
    {!loaded ? <p className="sg-connection-note">{t("Loading connections…", "연결 정보 불러오는 중…")}</p>
      : error ? <div className="sg-connection-note"><p role="alert">{t("Connections are unavailable.", "연결 정보를 불러오지 못했다.")}</p><button className="btn" type="button" onClick={onRetry}>{t("Retry connections", "연결 정보 다시 불러오기")}</button></div>
      : members.length === 0 ? <p className="sg-connection-note" role={incomplete ? "alert" : undefined}>{incomplete
        ? t("Some deployments could not be checked.", "일부 배포 정보를 확인하지 못했다.")
        : t("No MicroServices connected.", "연결된 MicroService가 없다.")}</p>
      : <div className="sg-connection-tree">
        <div className="sg-connection-node sg-connection-policy sg-connection-parent">
          <Shield aria-hidden="true" />
          <span><small>{t("Security group", "보안그룹")}</small><strong>{group.name}</strong></span>
        </div>
        <ul className="sg-connection-children sg-connection-services" aria-label="MicroServices">
          {members.map(({ service, deployment, error: deploymentError }) => {
            const instances = deployment?.spec.securityGroupId === group.id ? deployment.instances : [];
            const savedSelection = service.securityGroupId === group.id;
            return <li className="sg-connection-child sg-connection-service" key={service.id}>
              <div className={`sg-connection-node sg-connection-parent${savedSelection ? "" : " sg-connection-snapshot"}`}>
                <Boxes aria-hidden="true" />
                <span><small>MicroService</small><strong>{service.name}</strong><small>{savedSelection
                  ? t(`Configured: ${service.replicas} replicas`, `설정: 복제 ${service.replicas}개`)
                  : t("Previous deployment", "이전 배포")}</small></span>
              </div>
              {deploymentError ? <p className="sg-connection-note" role="alert">{t("VM status unavailable.", "VM 상태를 불러오지 못했다.")}</p>
                : instances.length === 0 ? <p className="sg-connection-note">{deployment && deployment.spec.securityGroupId !== group.id
                  ? t("Applies on the next deployment", "다음 배포에 적용")
                  : !deployment || deployment.phase === "stopped" || deployment.phase === "failed"
                    ? t("No deployed VMs", "배포된 VM 없음")
                    : t("Waiting for replica VMs", "복제 VM 생성 대기 중")}</p>
                  : <ul className="sg-connection-children sg-connection-replicas" aria-label={t(`Replica VMs for ${service.name}`, `${service.name} 복제 VM`)}>
                    {instances.map((instance) => <ReplicaNode key={instance.vmId} instance={instance} groupId={group.id}
                      vm={vmsById.get(instance.vmId)} />)}
                  </ul>}
            </li>;
          })}
        </ul>
      </div>}
  </figure>;
}

function ReplicaNode({ instance, groupId, vm }: { instance: ServiceInstance; groupId: string; vm?: VmResponse }) {
  const { t } = useI18n();
  const state = vm?.state ?? instance.state;
  const membership = vm?.securityGroupId === undefined ? "unknown" : vm.securityGroupId === groupId ? "inherited" : "mismatch";
  return <li className={`sg-connection-child sg-connection-replica ${membership}`}>
    <span className="sg-connection-circle"><Cpu aria-hidden="true" /></span>
    <span className="sg-connection-vm-label">
      <strong>{vm?.name ?? instance.vmId.slice(0, 8)}</strong>
      <small className="sg-connection-address">{vm?.ipv4 ?? instance.ipv4}</small>
      <span className="sg-connection-vm-status">
        <StatusBadge state={state} />
        <span className={`chip sg-connection-membership ${membership}`}>
          {membership === "inherited" && <Check aria-hidden="true" />}
          {membership === "inherited" ? t("Inherited", "상속됨")
            : membership === "mismatch" ? t("Group mismatch", "그룹 불일치") : t("Group unverified", "그룹 미확인")}
        </span>
      </span>
    </span>
  </li>;
}
