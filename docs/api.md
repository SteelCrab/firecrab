# API

`firecrab-api`는 MicroVM 관리를 위한 REST API 서버다 (Rust + axum).

## 실행

```sh
cd firecrab-api
cargo run
```

서버는 기본적으로 `0.0.0.0:3000`에서 대기한다.

## 현재 API

### 1) MicroVM 생성 — POST /api/vms

```sh
curl -X POST http://localhost:3000/api/vms \
  -H 'Content-Type: application/json' \
  -d '{"name":"test-vm","template":"ubuntu-rootfs-26.04","cpu":1,"ram":512}'
```

응답 예시 (201 Created):

```json
{
  "id": "<uuid>",
  "name": "test-vm",
  "state": "created",
  "template": "ubuntu-rootfs-26.04",
  "template_version": "ubuntu-26.04-v1",
  "template_kernel_sha256": "<sha256>",
  "template_rootfs_sha256": "<sha256>",
  "template_boot_args_sha256": "<sha256>",
  "cpu": 1.0,
  "ram": 512
}
```

### 2) MicroVM 목록 조회 — GET /api/vms

```sh
curl http://localhost:3000/api/vms
```

응답 예시 (200 OK):

```json
[
  {
    "id": "<uuid>",
    "name": "test-vm",
    "state": "created",
    "template": "ubuntu-rootfs-26.04",
    "template_version": "ubuntu-26.04-v1",
    "template_kernel_sha256": "<sha256>",
    "template_rootfs_sha256": "<sha256>",
    "template_boot_args_sha256": "<sha256>",
    "cpu": 1.0,
    "ram": 512
  }
]
```

### 3) 지원하지 않는 템플릿 — validation error

```sh
curl -X POST http://localhost:3000/api/vms \
  -H 'Content-Type: application/json' \
  -d '{"name":"bad-vm","template":"not-supported","cpu":1,"ram":512}'
```

응답 예시 (400 Bad Request):

```json
{
  "error": {
    "code": "validation_failed",
    "message": "request validation failed",
    "fields": {
      "template": "is not supported"
    },
    "request_id": "<uuid>"
  }
}
```

## 데이터 저장

VM 레코드는 생성/변경 시마다 `firecrab-api/data/vms.json`에 저장되며, 서버 재시작 시 이 파일에서 복원된다.


## 브라우저 테스트 페이지

`firecrab-frontend`로 분리되어 있다. [browser-test.md](browser-test.md) 참고.
