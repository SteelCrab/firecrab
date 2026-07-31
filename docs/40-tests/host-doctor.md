---
tags:
  - firecrab
  - host
  - diagnostics
  - test
updated: 2026-07-31
---

# Host doctor 테스트

> [!summary] 한 줄 요약
> `scripts/firecrab-doctor.sh` / `./install.sh --doctor`가 비특권·무변경으로 돌고,
> 표의 네 가지 host 실패를 원인으로 지목하는지 확인한다.

## CI

`.github/workflows/ci.yml`의 `install` job이 다음을 돌린다.

| 단계 | 내용 |
|---|---|
| shellcheck | `install.sh` + `scripts/firecrab-doctor.sh` |
| 설치 전 | `./install.sh --doctor` 비특권, exit 0/1, `/var/lib/firecrab` 무변경 |
| 설치 후 | `sudo firecrab-doctor` — fail이 있으면 **images 관련만** 허용(`--no-images`). MicroNetwork 0개일 때 dnsmasq idle은 fail 아님 |
| uninstall | `/usr/local/bin/firecrab-doctor` 제거 확인 |

## 자동·스모크 (root 불필요)

```sh
bash -n scripts/firecrab-doctor.sh
./scripts/firecrab-doctor.sh              # 또는 ./install.sh --doctor
./scripts/firecrab-doctor.sh --help
```

- **무변경**: 실행 전후 `ls -la /var/lib/firecrab /run/firecrab` 메타가 같음(없으면 그대로 없음)
- **exit**: fail 없으면 `0`, 하나라도 fail이면 `1`
- **잡음**: 전부 ok면 `doctor: all checks passed (N ok)` 한 줄뿐 (항목별 PASS 없음)
- 권한 없는 `nft`/`ufw`는 `[SKIP]` + 조치 한 줄 (fail로 세지 않음)

## 실패 4종 재현 (완료 기준)

| # | 재현 | 기대 doctor 출력 |
|---|---|---|
| 1 | UFW active + 브리지에 67/udp allow 없음 | `[FAIL] ufw: bridge … missing allow 67/udp` + `sudo ufw allow…` |
| 2 | UFW forward deny + route allow 없음 | `[FAIL] ufw: no route allow <br> → <uplink>` + `sudo ufw route allow…` |
| 3 | helper 소켓 없음 또는 mode `600` | `[FAIL] helper socket: …` |
| 4 | cwd DB와 `$DATADIR/data/firecrab.db` 둘 다 존재 | `[FAIL] data: multiple firecrab.db files found` |

비파괴 스모크 예시:

```sh
# 3) missing socket
FIRECRAB_NET_HELPER_SOCK=/tmp/no-such-firecrab.sock ./scripts/firecrab-doctor.sh
# → FAIL helper socket, exit 1

# 4) multiple DBs
tmpdir=$(mktemp -d)
mkdir -p "$tmpdir/data" && cp data/firecrab.db "$tmpdir/data/" 2>/dev/null || touch "$tmpdir/data/firecrab.db"
DATADIR=$tmpdir ./scripts/firecrab-doctor.sh
# → FAIL data: multiple … ; rm -rf "$tmpdir"
```

UFW 1·2는 root로 `ufw status verbose`를 읽을 수 있을 때만 검증 가능. 규칙 추가는 호스트 전역이므로
**버릴 수 있는 머신**에서만 규칙을 빼 보거나, 출력 파서를 수동으로 대조한다.

```sh
sudo ./scripts/firecrab-doctor.sh   # 정상 호스트: all checks passed (9 ok)
```

## root로 돌릴 때

- `nft list tables`에 `inet firecrab`, `bridge firecrab_l2`가 있으면 ok
- UFW inactive → UFW 항목 pass(규칙 검사 생략)
- UFW active + 모든 mnb* (및 레거시 fcbr0)에 DHCP·DNS·route 있으면 pass
- MicroNetwork 0개: dnsmasq 미기동은 fail이 아님 (명시적 네트워크 모델)

## 완료 기준 대조

- 네 실패 지목 — 스모크 3·4 확인; 1·2는 UFW 규칙 있는 호스트에서 수동
- 전부 정상 시 통과 요약만 — **확인**
- root 없이 실행, 권한 항목 SKIP — **확인**

## 정리

변경 없음(진단만). 임시 DB/소켓 경로를 썼으면 삭제.
