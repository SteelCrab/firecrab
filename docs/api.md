# API

`firecrab-api`는 MicroVM 관리를 위한 REST API 서버

## 실행

```sh
cd firecrab-api
cargo run
```

`127.0.0.1:3000` 에서 HTTP 서버가 시작 (기본값은 loopback 바인딩, 인증/TLS 없이도 허용됨)

모든 요청(method/path/status/소요시간/request_id)과 VM lifecycle 이벤트가 stdout에 로그로 출력된다. 로그 레벨은 `RUST_LOG`로 조절 (기본 `firecrab_api=info`):

```sh
RUST_LOG=firecrab_api=debug cargo run
```

### 환경 변수

| 변수 | 기본값 | 설명 |
| --- | --- | --- |
| `FIRECRAB_BIND_ADDR` | `127.0.0.1:3000` | 서버 바인드 주소. loopback이 아닌 주소는 `FIRECRAB_AUTHENTICATION_ENABLED`와 `FIRECRAB_TLS_ENABLED`가 모두 켜져 있어야 허용됨 |
| `FIRECRAB_AUTHENTICATION_ENABLED` | (없음) | `1`/`true`/`yes`면 활성화 |
| `FIRECRAB_TLS_ENABLED` | (없음) | `1`/`true`/`yes`면 활성화 |
| `FIRECRAB_ENV` | (없음) | `production`이면 `FIRECRAB_ALLOWED_ORIGINS` 기본값이 빈 값(=CORS 전체 차단)이 됨 |
| `FIRECRAB_ALLOWED_ORIGINS` | `http://localhost:8080` (비-production) | 콤마로 구분된 허용 Origin 목록. CORS 및 `Origin` 헤더 검사에 사용 |
| `FIRECRAB_IMAGE_ROOT` | `../images` (crate 기준 상대경로) | 템플릿 커널/rootfs 이미지가 위치한 루트 디렉터리 |
| `FIRECRAB_FIRECRACKER_BIN` | `firecracker` (PATH 탐색) | VM 시작 시 실행할 Firecracker 바이너리 경로 |

### 네트워크 helper 상태

`firecrab-api`에는 `firecrab-net-helper`와 통신하는 `NetworkClient`가 준비되어 있지만, 현재 VM start/stop 흐름에는 아직 연결되어 있지 않다. 따라서 지금 API 실행에는 network helper가 필수 조건이 아니다.

- helper protocol과 수동 검증 절차: [net-helper.md](net-helper.md), [firecrab-smoke/docs/net-helper.md](firecrab-smoke/docs/net-helper.md)
- helper socket 환경 변수: `FIRECRAB_NET_HELPER_SOCK` (helper와 API 클라이언트가 공유할 경로, 기본값 `/run/firecrab/net-helper.sock`)
- 실제 bridge/TAP/firewall 자동화는 TAP/network task에서 start/stop 흐름에 연결 예정

## 요청/응답 공통 사항

- 요청 바디는 `application/json`만 허용하며 최대 64 KiB로 제한됨
- 모든 응답에는 `X-Request-Id` 헤더가 포함됨 (에러 응답의 `error.requestId`와 동일한 값)
- 동시 처리 가능한 요청 수는 128개로 제한되며, 초과 시 `429 Too Many Requests` 반환
- 요청 처리 시간이 10초를 초과하면 `504 Gateway Timeout` 반환
- 허용되지 않은 Origin에서의 요청은 `403 Forbidden` 반환

에러 응답 형식과 코드는 [api-error.md](api-error.md) 참고.

## 현재 API

### 1) MicroVM 생성 — POST /api/vms

```sh
curl -X POST http://localhost:3000/api/vms \
  -H 'Content-Type: application/json' \
  -d '{"name":"test-vm","template":"ubuntu-26.04","cpu":5,"ram":512}'
```

요청 필드 검증 규칙:

| 필드 | 규칙 |
| --- | --- |
| `name` | 1~64자, ASCII 영숫자로 시작, 영숫자/`.`/`_`/`-`만 허용 |
| `template` | 템플릿 레지스트리에 등록된 alias만 허용 (`ubuntu-26.04`) |
| `cpu` | 1~32 (정수) |
| `ram` | 128~32768 (MiB) |

응답 (201 Created):

```json
{
  "id": "<uuid>",
  "name": "test-vm",
  "state": "created",
  "template": "ubuntu-26.04",
  "templateVersion": "ubuntu-26.04-v1",
  "cpu": 5,
  "ram": 512
}
```

### 2) MicroVM 목록 조회 — GET /api/vms

```sh
curl http://localhost:3000/api/vms
```

모든 VM을 이름 오름차순(같은 이름은 id 순)으로 반환. pagination 없음.

응답 (200 OK):

```json
[
  {
    "id": "<uuid>",
    "name": "test-vm",
    "state": "created",
    "template": "ubuntu-26.04",
    "templateVersion": "ubuntu-26.04-v1",
    "cpu": 5,
    "ram": 512
  }
]
```

VM이 없으면 `[]` 반환.

### 3) MicroVM 상세 조회 — GET /api/vms/{id}

생성 응답의 `id`를 그대로 사용해 조회한다. 생성 → id 추출 → 조회 흐름 예시:

