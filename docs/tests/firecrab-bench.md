# firecrab-bench 검증

## 목차

- [자동 검증](#자동-검증)
- [수동 검증](#수동-검증)

## 자동 검증

```bash
cargo test -p firecrab-bench
```

- CLI 인자 파싱
- Boot·동시 생성·밀도·Lifecycle 실행 흐름
- 동시 작업 이후 VM 정리
- 밀도 실패 단계 조기 종료
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
cargo run -p firecrab-bench -- create --concurrency 10 --template <template> --micro-network-id <uuid>
cargo run -p firecrab-bench -- density --max-vms 50 --step 10 --template <template> --micro-network-id <uuid>
cargo run -p firecrab-bench -- lifecycle --iterations 20 --template <template> --micro-network-id <uuid>
```

- KVM 활성화 환경
- 등록된 템플릿 별칭
- 생성된 벤치마크 VM 자동 정리
- `vm_boot`·`concurrent_creation`·`vm_density`·`vm_lifecycle` JSON 결과
- `vm_per_second`·`max_stable_microvms`·`iterations_per_second` 지표
