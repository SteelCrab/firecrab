# firecrab-bench 검증

## 목차

- [자동 검증](#자동-검증)
- [수동 검증](#수동-검증)

## 자동 검증

```bash
cargo test -p firecrab-bench
```

- CLI 인자 파싱
- API 기본 주소 정규화
- 지연 시간 백분위 계산
- JSON 결과 실패율 계산

## 수동 검증

### 터미널 세션 1

```bash
cargo run -p firecrab-api
```

### 터미널 세션 2

```bash
cargo run -p firecrab-bench -- boot --count 5 --template <template> --micro-network-id <uuid>
```

- KVM 활성화 환경
- 등록된 템플릿 별칭
- 생성된 벤치마크 VM 자동 정리
- `vm_boot` JSON 결과
