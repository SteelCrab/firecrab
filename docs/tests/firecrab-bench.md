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
- Run ID·Commit·Branch·Timestamp·Host·Version 메타데이터
- Host CPU·메모리·Load Average·Firecracker 프로세스 수

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
cargo run -p firecrab-bench -- api --requests 1000 --concurrency 100
cargo run -p firecrab-bench -- network --target <iperf-server-ip> --duration 10
cargo run -p firecrab-bench -- storage --directory <benchmark-directory> --mode random-read --size-mib 1024
cargo run -p firecrab-bench -- soak --duration 1h --template <template> --micro-network-id <uuid>
cargo run -p firecrab-bench -- leak --iterations 100 --template <template> --micro-network-id <uuid>
cargo run -p firecrab-bench -- regression --baseline <baseline.json> --current <current.json> --metric p95_ms --threshold-percent 10
cargo run -p firecrab-bench -- --output result.json --publish api --requests 100 --concurrency 10
```

- KVM 활성화 환경
- 등록된 템플릿 별칭
- 생성된 벤치마크 VM 자동 정리
- `vm_boot`·`concurrent_creation`·`vm_density`·`vm_lifecycle` JSON 결과
- `vm_per_second`·`max_stable_microvms`·`iterations_per_second` 지표
- `requests_per_second`·`throughput_mbps`·`iops` 지표
- Network 사전 조건: 측정 대상의 `iperf3 -s`
- Storage 임시 파일: 명시한 디렉터리 내부 생성 후 자동 삭제
- Soak 시간·반복 상한 종료
- Firecracker·TAP·Network Namespace·File Descriptor 누수 차이
- 낮을수록 좋은 지연 시간과 높을수록 좋은 처리량 Regression 판정
- SQLite 결과 저장과 `GET /api/benchmarks` Commit history
- `#/benchmarks` Boot P95·Failure Rate·Host CPU 추세 그래프
- 전체 VM 상태별 개수와 현재 VM 자원·네트워크 테이블
- P50·P95·P99·Host 자원 실행 이력과 최신 명령별 지표 테이블
