# API

`firecrab-api`는 MicroVM 관리를 위한 REST API 서버

## 실행

```sh
cd firecrab-api
cargo run
```

`127.0.0.1:3000` 에서 HTTP 서버가 시작 (기본값은 loopback 바인딩, 인증/TLS 없이도 허용됨)

### 환경 변수

| 변수 | 기본값 | 설명 |
| --- | --- | --- |
| `FIRECRAB_BIND_ADDR` | `127.0.0.1:3000` | 서버 바인드 주소. loopback이 아닌 주소는 `FIRECRAB_AUTHENTICATION_ENABLED`와 `FIRECRAB_TLS_ENABLED`가 모두 켜져 있어야 허용됨 |
| `FIRECRAB_AUTHENTICATION_ENABLED` | (없음) | `1`/`true`/`yes`면 활성화 |
| `FIRECRAB_TLS_ENABLED` | (없음) | `1`/`true`/`yes`면 활성화 |
| `FIRECRAB_ENV` | (없음) | `production`이면 `FIRECRAB_ALLOWED_ORIGINS` 기본값이 빈 값(=CORS 전체 차단)이 됨 |
| `FIRECRAB_ALLOWED_ORIGINS` | `http://localhost:8080` (비-production) | 콤마로 구분된 허용 Origin 목록. CORS 및 `Origin` 헤더 검사에 사용 |
| `FIRECRAB_IMAGE_ROOT` | `../images` (crate 기준 상대경로) | 템플릿 커널/rootfs 이미지가 위치한 루트 디렉터리 |

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
  -d '{"name":"test-vm","template":"ubuntu-rootfs-26.04","cpu":5,"ram":512}'
```

요청 필드 검증 규칙:

| 필드 | 규칙 |
| --- | --- |
| `name` | 1~64자, ASCII 영숫자로 시작, 영숫자/`.`/`_`/`-`만 허용 |
| `template` | 템플릿 레지스트리에 등록된 alias만 허용 (`ubuntu-rootfs-26.04`, `ubuntu-26.04`) |
| `cpu` | 1~32 (정수) |
| `ram` | 128~32768 (MiB) |

응답 (201 Created):

```json
{
  "id": "<uuid>",
  "name": "test-vm",
  "state": "created",
  "template": "ubuntu-rootfs-26.04",
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
    "template": "ubuntu-rootfs-26.04",
    "templateVersion": "ubuntu-26.04-v1",
    "cpu": 5,
    "ram": 512
  }
]
```

VM이 없으면 `[]` 반환.

## 템플릿 레지스트리

VM 생성 시 `template` alias는 `TemplateRegistry`(`firecrab-api/src/templates.rs`)를 통해 불변 버전으로 해석된다.

- 커널/rootfs 이미지는 `FIRECRAB_IMAGE_ROOT` 아래에서만 열리며, `openat2(RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS | ...)`로 경로 탈출·심볼릭 링크를 차단
- 레지스트리 로드 시 각 아티팩트의 SHA-256/디바이스/inode/크기를 기록해두고, VM 생성 시점에 재검증하여 파일이 변경되었으면 요청을 거부
- VM 레코드에는 해석된 `template_version`과 커널/rootfs/boot-args의 SHA-256 해시가 함께 저장됨
- 여러 alias(`ubuntu-rootfs-26.04`, `ubuntu-26.04`)가 같은 불변 버전(`ubuntu-26.04-v1`)을 가리킬 수 있음

## 데이터 저장

VM 레코드는 생성/변경 시마다 실행 디렉터리 기준 `data/vms.json`에 저장되며, 서버 재시작 시 이 파일에서 복원된다.

- 저장 실패 시 해당 VM은 메모리에도 반영되지 않고 `500 internal_error`를 반환
- 시작 시 손상된 `vms.json`은 빈 목록으로 무시되지 않고 서버가 원인과 함께 시작 실패

## 브라우저 테스트 페이지

`firecrab-frontend`로 분리되어 있다. [browser-test.md](browser-test.md) 참고.
