# net-helper smoke

framing/version 확인. 권한 불필요.

## 실행

```sh
FIRECRAB_NET_HELPER_SOCK=/tmp/firecrab-net.sock cargo run -p firecrab-net-helper
```

## 테스트

```sh
python3 docs/firecrab-smoke/net-helper.py /tmp/firecrab-net.sock
```

기본 operation `ensure_firewall` (미구현 → 결정적 `unsupported_operation`).

## 기대 결과

```text
정상 요청  : ... 'result': {'Err': {'code': 'unsupported_operation'}}
버전 불일치: ... 'result': {'Err': {'code': 'unsupported_version', 'supported': 1}}
이후 종료  : True
```

## 확인 항목

- socket bind
- length-prefixed JSON frame 왕복
- 정상 version → `unsupported_operation`
- 잘못된 version → `unsupported_version` + 연결 종료

## 실패 시

- helper 실행 여부·로그 (`listening on ...`)
- socket path 일치 (`ls -l`, `lsof -nP <path>`)
- 같은 사용자로 실행했는가

실제 bridge 동작 검증: [bridge.md](bridge.md)
