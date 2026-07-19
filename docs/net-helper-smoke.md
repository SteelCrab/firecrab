# firecrab-net-helper smoke test

이 문서는 `firecrab-net-helper`가 Unix socket으로 요청을 받고, protocol version과 응답 frame을 정상 처리하는지 수동으로 확인하는 절차다.

현재 bridge/TAP/firewall 실제 작업은 아직 구현 전이다. 따라서 정상 요청의 기대 응답은 성공이 아니라 `unsupported_operation`이다.

## 확인하는 것

이 smoke test는 다음을 확인한다.

- helper가 지정한 Unix socket에 bind하는가
- Python 클라이언트가 helper에 연결할 수 있는가
- length-prefixed JSON frame을 주고받을 수 있는가
- 정상 version 요청에 `unsupported_operation`을 반환하는가
- 잘못된 version 요청에 `unsupported_version`을 반환한 뒤 연결을 닫는가

## 준비

repo 루트에서 helper를 실행해야 한다.

문서 디렉터리에 있다면 repo 루트로 이동한다.

```sh
cd ..
```

helper를 `/tmp/firecrab-net.sock` 경로로 실행한다.

```sh
FIRECRAB_NET_HELPER_SOCK=/tmp/firecrab-net.sock cargo run -p firecrab-net-helper
```

정상 실행되면 다음과 비슷한 로그가 나온다.

```text
[INFO] net-helper listening on /tmp/firecrab-net.sock
```

이 터미널은 helper가 계속 떠 있어야 하므로 닫지 않는다.

## 실행

다른 터미널에서 문서 디렉터리로 이동한다.

```sh
cd docs
```

기본 경로(`/tmp/firecrab-net.sock`)로 smoke test를 실행한다.

```sh
python3 net-helper.py
```

다른 socket path를 사용하려면 인자로 넘긴다.

```sh
python3 net-helper.py /path/to/net-helper.sock
```

또는 환경 변수로 지정한다.

```sh
FIRECRAB_NET_HELPER_SOCK=/tmp/firecrab-net.sock python3 net-helper.py
```

## 기대 결과

```text
정상 요청  : {'version': 1, 'request_id': '...', 'result': {'Err': {'code': 'unsupported_operation'}}}
버전 불일치: {'version': 1, 'request_id': '...', 'result': {'Err': {'code': 'unsupported_version', 'supported': 1}}}
이후 종료  : True
```

출력 의미:

- `정상 요청`: 요청 frame과 protocol version은 정상이다. 실제 작업이 아직 없어 `unsupported_operation`이 반환된다.
- `버전 불일치`: helper가 지원하지 않는 protocol version을 거부한다.
- `이후 종료`: version mismatch 응답 이후 helper가 연결을 닫았다.

`request_id` 값은 실행할 때마다 달라진다. 정상 동작에서는 첫 번째 응답과 두 번째 응답의 `request_id`가 요청과 동일하게 echo된다.

## 실패 시 확인

`소켓 연결 실패`가 나오면 아래를 확인한다.

- helper를 먼저 실행했는가
- helper 로그에 `listening on /tmp/firecrab-net.sock`이 출력됐는가
- `net-helper.py`가 helper와 같은 socket path를 사용하고 있는가
- `/tmp/firecrab-net.sock` 파일이 존재하는가
- helper와 Python 스크립트를 같은 사용자로 실행했는가

socket 파일 확인:

```sh
ls -l /tmp/firecrab-net.sock
```

helper 실행 여부 확인:

```sh
lsof -nP /tmp/firecrab-net.sock
```

## 종료

테스트가 끝나면 helper를 실행한 터미널에서 `Ctrl-C`로 종료한다.

helper가 정상 종료되면 socket 파일은 제거된다.

## 주의

이 smoke test는 network 기능 자체를 검증하지 않는다.

검증 범위는 helper process, Unix socket 연결, request/response framing, version handling까지다. 실제 bridge/TAP/firewall 동작은 각 기능이 구현된 뒤 별도 통합 테스트에서 확인한다.
