# VM이 부팅 극초반에 간헐적으로 원인 불명 SIGKILL 당함

`docs/task-guest-network-configuration.md` 기능(콘솔로 네트워크 준비 신호 감지) 실사용 중
발견. 처음엔 단발성으로 재현돼 원인 불명으로 문서화해뒀다가(`docs/troubleshooting.md` 참고
이력), VM을 동시에 여러 대 띄우면 재현 빈도가 크게 올라간다는 걸 확인하고 다시 파고들었다.

## 증상

- `reason="network readiness check failed: console closed before network became ready"`로
  start가 실패
- `data/vms/<id>/console.log`가 커널 부팅 로그(`ACPI:`, `CPU topo:` 등) 중간에 뜬금없이 끊김 —
  항상 같은 지점은 아님
- `strace -f -p <firecracker_pid>`로 직접 잡아보면 프로세스가 외부에서 `SIGKILL` 당함
  (`+++ killed by SIGKILL +++`) — 패닉이나 세그폴트가 아님
- VM을 동시에 여러 대(10대 이상) 시작할수록 재현이 훨씬 잦아짐; 하나씩 시작하면 거의 안 보임

## 배제한 원인

- 커널 OOM·memory cgroup·`systemd-oomd`: `sudo dmesg -T`/`sudo journalctl -k`/
  `journalctl -u systemd-oomd`로 재현 시점 전후 확인 — 관련 로그 전혀 없음(재현 당시 메모리
  15GB, 디스크 40GB+ 여유)
- seccomp: 걸렸다면 강제종료가 `SIGSYS`로 보고되지 `SIGKILL`로 보이지 않음
- 시스템의 다른 프로세스: `bpftrace`로 `tracepoint:signal:signal_generate`(모든 SIGKILL 발신자)를
  전역 추적 — 발신자가 매번 **firecrab-api 자기 자신**(`tokio-rt-worker` 스레드)이었음
- `firecrab-api` 코드의 panic: `RUST_BACKTRACE=1`로 재기동해 stdout/stderr를 캡처한 뒤 재현 —
  panic 문자열 전혀 없음, `vm start failed reason=...` 로그가 정상적으로 찍힘(패닉으로 unwind
  됐다면 이 로그 줄 자체가 안 찍혔어야 함)

## 원인

`bpftrace`로 `tracepoint:syscalls:sys_enter_kill`에 유저스페이스 스택을 붙여 다시 잡으니 호출부가
`<tokio::process::imp::Child as Kill>::kill` — 즉 `kill_on_drop(true)`로 설정된
`FirecrackerProcess`(`firecrab-api/src/firecracker.rs`)가 예상치 못한 시점에 drop되며 자기 자식
Firecracker를 직접 죽이고 있었다.

`wait_for_network_ready`(`firecrab-api/src/handlers/vms.rs`)가 콘솔 브로드캐스트 채널의
`receiver.recv()` 에러를 `Err(_)`로 뭉뚱그려 전부 "console closed"로 취급하고 있었다. tokio의
`broadcast::Receiver`는 채널이 실제로 닫힐 때(`RecvError::Closed`)뿐 아니라, 컨슈머가 송신 속도를
못 따라가 버퍼(`ConsoleBroker`의 `BROADCAST_CAPACITY = 256`청크)가 넘칠 때도
(`RecvError::Lagged`) 에러를 반환한다. VM을 여러 대 동시에 부팅하면 이 대기 태스크가 CPU를
제때 못 받아 `Lagged`가 매우 흔하게 발생하는데(재현 시 15대 동시 시작에 수천 건), 기존 코드는
이를 진짜 채널 종료로 오판해 **아직 멀쩡히 부팅 중이던(그리고 몇 초 뒤
`FIRECRAB_NETWORK_READY`를 찍었을) VM을 실패로 간주**했다. `finish_run_start`가 이 가짜 실패를
반환하며 함수를 빠져나가고, 그 순간 지역변수 `process`(`kill_on_drop` 보유)가 drop되며 아직
살아있던 Firecracker를 직접 SIGKILL한 것.

## 수정

`RecvError::Lagged`와 `RecvError::Closed`를 구분해 `Lagged`는 치명적 실패로 취급하지 않고 계속
읽도록 수정(`wait_for_network_ready`, `firecrab-api/src/handlers/vms.rs`). `Closed`(진짜로
콘솔이 닫힌 경우)만 기존대로 실패 처리한다.

## 검증

- 회귀 테스트: `wait_for_network_ready_survives_a_lagged_broadcast_receiver` — 브로드캐스트
  용량(256)을 훨씬 넘는 300개 청크를 흘려보내 `Lagged`를 강제로 유발한 뒤, 마지막에 실제
  `FIRECRAB_NETWORK_READY`를 보내 `Ok(())`로 정상 종료하는지 확인
- 실제 재현: 수정 전 바이너리로 VM 15대 동시 생성·시작 → `console closed`로 여러 대 사망
  (`bpftrace` 로그에 `firecrab-api`의 SIGKILL 다건 기록). 수정 후 동일한 15대 동시 재현 →
  `console broadcast lagged` 경고 6000건+ 발생했지만 `console closed`로 죽은 VM **0건** — 남은
  실패는 전부 이미 알려진 별개 이슈(`FIRECRAB_NETWORK_FAILED no-ipv4-address`, DHCP 관련)뿐

## 산출물

`firecrab-api/src/handlers/vms.rs`(`wait_for_network_ready`의 `RecvError` 분기 처리 +
회귀 테스트)
