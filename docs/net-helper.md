# firecrab-net-helper

`firecrab-net-helper`는 bridge, TAP, firewall처럼 host 네트워크 권한이 필요한 작업을 대신 수행하는 작은 helper 데몬이다.

API 서버가 직접 `CAP_NET_ADMIN` 권한을 가지지 않도록 분리하는 것이 목적이다. API 서버는 비특권 프로세스로 실행되고, 네트워크 변경은 정해진 protocol을 통해 helper에게만 요청한다.

```text
firecrab-api
  비특권 프로세스
        |
        | Unix domain socket
        | length-prefixed JSON request
        v
firecrab-net-helper
  CAP_NET_ADMIN 보유
        |
        +-- bridge / TAP 설정
        +-- nftables firewall 설정
```

## 왜 분리하나

네트워크 설정에는 높은 권한이 필요하다. 이 권한을 API 서버 전체에 주면 API 서버의 버그나 취약점이 host 네트워크 권한으로 이어질 수 있다.

helper 방식에서는 API 서버가 할 수 있는 일을 protocol에 정의된 작업으로 제한한다. 예를 들어 API 서버가 임의 shell command, 임의 interface 이름, 임의 nftables rule을 helper에게 전달할 수 없다.

## 현재 상태

현재 helper는 protocol 경계와 권한 확인까지만 구현되어 있다.

- 소켓 연결 수락
- peer UID 확인
- length-prefixed JSON frame 읽기/쓰기
- protocol version 확인
- 허용된 `NetworkRequest`만 파싱
- 아직 실제 bridge/TAP/firewall 작업은 미구현

그래서 정상 요청을 보내도 현재 응답은 `unsupported_operation`이다. 이것은 지금 단계에서는 정상 동작이다.

## Protocol

요청은 `firecrab-helper-protocol` crate의 `NetworkRequest` enum으로 제한된다.

허용되는 작업:

- `ensure_bridge`
- `ensure_firewall`
- `create_tap`
- `delete_tap`
- `apply_vm_policy`
- `remove_vm_policy`

frame 형식:

```text
u32 big-endian length + JSON payload
```

주요 규칙:

- 최대 frame 크기는 64 KiB
- malformed JSON은 응답 없이 연결 종료
- protocol version이 다르면 `unsupported_version` 응답 후 연결 종료
- 인증되지 않은 UID는 응답 없이 연결 종료
- 동시 연결은 최대 16개
- 요청 timeout은 10초

## 환경 변수

| 변수 | 기본값 | 설명 |
| --- | --- | --- |
| `FIRECRAB_NET_HELPER_SOCK` | `/run/firecrab/net-helper.sock` | helper Unix socket 경로 |
| `FIRECRAB_NET_HELPER_ALLOWED_UID` | 없음 | 추가로 허용할 API 서버 UID |

## 개발 실행

개발 중에는 `/tmp/firecrab-net.sock`을 사용하면 root 권한 없이 protocol 왕복을 확인하기 쉽다.

```sh
FIRECRAB_NET_HELPER_SOCK=/tmp/firecrab-net.sock cargo run -p firecrab-net-helper
```

정상 실행되면 다음과 비슷하게 출력된다.

```text
[INFO] net-helper listening on /tmp/firecrab-net.sock
```

## 스모크 테스트

수동 검증 절차는 [firecrab-smoke/net-helper.md](firecrab-smoke/net-helper.md)에 따로 정리한다.

## 테스트 클라이언트

`firecrab-smoke/net-helper.py`는 수동 테스트용 클라이언트다.

하는 일:

- Unix socket에 연결
- JSON 요청을 length-prefixed frame으로 인코딩
- helper 응답 frame을 읽고 JSON으로 출력
- 정상 요청과 version mismatch 요청을 차례로 전송

이 스크립트는 운영 코드가 아니라 protocol 확인용 도구다.

## systemd 배포 예시

```ini
# /etc/systemd/system/firecrab-net-helper.service
[Unit]
Description=Firecrab privileged network helper
After=network.target

[Service]
ExecStart=/usr/local/bin/firecrab-net-helper
Environment=FIRECRAB_NET_HELPER_ALLOWED_UID=<firecrab-api service UID>
User=firecrab-net
Group=firecrab
AmbientCapabilities=CAP_NET_ADMIN
CapabilityBoundingSet=CAP_NET_ADMIN
NoNewPrivileges=yes
ProtectSystem=strict
ProtectHome=yes
PrivateTmp=yes
RuntimeDirectory=firecrab
RuntimeDirectoryMode=0750
Restart=on-failure

[Install]
WantedBy=multi-user.target
```

배포 시 주의할 점:

- helper 실행 파일은 일반 사용자가 수정할 수 없어야 한다.
- socket은 helper가 `0660` 권한으로 생성한다.
- API 서비스 계정을 `firecrab` 그룹에 넣어 socket 접근을 허용한다.
- 최종 인증은 group 권한이 아니라 `SO_PEERCRED` UID 검사로 한다.
- API와 helper는 같은 protocol version으로 함께 배포해야 한다.

## API 연동

API 쪽 클라이언트는 `firecrab-api/src/network.rs`의 `NetworkClient`다.

현재 동작:

- 호출마다 helper socket에 연결
- request마다 UUID `request_id` 생성
- 응답의 `request_id`가 요청과 같은지 확인
- timeout은 5초

VM start/stop 흐름에서 실제 TAP 생성과 firewall 정책 적용은 이후 네트워크 자동화 task에서 연결한다.
