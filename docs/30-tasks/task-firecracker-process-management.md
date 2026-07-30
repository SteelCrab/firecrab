---
tags:
  - firecrab
  - firecracker
status: 미완료
scope: 4주차
updated: 2026-07-23
---

# Firecracker 실행 프로세스 관리 구현

## 브랜치 개요

- 브랜치: `feat/firecracker-process-management`
- 커밋: `9c74030 feat: manage Firecracker runtime processes`
- 상태: 구현 브랜치 존재
- 변경 규모: 4개 파일, 677줄 추가
- 목적: Rust API의 runtime service가 Firecracker를 실행하고 VM별 child process, PID identity, API socket, config 및 bounded console log를 관리한다.
- Week 4 Jailer 적용 뒤에는 같은 interface의 구현을 runtime helper로 교체함.

## Process manager

```rust
use tokio::{process::Child, sync::{mpsc, oneshot}};

pub enum ProcessCommand {
    Stop { reply: oneshot::Sender<Result<(), RuntimeError>> },
    Inspect { reply: oneshot::Sender<ProcessStatus> },
}

pub enum BootMode {
    Fresh { config: RuntimeArtifactPath },
    SnapshotRestore { snapshot_id: Uuid },
}

#[derive(Clone)]
pub struct ProcessHandle {
    pub identity: ProcessIdentity,
    commands: mpsc::Sender<ProcessCommand>,
}
```

- 각 runtime monitor task 하나가 `Child`를 단독 소유하고 `child.wait()`, bounded stop/inspect command channel과 readiness timeout을 `tokio::select!`로 처리함.
- start operation의 유한 launch job이 readiness 이후 Child/identity를 별도 `RuntimeMonitorRegistry`에 넘기고 완료됨.
- shared mutex 안의 `Child`를 여러 task가 동시에 wait/kill하지 않음.

- 실행 중에는 Linux `pidfd`를 사용해 PID 재사용 경쟁 없이 signal과 생존 여부를 확인함.
- DB에는 재시작 복구용 PID, `/proc/<pid>/stat` start tick과 binary device/inode를 저장하고 Week 4 Jailer 적용 뒤에는 VM cgroup/jail ID도 함께 저장함.
- `/proc` stat는 process 이름의 공백·괄호를 처리하는 검증된 parser를 사용함.

## 실행

```rust
let console = bounded_console_pipe(paths.console_log.clone(), config.console_limit_bytes)?;

let mut command = tokio::process::Command::new(&config.firecracker_binary);
command
    .arg("--api-sock")
    .arg(&paths.api_socket)
    .stdin(std::process::Stdio::null())
    .stdout(console.stdout)
    .stderr(console.stderr)
    .kill_on_drop(false);

if let BootMode::Fresh { config } = &boot_mode {
    command.arg("--config-file").arg(config);
}
let mut child = command.spawn()?;
```

- shell을 사용하지 않고 관리자 설정과 검증된 artifact 경로만 argument로 전달함.
- snapshot restore mode는 config file을 넘기지 않고 빈 process를 API-ready까지만 올림.
- spawn 전에 Firecrab 소유로 확인된 stale socket만 제거함.
- socket 파일 존재만 확인하지 말고 Unix socket 연결과 Firecracker API 응답까지 확인한 후 fresh boot만 Guest readiness를 거쳐 `running`으로 전환함.
- guest가 console output으로 disk를 채울 수 있으므로 ring buffer 또는 rotation이 있는 bounded writer를 사용함.

## 비동기 감시와 종료

- start handler는 상태, event와 operation을 transaction으로 만든 뒤 `OperationSupervisor`에 job을 등록하고 즉시 `202`를 반환함.
- launch job은 readiness와 operation 결과를 기록하고 runtime monitor는 이후 `child.wait()`와 stop command를 담당함.
- API shutdown에서 VM을 유지하면 monitor는 process identity를 fsync하고 runtime helper에 wait/reap 소유권이 있음을 확인한 뒤 API-side task만 종료함.
- helper가 없는 direct Week 2 runtime은 development 전용이며 orphan/reap 동작을 별도로 test함.

- stop은 vsock guest agent를 우선 사용함.
- x86_64에서 guest가 i8042/AT keyboard와 종료 target을 지원할 때만 `SendCtrlAltDel`을 fallback으로 사용함.
- timeout 뒤 pidfd 또는 복구 identity를 재확인하고 `SIGTERM`, 마지막 수단으로 `SIGKILL`을 사용함.

## 테스트 및 검증

- 실행 파일 없음, 잘못된 config, readiness timeout을 각각 `error` 상태와 event로 남긴다.
- 예상 종료와 crash를 구분한다.
- 같은 VM에 두 process가 실행되지 않는다.
- API 재시작 후 PID 재사용을 다른 Firecracker로 오인하지 않는다.
- 무제한 console output, stale socket과 supervisor/stop/exit 경쟁이 bounded하게 처리된다.

## 완료 및 후속 범위

- 구현 브랜치와 커밋이 존재함.
- 위 테스트 및 검증 항목을 모두 통과한 뒤 완료로 판정함.
