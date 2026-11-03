import { useI18n } from "../i18n";
import type { ViewId } from "../navigation";
import { VIEW_ICONS } from "../viewIcons";

export type StackInfo = {
  id?: string;
  name: string;
  bindings?: { kind: string; key: string; resourceId: string }[];
};
export type Ownership = {
  resource: { kind: string; id: string };
  owner: { manager?: string; id?: string; key: string };
  generation?: number;
};

const KINDS: Record<string, [ViewId, string, string]> = {
  kernel: ["kernels", "Kernels", "커널"],
  image: ["images", "Images", "이미지"],
  storage_root: ["storages", "Storage roots", "스토리지 루트"],
  volume: ["volumes", "Volumes", "볼륨"],
  volume_attachment: ["volumes", "Volume attachments", "볼륨 연결"],
  network: ["networks", "Networks", "네트워크"],
  security_group: ["security-groups", "Security groups", "보안그룹"],
  shell: ["shells", "Shells", "셸"],
  vm: ["vms", "MicroVMs", "MicroVMs"],
  pool: ["pools", "Pools", "풀"],
  schedule: ["schedules", "Schedules", "스케줄"],
  secret: ["secrets", "Secrets", "시크릿"],
  secret_connection: ["secrets", "Secret connections", "시크릿 연결"],
  auth: ["security-groups", "Auth", "인증"],
  registry: ["images", "Registries", "레지스트리"],
  service: ["services", "MicroServices", "MicroServices"],
  service_auto_deploy: ["services", "Auto deploy", "자동 배포"],
  service_otlp: ["services", "Telemetry", "관측 설정"],
  service_deploy_token: ["secrets", "Deploy tokens", "배포 토큰"],
  ingress: ["networks", "Ingress", "인그레스"],
  quick_tunnel: ["pipe", "Quick tunnels", "빠른 터널"],
  named_tunnel: ["pipe", "Named tunnels", "이름 있는 터널"],
  firecrab_tunnel: ["pipe", "Firecrab Tunnels", "Firecrab Tunnels"],
  database_connection: [
    "storages",
    "Database connections",
    "데이터베이스 연결",
  ],
  database_binding: ["storages", "Database bindings", "데이터베이스 바인딩"],
  database_events: ["storages", "Database events", "데이터베이스 이벤트"],
  database_rabbitmq: ["queues", "RabbitMQ", "RabbitMQ"],
  database_migration: [
    "storages",
    "Database migrations",
    "데이터베이스 마이그레이션",
  ],
  storage_connection: ["storages", "Storage connections", "스토리지 연결"],
  queue_engine: ["queues", "Queue engines", "큐 엔진"],
  queue: ["queues", "Queues", "큐"],
  ai_connection: ["secrets", "AI connections", "AI 연결"],
  ai_model: ["services", "AI models", "AI 모델"],
  sandbox_project: ["ai-sandbox", "Sandbox projects", "Sandbox 프로젝트"],
  sandbox_access: ["security-groups", "Sandbox access", "Sandbox 접근 권한"],
  sandbox_environment: ["pools", "Sandbox environments", "Sandbox 환경"],
  sandbox_tunnel: ["pipe", "Sandbox tunnels", "Sandbox 터널"],
  cluster_host: ["host", "Cluster hosts", "클러스터 호스트"],
  cluster_placement: ["cluster", "Cluster placements", "클러스터 배치"],
  cluster_service: ["cluster", "Cluster services", "클러스터 서비스"],
  stack_source: ["stacks", "Git sources", "Git 소스"],
  management_settings: ["host", "Management settings", "관리 설정"],
  host_configuration: ["host", "Host configuration", "호스트 설정"],
};

export function nativeRecords(
  stack: StackInfo,
  owners: Ownership[],
): Ownership[] {
  const records = owners.filter(
    (record) =>
      Boolean(stack.id) &&
      record.owner.manager === "stack" &&
      record.owner.id === stack.id,
  );
  for (const binding of stack.bindings ?? []) {
    // A later authoritative owner wins over an earlier Stack list snapshot.
    if (
      owners.some(
        (record) =>
          record.resource.kind === binding.kind &&
          record.resource.id === binding.resourceId,
      )
    )
      continue;
    records.push({
      resource: { kind: binding.kind, id: binding.resourceId },
      owner: { key: binding.key },
    });
  }
  return records;
}

export function terraformStates(owners: Ownership[]): [string, Ownership[]][] {
  const states = new Map<string, Ownership[]>();
  for (const record of owners) {
    if (record.owner.manager !== "terraform" || !record.owner.id) continue;
    const records = states.get(record.owner.id) ?? [];
    records.push(record);
    states.set(record.owner.id, records);
  }
  return [...states.entries()].sort(([a], [b]) => a.localeCompare(b));
}

export function resourceKind(kind: string): [ViewId, string, string] {
  return KINDS[kind] ?? ["stacks", kind, kind];
}

export function ResourceGroups({ records }: { records: Ownership[] }) {
  const { t } = useI18n();
  const groups = new Map<string, Ownership[]>();
  for (const record of records) {
    const items = groups.get(record.resource.kind) ?? [];
    items.push(record);
    groups.set(record.resource.kind, items);
  }
  if (!groups.size) return null;
  return (
    <div className="stack-resource-groups">
      {[...groups.entries()]
        .sort(([a], [b]) => a.localeCompare(b))
        .map(([kind, items]) => {
          const [view, en, ko] = resourceKind(kind);
          const Icon = VIEW_ICONS[view];
          return (
            <section
              className="stack-resource-group"
              data-kind={kind}
              key={kind}
            >
              <header className="stack-resource-heading">
                <span className="stack-resource-icon">
                  <Icon aria-hidden="true" />
                </span>
                <h3>{t(en, ko)}</h3>
                <span className="stack-resource-count">{items.length}</span>
              </header>
              {[...items]
                .sort((a, b) => a.owner.key.localeCompare(b.owner.key))
                .map((record) => (
                  <article
                    className="stack-resource"
                    data-resource-id={record.resource.id}
                    key={record.resource.id}
                  >
                    <h4 className="mono">{record.owner.key}</h4>
                    <dl className="stack-status stack-resource-properties">
                      <dt>{t("Resource ID", "리소스 ID")}</dt>
                      <dd className="mono">{record.resource.id}</dd>
                      {record.generation !== undefined && (
                        <>
                          <dt>{t("Ownership generation", "소유권 세대")}</dt>
                          <dd>{record.generation}</dd>
                        </>
                      )}
                    </dl>
                  </article>
                ))}
            </section>
          );
        })}
    </div>
  );
}
