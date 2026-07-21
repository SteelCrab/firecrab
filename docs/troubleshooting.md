# 트러블슈팅

증상별로 찾아보는 문제 해결 모음. 아래 표에서 증상을 찾아 해당 섹션으로 이동한다. 기능별 상세
검증 절차는 `docs/tests/`, 개별 버그의 전체 원인·수정 기록은 `docs/bugs/`에 있다.

## 한눈에 보기

| 증상 | 분류 |
|---|---|
| "API 연결 안 됨 — 15s 간격 재시도" | [대시보드/API](#대시보드api) |
| `localhost:8080`에서 안 뜨거나 403 | [대시보드/API](#대시보드api) |
| VM 상세 모달 "불러오는 중…" 계속 | [대시보드/API](#대시보드api) |
| VM 여러 대 동시 시작 시 일부가 starting에서 멈춤 | [VM 생성·시작](#vm-생성시작) |
| 디스크 공간 부족으로 생성/시작이 느리거나 실패 | [VM 생성·시작](#vm-생성시작) |
| 터미널 "연결 끊김"만 뜨고 안 붙음 | [터미널](#터미널) |
| 터미널 프롬프트에 `;1R;80R;1R;80R...` 반복 | [터미널](#터미널) |

## 대시보드/API

### "API 연결 안 됨 — 15s 간격 재시도"

- **원인**: `firecrab-api`가 죽었거나, 3번 연속 폴링 실패로 15초 간격 재시도 모드로 전환됨
  (`App.tsx`의 `SLOW_POLL_AFTER`/`SLOW_POLL_MILLIS`)
- **해결**: `firecrab-api`가 살아있는지 확인 — 서버가 다시 뜨면 자동으로 3초 폴링으로 복귀

### `localhost:8080`에서 안 뜨거나 API 요청이 403

- **원인**: Origin 불일치로 CORS가 거부함
- **해결**: 꼭 `localhost`로 접속(`127.0.0.1` 아님)

### VM 상세 모달이 계속 "불러오는 중…"

- **원인**: `firecrab-api` 미기동, 또는 VM id가 더 이상 존재하지 않음(삭제됨)
- **해결**: `firecrab-api` 재시작 여부·VM id 존재 여부 확인

## VM 생성·시작

### VM을 여러 대 동시에 시작하면 일부가 "starting"(특히 "디스크 준비")에서 멈춘 채 안 넘어간다

- **원인·수정**: [bugs/vm-startup-stuck-under-concurrent-load.md](bugs/vm-startup-stuck-under-concurrent-load.md) —
  템플릿 재해싱 중복 + 타임아웃된 요청의 future가 drop되며 VM이 고아 상태가 되는 버그, 둘 다 수정됨

### 호스트 디스크가 꽉 차서 VM 생성/시작이 느리거나 실패한다

- **원인**: VM 하나당 디스크(`data/vms/<id>/rootfs.ext4`)가 기본 2GiB — 테스트/재현용으로 VM을
  많이 만들면 금방 쌓인다. ext4는 여유 공간이 임계치 이하로 떨어지면 쓰기 성능이 급격히 나빠진다
- **해결**: `df -h`로 확인하고, 안 쓰는 VM은 `DELETE /api/vms/{id}`로 정리(디스크 파일도 같이 삭제됨)

## 터미널

### 터미널 버튼을 눌러도 "연결 끊김"만 뜨고 안 붙는다

- **원인**: 백엔드에 `/ws` 콘솔 라우트(`firecrab-api/src/console.rs`, `handlers/console.rs`)가
  없는 브랜치 — `feat/microvm-terminal`에서 구현, 이후 브랜치에 병합됨
- **해결**: 현재 브랜치에 병합됐는지 확인

### 터미널 프롬프트에 `;1R;80R;1R;80R...` 같은 게 반복 출력된다

- **원인·수정**: [bugs/terminal-cursor-position-echo-loop.md](bugs/terminal-cursor-position-echo-loop.md) —
  xterm.js의 커서 위치 응답이 guest tty에 echo되며 생기는 루프, 수정됨

## 기능별 상세 디버깅

| 기능 | 문서 |
|---|---|
| MicroVM 터미널 | [tests/microvm-terminal.md](tests/microvm-terminal.md) |
| VM 시작 단계별 진행 상황 | [tests/vm-startup-progress.md](tests/vm-startup-progress.md) |
| VM 상세 모달 | [tests/vm-detail-modal.md](tests/vm-detail-modal.md) |
| VM 디스크 용량 설정 | [tests/vm-disk-capacity.md](tests/vm-disk-capacity.md) |
| VM 리소스(CPU/RAM/DISK) 수정 | [tests/vm-resource-update.md](tests/vm-resource-update.md) |
| 프론트엔드 React 이전 | [tests/frontend-react-migration.md](tests/frontend-react-migration.md) |
