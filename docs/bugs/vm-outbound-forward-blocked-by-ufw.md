# VM에서 나가는 새 연결이 전부 타임아웃(패키지 업데이트로 발견)

패키지 업데이트 기능(`POST /api/vms/{id}/packages/update`, `docs/task-guest-network-configuration.md`의
콘솔 sentinel 방식 재사용) 실사용 검증 중 발견. Ubuntu의 `apt-get update`는 성공했는데 Alpine의
`apk update`는 매번 `TLS: unspecified error`로 실패했다 — 그런데 실제 원인은 TLS가 전혀 아니었다.

## 증상

- Alpine 게스트에서 `apk update`가 항상 실패, 출력에 `WARNING: ... TLS: unspecified error`
- 게스트에서 직접 `curl -v https://<아무 IP>/`를 실행하면 TLS 핸드셰이크는커녕 TCP 연결 자체가
  `Operation timed out` (약 127~135초 — 리눅스 커널의 SYN 재전송 타임아웃과 일치)
- 호스트에서 같은 IP:443으로 직접 `curl`하면 즉시 정상 접속됨 → CDN/라우팅 문제 아님
- `curl -4 -v http://<새 IP>/` (포트 80)도 동일하게 타임아웃 — **443 전용 문제가 아니라, 이전에
  한 번도 접속한 적 없는 목적지로의 "새" 아웃바운드 연결 전체**가 안 되는 것이었다. Ubuntu의
  `apt-get update`가 우연히 성공했던 건 그 목적지가 이미 다른 경로로 한 번 뚫려서 conntrack에
  `established` 상태가 있었기 때문으로 추정(재현 확인은 안 함 — 아래 수정 후에는 모든 새 목적지가
  똑같이 정상 동작하므로 더 팔 필요 없음)

## 원인: 호스트의 UFW가 "라우팅(forward)"을 기본적으로 막고 있었음

`docs/bugs/dhcp-never-reaches-guest.md` 원인 3(호스트 UFW가 67/53 포트를 막던 것)과 **완전히 같은
클래스의 문제**다 — 이번엔 호스트 자신을 향한 INPUT 체인이 아니라, VM에서 인터넷으로 나가는
FORWARD 체인에서 발생했다.

```
$ sudo ufw status verbose
기본 설정: deny (내부로 들어옴), allow (외부로 나감), deny (라우팅 된)
```

`라우팅 된: deny`(`DEFAULT_FORWARD_POLICY="DROP"`)가 기본값이고, `tcpdump`로 확인한 결과 guest의
SYN 패킷은 `fcbr0`까지는 정상 도달하는데 업링크(`enp3s0`)로는 전혀 안 나갔다:

```
table ip filter {
    chain FORWARD {
        type filter hook forward priority filter; policy drop;
        ...
        jump ufw-before-forward
    }
}
chain ufw-before-forward {
    ct state related,established accept
    ip protocol icmp icmp type echo-request accept   # ping이 되는 이유
    jump ufw-user-forward
}
chain ufw-user-forward {}   # 비어 있음 — 새 연결을 허용하는 규칙이 하나도 없었다
```

`inet firecrab` 테이블(이 프로젝트가 관리, `forward_dispatch`/`firecrab_egress`)은 `policy accept`에
egress 정책이 `internet`이면 무조건 accept라 문제가 없었다 — 하지만 **같은 forward 훅에 등록된
완전히 별개의 `ip filter FORWARD` 테이블(UFW)이 독자적으로 정책 drop을 갖고 있어서, 새 연결(ct
state `new`)이 `ufw-user-forward`의 빈 체인을 그냥 통과해 나가버리면 FORWARD의 기본 정책(drop)에
걸려 죽었다**. `established,related`나 `icmp echo-request`만 명시적으로 허용돼 있어서 ping은 되고
이미 뚫린 연결의 재사용은 되지만, 새로 여는 연결은 전부 막혔다.

## 수정 (호스트 설정, 코드 아님)

```sh
sudo ufw route allow in on fcbr0 out on enp3s0
```

`fcbr0`(VM 브리지)에서 `enp3s0`(실제 업링크)로 나가는 라우팅만 명시적으로 허용 — 다른 인터페이스
쌍은 영향받지 않는다. 실제 업링크 인터페이스명은 `ip route get 8.8.8.8`로 확인(이 개발 머신은
`enp3s0`). `docs/bugs/dhcp-never-reaches-guest.md` 원인 3의 INPUT 규칙들과 마찬가지로 **이 프로젝트의
어떤 스크립트도 이 규칙을 만들지 않으므로, 새 개발 머신에 처음 셋업할 때 수동으로 한 번 해줘야
한다**(아직 스크립트화 안 함).

## 검증

수정 전: 게스트에서 새로운 목적지로 `curl -4 -v https://8.8.8.8/`, `http://93.184.215.14/` 모두
타임아웃. 수정 후: 같은 명령이 즉시 성공(TLS 핸드셰이크 포함). 실제 기능으로도 재검증 —
Alpine VM에서 `POST /api/vms/{id}/packages/update` 실행, `apk update && apk upgrade`가 실제 CDN에서
패키지 인덱스를 받아와 `succeeded`로 종료(exit code 0) 확인.

## 산출물

코드 변경 없음 — 호스트 UFW 설정 한 줄. `docs/troubleshooting.md`에 요약 추가.