```sh
VM_ID=$(curl -s -X POST http://localhost:3000/api/vms \
  -H 'Content-Type: application/json' \
  -d '{"name":"test-vm","template":"ubuntu-26.04","cpu":5,"ram":512}' \
  | jq -r '.id')

curl http://localhost:3000/api/vms/$VM_ID
```

응답 (200 OK): 생성 응답과 동일한 형식

```json
{
  "id": "<uuid>",
  "name": "test-vm",
  "state": "created",
  "template": "ubuntu-26.04",
  "templateVersion": "ubuntu-26.04-v1",
  "cpu": 5,
  "ram": 512
}
```

- 없는 UUID: `404 not_found`
- UUID 형식이 아닌 id: `400 validation_failed` (`fields.id`)

```sh
# 없는 UUID
curl -i http://localhost:3000/api/vms/00000000-0000-0000-0000-000000000000

# UUID 형식이 아닌 id
curl -i http://localhost:3000/api/vms/not-a-uuid
```

### 4) MicroVM 시작 — POST /api/vms/{id}/start

동기 처리: rootfs 준비(템플릿 복사) → Firecracker config 생성 → 프로세스 spawn → API socket readiness 확인 → `running` 저장.

```sh
curl -X POST http://localhost:3000/api/vms/$VM_ID/start
```

- 허용 상태: `created`/`stopped`/`error` → 성공 시 200 + `VmResponse` (`state: "running"`)
- 그 외 상태: `409 invalid_state` (`fields.state`에 현재 상태)
- 시작 실패(스폰 실패, readiness timeout 등): 상태를 `error`로 저장하고 `500` 반환, 프로세스 잔여물 없음
- 재시작 시 기존 VM 디스크(`rootfs.ext4`)를 재사용해 데이터가 보존됨

### 5) MicroVM 중지 — POST /api/vms/{id}/stop

동기 처리: `stopping` 저장 → SIGTERM → 유예시간(5s) 초과 시 SIGKILL → `stopped` 저장.

```sh
curl -X POST http://localhost:3000/api/vms/$VM_ID/stop
```

- 허용 상태: `running` → 성공 시 200 + `VmResponse` (`state: "stopped"`)
- 그 외 상태: `409 invalid_state`

### 6) MicroVM 삭제 — DELETE /api/vms/{id}

Hard delete: `data/vms/{id}` 디렉터리(디스크 포함)와 레코드를 제거.

```sh
curl -i -X DELETE http://localhost:3000/api/vms/$VM_ID
```

- `starting`/`running`/`stopping` 상태면 `409 invalid_state` — 먼저 stop 필요
- 성공 시 `204 No Content`, 이후 조회는 404

### VM 상태 lifecycle

```
created ──start──▶ starting ──▶ running ──stop──▶ stopping ──▶ stopped ──start──▶ …
                      │            │                  │
                      ▼            ▼(내부 종료)        ▼
                    error        stopped/error      error
```

- Guest 내부 종료(poweroff 등)는 종료 감시가 자동 반영: 정상 종료 → `stopped`, 비정상 종료(crash, kill) → `error`
- 서버 재시작 시 이전 실행이 남긴 `starting`/`running`/`stopping` 레코드는 `stopped`로 정리됨 (유령 running 방지)
- 삭제는 `created`/`stopped`/`error`에서만 허용

## VM 디렉터리

VM별 런타임 파일은 `data/vms/{id}/` 아래에 생성된다.

| 파일 | 내용 |
| --- | --- |
| `rootfs.ext4` | 템플릿에서 복사된 VM 전용 writable 디스크 (stop 후에도 보존, delete 시 제거) |
| `firecracker.json` | boot-source/drives/machine-config가 담긴 Firecracker 설정 (start마다 재생성) |
| `firecracker.sock` | Firecracker API socket (프로세스 종료 시 제거) |
| `console.log` | VM 부팅/콘솔 출력 (start마다 새로 씀) |

## 템플릿 레지스트리

VM 생성 시 `template` alias는 `TemplateRegistry`(`firecrab-api/src/templates.rs`)를 통해 불변 버전으로 해석된다.

- 커널/rootfs 이미지는 `FIRECRAB_IMAGE_ROOT` 아래에서만 열리며, `openat2(RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS | ...)`로 경로 탈출·심볼릭 링크를 차단
- 레지스트리 로드 시 각 아티팩트의 SHA-256/디바이스/inode/크기를 기록해두고, VM 생성 시점에 재검증하여 파일이 변경되었으면 요청을 거부
- VM 레코드에는 해석된 `template_version`과 커널/rootfs/boot-args의 SHA-256 해시가 함께 저장됨
- `template_version`은 `<alias>-v<n>` 형식 (예: `ubuntu-26.04` → `ubuntu-26.04-v1`)

## 데이터 저장

VM 레코드는 실행 디렉터리 기준 `data/firecrab.db`(SQLite, WAL mode)의 `vms` 테이블에 저장되며, 서버 재시작 시 여기서 복원된다.

- 저장 실패 시 해당 VM은 메모리에도 반영되지 않고 `500 internal_error`를 반환
- 레거시 `data/vms.json`이 있으면 시작 시 1회 import 후 `vms.json.imported`로 이름을 바꿔 보관
- 시작 시 손상된 `vms.json`은 빈 목록으로 무시되지 않고 서버가 원인과 함께 시작 실패

## 브라우저 테스트 페이지

`firecrab-frontend`로 분리되어 있다. [browser-test.md](browser-test.md) 참고.
