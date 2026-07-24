# 웹 대시보드 사용법

`http://localhost:8080/`에서 VM 생성부터 콘솔 접속까지 전부 브라우저로 처리한다. 개발 서버 띄우는
방법과 아키텍처는 [browser-test.md](browser-test.md) 참고 — 이 문서는 실제 사용 흐름만 다룬다.

## 실행

```sh
# 터미널 1
cargo run -p firecrab-api

# 터미널 2
cd firecrab-frontend && npm run dev

# 터미널 3
./scripts/dev-net-helper.sh
```

`http://localhost:8080/` 접속(꼭 `localhost`, `127.0.0.1` 아님 — Origin 불일치로 403).

터미널 3(net-helper)이 없으면 bridge/TAP/DHCP를 다루는 host 권한 작업을 대신할 프로세스가 없어서, VM
시작 시 "network helper is unavailable" 오류로 즉시 실패한다. `dev-net-helper.sh`는 sudo로 root
권한을 얻되 group만 `pista`로 지정해 소켓(`/run/firecrab/net-helper.sock`)을 `root:pista`로 만들어
비특권 API 프로세스가 접근할 수 있게 한다(`sudo -g pista`만 쓰면 root가 아니라 호출한 사용자로
실행되어 바인드가 실패하니 주의).

## 1. VM 생성

상단 "VM 생성" 패널에서 이름·template·cpu·ram 입력 후 생성. 목록에 `created` 상태로 즉시 반영된다.

| 필드 | 제약 |
|---|---|
| name | 1–64자, 영문/숫자/`.`/`_`/`-` |
| template | `ubuntu-26.04`, `alpine-3.24` |
| cpu | 1–32 |
| ram | 128–32768 MiB, 2의 거듭제곱만(128/256/512/.../32768) |
| disk | 템플릿 rootfs 크기(현재 2GiB) 이상 500GiB 이하 |
| 네트워크 | 인터넷 허용(기본) / 격리(게이트웨이만 허용) |

값이 잘못되면 해당 입력 아래 빨간 글씨로 서버 검증 오류가 바로 표시된다.

## 2. 시작 및 진행 상황

목록에서 `start` 클릭 → 테이블 STATE 셀은 배지만 바뀐다(`starting`). 단계별 진행 상황을 보려면
**VM 이름을 클릭**해 상세 모달을 연다(§3).

## 3. VM 상세 모달

목록에서 **VM 이름 클릭**(상태와 무관하게 언제든 가능) → 모달로 template/cpu/ram/disk/네트워크/ip/mac/
hostname/id 상세 정보, 상단에 단계 스테퍼(`[✓]─[✓]─[ ]` 형태), 아래에 로그창이 뜬다. ip/mac은 실제
할당된 lease 값(아직 시작 전이라도 생성 시점에 IPAM이 미리 할당), hostname은 VM id로부터 결정론적으로
계산되는 값(`fc-<hash>` 형태)이라 항상 표시된다.

`created`/`stopped`/`error` 상태에서는 필드 옆에 **"수정"** 버튼이 뜬다 — 누르면 cpu/ram/disk/네트워크가
입력 필드로 바뀌고 저장하면 즉시 반영된다(다음 시작부터 적용, 떠 있는 프로세스를 실시간으로 바꾸는
게 아님). disk는 축소 불가 — 현재 값보다 작은 값은 검증 오류. `running`/`starting`/`stopping`
중에는 "수정" 버튼 자체가 안 보인다.

스테퍼는 `디스크 준비` → `설정 생성` → `프로세스 시작` 순서로 진행하며, `running`에 도달하면 3단계
모두 체크 표시로 바뀐다. 첫 시작은 디스크 준비 단계에서 몇 초간 머무는 게 정상이다.

로그창은 진행 단계 메시지 뒤에 **실제 guest 콘솔 부팅 로그**가 이어붙는다 — 부팅 완료 후 다시 열어도
캡처된 로그 전체가 즉시 보인다.

`running` 상태 VM에는 목록 액션에 `terminal` 버튼도 뜬다 — 클릭하면 실제 serial console(ttyS0)에
WebSocket으로 실시간 접속해 부팅 로그부터 셸까지 그대로 보이고 타이핑이 guest에 도달한다(REST
타임아웃과 무관하게 유지). 자세한 내용은 [tests/microvm-terminal.md](tests/microvm-terminal.md) 참고.

## 4. HOST 정보

헤더 우측 **"HOST 정보"** 버튼 → 읽기 전용 모달로 두 가지를 보여준다.

- 네트워크 구성: bridge 이름, subnet CIDR, gateway (조회만 가능, 편집 불가)
- host 상태: load average(1분), memory(total/available), disk(total/available), uptime — 실시간
  갱신되는 스냅샷

## 5. 상태 배지

| 배지 | 의미 | 가능한 다음 action |
|---|---|---|
| `created` | 생성됨, 아직 시작 안 함 | start, delete |
| `starting` | 시작 파이프라인 진행 중(이름 클릭 시 단계 확인) | 없음(진행 중 대기) |
| `running` | 부팅 완료, 콘솔 접속 가능 | stop |
| `stopping` | 종료 처리 중 | 없음(진행 중 대기) |
| `stopped` | 정상 종료됨 | start, delete |
| `error` | 비정상 종료(크래시 등) | start, delete |

## 6. 삭제

`stopped`/`error`/`created` 상태에서만 `delete` 가능. 클릭 시 확인 대화상자가 뜨고, 확인하면 VM
레코드와 디스크 파일이 함께 삭제된다(복구 불가).

## 문제가 생기면

증상별 대응은 [troubleshooting.md](troubleshooting.md) 참고(API 연결 안 됨, VM이 starting에서
멈춤, 터미널 garbage 출력 등). 기능별 상세 디버깅 절차는 `docs/tests/`, 개별 버그 원인·수정 기록
전체는 `docs/bugs/`에 있다.
