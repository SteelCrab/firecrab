# api

`firecrab-api`는 MicroVM 관리를 위한 REST API 서버다 (Rust + axum).

## 실행

```sh
cd firecrab-api
cargo run
```

`0.0.0.0:3000`에서 대기한다.

## 테스트

### MicroVM 생성 — POST /api/vms

```sh
curl -X POST http://localhost:3000/api/vms \
  -H 'Content-Type: application/json' \
  -d '{"name":"test-vm","template":"ubuntu-rootfs-26.04","cpu":0.5,"ram":512}'
```

응답 (201 Created):

```json
{"id":"<uuid>","name":"test-vm","state":"Created","template":"ubuntu-rootfs-26.04","cpu":0.5,"ram":512}
```

## 데이터 저장

VM 레코드는 생성/변경 시마다 `firecrab-api/data/vms.json`에 저장되며, 서버 재시작 시 이 파일에서 복원된다.

### 브라우저 테스트 페이지

`firecrab-frontend`로 분리되어 있다. [browser-test.md](browser-test.md) 참고.
