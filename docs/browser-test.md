# 브라우저 테스트 페이지

`firecrab-frontend`는 `firecrab-api`와 분리된 바닐라 HTML/JS 페이지다 (프레임워크, 빌드 도구 없음). VM 이름을 입력해 실제로 `POST /api/vms`를 호출하는 launch 콘솔이다.

`firecrab-api`와 별도 origin에서 열리므로 API 서버는 CORS를 허용한다 (`firecrab-api/src/main.rs`의 `CorsLayer`).

## 사용

API 서버를 먼저 띄운다.

```sh
cd firecrab-api
cargo run
```

프론트엔드는 정적 파일이므로 브라우저에서 직접 열거나 아무 정적 서버로 서빙한다.

```sh
cd firecrab-frontend
python3 -m http.server 8080
open http://localhost:8080/
```

`vm name`을 입력하고 `launch microvm`을 누르면 콘솔 패널이 열리며 실제 API 응답(`id`, `name`, `state`)과 요청 왕복 시간(`ready in Nms`)을 보여준다. `template`/`vcpu`/`memory`는 현재 고정값이며 이후 주차에서 설정 가능해질 예정이다.

## 파일

- `firecrab-frontend/index.html`: 페이지 전체 (HTML + inline JS), API 주소는 `API_BASE` 상수로 고정 (`http://localhost:3000`)
- `firecrab-api/src/main.rs`: `CorsLayer`로 cross-origin 요청 허용
