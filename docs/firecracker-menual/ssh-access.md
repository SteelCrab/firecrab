# MicroVM SSH 접속

rootfs를 다시 만들면 guest SSH host key가 바뀔 수 있다.
이때 Host의 `known_hosts`에 남은 이전 키를 지운 뒤 다시 접속한다.

```sh
# 이전 guest host key 기록을 지운다.
ssh-keygen -f "$HOME/.ssh/known_hosts" -R '172.16.20.2'

# guest root 계정으로 다시 접속한다.
ssh root@172.16.20.2
```

접속 시 host key 확인 질문이 나오면 `yes`를 입력한다.
