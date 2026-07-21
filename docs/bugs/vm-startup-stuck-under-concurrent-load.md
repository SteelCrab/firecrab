# VM을 여러 대 동시에 시작하면 "starting"에서 멈춤

`docs/task-vm-startup-progress.md` 기능(시작 단계 표시)을 실사용하던 중 발견. 두 개의 별개
버그가 겹쳐 있었다 — 첫 번째만 고치고 나서도 재현됐다.

## 버그 1: 템플릿 재해싱 중복 (2026-07-21 수정)

**원인**: `run_start`가 실제 사용 직전 안전장치로 매번 템플릿(2GB)을 `open_verified`로 전체
재해싱하는데, 이건 VM마다 독립적으로 실행됨 — 10대를 동시에 시작하면 **똑같은 파일**을 10번
동시에 통째로 읽어 SHA256 해싱하느라 디스크 I/O와 CPU가 극심하게 경합해 "디스크 준비" 단계에서
멈춘 것처럼 보임(디스크 복사 자체는 VM마다 별도 파일이라 불가피하지만, 해싱은 전부 동일한 결과를
중복 계산하는 낭비).

**수정**: `TemplateRegistry`에 (device, inode, length, mtime) 키로 해시를 캐싱(`templates.rs`의
`hash_cached`) — 파일이 실제로 안 바뀐 한 재해싱하지 않는다. mtime까지 키에 포함해서 내용이 진짜
바뀌면(길이가 같아도) 여전히 감지됨(`cache_is_invalidated_by_a_same_length_content_change` 테스트).

**검증**: 실제 문제를 재현한 것과 동일한 VM 12대(이름 `0`~`9`, `11`, `12`)를 동시에 start 요청 →
전부 3.2초 안에 `running` 도달, 멈춘 VM 없음.

## 버그 2: 타임아웃된 요청의 future가 drop되며 VM이 영구 고아 상태가 됨 (2026-07-21 추가 수정)

**증상**: 버그 1 수정 뒤에도 신규 VM 여러 대를 동시에 시작하면 일부가 `starting`에서 영원히
멈춤(재시작해도 재현). 디스크 공간 부족(호스트가 한때 99% 참, 5.3GB만 남음)도 겹쳐 있었지만,
디스크를 넉넉히 비운 뒤에도 동일하게 재현됨 — 별개의 진짜 버그였음.

**원인**: 디스크 복사가 몰릴 때 순서대로 처리되도록 `run_start`에 동시 실행 제한(세마포어, 동시
2개)을 추가했는데, 그 대기 시간까지 합치면 `enforce_limits`의 요청 타임아웃(10초)을 쉽게 넘김.
axum이 타임아웃 시 handler의 future를 **drop**하는데, `start_vm`이 `run_start(...).await`를 그냥
중첩 호출하는 구조라 — spawn_blocking 작업 자체는 tokio 특성상 취소 없이 백그라운드에서 계속
돌지만(디스크 복사·설정 파일 생성은 실제로 끝까지 실행됨), **그 이후 이어서 실행됐어야 할
`set_startup_step(StartingProcess)`·`firecracker::spawn_vm`·`running` 전이 코드를 실행할 사람이
아무도 안 남음** — HTTP 요청 자체가 취소되면서 그 뒤에 이어질 예정이던 코드까지 통째로 사라짐.
결과: 디스크 복사는 됐는데 VM은 `starting`(`generatingConfig` 단계)에 영구히 멈춤.

**수정**: `start_vm`에서 claim 이후의 실제 작업(`run_start`+프로세스 등록+`running` 전이)을
`tokio::spawn`으로 완전히 분리(detach)함 — 요청이 타임아웃돼도 spawn된 task는 독립적으로 끝까지
실행됨. 빠른 경로(테스트에서처럼 즉시 끝나는 경우)는 handler가 그 결과를 그대로 기다렸다가
동기 응답처럼 반환하므로 기존 동작·테스트와 100% 동일.

**검증**: 신규 VM 10대를 동시에 시작 → 전부 클라이언트 쪽에서 10초 뒤 504(예상된 동작, 세마포어
대기 때문) → 아무 추가 조작 없이 그대로 관찰만 지속 → **2분 내에 10대 전부 `running` 도달**
(20초 간격 폴링으로 2→4→6→8→10대씩 순차 완료 확인). 예전 같으면 여기서 영구히 멈췄을 케이스.

## 산출물

`firecrab-api/src/templates.rs`(해시 캐싱), `firecrab-api/src/state.rs`(`disk_prep_permits`
세마포어), `firecrab-api/src/handlers/vms.rs`(`start_vm`의 `tokio::spawn` 분리)
