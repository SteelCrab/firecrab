# VM IP·MAC 할당(IPAM) 테스트

## 자동 테스트

```sh
cargo test -p firecrab-api ipam::
cargo test -p firecrab-api persistence::
```

## 확인 항목

- 중복 없음 — 순차 50개, 동시 16-thread
- 예약 주소 제외 — network(`.0`)·gateway(`.1`)·broadcast(`.255`) 안 나옴
- pool 고갈 — 253개 다 쓰면 `PoolExhausted`
- MAC 충돌 회피 — salt 증가로 재시도
- MAC pool 고갈 — 8회 다 막히면 `MacPoolExhausted`, 실패 시 row 안 남음(rollback)
- 중복 할당 거부 — 이미 active lease 있으면 `AlreadyLeased`
- 유령 release 거부 — active lease 없으면 `NotLeased`
- release 후 재사용 — 해제된 주소가 다음 할당에 나옴
- stop/start 무관 — lease는 create~delete 사이 그대로 유지(테스트에서는 재할당 시도가 `AlreadyLeased`로 막히는 것으로 확인)

## 수동 확인 (선택)

실제 DB 파일의 `network_leases` 테이블을 눈으로 보고 싶을 때. `network_leases` 테이블은 `Store::open`에서 생성되므로, 이 코드 반영 후 `firecrab-api`를 한 번도 실행한 적이 없다면 `no such table: network_leases` 오류가 난다 — `cargo run -p firecrab-api`를 한 번 띄웠다 내리면 해결된다.

```sh
python3 docs/tests/vm-ip-mac-allocation.py data/firecrab.db
```

sqlite3 CLI 없이 Python 표준 라이브러리로만 동작한다.
