import type { PipeCardId } from "../navigation";

export const PIPE_GROUPS = [
  { label: ["Connections", "연결"], pages: ["auth", "ai"] },
  {
    label: ["Public addresses", "공개 주소"],
    pages: ["firecrab-tunnel", "quick-tunnel", "named-tunnel", "route"],
  },
  { label: ["Data", "데이터"], pages: ["storage", "registry", "database"] },
] as const;

export const PIPE_PAGES: readonly {
  id: PipeCardId;
  title: readonly [string, string];
  description: readonly [string, string];
}[] = [
  {
    id: "auth",
    title: ["Account", "계정"],
    description: [
      "AWS, GitHub and Firecrab accounts",
      "AWS, GitHub, Firecrab 계정",
    ],
  },
  {
    id: "ai",
    title: ["Pipe AI", "Pipe AI"],
    description: ["Connect and manage AI accounts", "AI 계정 연결 및 관리"],
  },
  {
    id: "firecrab-tunnel",
    title: ["Firecrab address", "Firecrab 주소"],
    description: ["Publish an application address", "앱 주소 공개"],
  },
  {
    id: "quick-tunnel",
    title: ["Quick tunnel", "빠른 터널"],
    description: ["Temporary public application URLs", "임시 주소로 앱 공개"],
  },
  {
    id: "named-tunnel",
    title: ["Cloudflare", "Cloudflare"],
    description: ["Connect a custom domain", "사용자 도메인 연결"],
  },
  {
    id: "route",
    title: ["Routing", "라우팅"],
    description: ["Route traffic to applications", "앱으로 트래픽 전달"],
  },
  {
    id: "storage",
    title: ["PipeStorage", "PipeStorage"],
    description: ["Access your files in AWS", "AWS에 저장된 파일 관리"],
  },
  {
    id: "registry",
    title: ["Registry", "레지스트리"],
    description: [
      "Connect and pull OCI images",
      "레지스트리 연결 및 OCI 이미지 가져오기",
    ],
  },
  {
    id: "database",
    title: ["Database", "데이터베이스"],
    description: [
      "Connect and browse tables",
      "데이터베이스 연결 및 테이블 조회",
    ],
  },
];
